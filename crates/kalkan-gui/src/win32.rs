//! Win32 implementation: main window, message loop, GDI painting.
//!
//! Design notes
//! * One top-level window. Interactive elements (sidebar entries, Start, Stop)
//!   are owner-drawn native `BUTTON` controls. They keep keyboard focus, Tab
//!   order (`IsDialogMessageW`) and accessibility names while we draw them.
//! * Static content is painted in `WM_PAINT` into a temporary back buffer and
//!   then blitted in one step, so there is no flicker.
//! * `GetMessageW` blocks, so the app uses no CPU while idle. There are no timers.
//! * Per-window state lives in a `RefCell` behind `GWLP_USERDATA`. A message
//!   that arrives while the state is borrowed (re-entrancy) goes to
//!   `DefWindowProcW` instead of aliasing the borrow.
//! * Panics are caught at the window procedure boundary. The app then shuts
//!   down cleanly instead of unwinding into the OS.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, DeleteDC,
    DeleteObject, EndPaint, GetDC, GetDIBits, InvalidateRect, RedrawWindow, ReleaseDC,
    SelectObject, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    DEFAULT_CHARSET, DIB_RGB_COLORS, FW_NORMAL, FW_SEMIBOLD, HDC, HGDIOBJ, LOGFONTW, PAINTSTRUCT,
    RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW,
    GetClientRect, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    IsDialogMessageW, KillTimer, LoadCursorW, MessageBoxW, MoveWindow, PeekMessageW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SendMessageW, SetCursor, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, BM_CLICK, BS_OWNERDRAW, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, GWLP_WNDPROC, HICON, HMENU, ICON_BIG, ICON_SMALL,
    IDC_ARROW, IDC_HAND, MB_ICONERROR, MB_OK, MINMAXINFO, MSG, PM_REMOVE, SM_CXICON, SM_CXSMICON,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SW_SHOW,
    SW_SHOWMINNOACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_MOUSEMOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR, WM_SETICON, WM_SIZE, WM_TIMER, WNDCLASSEXW,
    WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use kalkan_core::{Command, EngineLink, EngineState};

use crate::layout::{self, Content, Layout};
use crate::model::{page_view, AppInfo, NoticeKind, Page, PageView, UiState};
use crate::theme::{self, scale};
use crate::{Check, GuiError, RunOptions, SelfTestReport};

#[path = "win32_pages.rs"]
mod pages;
#[path = "win32_paint.rs"]
mod paint;

const CLASS_NAME: PCWSTR = w!("KalkanMainWindow");

thread_local! {
    /// The owner-drawn button under the mouse (0 = none). UI thread only.
    static HOVERED: Cell<isize> = const { Cell::new(0) };
}
/// Posted by an engine link when events are waiting (used from Phase 3).
pub const WM_APP_ENGINE: u32 = WM_APP + 1;

const ID_NAV_BASE: u16 = 100;
const ID_START: u16 = 200;
const ID_STOP: u16 = 201;
/// One-shot timer armed only while closing: the bound on waiting for the
/// engine's shutdown confirmation.
const CLOSE_TIMER_ID: usize = 1;
const CLOSE_TIMEOUT_MS: u32 = 7_000;

// ── Small RAII wrappers ──────────────────────────────────────────────────────

/// An owned GDI object, deleted on drop.
struct Gdi(HGDIOBJ);

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: we created this object and nothing else owns it. It is not
        // selected into any DC when dropped (selection is always restored).
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

struct Fonts {
    title: Gdi,
    value: Gdi,
    brand: Gdi,
    body: Gdi,
    body_bold: Gdi,
    small: Gdi,
}

impl Fonts {
    fn new(dpi: u32) -> Fonts {
        let mk = |pt: i32, weight: i32| -> Gdi {
            let mut lf = LOGFONTW {
                // Negative height = character height in pixels: pt * dpi / 72.
                lfHeight: -((pt * dpi as i32 + 36) / 72),
                lfWeight: weight,
                lfCharSet: DEFAULT_CHARSET,
                lfQuality: CLEARTYPE_QUALITY,
                ..Default::default()
            };
            for (dst, src) in lf
                .lfFaceName
                .iter_mut()
                .zip(theme::FONT_FACE.encode_utf16())
            {
                *dst = src;
            }
            // SAFETY: `lf` is a fully initialised LOGFONTW on the stack.
            Gdi(unsafe { CreateFontIndirectW(&lf) }.into())
        };
        let (n, b) = (FW_NORMAL.0 as i32, FW_SEMIBOLD.0 as i32);
        Fonts {
            title: mk(theme::FONT_TITLE_PT, b),
            value: mk(theme::FONT_VALUE_PT, b),
            brand: mk(theme::FONT_BRAND_PT, b),
            body: mk(theme::FONT_BODY_PT, n),
            body_bold: mk(theme::FONT_BODY_PT, b),
            small: mk(theme::FONT_SMALL_PT, n),
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn win_err(call: &'static str, e: windows::core::Error) -> GuiError {
    GuiError::Win32 {
        call,
        message: e.message(),
    }
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

// ── Window state ─────────────────────────────────────────────────────────────

struct WindowState {
    hwnd: HWND,
    dpi: u32,
    fonts: Fonts,
    nav: [HWND; 5],
    start_btn: HWND,
    stop_btn: HWND,
    ui: UiState,
    info: AppInfo,
    engine: Box<dyn EngineLink>,
    view: PageView,
    layout: Layout,
    /// Set by the engine waker, cleared before draining; coalesces wake-ups
    /// so a burst of events posts at most one WM_APP_ENGINE.
    wake_pending: Arc<AtomicBool>,
    /// WM_CLOSE received; waiting for the engine's ShutdownComplete.
    closing: bool,
    /// The engine did not confirm in time; close anyway.
    close_forced: bool,
    /// Window/taskbar icons created at runtime; destroyed on drop.
    icons: Vec<HICON>,
    /// Logs and Settings page controls and state.
    pages: pages::Pages,
    /// Set to `true` when the state is dropped (clean shutdown check).
    dropped: Rc<Cell<bool>>,
    /// Records whether the engine confirmed shutdown before the window died.
    shutdown_confirmed: Rc<Cell<bool>>,
}

impl Drop for WindowState {
    fn drop(&mut self) {
        self.shutdown_confirmed.set(self.ui.shutdown_complete);
        self.dropped.set(true);
        for icon in self.icons.drain(..) {
            // SAFETY: icons we created with CreateIconIndirect; the window
            // that used them is being destroyed.
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
        // Dropping `engine` joins the engine threads (EngineHandle::drop).
    }
}

/// Data passed through `CreateWindowExW` to `WM_NCCREATE`.
struct CreateParams {
    info: AppInfo,
    engine: Option<Box<dyn EngineLink>>,
    services: Option<crate::Services>,
    dropped: Rc<Cell<bool>>,
    shutdown_confirmed: Rc<Cell<bool>>,
    panicked: Rc<Cell<bool>>,
}

/// What `GWLP_USERDATA` points at.
struct Slot {
    state: RefCell<Option<WindowState>>,
    panicked: Rc<Cell<bool>>,
}

impl WindowState {
    fn content(&self) -> Content {
        Content {
            engine_panel: self.view.engine_panel,
            notice: self.ui.visible_notice().is_some(),
            tiles: self.view.tiles.len(),
            cards: self.view.cards.len(),
            strip: !self.view.strip.is_empty(),
            table_rows: self.view.table.as_ref().map(|t| t.rows.len()),
            note: self.view.note.is_some(),
            header_buttons: self.view.header_buttons,
            log_viewer: self.view.log_viewer,
            settings_rows: self.view.settings_rows,
        }
    }

    fn client_size(&self) -> (i32, i32) {
        let mut rc = RECT::default();
        // SAFETY: valid window handle; `rc` is a valid out-pointer.
        let _ = unsafe { GetClientRect(self.hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    /// Recompute view + layout and move/show/enable child controls to match.
    fn relayout(&mut self) {
        self.view = page_view(self.ui.page, &self.ui, &self.info);
        let (w, h) = self.client_size();
        self.layout = layout::compute(w, h, self.dpi, self.content());
        let l = &self.layout;
        // SAFETY: all handles are our live child windows.
        unsafe {
            for (hwnd, r) in self.nav.iter().zip(l.nav.iter()) {
                let _ = MoveWindow(*hwnd, r.x, r.y, r.w, r.h, false);
            }
            let show = if self.view.engine_panel {
                SW_SHOW
            } else {
                SW_HIDE
            };
            for (hwnd, r) in [(self.start_btn, l.start_btn), (self.stop_btn, l.stop_btn)] {
                if let Some(r) = r {
                    let _ = MoveWindow(hwnd, r.x, r.y, r.w, r.h, false);
                }
                let _ = ShowWindow(hwnd, show);
            }
            // Buttons mirror the engine's own transition table; both are
            // disabled while the application is closing.
            let open = !self.closing;
            let _ = EnableWindow(self.start_btn, open && self.ui.can_start());
            let _ = EnableWindow(self.stop_btn, open && self.ui.can_stop());
        }
        self.layout_pages();
        self.invalidate();
    }

    fn invalidate(&self) {
        // SAFETY: valid window handle; NULL region = whole window.
        unsafe {
            let _ = RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_ALLCHILDREN,
            );
        }
    }

    fn send(&mut self, cmd: Command) {
        tracing::debug!(?cmd, "UI -> engine");
        self.engine.send(cmd);
        self.drain_events();
    }

    /// Apply all queued engine events. Never blocks.
    fn drain_events(&mut self) {
        // Clear first: an event queued while draining posts a new wake-up.
        self.wake_pending.store(false, Ordering::Release);
        let (mut structural, mut metrics) = (false, false);
        while let Some(ev) = self.engine.try_recv() {
            tracing::debug!(?ev, "engine -> UI");
            let is_status = matches!(ev, kalkan_core::Event::Status(_));
            let changed = self.ui.apply(ev);
            if is_status {
                metrics |= changed;
            } else {
                structural |= changed;
            }
        }
        // Periodic metric updates only repaint pages that show live data.
        let live_page = matches!(
            self.ui.page,
            Page::Dashboard | Page::Network | Page::Statistics
        );
        if structural || (metrics && live_page) {
            self.relayout();
        }
        if self.closing && self.ui.shutdown_complete {
            tracing::info!("engine confirmed shutdown; closing the window");
            self.post_close();
        }
    }

    fn post_close(&self) {
        // SAFETY: posting to our own window; WM_CLOSE runs outside this borrow.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    /// First WM_CLOSE: ask the engine to shut down and wait (asynchronously)
    /// for its confirmation. Returns `true` when the window may be destroyed.
    fn on_close(&mut self) -> bool {
        if self.ui.shutdown_complete || self.close_forced {
            return true;
        }
        if !self.closing {
            tracing::info!(state = %self.ui.engine, "window closing; requesting engine shutdown");
            self.closing = true;
            // SAFETY: one-shot timer on our own window, killed in WM_TIMER.
            unsafe { SetTimer(Some(self.hwnd), CLOSE_TIMER_ID, CLOSE_TIMEOUT_MS, None) };
            self.send(Command::Shutdown);
            self.relayout();
        }
        false
    }

    fn on_command(&mut self, id: u16) {
        match id {
            ID_START => self.send(Command::Start),
            ID_STOP => self.send(Command::Stop),
            id if (ID_NAV_BASE..ID_NAV_BASE + 5).contains(&id) => {
                if let Some(p) = Page::from_index((id - ID_NAV_BASE) as usize) {
                    if self.ui.navigate(p) {
                        self.relayout();
                        if p == Page::Logs {
                            self.refresh_logs();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn on_dpi_changed(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.fonts = Fonts::new(dpi);
        self.pages.apply_fonts(&self.fonts.body, dpi);
        // Re-layout now (the window size may not change, so WM_SIZE may not
        // follow); WM_SIZE re-lays out again if it does.
        self.relayout();
    }
}

// ── Window procedure ─────────────────────────────────────────────────────────

fn slot<'a>(hwnd: HWND) -> Option<&'a Slot> {
    // SAFETY: GWLP_USERDATA is either 0 or a pointer to a `Slot` that we
    // leaked in WM_NCCREATE and only reclaim in WM_NCDESTROY, after which it
    // is reset to 0. All access is on this window's (single) UI thread.
    unsafe {
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Slot;
        p.as_ref()
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, wp, lp)));
    match result {
        Ok(r) => r,
        Err(_) => {
            tracing::error!(msg, "panic in window procedure; closing the window");
            if let Some(s) = slot(hwnd) {
                s.panicked.set(true);
            }
            // SAFETY: valid hwnd; posting is always safe.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
    }
}

fn def(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: forwarding the original message parameters unchanged.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: for WM_NCCREATE, lParam points to a CREATESTRUCTW whose
            // lpCreateParams is the `*mut CreateParams` we passed to
            // CreateWindowExW. `run` keeps it alive until the call returns.
            let cp = unsafe {
                &*((*(lp.0 as *const CREATESTRUCTW)).lpCreateParams as *const CreateParams)
            };
            let s = Box::new(Slot {
                state: RefCell::new(None),
                panicked: cp.panicked.clone(),
            });
            // SAFETY: storing our own pointer; reclaimed in WM_NCDESTROY.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(s) as isize) };
            def(hwnd, msg, wp, lp)
        }
        WM_CREATE => match on_create(hwnd, lp) {
            Ok(()) => LRESULT(0),
            Err(e) => {
                tracing::error!(%e, "failed to create window contents");
                LRESULT(-1)
            }
        },
        WM_CLOSE => {
            // Two-step close: the first WM_CLOSE asks the engine to shut down;
            // the window is destroyed only after ShutdownComplete (or the
            // close timeout). The borrow is released *before* DestroyWindow:
            // destruction re-enters this procedure and WM_NCDESTROY frees the
            // slot.
            let destroy = match slot(hwnd).map(|s| s.state.try_borrow_mut()) {
                Some(Ok(mut g)) => match g.as_mut() {
                    Some(st) => st.on_close(),
                    None => true,
                },
                Some(Err(_)) => {
                    // Re-entrant WM_CLOSE: retry once the current handler is done.
                    // SAFETY: posting to our own window.
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    false
                }
                None => true,
            };
            if destroy {
                // SAFETY: destroying our own window; no borrows are held.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: always safe.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: reclaim the Slot leaked in WM_NCCREATE exactly once. No
            // borrow of it is live: the only path to window destruction
            // (WM_CLOSE above) releases its borrow first.
            unsafe {
                let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut Slot;
                if !p.is_null() {
                    drop(Box::from_raw(p));
                }
            }
            def(hwnd, msg, wp, lp)
        }
        _ => {
            let Some(slot) = slot(hwnd) else {
                return def(hwnd, msg, wp, lp);
            };
            let Ok(mut guard) = slot.state.try_borrow_mut() else {
                // Re-entrant message while we are handling another one. Our
                // own wake-up messages must not be lost (their senders only
                // post again after we drain): retry them later.
                if matches!(msg, WM_APP_ENGINE | pages::WM_APP_LOG | pages::WM_APP_IO) {
                    // SAFETY: posting to our own window.
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), msg, wp, lp);
                    }
                    return LRESULT(0);
                }
                return def(hwnd, msg, wp, lp);
            };
            let Some(st) = guard.as_mut() else {
                return def(hwnd, msg, wp, lp);
            };
            let outcome = handle_with_state(st, hwnd, msg, wp, lp);
            // Release the borrow BEFORE anything that can re-enter this
            // procedure. DefWindowProcW does: e.g. WM_WINDOWPOSCHANGING sends
            // WM_GETMINMAXINFO, and WM_WINDOWPOSCHANGED sends WM_SIZE. Both
            // must see the state, or minimum size and re-layout are lost.
            drop(guard);
            match outcome {
                Outcome::Done(r) => r,
                Outcome::Default => def(hwnd, msg, wp, lp),
                Outcome::Deferred(d) => {
                    run_deferred(hwnd, d);
                    LRESULT(0)
                }
                Outcome::MoveTo(r) => {
                    // SAFETY: valid window handle; no borrows are held, so the
                    // WM_SIZE this triggers re-lays out normally.
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    LRESULT(0)
                }
            }
        }
    }
}

/// Subclass procedure for our owner-drawn buttons: hover tracking and a hand
/// cursor. The original BUTTON procedure is kept in the button's
/// GWLP_USERDATA (unused by BUTTON) and called for everything.
extern "system" fn button_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let me = hwnd.0 as isize;
    // SAFETY: all calls target `hwnd`, a live button we subclassed on the UI
    // thread; GWLP_USERDATA holds its original window procedure.
    unsafe {
        match msg {
            WM_MOUSEMOVE if HOVERED.with(|h| h.get()) != me => {
                let prev = HOVERED.with(|h| h.replace(me));
                if prev != 0 {
                    let _ = InvalidateRect(Some(HWND(prev as *mut c_void)), None, false);
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            WM_MOUSELEAVE if HOVERED.with(|h| h.get()) == me => {
                HOVERED.with(|h| h.set(0));
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            WM_SETCURSOR => {
                SetCursor(LoadCursorW(None, IDC_HAND).ok());
                return LRESULT(1);
            }
            _ => {}
        }
        let orig = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        let orig: WNDPROC = std::mem::transmute::<isize, WNDPROC>(orig);
        CallWindowProcW(orig, hwnd, msg, wp, lp)
    }
}

fn create_button(parent: HWND, hinst: HINSTANCE, id: u16, text: &str) -> Result<HWND, GuiError> {
    let text = wide(text);
    // SAFETY: valid parent; `text` outlives the call; the control ID travels in
    // the HMENU parameter as documented for child windows.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as usize as *mut c_void)),
            Some(hinst),
            None,
        )
    }
    .map_err(|e| win_err("CreateWindowExW(BUTTON)", e))
    .inspect(|&btn| {
        // SAFETY: subclassing our own new child; the original procedure is
        // stored first so `button_proc` can always forward to it.
        unsafe {
            let orig = GetWindowLongPtrW(btn, GWLP_WNDPROC);
            SetWindowLongPtrW(btn, GWLP_USERDATA, orig);
            SetWindowLongPtrW(btn, GWLP_WNDPROC, button_proc as *const () as isize);
        }
    })
}

fn on_create(hwnd: HWND, lp: LPARAM) -> Result<(), GuiError> {
    // SAFETY: for WM_CREATE, lParam points to a CREATESTRUCTW whose
    // lpCreateParams is the live `*mut CreateParams` owned by `run`.
    let cp =
        unsafe { ((*(lp.0 as *const CREATESTRUCTW)).lpCreateParams as *mut CreateParams).as_mut() };
    let Some(cp) = cp else {
        return Err(GuiError::Win32 {
            call: "WM_CREATE",
            message: "missing create params".into(),
        });
    };
    // SAFETY: plain query on our own window.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    // SAFETY: None = handle of the current executable.
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| win_err("GetModuleHandleW", e))?
        .into();

    let mut nav = [HWND::default(); 5];
    for (i, p) in Page::ALL.iter().enumerate() {
        nav[i] = create_button(hwnd, hinst, ID_NAV_BASE + i as u16, p.label())?;
    }
    let start_btn = create_button(hwnd, hinst, ID_START, "Start")?;
    let stop_btn = create_button(hwnd, hinst, ID_STOP, "Stop")?;

    // Window and taskbar icons, drawn at runtime (no resource file).
    let mut icons = Vec::new();
    for (which, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
        // SAFETY: plain query.
        let size = unsafe { GetSystemMetrics(metric) }.max(16);
        if let Some(icon) = paint::make_shield_icon(size) {
            // SAFETY: WM_SETICON on our own window with an icon we own.
            unsafe {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
            }
            icons.push(icon);
        }
    }

    // Take ownership out of CreateParams.
    let info = std::mem::take(&mut cp.info);
    let Some(mut engine) = cp.engine.take() else {
        return Err(GuiError::Win32 {
            call: "WM_CREATE",
            message: "engine link already taken".into(),
        });
    };

    // The engine calls this from its own thread. PostMessageW is
    // thread-safe; the flag coalesces bursts into one message.
    let wake_pending = Arc::new(AtomicBool::new(false));
    let (flag, target) = (wake_pending.clone(), hwnd.0 as isize);
    engine.set_waker(Box::new(move || {
        if !flag.swap(true, Ordering::AcqRel) {
            // SAFETY: PostMessageW may be called from any thread. If the
            // window no longer exists the call fails harmlessly.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(target as *mut c_void)),
                    WM_APP_ENGINE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }));

    let Some(services) = cp.services.take() else {
        return Err(GuiError::Win32 {
            call: "WM_CREATE",
            message: "services already taken".into(),
        });
    };
    let mut pages = pages::Pages::create(hwnd, services)?;
    let fonts = Fonts::new(dpi);
    pages.apply_fonts(&fonts.body, dpi);
    if let Some(logging) = pages.logging.clone() {
        // New log entries wake the window (coalesced by the buffer).
        let target = hwnd.0 as isize;
        logging.buffer().set_waker(Box::new(move || {
            // SAFETY: PostMessageW may be called from any thread.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(target as *mut c_void)),
                    pages::WM_APP_LOG,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }));
        // Previous run's log tail, read off the UI thread.
        if let Some(t) = logging.spawn_history_load() {
            pages.track_thread(t);
        }
    }

    let mut st = WindowState {
        hwnd,
        dpi,
        fonts,
        nav,
        start_btn,
        stop_btn,
        ui: UiState::default(),
        info,
        engine,
        view: PageView::default(),
        layout: Layout::default(),
        wake_pending,
        icons,
        pages,
        closing: false,
        close_forced: false,
        dropped: cp.dropped.clone(),
        shutdown_confirmed: cp.shutdown_confirmed.clone(),
    };
    st.send(Command::QueryStatus);
    if st.info.start_engine_on_launch {
        tracing::info!("starting the engine on launch (general.start_engine_on_launch)");
        st.send(Command::Start);
    }
    st.relayout();
    if let Some(s) = slot(hwnd) {
        *s.state.borrow_mut() = Some(st);
    }
    Ok(())
}

/// Run a modal dialog with no borrow held, then apply the answer.
fn run_deferred(hwnd: HWND, d: pages::Deferred) {
    match d {
        pages::Deferred::Export => {
            if let Some(path) = pages::ask_export_path(hwnd) {
                with_state_mut(hwnd, |s| s.export_logs_to(path));
            }
        }
        pages::Deferred::ConfirmReset => {
            if pages::confirm_reset(hwnd) {
                with_state_mut(hwnd, |s| s.reset_settings_confirmed());
            }
        }
    }
}

/// Result of handling a message while the window state is borrowed. Anything
/// that may re-enter the window procedure runs after the borrow is released.
enum Outcome {
    Done(LRESULT),
    /// Forward to `DefWindowProcW`.
    Default,
    /// Move/resize the window to this rectangle, then return 0.
    MoveTo(RECT),
    /// Run a modal dialog, then act on the answer (re-borrowing the state).
    Deferred(pages::Deferred),
}

fn handle_with_state(
    st: &mut WindowState,
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> Outcome {
    Outcome::Done(match msg {
        WM_ERASEBKGND => LRESULT(1), // everything is painted in WM_PAINT
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: standard BeginPaint/EndPaint pairing on our window; the
            // back buffer objects are deselected and deleted before return.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut ps);
                let (w, h) = st.client_size();
                if w > 0 && h > 0 {
                    let mem = CreateCompatibleDC(Some(hdc));
                    let bmp = Gdi(CreateCompatibleBitmap(hdc, w, h).into());
                    let old = SelectObject(mem, bmp.0);
                    st.paint(mem);
                    let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
                    SelectObject(mem, old);
                    let _ = DeleteDC(mem);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            st.relayout();
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mut rc = RECT {
                left: 0,
                top: 0,
                right: scale(theme::MIN_CLIENT_W, st.dpi),
                bottom: scale(theme::MIN_CLIENT_H, st.dpi),
            };
            // SAFETY: lParam points to a MINMAXINFO for this message; `rc` is ours.
            unsafe {
                let _ = AdjustWindowRectExForDpi(
                    &mut rc,
                    WS_OVERLAPPEDWINDOW,
                    false,
                    WINDOW_EX_STYLE(0),
                    st.dpi,
                );
                let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                mmi.ptMinTrackSize = POINT {
                    x: rc.right - rc.left,
                    y: rc.bottom - rc.top,
                };
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            st.on_dpi_changed(loword(wp.0) as u32);
            // SAFETY: lParam points to the suggested RECT for this message.
            let r = unsafe { *(lp.0 as *const RECT) };
            // The resize (and the WM_SIZE re-layout it triggers) happens after
            // the borrow is released.
            return Outcome::MoveTo(r);
        }
        WM_DRAWITEM => {
            // SAFETY: lParam points to a DRAWITEMSTRUCT for this message.
            let dis = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
            st.draw_item(dis);
            LRESULT(1)
        }
        WM_COMMAND => {
            let (id, code) = (loword(wp.0), hiword(wp.0));
            match st.on_page_command(id, code) {
                Some(Some(d)) => return Outcome::Deferred(d),
                Some(None) => {}
                None => st.on_command(id),
            }
            LRESULT(0)
        }
        pages::WM_CTLCOLOREDIT | pages::WM_CTLCOLORLISTBOX | pages::WM_CTLCOLORSTATIC => {
            match st.ctl_color(msg, HDC(wp.0 as *mut c_void), HWND(lp.0 as *mut c_void)) {
                Some(r) => r,
                None => return Outcome::Default,
            }
        }
        pages::WM_VKEYTOITEM if HWND(lp.0 as *mut c_void) == st.pages.ctl.list => {
            st.on_list_key(loword(wp.0))
        }
        pages::WM_APP_LOG => {
            st.refresh_logs();
            LRESULT(0)
        }
        pages::WM_APP_IO => {
            st.on_io();
            LRESULT(0)
        }
        WM_APP_ENGINE => {
            st.drain_events();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == CLOSE_TIMER_ID => {
            // SAFETY: killing our own timer.
            unsafe {
                let _ = KillTimer(Some(hwnd), CLOSE_TIMER_ID);
            }
            if st.closing && !st.ui.shutdown_complete {
                tracing::error!(
                    timeout_ms = CLOSE_TIMEOUT_MS,
                    "engine did not confirm shutdown in time; closing anyway"
                );
                st.close_forced = true;
                st.post_close();
            }
            LRESULT(0)
        }
        _ => return Outcome::Default,
    })
}

// ── Entry points ─────────────────────────────────────────────────────────────

pub fn message_box_error(title: &str, message: &str) {
    let (t, m) = (wide(title), wide(message));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(m.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn run(
    info: AppInfo,
    engine: Box<dyn EngineLink>,
    services: crate::Services,
    opts: RunOptions,
) -> Result<Option<SelfTestReport>, GuiError> {
    // SAFETY: process-wide setting; failure (e.g. already set by a manifest)
    // is harmless and ignored.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // SAFETY: None = current executable.
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| win_err("GetModuleHandleW", e))?
        .into();

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        // SAFETY: loading a stock system cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: `wc` is fully initialised; the class name is a static string.
    // Re-registration (second run in one process) fails harmlessly.
    unsafe { RegisterClassExW(&wc) };

    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForSystem() }.max(96);
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: scale(theme::DEFAULT_CLIENT_W, dpi),
        bottom: scale(theme::DEFAULT_CLIENT_H, dpi),
    };
    // SAFETY: `rc` is a valid in/out pointer.
    unsafe {
        let _ =
            AdjustWindowRectExForDpi(&mut rc, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), dpi);
    }

    let dropped = Rc::new(Cell::new(false));
    let shutdown_confirmed = Rc::new(Cell::new(false));
    let panicked = Rc::new(Cell::new(false));
    let mut cp = CreateParams {
        info,
        engine: Some(engine),
        services: Some(services),
        dropped: dropped.clone(),
        shutdown_confirmed: shutdown_confirmed.clone(),
        panicked: panicked.clone(),
    };
    // SAFETY: `cp` outlives CreateWindowExW, which is the only time
    // WM_NCCREATE/WM_CREATE read it.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("KALKAN"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rc.right - rc.left,
            rc.bottom - rc.top,
            None,
            None,
            Some(hinst),
            Some(&mut cp as *mut CreateParams as *const c_void),
        )
    }
    .map_err(|e| win_err("CreateWindowExW", e))?;

    let show = if opts.start_minimized && !opts.self_test {
        SW_SHOWMINNOACTIVE
    } else {
        SW_SHOW
    };
    // SAFETY: valid top-level window.
    unsafe {
        let _ = ShowWindow(hwnd, show);
        let _ = UpdateWindow(hwnd);
    }

    let report = if opts.self_test {
        Some(self_test(
            hwnd,
            opts.screenshot_dir.as_deref(),
            opts.cpu_time,
        ))
    } else {
        None
    };

    let code = message_loop(hwnd)?;
    if panicked.get() {
        return Err(GuiError::Panicked);
    }
    tracing::info!(code, "message loop finished");

    Ok(report.map(|mut r| {
        r.checks.push(Check {
            name: "message loop exited with code 0".into(),
            passed: code == 0,
        });
        r.checks.push(Check {
            name: "window state released on shutdown".into(),
            passed: dropped.get(),
        });
        r.checks.push(Check {
            name: "engine confirmed shutdown before the window closed".into(),
            passed: shutdown_confirmed.get(),
        });
        let live = kalkan_core::live_engine_threads();
        r.checks.push(Check {
            name: format!("no engine threads remain after exit ({live} alive)"),
            passed: live == 0,
        });
        r
    }))
}

fn message_loop(hwnd: HWND) -> Result<i32, GuiError> {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-pointer; standard message loop.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match r.0 {
            0 => return Ok(msg.wParam.0 as i32),
            -1 => {
                return Err(GuiError::Win32 {
                    call: "GetMessageW",
                    message: windows::core::Error::from_thread().message(),
                })
            }
            _ => {
                // SAFETY: dispatching a message we just received.
                unsafe {
                    if !IsDialogMessageW(hwnd, &msg).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
}

/// Process all pending messages (self-test only).
fn pump() {
    let mut msg = MSG::default();
    // SAFETY: standard PeekMessage loop on the UI thread.
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ── Self-test ────────────────────────────────────────────────────────────────

/// Read something from the window state without holding the borrow.
fn with_state<T>(hwnd: HWND, f: impl FnOnce(&WindowState) -> T) -> Option<T> {
    let s = slot(hwnd)?;
    let g = s.state.try_borrow().ok()?;
    g.as_ref().map(f)
}

fn click(btn: HWND) {
    // SAFETY: BM_CLICK on our own child button; handled synchronously.
    unsafe {
        SendMessageW(btn, BM_CLICK, None, None);
    }
    pump();
}

/// Mutate the window state without holding the borrow afterwards.
fn with_state_mut<T>(hwnd: HWND, f: impl FnOnce(&mut WindowState) -> T) -> Option<T> {
    let s = slot(hwnd)?;
    let mut g = s.state.try_borrow_mut().ok()?;
    g.as_mut().map(f)
}

/// Pump messages until `pred` holds or `timeout` passes. The engine runs on
/// its own threads; its events arrive here as WM_APP_ENGINE.
fn wait_until(hwnd: HWND, timeout: Duration, pred: impl Fn(&WindowState) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        pump();
        if with_state(hwnd, &pred) == Some(true) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

const STEP_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_TIMER_ID: usize = 99;

/// Run a normal blocking message loop (GetMessageW) for `d`, so measurements
/// see the application's real idle behaviour rather than a polling loop.
fn run_message_loop_for(hwnd: HWND, d: Duration) {
    // SAFETY: a one-shot timer on our own window; GetMessageW/Dispatch is the
    // standard loop on the UI thread; the timer is killed before returning.
    unsafe {
        SetTimer(Some(hwnd), TEST_TIMER_ID, d.as_millis() as u32, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_TIMER && msg.hwnd == hwnd && msg.wParam.0 == TEST_TIMER_ID {
                break;
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = KillTimer(Some(hwnd), TEST_TIMER_ID);
    }
}

/// Force every page/engine-state/notice/DPI combination through WM_PAINT
/// (RDW_UPDATENOW), and hover in/out on every button. Returns frames painted.
fn paint_every_state(hwnd: HWND, nav: [HWND; 5]) -> usize {
    let redraw = || {
        // SAFETY: synchronous repaint of our own window; no borrow is held.
        unsafe {
            let _ = RedrawWindow(
                Some(hwnd),
                None,
                None,
                RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
            );
        }
    };
    let notices = [
        None,
        Some(crate::model::Notice {
            kind: NoticeKind::Info,
            message: "Wi-Fi connected".into(),
        }),
        Some(crate::model::Notice {
            kind: NoticeKind::Warning,
            message: "Start ignored".into(),
        }),
        Some(crate::model::Notice {
            kind: NoticeKind::Error,
            message: "The engine failed to start: test".into(),
        }),
    ];
    let mut frames = 0;
    for dpi in [96u32, 144] {
        for state in EngineState::ALL {
            for notice in &notices {
                for page in Page::ALL {
                    with_state_mut(hwnd, |s| {
                        if s.dpi != dpi {
                            s.on_dpi_changed(dpi);
                        }
                        s.ui.engine = state;
                        s.ui.status.state = Some(state);
                        s.ui.notice = notice.clone();
                        s.ui.navigate(page);
                        s.relayout();
                    });
                    redraw();
                    frames += 1;
                }
            }
        }
    }
    // Hover in and out of every button (exercises the subclass procedure).
    with_state_mut(hwnd, |s| {
        s.ui.engine = EngineState::Stopped;
        s.ui.navigate(Page::Dashboard);
        s.relayout();
    });
    let buttons: Vec<HWND> = nav
        .iter()
        .copied()
        .chain(with_state(hwnd, |s| [s.start_btn, s.stop_btn]).unwrap_or_default())
        .collect();
    for b in buttons {
        // SAFETY: synthetic mouse messages to our own child buttons.
        unsafe {
            SendMessageW(b, WM_MOUSEMOVE, Some(WPARAM(0)), Some(LPARAM(0x0005_0005)));
            redraw();
            SendMessageW(b, WM_MOUSELEAVE, Some(WPARAM(0)), Some(LPARAM(0)));
            redraw();
        }
        frames += 2;
    }
    with_state_mut(hwnd, |s| s.on_dpi_changed(96));
    redraw();
    frames
}

/// Simulate the window moving to a monitor with `dpi` (WM_DPICHANGED with the
/// current window rectangle as the suggestion).
fn simulate_dpi(hwnd: HWND, dpi: u32) {
    let mut rc = RECT::default();
    // SAFETY: valid window; `rc` outlives the synchronous SendMessageW.
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            Some(WPARAM((dpi | (dpi << 16)) as usize)),
            Some(LPARAM(&rc as *const RECT as isize)),
        );
    }
    pump();
}

fn self_test(
    hwnd: HWND,
    shots: Option<&Path>,
    cpu_time: Option<fn() -> Option<Duration>>,
) -> SelfTestReport {
    let mut r = SelfTestReport::default();
    let mut check = |name: String, passed: bool| {
        tracing::info!(passed, "self-test: {name}");
        r.checks.push(Check { name, passed });
    };
    let mut shot_paths: Vec<PathBuf> = Vec::new();
    let mut shot_errors: Vec<String> = Vec::new();
    let mut capture = |name: &str| {
        if let Some(dir) = shots {
            pump();
            let path = dir.join(format!("{name}.bmp"));
            match capture_client(hwnd, &path) {
                Ok(()) => shot_paths.push(path),
                Err(e) => shot_errors.push(format!("{name}: {e}")),
            }
        }
    };
    if let Some(dir) = shots {
        let _ = std::fs::create_dir_all(dir);
    }
    pump();

    let Some((nav, start, stop)) = with_state(hwnd, |s| (s.nav, s.start_btn, s.stop_btn)) else {
        check("window state available".into(), false);
        // SAFETY: posting to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        return r;
    };
    // SAFETY: plain queries on our child windows.
    let enabled = |h: HWND| unsafe { IsWindowEnabled(h) }.as_bool();
    let state = || with_state(hwnd, |s| s.ui.engine);
    let counts = || with_state(hwnd, |s| s.ui.counts).unwrap_or_default();
    let metrics_live = |s: &WindowState| {
        let v = page_view(Page::Dashboard, &s.ui, &s.info);
        v.cards[0].value.is_real() && v.cards[1].value.is_real()
    };

    // ── Initial state ────────────────────────────────────────────────────
    check(
        "initial page is Dashboard".into(),
        with_state(hwnd, |s| s.ui.page) == Some(Page::Dashboard),
    );
    check(
        format!("engine reports Stopped at start ({:?})", state()),
        state() == Some(EngineState::Stopped),
    );
    check("Start enabled while stopped".into(), enabled(start));
    check("Stop disabled while stopped".into(), !enabled(stop));

    // ── Navigation ───────────────────────────────────────────────────────
    for (i, page) in Page::ALL.iter().enumerate() {
        click(nav[i]);
        check(
            format!("sidebar navigates to {}", page.label()),
            with_state(hwnd, |s| s.ui.page) == Some(*page),
        );
        let fits = with_state(hwnd, |s| {
            s.layout.content_bottom() <= s.layout.client.bottom()
        });
        check(
            format!("{} content fits the window", page.label()),
            fits == Some(true),
        );
        capture(&format!(
            "{}-{}",
            i + 1,
            page.label().to_lowercase().replace(' ', "-")
        ));
    }
    click(nav[0]);

    // ── Start: real engine, real events ──────────────────────────────────
    let t0 = Instant::now();
    click(start);
    let running = wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.engine == EngineState::Running);
    check(
        format!(
            "Start: engine reports Running after initialisation ({} ms)",
            t0.elapsed().as_millis()
        ),
        running,
    );
    check(
        "EngineStarted event received once".into(),
        counts().started == 1,
    );
    check(
        "while running: Start disabled, Stop enabled".into(),
        !enabled(start) && enabled(stop),
    );
    let t0 = Instant::now();
    let live = wait_until(hwnd, STEP_TIMEOUT, metrics_live);
    check(
        format!(
            "live CPU and memory samples arrive from the engine ({} ms)",
            t0.elapsed().as_millis()
        ),
        live,
    );
    // ── Real network monitoring (GetIfTable2 on this machine) ────────────
    let t0 = Instant::now();
    let have_net = wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.status.network.is_some());
    let (n_if, names, err) = with_state(hwnd, |s| {
        let n = s.ui.status.network.as_ref();
        (
            n.map_or(0, |n| n.interfaces.len()),
            n.map_or(String::new(), |n| {
                n.interfaces
                    .iter()
                    .map(|i| {
                        format!(
                            "{} [{}{}]",
                            i.name,
                            i.kind.label(),
                            if i.counted { ", counted" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            }),
            s.ui.status.network_error.clone(),
        )
    })
    .unwrap_or_default();
    check(
        format!("network monitor reports real interfaces ({} ms): {n_if} found: {names}; error: {err:?}", t0.elapsed().as_millis()),
        have_net && n_if > 0,
    );
    let counted = with_state(hwnd, |s| {
        s.ui.status
            .network
            .as_ref()
            .is_some_and(|n| n.interfaces.iter().any(|i| i.counted))
    });
    check(
        "at least one connected hardware interface is counted".into(),
        counted == Some(true),
    );
    let t0 = Instant::now();
    let rates = wait_until(hwnd, STEP_TIMEOUT, |s| {
        s.ui.status
            .network
            .as_ref()
            .is_some_and(|n| n.rx_rate.is_some() && n.tx_rate.is_some())
    });
    check(
        format!(
            "download/upload rates computed from two real samples ({} ms)",
            t0.elapsed().as_millis()
        ),
        rates,
    );
    let dash_live = with_state(hwnd, |s| {
        let v = page_view(Page::Dashboard, &s.ui, &s.info);
        v.cards[0].value.is_real() && v.cards[1].value.is_real() && v.strip[0].value.is_real()
    });
    check(
        "Dashboard shows live download, upload and active interface".into(),
        dash_live == Some(true),
    );
    capture("6-dashboard-running");

    // Live Network and Statistics pages (graphs need a few samples).
    click(nav[Page::Network.index()]);
    wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.history.len() >= 3);
    let table_rows =
        with_state(hwnd, |s| s.view.table.as_ref().map_or(0, |t| t.rows.len())).unwrap_or(0);
    check(
        format!("Network page lists the interfaces ({table_rows} rows)"),
        table_rows == n_if && n_if > 0,
    );
    capture("6a-network-running");
    click(nav[Page::Statistics.index()]);
    let stats_live = with_state(hwnd, |s| {
        page_view(Page::Statistics, &s.ui, &s.info).cards[0]
            .value
            .is_real()
    });
    check(
        "Statistics shows live session totals".into(),
        stats_live == Some(true),
    );
    capture("6b-statistics-running");

    // CPU of the whole process (engine sampling + GUI repainting the live
    // Network page) over 5 s, using a real blocking message loop.
    click(nav[Page::Network.index()]);
    if let Some(cpu) = cpu_time {
        let (c0, t0) = (cpu(), Instant::now());
        run_message_loop_for(hwnd, Duration::from_secs(5));
        let (c1, wall) = (cpu(), t0.elapsed());
        if let (Some(c0), Some(c1)) = (c0, c1) {
            let pct = c1.saturating_sub(c0).as_secs_f64() / wall.as_secs_f64() * 100.0;
            // A sanity bound against busy loops, not a performance target.
            check(format!("process CPU while running with live Network page: {pct:.2} % of one core over {:.1} s (bound: < 15 %)", wall.as_secs_f64()), pct < 15.0);
        }
    }
    click(nav[0]);

    // Duplicate Start (bypassing the disabled button) is rejected by the engine.
    with_state_mut(hwnd, |s| s.send(Command::Start));
    let rejected = wait_until(hwnd, STEP_TIMEOUT, |s| {
        s.ui.notice
            .as_ref()
            .is_some_and(|n| n.kind == NoticeKind::Warning && n.message.contains("already running"))
    });
    check(
        "duplicate Start is rejected with a warning".into(),
        rejected,
    );
    check(
        "duplicate Start leaves the engine Running".into(),
        state() == Some(EngineState::Running) && counts().started == 1,
    );
    capture("7-dashboard-duplicate-start");

    // ── Stop: confirmed only after the worker terminated ─────────────────
    let t0 = Instant::now();
    click(stop);
    let stopped = wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.counts.stopped == 1);
    check(
        format!(
            "Stop: EngineStopped confirmed ({} ms)",
            t0.elapsed().as_millis()
        ),
        stopped && state() == Some(EngineState::Stopped),
    );
    check(
        "worker thread terminated after Stop (controller only)".into(),
        kalkan_core::live_engine_threads() == 1,
    );
    check(
        "metrics cleared after Stop".into(),
        with_state(hwnd, metrics_live) == Some(false),
    );
    check(
        "after stop: Start enabled, Stop disabled".into(),
        enabled(start) && !enabled(stop),
    );

    with_state_mut(hwnd, |s| s.send(Command::Stop));
    let rejected = wait_until(hwnd, STEP_TIMEOUT, |s| {
        s.ui.notice
            .as_ref()
            .is_some_and(|n| n.kind == NoticeKind::Warning && n.message.contains("not running"))
    });
    check(
        "duplicate Stop is rejected with a warning".into(),
        rejected && counts().stopped == 1,
    );

    // ── Repeated cycles through the real buttons ─────────────────────────
    const CYCLES: u32 = 10;
    let t0 = Instant::now();
    let mut ok = true;
    for _ in 0..CYCLES {
        click(start);
        ok &= wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.engine == EngineState::Running);
        click(stop);
        ok &= wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.engine == EngineState::Stopped);
    }
    let c = counts();
    check(
        format!(
            "{CYCLES} start/stop cycles ({} ms total; started {}, stopped {}, errors {})",
            t0.elapsed().as_millis(),
            c.started,
            c.stopped,
            c.errors
        ),
        ok && c.started == CYCLES + 1 && c.stopped == CYCLES + 1 && c.errors == 0,
    );
    check(
        "no worker thread left after the cycles".into(),
        kalkan_core::live_engine_threads() == 1,
    );

    // ── Logs page: the real log buffer ───────────────────────────────────
    self_test_logs(hwnd, nav, &mut check, &mut capture);
    // ── Settings page: real file, validation, live apply, reset ──────────
    self_test_settings(hwnd, nav, &mut check, &mut capture);
    click(nav[0]);

    // ── Responsive layout (unchanged from Phase 2) ───────────────────────
    let actual_client = || {
        let mut rc = RECT::default();
        // SAFETY: valid window; out-pointer on the stack.
        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
        (rc.right - rc.left, rc.bottom - rc.top)
    };
    let layout_matches = |(w, h): (i32, i32)| {
        with_state(hwnd, |s| {
            let l = &s.layout;
            l.client.w == w
                && l.client.h == h
                && l.content_bottom() <= l.client.bottom()
                && l.cards.iter().all(|c| c.right() <= l.client.right())
        }) == Some(true)
    };
    let dpi = with_state(hwnd, |s| s.dpi).unwrap_or(96);
    // SAFETY: resizing our own window.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            1,
            1,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    pump();
    let (w, h) = actual_client();
    let (min_w, min_h) = (
        scale(theme::MIN_CLIENT_W, dpi),
        scale(theme::MIN_CLIENT_H, dpi),
    );
    check(
        format!("minimum size enforced (client {w}x{h}, minimum {min_w}x{min_h})"),
        w >= min_w && h >= min_h,
    );
    check(
        format!("layout follows resize to {w}x{h}"),
        layout_matches((w, h)),
    );
    capture("8-dashboard-minimum-size");
    // SAFETY: plain ShowWindow calls on our own window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
    }
    pump();
    let (w, h) = actual_client();
    check(
        format!("layout follows maximise to {w}x{h}"),
        layout_matches((w, h)),
    );
    // SAFETY: as above.
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    pump();
    let (w, h) = actual_client();
    check(
        format!("layout follows restore to {w}x{h}"),
        layout_matches((w, h)),
    );

    // ── Deterministic paint coverage ─────────────────────────────────────
    // Paint every page in every engine state, with every notice kind, at
    // 96 and 144 DPI, synchronously. Transitional states normally last only
    // milliseconds, so without this a paint bug in them shows up only when a
    // WM_PAINT happens to land in that window.
    let painted = paint_every_state(hwnd, nav);
    check(
        format!("synchronous paint of all pages x states x notices x DPI ({painted} frames)"),
        painted > 0,
    );
    // Restore the real state from the engine.
    with_state_mut(hwnd, |s| {
        s.ui.notice = None;
        s.send(Command::QueryStatus);
    });
    let restored = wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.engine == EngineState::Stopped);
    check(
        "real engine state restored after paint coverage".into(),
        restored,
    );

    // ── Display scaling: simulate moving to a 150 % monitor and back ──────
    for (dpi, name) in [(144u32, "10-dashboard-150-percent"), (96, "")] {
        simulate_dpi(hwnd, dpi);
        let (got, w, h) =
            with_state(hwnd, |s| (s.dpi, s.layout.client.w, s.layout.client.h)).unwrap_or_default();
        let (aw, ah) = actual_client();
        check(
            format!("WM_DPICHANGED to {dpi} DPI: fonts and layout rebuilt (layout {w}x{h}, client {aw}x{ah})"),
            got == dpi && (w, h) == (aw, ah),
        );
        if !name.is_empty() {
            capture(name);
        }
    }

    // ── Close while running: the app must stop the engine first ──────────
    click(start);
    let running = wait_until(hwnd, STEP_TIMEOUT, |s| s.ui.engine == EngineState::Running);
    check("engine running before application close".into(), running);

    r.screenshots = shot_paths;
    r.screenshot_errors = shot_errors;
    // SAFETY: posting to our own window; the message loop handles the
    // two-step close (Shutdown → ShutdownComplete → DestroyWindow).
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
    r
}

const LB_GETCOUNT: u32 = 0x018B;
const LB_SETCURSEL_MSG: u32 = 0x0186;

fn set_edit_text(h: HWND, text: &str) {
    let w = wide(text);
    // SAFETY: WM_SETTEXT to our own EDIT; it notifies the parent with
    // EN_CHANGE synchronously (no borrow is held in the self-test).
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowTextW(h, PCWSTR(w.as_ptr()));
    }
    pump();
}

fn self_test_logs(
    hwnd: HWND,
    nav: [HWND; 5],
    check: &mut impl FnMut(String, bool),
    capture: &mut impl FnMut(&str),
) {
    use crate::logview::LevelChip;
    click(nav[Page::Logs.index()]);
    let has_logging = with_state(hwnd, |s| s.pages.logging.is_some()) == Some(true);
    check("log buffer available".into(), has_logging);
    if !has_logging {
        return;
    }
    let ok = wait_until(hwnd, STEP_TIMEOUT, |s| {
        (0..s.pages.log.visible_len()).any(|i| {
            s.pages
                .log
                .row(i)
                .is_some_and(|e| e.message == "engine started")
        })
    });
    let (total, visible) = with_state(hwnd, |s| {
        (s.pages.log.total_len(), s.pages.log.visible_len())
    })
    .unwrap_or_default();
    check(
        format!("Logs page shows real engine log entries ({visible} of {total})"),
        ok,
    );
    let (list, search) =
        with_state(hwnd, |s| (s.pages.ctl.list, s.pages.ctl.search)).unwrap_or_default();
    // SAFETY: plain query on our own list box.
    let lb = unsafe { SendMessageW(list, LB_GETCOUNT, None, None).0 } as usize;
    check(
        format!("virtual list box row count matches the filtered entries ({lb})"),
        lb == visible,
    );

    set_edit_text(search, "ENGINE STARTED");
    let all_match = with_state(hwnd, |s| {
        let v = &s.pages.log;
        v.visible_len() > 0
            && (0..v.visible_len()).all(|i| {
                v.row(i)
                    .is_some_and(|e| e.message.to_lowercase().contains("engine started"))
            })
    });
    check(
        "search filters to matching entries (case-insensitive)".into(),
        all_match == Some(true),
    );
    capture("11-logs-search");
    set_edit_text(search, "");

    let chips = with_state(hwnd, |s| s.pages.ctl.chips).unwrap_or_default();
    for c in [LevelChip::Info, LevelChip::Debug] {
        click(chips[c as usize]);
    }
    let only_warn_err = with_state(hwnd, |s| {
        let v = &s.pages.log;
        (0..v.visible_len()).all(|i| {
            v.row(i).is_some_and(|e| {
                matches!(
                    e.level,
                    kalkan_config::LogLevel::Warn | kalkan_config::LogLevel::Error
                )
            })
        }) && v.visible_len() < v.total_len()
    });
    check(
        "level chips hide INFO and DEBUG entries".into(),
        only_warn_err == Some(true),
    );
    for c in [LevelChip::Info, LevelChip::Debug] {
        click(chips[c as usize]);
    }

    // Pause: new entries must not appear until resumed.
    let pause = with_state(hwnd, |s| s.pages.ctl.pause).unwrap_or_default();
    click(pause);
    tracing::info!("self-test marker: logged while paused");
    pump();
    let has_marker = |s: &WindowState| {
        (0..s.pages.log.visible_len()).any(|i| {
            s.pages
                .log
                .row(i)
                .is_some_and(|e| e.message == "self-test marker: logged while paused")
        })
    };
    let hidden = with_state(hwnd, |s| s.pages.log.paused && !has_marker(s));
    check("paused view does not change".into(), hidden == Some(true));
    click(pause);
    let shown = wait_until(hwnd, STEP_TIMEOUT, has_marker);
    check("resume shows entries logged while paused".into(), shown);

    // Copy the newest entry.
    let n = with_state(hwnd, |s| s.pages.log.visible_len()).unwrap_or(0);
    let expected = with_state_mut(hwnd, |s| {
        s.pages.log.selected = n.checked_sub(1);
        // SAFETY: select the same row in our list box.
        unsafe {
            SendMessageW(
                s.pages.ctl.list,
                LB_SETCURSEL_MSG,
                Some(WPARAM(n.saturating_sub(1))),
                None,
            );
        }
        s.relayout();
        s.pages.log.selected_entry().map(|e| e.to_line())
    })
    .flatten();
    let copy = with_state(hwnd, |s| s.pages.ctl.copy).unwrap_or_default();
    click(copy);
    let got = pages::read_clipboard(hwnd);
    check(
        format!(
            "Copy puts the selected entry on the clipboard ({} chars)",
            got.as_deref().map_or(0, str::len)
        ),
        expected.is_some() && got == expected,
    );

    // Export (the path the Save As dialog would return).
    let path =
        std::env::temp_dir().join(format!("kalkan-selftest-export-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    with_state_mut(hwnd, |s| s.export_logs_to(path.clone()));
    let done = wait_until(hwnd, STEP_TIMEOUT, |s| !s.pages.io_busy);
    let lines = std::fs::read_to_string(&path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    let visible = with_state(hwnd, |s| s.pages.log.visible_len()).unwrap_or(0);
    check(
        format!("Export writes the visible entries off the UI thread ({lines} lines)"),
        done && lines == visible && lines > 0,
    );
    let _ = std::fs::remove_file(&path);
    capture("11-logs");
}

fn self_test_settings(
    hwnd: HWND,
    nav: [HWND; 5],
    check: &mut impl FnMut(String, bool),
    capture: &mut impl FnMut(&str),
) {
    use crate::settings::FileState;
    click(nav[Page::Settings.index()]);
    let (path, initial, edits, levels, save) = match with_state(hwnd, |s| {
        (
            s.pages.config_path.clone(),
            s.pages.settings.file.clone(),
            s.pages.ctl.edits,
            s.pages.ctl.levels,
            s.pages.ctl.save,
        )
    }) {
        Some(v) => v,
        None => return,
    };
    check(
        format!("Settings page loaded the configuration state ({initial:?})"),
        true,
    );
    capture("12-settings");
    let before = std::fs::read(&path).ok();

    // Invalid value: rejected with a message, nothing written.
    set_edit_text(edits[0], "0");
    click(save);
    let rejected = with_state(hwnd, |s| {
        !s.pages.settings.errors.is_empty()
            && s.ui
                .visible_notice()
                .is_some_and(|n| n.kind == NoticeKind::Error)
    });
    check(
        "out-of-range value is rejected with an explanation".into(),
        rejected == Some(true),
    );
    check(
        "rejected save leaves the file untouched".into(),
        std::fs::read(&path).ok() == before,
    );
    capture("12-settings-invalid");

    // Valid save: written, applied live (log level), form clean.
    set_edit_text(edits[0], "12");
    click(levels[3]); // debug
    click(save);
    let saved = wait_until(hwnd, STEP_TIMEOUT, |s| {
        !s.pages.settings.saving && !s.pages.io_busy
    });
    let on_disk = kalkan_config::Config::load(&path).ok();
    check(
        "saved settings are on disk".into(),
        saved
            && on_disk.as_ref().is_some_and(|c| {
                c.logging.max_file_size_mb == 12
                    && c.logging.level == kalkan_config::LogLevel::Debug
            }),
    );
    let live = with_state(hwnd, |s| {
        s.pages.logging.as_ref().map(|l| l.level()) == Some(kalkan_config::LogLevel::Debug)
            && !s.pages.settings.dirty()
    });
    check(
        "log level applied live; form no longer dirty".into(),
        live == Some(true),
    );
    if matches!(initial, FileState::Invalid(_)) {
        let bak = {
            let mut n = path.as_os_str().to_owned();
            n.push(".bak");
            std::path::PathBuf::from(n)
        };
        check(
            "the invalid file was kept as a .bak copy before saving".into(),
            bak.is_file(),
        );
    }
    capture("12-settings-saved");

    // Reset to defaults (the confirmed path; the dialog itself is modal).
    with_state_mut(hwnd, |s| s.reset_settings_confirmed());
    let done = wait_until(hwnd, STEP_TIMEOUT, |s| {
        !s.pages.settings.saving && !s.pages.io_busy
    });
    let reset_ok =
        kalkan_config::Config::load(&path).ok() == Some(kalkan_config::Config::default());
    let level_back = with_state(hwnd, |s| {
        s.pages.logging.as_ref().map(|l| l.level()) == Some(kalkan_config::LogLevel::Info)
    });
    check(
        "Reset to defaults saves the defaults and re-applies them".into(),
        done && reset_ok && level_back == Some(true),
    );
}

/// Save the window's client area (including child controls) as a 32-bit BMP.
fn capture_client(hwnd: HWND, path: &Path) -> Result<(), String> {
    // PrintWindow(PW_RENDERFULLCONTENT) returns the last frame composed by
    // DWM, which can lag one repaint behind a resize. Repaint synchronously
    // (no window-state borrow is held here) and wait for the next DWM
    // composition so the capture shows the current layout.
    // SAFETY: plain calls on our own window from the UI thread.
    unsafe {
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
        );
        let _ = DwmFlush();
    }
    let mut rc = RECT::default();
    // SAFETY: valid window; out-pointer on the stack.
    unsafe { GetClientRect(hwnd, &mut rc) }.map_err(|e| e.message())?;
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return Err("empty client area".into());
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    // SAFETY: GDI objects are created, used and released in this block; the
    // pixel buffer is exactly w*h*4 bytes as described by `bi`.
    unsafe {
        let screen = GetDC(Some(hwnd));
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = Gdi(CreateCompatibleBitmap(screen, w, h).into());
        let old = SelectObject(mem, bmp.0);
        // PW_CLIENTONLY (1) | PW_RENDERFULLCONTENT (2)
        let ok = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(3)).as_bool();
        SelectObject(mem, old);
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let lines = GetDIBits(
            mem,
            windows::Win32::Graphics::Gdi::HBITMAP(bmp.0 .0),
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), screen);
        if !ok {
            return Err("PrintWindow failed".into());
        }
        if lines != h {
            return Err(format!("GetDIBits returned {lines} of {h} lines"));
        }
    }
    write_bmp(path, w, h, &pixels).map_err(|e| e.to_string())
}

fn write_bmp(path: &Path, w: i32, h: i32, bgra: &[u8]) -> std::io::Result<()> {
    let data_len = bgra.len() as u32;
    let mut out = Vec::with_capacity(54 + bgra.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data_len).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&(-h).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes()); // 72 DPI
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(bgra);
    std::fs::write(path, out)
}
