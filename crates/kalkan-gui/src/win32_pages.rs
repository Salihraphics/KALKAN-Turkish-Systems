//! Logs and Settings pages: native controls, their painting and actions.
//!
//! * Log list: a native owner-drawn **virtual** LISTBOX (`LBS_NODATA`), so
//!   scrolling, keyboard navigation and selection come from Windows and no
//!   per-row strings are stored in the control.
//! * Search and number fields: native EDIT controls (dark via WM_CTLCOLOR*).
//! * Chips, level segments, switches and header actions: owner-drawn BUTTONs
//!   (same hover/focus handling as the rest of the GUI).
//! * File writes (settings save, log export) run on a short-lived I/O thread;
//!   the result comes back as `WM_APP_IO`. Modal dialogs (Save As, reset
//!   confirmation) run *outside* the window-state borrow (see `Deferred`).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    SetBkColor, SetTextColor, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HDC,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON, ODT_LISTBOX,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetWindowTextLengthW, GetWindowTextW, MessageBoxW, MoveWindow, PostMessageW,
    SendMessageW, SetWindowTextW, ShowWindow, HMENU, IDYES, MB_DEFBUTTON2, MB_ICONWARNING,
    MB_YESNO, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CHILD, WS_TABSTOP, WS_VSCROLL,
};

use kalkan_config::{Config, LogLevel};
use kalkan_core::Command;
use kalkan_logging::{EntrySource, LogEntry, LoggingHandle, RotationPolicy};

use super::paint::{brush, Painter};
use super::{create_button, wide, win_err, Gdi, WindowState, HOVERED};
use crate::layout::Rect;
use crate::logview::{export_file_name, LevelChip, LogView};
use crate::model::{format_bytes, NoticeKind, Page};
use crate::settings::{changed_fields, save_message, Field, FileState, Kind, SettingsState};
use crate::theme::{self, scale, Rgb};
use crate::{GuiError, Services};

// ── Message and control constants (winuser.h) ────────────────────────────────
pub(super) const WM_APP_LOG: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
pub(super) const WM_APP_IO: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;
pub(super) const WM_CTLCOLOREDIT: u32 = 0x0133;
pub(super) const WM_CTLCOLORLISTBOX: u32 = 0x0134;
pub(super) const WM_CTLCOLORSTATIC: u32 = 0x0138;
pub(super) const WM_VKEYTOITEM: u32 = 0x002E;
const WM_SETFONT: u32 = 0x0030;
const EM_SETLIMITTEXT: u32 = 0x00C5;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const LB_GETTOPINDEX: u32 = 0x018E;
const LB_SETTOPINDEX: u32 = 0x0197;
const LB_SETITEMHEIGHT: u32 = 0x01A0;
const LB_SETCOUNT: u32 = 0x01A7;
const LBN_SELCHANGE: u16 = 1;
const EN_SETFOCUS: u16 = 0x0100;
const EN_KILLFOCUS: u16 = 0x0200;
const EN_CHANGE: u16 = 0x0300;
const ES_RIGHT: u32 = 0x0002;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_NUMBER: u32 = 0x2000;
const LBS_NOTIFY: u32 = 0x0001;
const LBS_OWNERDRAWFIXED: u32 = 0x0010;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0100;
const LBS_WANTKEYBOARDINPUT: u32 = 0x0400;
const LBS_NODATA: u32 = 0x2000;
const CF_UNICODETEXT: u32 = 13;
const VK_CONTROL: i32 = 0x11;

// ── Control ids ──────────────────────────────────────────────────────────────
const ID_LOG_CHIP: u16 = 300; // ..=303
const ID_LOG_SEARCH: u16 = 310;
const ID_LOG_LIST: u16 = 311;
const ID_LOG_PAUSE: u16 = 320;
const ID_LOG_COPY: u16 = 321;
const ID_LOG_EXPORT: u16 = 322;
const ID_SET_LEVEL: u16 = 400; // ..=404
const ID_SET_EDIT: u16 = 410; // 410..=412: size, keep, interval
const ID_SET_TOGGLE: u16 = 420; // 420..=422: file logging, start engine, start minimized
const ID_SET_SAVE: u16 = 430;
const ID_SET_REVERT: u16 = 431;
const ID_SET_RESET: u16 = 432;

const NUMBER_FIELDS: [Field; 3] = [
    Field::MaxFileSizeMb,
    Field::MaxLogFiles,
    Field::SampleIntervalMs,
];
const TOGGLE_FIELDS: [Field; 3] = [
    Field::FileLogging,
    Field::StartEngineOnLaunch,
    Field::StartMinimized,
];

/// Work that must run after the window-state borrow is released.
pub(super) enum Deferred {
    /// Show the Save As dialog, then export.
    Export,
    /// Ask for confirmation, then reset to defaults.
    ConfirmReset,
}

pub(super) enum IoResult {
    Saved {
        result: Result<(Config, Option<PathBuf>), String>,
    },
    Exported {
        result: Result<(PathBuf, usize), String>,
    },
}

pub(super) struct Controls {
    pub chips: [HWND; 4],
    pub search: HWND,
    pub list: HWND,
    pub pause: HWND,
    pub copy: HWND,
    pub export: HWND,
    pub levels: [HWND; 5],
    pub edits: [HWND; 3],
    pub toggles: [HWND; 3],
    pub save: HWND,
    pub revert: HWND,
    pub reset: HWND,
}

pub(super) struct Pages {
    pub ctl: Controls,
    pub log: LogView,
    pub logging: Option<Arc<LoggingHandle>>,
    pub settings: SettingsState,
    pub config_path: PathBuf,
    io_tx: Sender<IoResult>,
    io_rx: Receiver<IoResult>,
    io_threads: Vec<JoinHandle<()>>,
    pub io_busy: bool,
    /// Edit control that has keyboard focus (frame highlight).
    pub focused_edit: Option<HWND>,
    /// Background brush for inputs and the list (WM_CTLCOLOR*).
    input_brush: Gdi,
    list_brush: Gdi,
}

impl Pages {
    /// Track a short-lived helper thread so it is joined with the window.
    pub fn track_thread(&mut self, t: JoinHandle<()>) {
        self.io_threads.push(t);
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // Writes are small; joining makes sure no I/O thread outlives the GUI.
        for t in self.io_threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn create_control(parent: HWND, class: PCWSTR, style: u32, id: u16) -> Result<HWND, GuiError> {
    // SAFETY: valid parent; the control id travels in the HMENU parameter as
    // documented for child windows.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR::null(),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as usize as *mut core::ffi::c_void)),
            None,
            None,
        )
    }
    .map_err(|e| win_err("CreateWindowExW(control)", e))
}

fn set_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    // SAFETY: NUL-terminated buffer that outlives the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(w.as_ptr()));
    }
}

fn get_text(hwnd: HWND) -> String {
    // SAFETY: buffer sized from GetWindowTextLengthW (+1 for the NUL).
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

fn send(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    // SAFETY: synchronous message to one of our own child controls.
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp))).0 }
}

fn show(hwnd: HWND, visible: bool) {
    // SAFETY: plain call on our own child control.
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
    }
}

fn enable(hwnd: HWND, on: bool) {
    // SAFETY: plain call on our own child control.
    unsafe {
        let _ = EnableWindow(hwnd, on);
    }
}

fn place(hwnd: HWND, r: Rect) {
    // SAFETY: plain call on our own child control.
    unsafe {
        let _ = MoveWindow(hwnd, r.x, r.y, r.w, r.h, true);
    }
}

/// Put `text` on the clipboard as Unicode text.
pub(super) fn copy_to_clipboard(owner: HWND, text: &str) -> Result<(), String> {
    let data: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = data.len() * 2;
    // SAFETY: standard clipboard sequence on the UI thread. The global memory
    // block is sized for `data`, filled while locked, and ownership passes to
    // the system on a successful SetClipboardData.
    unsafe {
        OpenClipboard(Some(owner))
            .map_err(|e| format!("could not open the clipboard: {}", e.message()))?;
        let result = (|| {
            EmptyClipboard().map_err(|e| e.message())?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| e.message())?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                return Err("GlobalLock failed".to_owned());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).map_err(|e| e.message())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result.map_err(|e| format!("could not copy: {e}"))
    }
}

/// Read Unicode text from the clipboard (self-test verification).
pub(super) fn read_clipboard(owner: HWND) -> Option<String> {
    // SAFETY: standard clipboard read; the handle is only read while locked.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let out = GetClipboardData(CF_UNICODETEXT).ok().and_then(|h| {
            let p = GlobalLock(windows::Win32::Foundation::HGLOBAL(h.0)) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(h.0));
            Some(s)
        });
        let _ = CloseClipboard();
        out
    }
}

/// Modal Save As dialog. Must be called with no window-state borrow held.
pub(super) fn ask_export_path(owner: HWND) -> Option<PathBuf> {
    let name = export_file_name(kalkan_logging::buffer::now_ms());
    let mut file = [0u16; 1024];
    for (d, s) in file.iter_mut().zip(name.encode_utf16()) {
        *d = s;
    }
    let filter = wide("Text files (*.txt)\0*.txt\0All files (*.*)\0*.*\0");
    let ext = wide("txt");
    let title = wide("Export KALKAN logs");
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrDefExt: PCWSTR(ext.as_ptr()),
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every pointer in `ofn` refers to a buffer that outlives the
    // modal call; `file` receives the chosen path.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) }.as_bool();
    if !ok {
        return None;
    }
    let n = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(PathBuf::from(String::from_utf16_lossy(&file[..n])))
}

/// Modal confirmation. Must be called with no window-state borrow held.
pub(super) fn confirm_reset(owner: HWND) -> bool {
    let text = wide(
        "Reset all settings to their defaults?\n\nThe form is filled with the default values and \
         saved to kalkan.toml immediately.",
    );
    let caption = wide("Reset settings");
    // SAFETY: NUL-terminated strings that outlive the modal call.
    unsafe {
        MessageBoxW(
            Some(owner),
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
        ) == IDYES
    }
}

fn level_color(l: LogLevel) -> (Rgb, Rgb) {
    match l {
        LogLevel::Error => (theme::ERROR, theme::ERROR_SOFT),
        LogLevel::Warn => (theme::WARNING, theme::WARNING_SOFT),
        LogLevel::Info => (theme::PRIMARY, theme::PRIMARY_SOFT),
        LogLevel::Debug => (theme::UPLOAD, Rgb(0x15, 0x22, 0x36)),
        LogLevel::Trace => (theme::TEXT_MUTED, theme::NEUTRAL_SOFT),
    }
}

fn chip_level(c: LevelChip) -> LogLevel {
    match c {
        LevelChip::Error => LogLevel::Error,
        LevelChip::Warn => LogLevel::Warn,
        LevelChip::Info => LogLevel::Info,
        LevelChip::Debug => LogLevel::Debug,
    }
}

impl Pages {
    pub fn create(parent: HWND, services: Services) -> Result<Pages, GuiError> {
        let h = parent;
        let btn = |id: u16, text: &str| -> Result<HWND, GuiError> {
            // SAFETY: plain query.
            let inst = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
                .map_err(|e| win_err("GetModuleHandleW", e))?;
            create_button(h, inst.into(), id, text)
        };
        let chips = [
            btn(ID_LOG_CHIP, "Show errors")?,
            btn(ID_LOG_CHIP + 1, "Show warnings")?,
            btn(ID_LOG_CHIP + 2, "Show info")?,
            btn(ID_LOG_CHIP + 3, "Show debug")?,
        ];
        let search = create_control(h, windows::core::w!("EDIT"), ES_AUTOHSCROLL, ID_LOG_SEARCH)?;
        send(search, EM_SETLIMITTEXT, 200, 0);
        let list = create_control(
            h,
            windows::core::w!("LISTBOX"),
            WS_VSCROLL.0
                | LBS_NOTIFY
                | LBS_OWNERDRAWFIXED
                | LBS_NODATA
                | LBS_NOINTEGRALHEIGHT
                | LBS_WANTKEYBOARDINPUT,
            ID_LOG_LIST,
        )?;
        let pause = btn(ID_LOG_PAUSE, "Pause")?;
        let copy = btn(ID_LOG_COPY, "Copy")?;
        let export = btn(ID_LOG_EXPORT, "Export")?;
        let mut levels = [HWND::default(); 5];
        for (i, l) in LogLevel::ALL.iter().enumerate() {
            levels[i] = btn(ID_SET_LEVEL + i as u16, l.as_str())?;
        }
        let mut edits = [HWND::default(); 3];
        for (i, f) in NUMBER_FIELDS.iter().enumerate() {
            edits[i] = create_control(
                h,
                windows::core::w!("EDIT"),
                ES_AUTOHSCROLL | ES_NUMBER | ES_RIGHT,
                ID_SET_EDIT + i as u16,
            )?;
            send(edits[i], EM_SETLIMITTEXT, 6, 0);
            set_text(edits[i], services.settings.form.text(*f).unwrap_or(""));
        }
        let mut toggles = [HWND::default(); 3];
        for (i, f) in TOGGLE_FIELDS.iter().enumerate() {
            toggles[i] = btn(ID_SET_TOGGLE + i as u16, f.label())?;
        }
        let save = btn(ID_SET_SAVE, "Save")?;
        let revert = btn(ID_SET_REVERT, "Revert")?;
        let reset = btn(ID_SET_RESET, "Reset defaults")?;
        let (io_tx, io_rx) = mpsc::channel();
        Ok(Pages {
            ctl: Controls {
                chips,
                search,
                list,
                pause,
                copy,
                export,
                levels,
                edits,
                toggles,
                save,
                revert,
                reset,
            },
            log: LogView::new(),
            logging: services.logging,
            settings: services.settings,
            config_path: services.config_path,
            io_tx,
            io_rx,
            io_threads: Vec::new(),
            io_busy: false,
            focused_edit: None,
            input_brush: brush(theme::BG),
            list_brush: brush(theme::CARD),
        })
    }

    /// Fonts and row height for the native controls (creation and DPI change).
    pub fn apply_fonts(&self, body: &Gdi, dpi: u32) {
        for h in [
            self.ctl.search,
            self.ctl.list,
            self.ctl.edits[0],
            self.ctl.edits[1],
            self.ctl.edits[2],
        ] {
            send(h, WM_SETFONT, body.0 .0 as usize, 1);
        }
        send(
            self.ctl.list,
            LB_SETITEMHEIGHT,
            0,
            scale(theme::LOG_ROW_H, dpi) as isize,
        );
    }

    fn all_page_controls(&self) -> Vec<(HWND, Page)> {
        let c = &self.ctl;
        let mut v: Vec<(HWND, Page)> = c.chips.iter().map(|&h| (h, Page::Logs)).collect();
        v.extend([c.search, c.list, c.pause, c.copy, c.export].map(|h| (h, Page::Logs)));
        v.extend(c.levels.iter().map(|&h| (h, Page::Settings)));
        v.extend(c.edits.iter().map(|&h| (h, Page::Settings)));
        v.extend(c.toggles.iter().map(|&h| (h, Page::Settings)));
        v.extend([c.save, c.revert, c.reset].map(|h| (h, Page::Settings)));
        v
    }
}

impl WindowState {
    /// Move/show/hide/enable the page controls for the current layout.
    pub(super) fn layout_pages(&mut self) {
        let page = self.ui.page;
        let dpi = self.dpi;
        let l = &self.layout;
        let pg = &self.pages;
        for (h, p) in pg.all_page_controls() {
            show(h, p == page);
        }
        let s = |d| scale(d, dpi);
        match page {
            Page::Logs => {
                if let Some(r) = l.log_search {
                    // EDIT inside a painted frame: inset, vertically centred.
                    let eh = s(18);
                    place(
                        pg.ctl.search,
                        Rect::new(r.x + s(34), r.y + (r.h - eh) / 2, (r.w - s(44)).max(0), eh),
                    );
                }
                for (i, r) in l.log_chips.iter().enumerate() {
                    place(pg.ctl.chips[i], *r);
                }
                for (h, r) in [pg.ctl.pause, pg.ctl.copy, pg.ctl.export]
                    .into_iter()
                    .zip(&l.header_buttons)
                {
                    place(h, *r);
                }
                let empty = pg.log.visible_len() == 0;
                if let Some(r) = l.log_list {
                    place(pg.ctl.list, r.inset(1));
                }
                show(pg.ctl.list, !empty);
                enable(pg.ctl.copy, pg.log.selected_entry().is_some());
                enable(pg.ctl.export, !empty && !pg.io_busy);
            }
            Page::Settings => {
                for (i, f) in Field::ALL.iter().enumerate() {
                    let kind = match f.kind() {
                        Kind::Level => 0,
                        Kind::Number => 1,
                        Kind::Toggle => 2,
                    };
                    let Some(row) = l.form_row(i, kind, dpi) else {
                        continue;
                    };
                    let c = row.control;
                    match f.kind() {
                        Kind::Level => {
                            let (w, g) = (s(theme::SEGMENT_W), s(4));
                            for (k, h) in pg.ctl.levels.iter().enumerate() {
                                place(*h, Rect::new(c.x + k as i32 * (w + g), c.y, w, c.h));
                            }
                        }
                        Kind::Number => {
                            if let Some(k) = NUMBER_FIELDS.iter().position(|x| x == f) {
                                let eh = s(18);
                                place(
                                    pg.ctl.edits[k],
                                    Rect::new(c.x + s(10), c.y + (c.h - eh) / 2, c.w - s(20), eh),
                                );
                            }
                        }
                        Kind::Toggle => {
                            if let Some(k) = TOGGLE_FIELDS.iter().position(|x| x == f) {
                                place(pg.ctl.toggles[k], c);
                            }
                        }
                    }
                }
                for (h, r) in [pg.ctl.save, pg.ctl.revert, pg.ctl.reset]
                    .into_iter()
                    .zip(&l.header_buttons)
                {
                    place(h, *r);
                }
                let st = &pg.settings;
                let dirty = st.dirty();
                enable(
                    pg.ctl.save,
                    (dirty || matches!(st.file, FileState::Invalid(_))) && !st.saving,
                );
                enable(pg.ctl.revert, dirty && !st.saving);
                enable(
                    pg.ctl.reset,
                    !st.saving && (st.saved != Config::default() || dirty),
                );
            }
            _ => {}
        }
    }

    /// Re-read the log buffer (if visible and not paused) and sync the list.
    pub(super) fn refresh_logs(&mut self) {
        let Some(logging) = self.pages.logging.clone() else {
            return;
        };
        let buffer = logging.buffer();
        buffer.ack();
        if self.ui.page != Page::Logs {
            return;
        }
        let list = self.pages.ctl.list;
        let old_len = self.pages.log.visible_len();
        let top = send(list, LB_GETTOPINDEX, 0, 0).max(0) as usize;
        let rows = self
            .layout
            .log_list
            .map_or(1, |r| (r.h / scale(theme::LOG_ROW_H, self.dpi)).max(1))
            as usize;
        // Follow new entries only while the user is looking at the end.
        let at_end = old_len == 0 || top + rows >= old_len;
        if self
            .pages
            .log
            .refresh(buffer.version(), || buffer.snapshot())
        {
            self.sync_list(at_end);
        }
        self.invalidate();
    }

    /// Push the filtered row count and selection into the virtual list box.
    fn sync_list(&mut self, follow: bool) {
        let list = self.pages.ctl.list;
        let n = self.pages.log.visible_len();
        send(list, LB_SETCOUNT, n, 0);
        match self.pages.log.selected {
            Some(i) => {
                send(list, LB_SETCURSEL, i, 0);
            }
            None => {
                send(list, LB_SETCURSEL, usize::MAX, 0);
            }
        }
        if follow && n > 0 {
            send(list, LB_SETTOPINDEX, n - 1, 0);
        }
        self.relayout();
    }

    fn set_filter_and_refresh(&mut self) {
        self.pages.log.refilter();
        self.sync_list(self.pages.log.follow);
        self.invalidate_page_controls();
    }

    fn invalidate_page_controls(&self) {
        for (h, _) in self.pages.all_page_controls() {
            // SAFETY: repaint request for our own child.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(h), None, false);
            }
        }
    }

    /// Handle WM_COMMAND for page controls. Returns `None` if not ours.
    pub(super) fn on_page_command(&mut self, id: u16, code: u16) -> Option<Option<Deferred>> {
        match id {
            id if (ID_LOG_CHIP..ID_LOG_CHIP + 4).contains(&id) => {
                let chip = LevelChip::ALL[(id - ID_LOG_CHIP) as usize];
                let on = !self.pages.log.filter.shows(chip);
                self.pages.log.filter.set_shown(chip, on);
                self.set_filter_and_refresh();
            }
            ID_LOG_SEARCH => match code {
                EN_CHANGE => {
                    let q = get_text(self.pages.ctl.search);
                    self.pages.log.filter.set_query(&q);
                    self.set_filter_and_refresh();
                }
                EN_SETFOCUS => {
                    self.pages.focused_edit = Some(self.pages.ctl.search);
                    self.invalidate();
                }
                EN_KILLFOCUS => {
                    self.pages.focused_edit = None;
                    self.invalidate();
                }
                _ => {}
            },
            ID_LOG_LIST if code == LBN_SELCHANGE => {
                let sel = send(self.pages.ctl.list, LB_GETCURSEL, 0, 0);
                self.pages.log.selected = (sel >= 0).then_some(sel as usize);
                self.relayout();
            }
            ID_LOG_LIST => {}
            ID_LOG_PAUSE => {
                let paused = !self.pages.log.paused;
                self.pages.log.set_paused(paused);
                tracing::info!(
                    paused,
                    "log view {}",
                    if paused { "paused" } else { "resumed" }
                );
                if !paused {
                    self.refresh_logs();
                }
                self.invalidate_page_controls();
                self.invalidate();
            }
            ID_LOG_COPY => self.copy_selected_entry(),
            ID_LOG_EXPORT => return Some(Some(Deferred::Export)),
            id if (ID_SET_LEVEL..ID_SET_LEVEL + 5).contains(&id) => {
                self.pages.settings.form.log_level = LogLevel::ALL[(id - ID_SET_LEVEL) as usize];
                self.after_form_edit();
            }
            id if (ID_SET_EDIT..ID_SET_EDIT + 3).contains(&id) => {
                let k = (id - ID_SET_EDIT) as usize;
                let h = self.pages.ctl.edits[k];
                match code {
                    EN_CHANGE => {
                        let v = get_text(h);
                        self.pages.settings.form.set_text(NUMBER_FIELDS[k], v);
                        self.after_form_edit();
                    }
                    EN_SETFOCUS => {
                        self.pages.focused_edit = Some(h);
                        self.invalidate();
                    }
                    EN_KILLFOCUS => {
                        self.pages.focused_edit = None;
                        self.invalidate();
                    }
                    _ => {}
                }
            }
            id if (ID_SET_TOGGLE..ID_SET_TOGGLE + 3).contains(&id) => {
                let f = TOGGLE_FIELDS[(id - ID_SET_TOGGLE) as usize];
                let v = !self.pages.settings.form.toggle(f).unwrap_or(false);
                self.pages.settings.form.set_toggle(f, v);
                self.after_form_edit();
            }
            ID_SET_SAVE => self.save_settings(),
            ID_SET_REVERT => {
                self.pages.settings.revert();
                self.sync_form_controls();
                self.ui.page_notice = None;
                self.after_form_edit();
            }
            ID_SET_RESET => return Some(Some(Deferred::ConfirmReset)),
            _ => return None,
        }
        Some(None)
    }

    fn after_form_edit(&mut self) {
        // Live validation clears stale errors as the user fixes them.
        if !self.pages.settings.errors.is_empty() {
            self.pages.settings.errors = match self
                .pages
                .settings
                .form
                .to_config(&self.pages.settings.saved)
            {
                Ok(_) => Vec::new(),
                Err(e) => e,
            };
        }
        self.relayout();
        self.invalidate_page_controls();
    }

    /// Write form values into the native edits (after revert/reset/save).
    /// EN_CHANGE notifications triggered here arrive re-entrantly and are
    /// ignored (the form already holds these values).
    fn sync_form_controls(&mut self) {
        for (k, f) in NUMBER_FIELDS.iter().enumerate() {
            let v = self.pages.settings.form.text(*f).unwrap_or("").to_owned();
            set_text(self.pages.ctl.edits[k], &v);
        }
        self.invalidate_page_controls();
    }

    pub(super) fn copy_selected_entry(&mut self) {
        let Some(e) = self.pages.log.selected_entry().cloned() else {
            self.ui.set_page_notice(
                Page::Logs,
                NoticeKind::Warning,
                "Select a log entry to copy.",
            );
            self.relayout();
            return;
        };
        match copy_to_clipboard(self.hwnd, &e.to_line()) {
            Ok(()) => self.ui.set_page_notice(
                Page::Logs,
                NoticeKind::Info,
                "Log entry copied to the clipboard.",
            ),
            Err(err) => {
                tracing::warn!(error = %err, "copy to clipboard failed");
                self.ui.set_page_notice(Page::Logs, NoticeKind::Error, err)
            }
        }
        self.relayout();
    }

    fn spawn_io(&mut self, job: impl FnOnce() -> IoResult + Send + 'static) {
        let tx = self.pages.io_tx.clone();
        let target = self.hwnd.0 as isize;
        self.pages.io_busy = true;
        let spawned = std::thread::Builder::new()
            .name("kalkan-io".into())
            .spawn(move || {
                let r = job();
                let _ = tx.send(r);
                // SAFETY: PostMessageW is thread-safe; a dead window just fails.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(target as *mut core::ffi::c_void)),
                        WM_APP_IO,
                        WPARAM(0),
                        LPARAM(0),
                    );
                }
            });
        match spawned {
            Ok(h) => self.pages.io_threads.push(h),
            Err(e) => {
                self.pages.io_busy = false;
                tracing::error!(error = %e, "could not start the I/O thread");
                self.ui.set_page_notice(
                    self.ui.page,
                    NoticeKind::Error,
                    format!("Could not start the file operation: {e}"),
                );
            }
        }
    }

    /// Validate and save on the I/O thread.
    pub(super) fn save_settings(&mut self) {
        let st = &mut self.pages.settings;
        let config = match st.form.to_config(&st.saved) {
            Ok(c) => c,
            Err(errors) => {
                let summary = errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                tracing::warn!(%summary, "settings not saved: invalid values");
                st.errors = errors;
                self.ui.set_page_notice(
                    Page::Settings,
                    NoticeKind::Error,
                    format!("Not saved. {summary}"),
                );
                self.relayout();
                self.invalidate_page_controls();
                return;
            }
        };
        st.errors.clear();
        st.saving = true;
        let path = self.pages.config_path.clone();
        let backup_invalid = matches!(st.file, FileState::Invalid(_)) && path.is_file();
        self.spawn_io(move || {
            let result = (|| {
                let backup = if backup_invalid {
                    Some(Config::backup_file(&path).map_err(|e| e.to_string())?)
                } else {
                    None
                };
                config.save(&path).map_err(|e| e.to_string())?;
                Ok((config, backup))
            })();
            IoResult::Saved { result }
        });
        self.relayout();
    }

    /// After the user confirmed: defaults into the form, then save.
    pub(super) fn reset_settings_confirmed(&mut self) {
        tracing::info!("settings reset to defaults requested");
        self.pages.settings.load_defaults_into_form();
        self.sync_form_controls();
        self.save_settings();
    }

    /// Export the visible log rows to `path` on the I/O thread.
    pub(super) fn export_logs_to(&mut self, path: PathBuf) {
        let text = self.pages.log.export_text();
        let count = self.pages.log.visible_len();
        self.spawn_io(move || {
            let result = std::fs::write(&path, text.as_bytes())
                .map(|()| (path.clone(), count))
                .map_err(|e| format!("could not write {}: {e}", path.display()));
            IoResult::Exported { result }
        });
        self.relayout();
    }

    /// Results from the I/O thread (WM_APP_IO).
    pub(super) fn on_io(&mut self) {
        while let Ok(r) = self.pages.io_rx.try_recv() {
            self.pages.io_busy = false;
            match r {
                IoResult::Saved {
                    result: Ok((config, backup)),
                } => {
                    let changed = changed_fields(&self.pages.settings.saved, &config);
                    self.apply_live_settings(&config, &changed);
                    let mut msg = save_message(&changed);
                    if let Some(b) = backup {
                        msg.push_str(&format!(
                            " The previous (invalid) file was kept as {}.",
                            b.display()
                        ));
                    }
                    tracing::info!(changed = changed.len(), "settings saved");
                    self.pages.settings.saved_as(config);
                    self.sync_form_controls();
                    self.ui
                        .set_page_notice(Page::Settings, NoticeKind::Info, msg);
                }
                IoResult::Saved { result: Err(e) } => {
                    tracing::error!(error = %e, "settings could not be saved");
                    self.pages.settings.saving = false;
                    self.ui.set_page_notice(
                        Page::Settings,
                        NoticeKind::Error,
                        format!("Settings were not saved: {e}"),
                    );
                }
                IoResult::Exported {
                    result: Ok((path, n)),
                } => {
                    tracing::info!(path = %path.display(), entries = n, "logs exported");
                    self.ui.set_page_notice(
                        Page::Logs,
                        NoticeKind::Info,
                        format!("Exported {n} entries to {}", path.display()),
                    );
                }
                IoResult::Exported { result: Err(e) } => {
                    tracing::error!(error = %e, "log export failed");
                    self.ui.set_page_notice(
                        Page::Logs,
                        NoticeKind::Error,
                        format!("Export failed: {e}"),
                    );
                }
            }
        }
        // Finished threads have sent their result; join them now.
        let (done, running): (Vec<_>, Vec<_>) = self
            .pages
            .io_threads
            .drain(..)
            .partition(|t| t.is_finished());
        self.pages.io_threads = running;
        for t in done {
            let _ = t.join();
        }
        self.relayout();
        self.invalidate_page_controls();
    }

    /// Apply what can take effect immediately; the rest is reported.
    fn apply_live_settings(&mut self, c: &Config, changed: &[Field]) {
        if let Some(log) = &self.pages.logging {
            if changed.contains(&Field::LogLevel) {
                if let Err(e) = log.set_level(c.logging.level) {
                    tracing::error!(error = %e, "log level not applied");
                }
            }
            if changed.contains(&Field::MaxFileSizeMb) || changed.contains(&Field::MaxLogFiles) {
                log.set_rotation(RotationPolicy::from_mb(
                    c.logging.max_file_size_mb,
                    c.logging.max_log_files,
                ));
            }
        }
        if changed.contains(&Field::SampleIntervalMs) {
            self.send(Command::SetSampleInterval(
                std::time::Duration::from_millis(c.monitor.sample_interval_ms.into()),
            ));
        }
        self.info.log_level = c.logging.level.to_string();
        self.info.sample_interval_ms = c.monitor.sample_interval_ms;
    }

    /// WM_CTLCOLOR* for our inputs and list.
    pub(super) fn ctl_color(&self, msg: u32, hdc: HDC, ctl: HWND) -> Option<LRESULT> {
        let pg = &self.pages;
        let ours = ctl == pg.ctl.search || ctl == pg.ctl.list || pg.ctl.edits.contains(&ctl);
        if !ours {
            return None;
        }
        let (bg, b) = if msg == WM_CTLCOLORLISTBOX || ctl == pg.ctl.list {
            (theme::CARD, &pg.list_brush)
        } else {
            (theme::BG, &pg.input_brush)
        };
        // SAFETY: the DC passed with the message for this control.
        unsafe {
            SetTextColor(hdc, COLORREF(theme::TEXT.colorref()));
            SetBkColor(hdc, COLORREF(bg.colorref()));
        }
        Some(LRESULT(b.0 .0 as isize))
    }

    /// WM_VKEYTOITEM from the list: Ctrl+C copies the selected entry.
    pub(super) fn on_list_key(&mut self, vkey: u16) -> LRESULT {
        // SAFETY: plain keyboard state query.
        let ctrl = unsafe { GetKeyState(VK_CONTROL) } < 0;
        if ctrl && vkey == u16::from(b'C') {
            self.copy_selected_entry();
            return LRESULT(-2);
        }
        LRESULT(-1)
    }

    // ── Painting ─────────────────────────────────────────────────────────

    pub(super) fn paint_pages(&self, p: &Painter) {
        match self.ui.page {
            Page::Logs => self.paint_logs(p),
            Page::Settings => self.paint_settings(p),
            _ => {}
        }
    }

    fn input_frame(&self, p: &Painter, r: Rect, focused: bool, error: bool) {
        let border = if error {
            theme::ERROR
        } else if focused {
            theme::PRIMARY
        } else {
            theme::BORDER_STRONG
        };
        p.round_outline(r, theme::BG, border, 8);
    }

    fn paint_logs(&self, p: &Painter) {
        let l = &self.layout;
        let s = |d| p.s(d);
        if let Some(r) = l.log_search {
            self.input_frame(
                p,
                r,
                self.pages.focused_edit == Some(self.pages.ctl.search),
                false,
            );
            p.icon(
                crate::model::Icon::Search,
                r.x + s(10),
                r.y + (r.h - s(16)) / 2,
                theme::TEXT_MUTED,
            );
            if self.pages.log.filter.query().is_empty()
                && self.pages.focused_edit != Some(self.pages.ctl.search)
            {
                p.line(
                    "Search messages, targets, threads…",
                    Rect::new(r.x + s(36), r.y, r.w - s(44), r.h),
                    &self.fonts.body,
                    theme::TEXT_DISABLED,
                );
            }
        }
        let log = &self.pages.log;
        if let Some(r) = l.log_status {
            let (left, warn) = match self.pages.logging.as_ref().and_then(|h| h.file_status()) {
                Some(fs) => {
                    let mut t = format!(
                        "{}  ·  {}  ·  {} of {} archives  ·  rotates at {}",
                        fs.path.display(),
                        format_bytes(fs.size),
                        fs.archives,
                        fs.policy.keep,
                        format_bytes(fs.policy.max_bytes)
                    );
                    if let Some(e) = &fs.last_error {
                        t = format!("{t}  ·  {e}");
                    }
                    (t, fs.last_error.is_some())
                }
                None => (format!("No log file: {}", self.info.log_note), false),
            };
            let right = {
                let mut t = format!("{} of {} entries", log.visible_len(), log.total_len());
                if log.paused {
                    t.push_str("  ·  paused");
                }
                if let Some(h) = self.pages.logging.as_ref().map(|h| h.buffer().history()) {
                    if h.skipped_lines > 0 {
                        t.push_str(&format!("  ·  {} damaged line(s) skipped", h.skipped_lines));
                    }
                    if let Some(e) = h.error {
                        t.push_str(&format!("  ·  history: {e}"));
                    }
                }
                t
            };
            let rw = p.measure(&right, &self.fonts.small) + s(8);
            p.line(
                &left,
                Rect::new(r.x, r.y, (r.w - rw - s(12)).max(0), r.h),
                &self.fonts.small,
                if warn {
                    theme::WARNING
                } else {
                    theme::TEXT_MUTED
                },
            );
            p.text(
                &right,
                Rect::new(r.right() - rw, r.y, rw, r.h),
                &self.fonts.small,
                theme::TEXT_MUTED,
                windows::Win32::Graphics::Gdi::DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
            );
        }
        if let Some(r) = l.log_list {
            p.round_outline(r, theme::CARD, theme::BORDER, 6);
            if log.visible_len() == 0 {
                let msg = if self.pages.logging.is_none() {
                    "Logging is not available in this run."
                } else if log.total_len() == 0 {
                    if log.paused {
                        "Paused before any entries arrived."
                    } else {
                        "No log entries yet."
                    }
                } else {
                    "No entries match the search and level filters."
                };
                p.text(
                    msg,
                    Rect::new(r.x, r.y, r.w, r.h.min(s(120))),
                    &self.fonts.body,
                    theme::TEXT_MUTED,
                    windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
            }
        }
    }

    fn paint_settings(&self, p: &Painter) {
        let l = &self.layout;
        let s = |d| p.s(d);
        let st = &self.pages.settings;
        if let Some(r) = l.settings_status {
            p.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
            let inner = r.inset(s(16));
            p.icon(
                crate::model::Icon::File,
                inner.x,
                inner.y + s(1),
                theme::TEXT_MUTED,
            );
            let (badge, fg, bg, detail) = match &st.file {
                FileState::Loaded => ("LOADED", theme::TEXT_MUTED, theme::NEUTRAL_SOFT, st.path.clone()),
                FileState::Missing => ("DEFAULTS", theme::TEXT_MUTED, theme::NEUTRAL_SOFT, format!("{} (no file yet; Save creates it)", st.path)),
                FileState::Invalid(e) => ("INVALID", theme::ERROR, theme::ERROR_SOFT, format!("Using defaults; the file is untouched until you save (a .bak copy is kept). {e}")),
            };
            let bw = p.badge(badge, inner.right(), inner.y + s(9), fg, bg);
            p.line(
                "Configuration file",
                Rect::new(inner.x + s(24), inner.y, inner.w - bw - s(32), s(18)),
                &self.fonts.body_bold,
                theme::TEXT_MUTED,
            );
            let dc = if matches!(st.file, FileState::Invalid(_)) {
                theme::ERROR
            } else {
                theme::TEXT
            };
            p.line(
                &detail,
                Rect::new(inner.x, inner.y + s(20), inner.w, s(18)),
                &self.fonts.small,
                dc,
            );
        }
        if let Some(form) = l.settings_form {
            p.round_outline(form, theme::CARD, theme::BORDER, theme::RADIUS);
        }
        for (i, f) in Field::ALL.iter().enumerate() {
            let kind = match f.kind() {
                Kind::Level => 0,
                Kind::Number => 1,
                Kind::Toggle => 2,
            };
            let Some(row) = l.form_row(i, kind, self.dpi) else {
                continue;
            };
            if i > 0 {
                p.fill(
                    Rect::new(row.row.x + s(16), row.row.y, row.row.w - s(32), 1),
                    theme::BORDER,
                );
            }
            let t = row.text;
            p.line(
                f.label(),
                Rect::new(t.x, t.y + s(6), t.w, s(18)),
                &self.fonts.body_bold,
                theme::TEXT,
            );
            match st.error_for(*f) {
                Some(e) => p.line(
                    &e.message,
                    Rect::new(t.x, t.y + s(25), t.w, s(16)),
                    &self.fonts.small,
                    theme::ERROR,
                ),
                None => p.line(
                    &f.help(),
                    Rect::new(t.x, t.y + s(25), t.w, s(16)),
                    &self.fonts.small,
                    theme::TEXT_MUTED,
                ),
            }
            if f.kind() == Kind::Number {
                let k = NUMBER_FIELDS.iter().position(|x| x == f).unwrap_or(0);
                let focused = self.pages.focused_edit == Some(self.pages.ctl.edits[k]);
                self.input_frame(p, row.control, focused, st.error_for(*f).is_some());
            }
        }
    }

    /// Owner-draw for page controls. Returns `false` if the item is not ours.
    pub(super) fn draw_page_item(&self, dis: &DRAWITEMSTRUCT) -> bool {
        let p = Painter {
            hdc: dis.hDC,
            dpi: self.dpi,
            fonts: &self.fonts,
        };
        let rc = dis.rcItem;
        let r = Rect::new(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        let state = dis.itemState.0;
        let pressed = state & ODS_SELECTED.0 != 0;
        let disabled = state & ODS_DISABLED.0 != 0;
        let focus = state & ODS_FOCUS.0 != 0 && state & ODS_NOFOCUSRECT.0 == 0;
        let hovered = HOVERED.with(|h| h.get()) == dis.hwndItem.0 as isize && !disabled;
        let id = dis.CtlID as u16;
        let s = |d| p.s(d);

        if dis.CtlType == ODT_LISTBOX && id == ID_LOG_LIST {
            self.draw_log_row(&p, dis, r, pressed || state & ODS_SELECTED.0 != 0, focus);
            return true;
        }
        if dis.CtlType != ODT_BUTTON {
            return false;
        }
        match id {
            id if (ID_LOG_CHIP..ID_LOG_CHIP + 4).contains(&id) => {
                let chip = LevelChip::ALL[(id - ID_LOG_CHIP) as usize];
                let on = self.pages.log.filter.shows(chip);
                let (fg, soft) = level_color(chip_level(chip));
                p.fill(r, theme::BG);
                let count = self.pages.log.counts()[chip as usize];
                let label = format!("{} {}", chip.label(), count);
                if on {
                    p.round(r, if hovered { theme::CARD_HOVER } else { soft }, 8);
                    p.text(
                        &label,
                        r,
                        &self.fonts.small,
                        fg,
                        windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                    );
                } else {
                    p.round_outline(
                        r,
                        if hovered {
                            theme::CARD_HOVER
                        } else {
                            theme::BG
                        },
                        theme::BORDER_STRONG,
                        8,
                    );
                    p.text(
                        &label,
                        r,
                        &self.fonts.small,
                        theme::TEXT_DISABLED,
                        windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                    );
                }
                if focus {
                    p.ring(r, theme::PRIMARY, 8);
                }
            }
            id if (ID_SET_LEVEL..ID_SET_LEVEL + 5).contains(&id) => {
                let level = LogLevel::ALL[(id - ID_SET_LEVEL) as usize];
                let on = self.pages.settings.form.log_level == level;
                p.fill(r, theme::CARD);
                let (bg, border, fg) = if on {
                    (theme::PRIMARY_SOFT, theme::PRIMARY, theme::PRIMARY)
                } else if hovered {
                    (theme::CARD_HOVER, theme::BORDER_STRONG, theme::TEXT)
                } else {
                    (theme::CARD, theme::BORDER_STRONG, theme::TEXT_MUTED)
                };
                p.round_outline(r, bg, border, 6);
                p.text(
                    level.as_str(),
                    r,
                    &self.fonts.small,
                    fg,
                    windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
                if focus {
                    p.ring(r, theme::TEXT, 6);
                }
            }
            id if (ID_SET_TOGGLE..ID_SET_TOGGLE + 3).contains(&id) => {
                let f = TOGGLE_FIELDS[(id - ID_SET_TOGGLE) as usize];
                let on = self.pages.settings.form.toggle(f).unwrap_or(false);
                p.fill(r, theme::CARD);
                let th = s(22);
                let track = Rect::new(r.x, r.y + (r.h - th) / 2, r.w, th);
                let track_c = match (on, hovered) {
                    (true, true) => theme::PRIMARY_HOVER,
                    (true, false) => theme::PRIMARY,
                    (false, true) => theme::BORDER_STRONG,
                    (false, false) => theme::BORDER,
                };
                p.round(track, track_c, 11);
                let k = th - s(6);
                let kx = if on {
                    track.right() - s(3) - k
                } else {
                    track.x + s(3)
                };
                p.dot(
                    kx + k / 2,
                    track.y + th / 2,
                    k / 2,
                    if on {
                        theme::ON_PRIMARY
                    } else {
                        theme::TEXT_MUTED
                    },
                );
                if focus {
                    p.ring(track.inset(-s(2)), theme::TEXT, 13);
                }
            }
            ID_LOG_PAUSE | ID_LOG_COPY | ID_LOG_EXPORT | ID_SET_SAVE | ID_SET_REVERT
            | ID_SET_RESET => {
                let (label, primary) = match id {
                    ID_LOG_PAUSE => (
                        if self.pages.log.paused {
                            "Resume"
                        } else {
                            "Pause"
                        },
                        self.pages.log.paused,
                    ),
                    ID_LOG_COPY => ("Copy", false),
                    ID_LOG_EXPORT => (
                        if self.pages.io_busy {
                            "Exporting…"
                        } else {
                            "Export"
                        },
                        false,
                    ),
                    ID_SET_SAVE => (
                        if self.pages.settings.saving {
                            "Saving…"
                        } else {
                            "Save"
                        },
                        true,
                    ),
                    ID_SET_REVERT => ("Revert", false),
                    _ => ("Reset defaults", false),
                };
                p.fill(r, theme::BG);
                let (bg, border, fg) = match (primary, disabled, pressed, hovered) {
                    (_, true, _, _) => (theme::BG, theme::BORDER, theme::TEXT_DISABLED),
                    (true, false, true, _) => (
                        theme::PRIMARY_PRESSED,
                        theme::PRIMARY_PRESSED,
                        theme::ON_PRIMARY,
                    ),
                    (true, false, false, true) => (
                        theme::PRIMARY_HOVER,
                        theme::PRIMARY_HOVER,
                        theme::ON_PRIMARY,
                    ),
                    (true, false, false, false) => {
                        (theme::PRIMARY, theme::PRIMARY, theme::ON_PRIMARY)
                    }
                    (false, false, true, _) => (theme::BORDER, theme::BORDER_STRONG, theme::TEXT),
                    (false, false, false, true) => {
                        (theme::CARD_HOVER, theme::TEXT_MUTED, theme::TEXT)
                    }
                    (false, false, false, false) => {
                        (theme::CARD, theme::BORDER_STRONG, theme::TEXT)
                    }
                };
                p.round_outline(r, bg, border, 8);
                p.text(
                    label,
                    r,
                    &self.fonts.body_bold,
                    fg,
                    windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
                );
                if focus {
                    p.ring(
                        r.inset(s(2)),
                        if primary { theme::TEXT } else { theme::PRIMARY },
                        6,
                    );
                }
            }
            _ => return false,
        }
        true
    }

    fn draw_log_row(
        &self,
        p: &Painter,
        dis: &DRAWITEMSTRUCT,
        r: Rect,
        selected: bool,
        focus: bool,
    ) {
        let s = |d| p.s(d);
        let idx = dis.itemID as usize;
        let bg = if selected {
            theme::PRIMARY_SOFT
        } else {
            theme::CARD
        };
        p.fill(r, bg);
        let Some(e) = self.pages.log.row(idx) else {
            return;
        };
        let e: &LogEntry = e;
        if e.level == LogLevel::Error && !selected {
            p.fill(Rect::new(r.x, r.y, s(3), r.h), theme::ERROR);
        }
        let mut x = r.x + s(10);
        let time = kalkan_logging::format_utc(e.unix_ms);
        let tw = s(150);
        p.line(
            &time,
            Rect::new(x, r.y, tw, r.h),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
        x += tw + s(6);
        let (fg, soft) = level_color(e.level);
        let lw = s(48);
        let pill = Rect::new(x, r.y + s(4), lw, r.h - s(8));
        p.round(pill, soft, 6);
        p.text(
            &e.level.as_str().to_uppercase(),
            pill,
            &self.fonts.small,
            fg,
            windows::Win32::Graphics::Gdi::DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        x += lw + s(10);
        // The target column is dropped on narrow windows to keep messages readable.
        if r.w > s(720) {
            let gw = s(170);
            p.line(
                &e.target,
                Rect::new(x, r.y, gw, r.h),
                &self.fonts.small,
                theme::TEXT_MUTED,
            );
            x += gw + s(8);
        }
        let mc = match e.source {
            EntrySource::History => theme::TEXT_MUTED,
            EntrySource::Live => theme::TEXT,
        };
        let first_line = e.message.lines().next().unwrap_or("");
        p.text(
            first_line,
            Rect::new(x, r.y, (r.right() - x - s(8)).max(0), r.h),
            &self.fonts.body,
            mc,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS,
        );
        if focus && selected {
            p.ring(r, theme::PRIMARY, 2);
        }
    }
}
