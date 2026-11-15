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
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect, GetDC,
    GetDIBits, GetStockObject, Polygon, RedrawWindow, ReleaseDC, RoundRect, SelectObject,
    SetBkMode, SetTextColor, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FW_NORMAL,
    FW_SEMIBOLD, HBRUSH, HDC, HGDIOBJ, LOGFONTW, NULL_BRUSH, NULL_PEN, PAINTSTRUCT, PS_SOLID,
    RDW_ALLCHILDREN, RDW_INVALIDATE, SRCCOPY, TRANSPARENT,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsDialogMessageW, KillTimer, LoadCursorW, MessageBoxW, MoveWindow,
    PeekMessageW, PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageW, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, BM_CLICK, BS_OWNERDRAW,
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HMENU, IDC_ARROW,
    MB_ICONERROR, MB_OK, MINMAXINFO, MSG, PM_REMOVE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWMINNOACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSEXW,
    WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use kalkan_core::{Command, EngineLink, EngineState, ENGINE_SCOPE, ENGINE_TITLE};

use crate::layout::{self, Content, Layout, Rect};
use crate::model::{page_view, AppInfo, NoticeKind, Page, PageView, UiState, Value};
use crate::theme::{self, scale, Rgb};
use crate::{Check, GuiError, RunOptions, SelfTestReport};

const CLASS_NAME: PCWSTR = w!("KalkanMainWindow");
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

fn brush(c: Rgb) -> Gdi {
    // SAFETY: plain GDI object creation, no pointers involved.
    Gdi(unsafe { CreateSolidBrush(COLORREF(c.colorref())) }.into())
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

fn to_rect(r: Rect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

fn win_err(call: &'static str, e: windows::core::Error) -> GuiError {
    GuiError::Win32 {
        call,
        message: e.message(),
    }
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

// ── Drawing helpers (operate on a valid HDC) ─────────────────────────────────

struct Painter<'a> {
    hdc: HDC,
    dpi: u32,
    fonts: &'a Fonts,
}

impl Painter<'_> {
    fn s(&self, dip: i32) -> i32 {
        scale(dip, self.dpi)
    }

    fn fill(&self, r: Rect, c: Rgb) {
        let b = brush(c);
        // SAFETY: valid DC and brush; RECT lives for the call.
        unsafe { FillRect(self.hdc, &to_rect(r), HBRUSH(b.0 .0)) };
    }

    /// Filled rounded rectangle without an outline.
    fn round(&self, r: Rect, c: Rgb, radius: i32) {
        let b = brush(c);
        let d = self.s(radius) * 2;
        // SAFETY: valid DC; selected objects are restored before `b` drops.
        unsafe {
            let old_b = SelectObject(self.hdc, b.0);
            let old_p = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            // NULL_PEN draws nothing on the edge, so extend by 1 px to cover it.
            let _ = RoundRect(self.hdc, r.x, r.y, r.right() + 1, r.bottom() + 1, d, d);
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    /// Rounded rectangle with a 1 px border.
    fn round_outline(&self, r: Rect, fill: Rgb, border: Rgb, radius: i32) {
        let b = brush(fill);
        // SAFETY: plain pen creation.
        let p = Gdi(unsafe { CreatePen(PS_SOLID, 1, COLORREF(border.colorref())) }.into());
        let d = self.s(radius) * 2;
        // SAFETY: valid DC; selections restored before the objects drop.
        unsafe {
            let old_b = SelectObject(self.hdc, b.0);
            let old_p = SelectObject(self.hdc, p.0);
            let _ = RoundRect(self.hdc, r.x, r.y, r.right(), r.bottom(), d, d);
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    /// Unfilled rounded outline (keyboard focus indicator), 2 px wide.
    fn ring(&self, r: Rect, c: Rgb, radius: i32) {
        // SAFETY: plain pen creation.
        let pen = Gdi(unsafe { CreatePen(PS_SOLID, self.s(2), COLORREF(c.colorref())) }.into());
        let d = self.s(radius) * 2;
        // SAFETY: valid DC; selections restored before the pen drops.
        unsafe {
            let old_b = SelectObject(self.hdc, GetStockObject(NULL_BRUSH));
            let old_p = SelectObject(self.hdc, pen.0);
            let _ = RoundRect(
                self.hdc,
                r.x + 1,
                r.y + 1,
                r.right() - 1,
                r.bottom() - 1,
                d,
                d,
            );
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    fn dot(&self, cx: i32, cy: i32, radius: i32, c: Rgb) {
        let b = brush(c);
        // SAFETY: valid DC; selections restored.
        unsafe {
            let old_b = SelectObject(self.hdc, b.0);
            let old_p = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            let _ = Ellipse(
                self.hdc,
                cx - radius,
                cy - radius,
                cx + radius + 1,
                cy + radius + 1,
            );
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    fn polygon(&self, pts: &[POINT], c: Rgb) {
        let b = brush(c);
        // SAFETY: valid DC; `pts` outlives the call; selections restored.
        unsafe {
            let old_b = SelectObject(self.hdc, b.0);
            let old_p = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            let _ = Polygon(self.hdc, pts);
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    fn text(&self, s: &str, r: Rect, font: &Gdi, c: Rgb, flags: DRAW_TEXT_FORMAT) {
        let mut buf: Vec<u16> = s.encode_utf16().collect();
        let mut rc = to_rect(r);
        // SAFETY: valid DC; `buf` and `rc` outlive the call; font restored.
        unsafe {
            let old = SelectObject(self.hdc, font.0);
            SetTextColor(self.hdc, COLORREF(c.colorref()));
            SetBkMode(self.hdc, TRANSPARENT);
            DrawTextW(self.hdc, &mut buf, &mut rc, flags | DT_NOPREFIX);
            SelectObject(self.hdc, old);
        }
    }

    /// Width of `s` in pixels when drawn with `font`.
    fn measure(&self, s: &str, font: &Gdi) -> i32 {
        let mut buf: Vec<u16> = s.encode_utf16().collect();
        let mut rc = RECT::default();
        // SAFETY: valid DC; DT_CALCRECT only measures; font restored.
        unsafe {
            let old = SelectObject(self.hdc, font.0);
            DrawTextW(
                self.hdc,
                &mut buf,
                &mut rc,
                DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(self.hdc, old);
        }
        rc.right - rc.left
    }

    fn line(&self, s: &str, r: Rect, font: &Gdi, c: Rgb) {
        self.text(
            s,
            r,
            font,
            c,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
    }

    /// Small pill badge, right-aligned at `right`, vertically centred on `cy`.
    /// Returns its width.
    fn badge(&self, label: &str, right: i32, cy: i32, fg: Rgb, bg: Rgb) -> i32 {
        let w = self.measure(label, &self.fonts.small) + self.s(14);
        let h = self.s(20);
        let r = Rect::new(right - w, cy - h / 2, w, h);
        self.round(r, bg, 6);
        self.text(
            label,
            r,
            &self.fonts.small,
            fg,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        w
    }

    /// KALKAN shield mark inside a square of side `size` at (x, y).
    fn shield(&self, x: i32, y: i32, size: i32) {
        let p = |fx: f32, fy: f32| POINT {
            x: x + (fx * size as f32) as i32,
            y: y + (fy * size as f32) as i32,
        };
        let outer = [
            p(0.5, 0.0),
            p(0.95, 0.15),
            p(0.9, 0.6),
            p(0.5, 1.0),
            p(0.1, 0.6),
            p(0.05, 0.15),
        ];
        self.polygon(&outer, theme::ACCENT);
        let inner = [p(0.5, 0.16), p(0.8, 0.26), p(0.76, 0.58), p(0.5, 0.84)];
        self.polygon(&inner, Rgb(0x93, 0xC5, 0xFD));
    }
}

fn state_color(s: EngineState) -> Rgb {
    match s {
        EngineState::Stopped => theme::NEUTRAL,
        EngineState::Starting | EngineState::Stopping => theme::WARN,
        EngineState::Running => theme::OK,
        EngineState::Failed => theme::DANGER,
    }
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
    /// Set to `true` when the state is dropped (clean shutdown check).
    dropped: Rc<Cell<bool>>,
    /// Records whether the engine confirmed shutdown before the window died.
    shutdown_confirmed: Rc<Cell<bool>>,
}

impl Drop for WindowState {
    fn drop(&mut self) {
        self.shutdown_confirmed.set(self.ui.shutdown_complete);
        self.dropped.set(true);
        // Dropping `engine` joins the engine threads (EngineHandle::drop).
    }
}

/// Data passed through `CreateWindowExW` to `WM_NCCREATE`.
struct CreateParams {
    info: AppInfo,
    engine: Option<Box<dyn EngineLink>>,
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
            notice: self.view.engine_panel && self.ui.notice.is_some(),
            cards: self.view.cards.len(),
            table_rows: self.view.table.len(),
            note: self.view.note.is_some(),
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
        // Periodic metric updates only repaint the page that shows them.
        if structural || (metrics && self.ui.page == Page::Dashboard) {
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
                    }
                }
            }
            _ => {}
        }
    }

    fn on_dpi_changed(&mut self, dpi: u32) {
        self.dpi = dpi;
        self.fonts = Fonts::new(dpi);
        // Layout happens in the WM_SIZE that follows SetWindowPos.
    }

    // ── Painting ─────────────────────────────────────────────────────────────

    fn paint(&self, hdc: HDC) {
        let p = Painter {
            hdc,
            dpi: self.dpi,
            fonts: &self.fonts,
        };
        let l = &self.layout;
        let s = |d| p.s(d);

        p.fill(l.client, theme::BG);

        // Sidebar
        p.fill(l.sidebar, theme::SIDEBAR);
        let mark = s(30);
        let lx = s(20);
        let ly = l.logo.y + (l.logo.h - mark) / 2;
        p.shield(lx, ly, mark);
        let tx = lx + mark + s(12);
        p.line(
            "KALKAN",
            Rect::new(tx, ly - s(4), l.logo.w - tx, s(22)),
            &self.fonts.brand,
            theme::TEXT,
        );
        p.line(
            &format!("v{}", self.info.version),
            Rect::new(tx, ly + s(18), l.logo.w - tx, s(16)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
        let f = l.sidebar_footer;
        p.dot(f.x + s(26), f.y + s(18), s(4), state_color(self.ui.engine));
        p.line(
            &format!("Engine: {}", self.ui.engine),
            Rect::new(f.x + s(38), f.y + s(8), f.w - s(46), s(20)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
        p.line(
            crate::model::privilege_value(self.info.elevated).text(),
            Rect::new(f.x + s(38), f.y + s(28), f.w - s(46), s(20)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );

        // Header
        let h = l.header;
        p.line(
            self.ui.page.label(),
            Rect::new(h.x, h.y, h.w, s(36)),
            &self.fonts.title,
            theme::TEXT,
        );
        p.line(
            self.ui.page.subtitle(),
            Rect::new(h.x, h.y + s(38), h.w, s(22)),
            &self.fonts.body,
            theme::TEXT_MUTED,
        );

        if let (Some(r), Some(n)) = (l.notice, &self.ui.notice) {
            let (bg, fg) = match n.kind {
                NoticeKind::Info => (theme::NOTICE_INFO_BG, theme::ACCENT),
                NoticeKind::Warning => (theme::NOTICE_WARN_BG, theme::WARN),
                NoticeKind::Error => (theme::NOTICE_ERROR_BG, theme::DANGER),
            };
            p.round(r, bg, 8);
            p.fill(Rect::new(r.x, r.y + s(8), s(3), r.h - s(16)), fg);
            p.line(
                &n.message,
                Rect::new(r.x + s(16), r.y, r.w - s(24), r.h),
                &self.fonts.body,
                theme::TEXT,
            );
        }

        if let Some(r) = l.engine_panel {
            p.round_outline(r, theme::CARD, theme::CARD_BORDER, theme::RADIUS);
            let btn_left = l.start_btn.map_or(r.right(), |b| b.x);
            let text_w = (btn_left - r.x - s(36)).max(0);
            p.line(
                ENGINE_TITLE,
                Rect::new(r.x + s(20), r.y + s(12), text_w, s(20)),
                &self.fonts.body_bold,
                theme::TEXT_MUTED,
            );
            let cy = r.y + s(50);
            p.dot(r.x + s(28), cy, s(6), state_color(self.ui.engine));
            let label = if self.closing && !self.ui.shutdown_complete {
                "Shutting down…"
            } else {
                self.ui.engine.label()
            };
            p.line(
                label,
                Rect::new(r.x + s(44), cy - s(14), (text_w - s(24)).max(0), s(28)),
                &self.fonts.value,
                theme::TEXT,
            );
            // The scope line sits below the buttons, so it may use the full
            // panel width: this disclaimer must stay readable at any size.
            p.line(
                ENGINE_SCOPE,
                Rect::new(
                    r.x + s(20),
                    r.y + s(theme::PANEL_SCOPE_Y),
                    (r.w - s(40)).max(0),
                    s(20),
                ),
                &self.fonts.small,
                theme::TEXT_MUTED,
            );
        }

        for (card, r) in self.view.cards.iter().zip(&l.cards) {
            p.round_outline(*r, theme::CARD, theme::CARD_BORDER, theme::RADIUS);
            let inner = r.inset(s(16));
            let (badge_fg, badge_bg, value_c) = match &card.value {
                Value::Real { .. } => (theme::OK, Rgb(0x14, 0x2E, 0x22), theme::TEXT),
                Value::Unavailable { .. } => {
                    (theme::WARN, Rgb(0x33, 0x27, 0x10), theme::TEXT_DISABLED)
                }
            };
            let badge_w = p.badge(
                card.value.badge(),
                inner.right(),
                inner.y + s(10),
                badge_fg,
                badge_bg,
            );
            p.line(
                card.title,
                Rect::new(inner.x, inner.y, (inner.w - badge_w - s(8)).max(0), s(20)),
                &self.fonts.body_bold,
                theme::TEXT_MUTED,
            );
            p.line(
                card.value.text(),
                Rect::new(inner.x, inner.y + s(26), inner.w, s(30)),
                &self.fonts.value,
                value_c,
            );
            p.line(
                &card.detail,
                Rect::new(inner.x, inner.y + s(60), inner.w, s(20)),
                &self.fonts.small,
                theme::TEXT_MUTED,
            );
        }

        if let Some(r) = l.table {
            p.round_outline(r, theme::CARD, theme::CARD_BORDER, theme::RADIUS);
            let row_h = s(theme::ROW_H);
            let key_w = (r.w / 2).min(s(280));
            for (i, (k, v)) in self.view.table.iter().enumerate() {
                let y = r.y + s(4) + i as i32 * row_h;
                if i > 0 {
                    p.fill(
                        Rect::new(r.x + s(16), y, r.w - s(32), 1),
                        theme::CARD_BORDER,
                    );
                }
                p.line(
                    k,
                    Rect::new(r.x + s(16), y, key_w, row_h),
                    &self.fonts.body,
                    theme::TEXT_MUTED,
                );
                p.text(
                    v,
                    Rect::new(r.x + s(16) + key_w, y, r.w - key_w - s(32), row_h),
                    &self.fonts.body,
                    theme::TEXT,
                    DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
            }
        }

        if let (Some(r), Some(note)) = (l.note, self.view.note) {
            p.text(
                note,
                r,
                &self.fonts.small,
                theme::TEXT_MUTED,
                DT_LEFT | DT_WORDBREAK,
            );
        }
    }

    fn draw_item(&self, dis: &DRAWITEMSTRUCT) {
        if dis.CtlType != ODT_BUTTON {
            return;
        }
        let p = Painter {
            hdc: dis.hDC,
            dpi: self.dpi,
            fonts: &self.fonts,
        };
        let rc = dis.rcItem;
        let r = Rect::new(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
        let pressed = dis.itemState.0 & ODS_SELECTED.0 != 0;
        let disabled = dis.itemState.0 & ODS_DISABLED.0 != 0;
        let focus = dis.itemState.0 & ODS_FOCUS.0 != 0 && dis.itemState.0 & ODS_NOFOCUSRECT.0 == 0;
        let id = dis.CtlID as u16;

        if (ID_NAV_BASE..ID_NAV_BASE + 5).contains(&id) {
            let page = Page::from_index((id - ID_NAV_BASE) as usize).unwrap_or(Page::Dashboard);
            let active = page == self.ui.page;
            p.fill(r, theme::SIDEBAR);
            let bg = if pressed {
                theme::SIDEBAR_PRESSED
            } else if active {
                theme::SIDEBAR_ACTIVE
            } else {
                theme::SIDEBAR
            };
            let pill = Rect::new(r.x + p.s(8), r.y, r.w - p.s(16), r.h);
            if bg != theme::SIDEBAR {
                p.round(pill, bg, 8);
            }
            if active {
                p.round(
                    Rect::new(pill.x, pill.y + p.s(10), p.s(3), pill.h - p.s(20)),
                    theme::ACCENT,
                    2,
                );
            }
            let fg = if active {
                theme::TEXT
            } else {
                theme::TEXT_MUTED
            };
            let font = if active {
                &self.fonts.body_bold
            } else {
                &self.fonts.body
            };
            p.line(
                page.label(),
                Rect::new(pill.x + p.s(16), r.y, pill.w - p.s(20), r.h),
                font,
                fg,
            );
            if focus {
                p.ring(pill, theme::ACCENT, 8);
            }
            return;
        }

        let (label, primary) = match id {
            ID_START => ("Start", true),
            ID_STOP => ("Stop", false),
            _ => return,
        };
        p.fill(r, theme::CARD);
        let (bg, border, fg) = match (primary, disabled, pressed) {
            (_, true, _) => (theme::CARD, theme::CARD_BORDER, theme::TEXT_DISABLED),
            (true, false, false) => (theme::ACCENT, theme::ACCENT, Rgb(0xFF, 0xFF, 0xFF)),
            (true, false, true) => (
                theme::ACCENT_PRESSED,
                theme::ACCENT_PRESSED,
                Rgb(0xFF, 0xFF, 0xFF),
            ),
            (false, false, false) => (theme::CARD, theme::TEXT_MUTED, theme::TEXT),
            (false, false, true) => (theme::SIDEBAR_PRESSED, theme::TEXT_MUTED, theme::TEXT),
        };
        p.round_outline(r, bg, border, 8);
        p.text(
            label,
            r,
            &self.fonts.body_bold,
            fg,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        if focus {
            p.ring(r.inset(p.s(2)), Rgb(0xFF, 0xFF, 0xFF), 6);
        }
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
                // Re-entrant message while we are handling another one.
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

    let mut st = WindowState {
        hwnd,
        dpi,
        fonts: Fonts::new(dpi),
        nav,
        start_btn,
        stop_btn,
        ui: UiState::default(),
        info,
        engine,
        view: PageView::default(),
        layout: Layout::default(),
        wake_pending,
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

/// Result of handling a message while the window state is borrowed. Anything
/// that may re-enter the window procedure runs after the borrow is released.
enum Outcome {
    Done(LRESULT),
    /// Forward to `DefWindowProcW`.
    Default,
    /// Move/resize the window to this rectangle, then return 0.
    MoveTo(RECT),
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
            st.on_command(loword(wp.0));
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
        Some(self_test(hwnd, opts.screenshot_dir.as_deref()))
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

fn self_test(hwnd: HWND, shots: Option<&Path>) -> SelfTestReport {
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
    let no_net = with_state(hwnd, |s| {
        s.ui.status.rx_bytes_per_sec.is_none() && s.ui.status.tx_bytes_per_sec.is_none()
    });
    check(
        "no network values reported (monitor is Phase 4)".into(),
        no_net == Some(true),
    );
    capture("6-dashboard-running");

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

/// Save the window's client area (including child controls) as a 32-bit BMP.
fn capture_client(hwnd: HWND, path: &Path) -> Result<(), String> {
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
