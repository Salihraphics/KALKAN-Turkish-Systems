//! GDI painting: components (cards, tiles, strip, table, pills, buttons),
//! line icons, the shield mark and the window icon.
//!
//! Everything is drawn from `theme` tokens. Nothing here allocates per frame
//! except short UTF-16 conversions for text.

use windows::Win32::Foundation::{COLORREF, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush,
    DeleteDC, DrawTextW, Ellipse, FillRect, GetDC, GetStockObject, Polygon, Polyline, Rectangle,
    ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, DRAW_TEXT_FORMAT, DT_CALCRECT,
    DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER,
    DT_WORDBREAK, HBITMAP, HBRUSH, HDC, NULL_BRUSH, NULL_PEN, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

use kalkan_core::{EngineState, ENGINE_SCOPE, ENGINE_TITLE};

use super::{Fonts, Gdi, WindowState, HOVERED, ID_NAV_BASE, ID_START, ID_STOP};
use crate::layout::{place_columns, Rect};
use crate::model::{
    format_duration, Align, Card, Icon, NoticeKind, Page, RowStatus, Series, Source, StripItem,
    Table, Tile, Value, HISTORY_LEN,
};
use crate::theme::{self, scale, Rgb};

pub(super) fn brush(c: Rgb) -> Gdi {
    // SAFETY: plain GDI object creation, no pointers involved.
    Gdi(unsafe { CreateSolidBrush(COLORREF(c.colorref())) }.into())
}

fn pen(c: Rgb, width: i32) -> Gdi {
    // SAFETY: plain GDI object creation.
    Gdi(unsafe { CreatePen(PS_SOLID, width.max(1), COLORREF(c.colorref())) }.into())
}

fn to_rect(r: Rect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}

/// Filled polygon without outline on any DC.
fn fill_polygon(hdc: HDC, pts: &[POINT], c: Rgb) {
    let b = brush(c);
    // SAFETY: valid DC; `pts` outlives the call; selections restored.
    unsafe {
        let old_b = SelectObject(hdc, b.0);
        let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Polygon(hdc, pts);
        SelectObject(hdc, old_p);
        SelectObject(hdc, old_b);
    }
}

/// The KALKAN shield inside a square of side `size` at (x, y).
fn shield_points(x: i32, y: i32, size: i32) -> ([POINT; 6], [POINT; 4]) {
    let p = |fx: f32, fy: f32| POINT {
        x: x + (fx * size as f32).round() as i32,
        y: y + (fy * size as f32).round() as i32,
    };
    (
        [
            p(0.5, 0.0),
            p(0.95, 0.15),
            p(0.9, 0.6),
            p(0.5, 1.0),
            p(0.1, 0.6),
            p(0.05, 0.15),
        ],
        [p(0.5, 0.17), p(0.79, 0.27), p(0.75, 0.57), p(0.5, 0.83)],
    )
}

fn draw_shield(hdc: HDC, x: i32, y: i32, size: i32) {
    let (outer, inner) = shield_points(x, y, size);
    fill_polygon(hdc, &outer, theme::PRIMARY);
    fill_polygon(hdc, &inner, theme::PRIMARY_HOVER);
}

/// Build a window/taskbar icon showing the shield (no resource file needed).
pub(super) fn make_shield_icon(size: i32) -> Option<HICON> {
    let full = RECT {
        left: 0,
        top: 0,
        right: size,
        bottom: size,
    };
    // SAFETY: all GDI objects are created, used and released here; the
    // bitmaps are copied by CreateIconIndirect and freed by `Gdi` drops.
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let color = Gdi(CreateCompatibleBitmap(screen, size, size).into());
        let mask = Gdi(CreateBitmap(size, size, 1, 1, None).into());
        let old = SelectObject(dc, color.0);
        let black = brush(Rgb(0, 0, 0));
        FillRect(dc, &full, HBRUSH(black.0 .0));
        draw_shield(dc, 0, 0, size);
        // Mask: white = transparent, black = icon pixels.
        SelectObject(dc, mask.0);
        let white = brush(Rgb(255, 255, 255));
        FillRect(dc, &full, HBRUSH(white.0 .0));
        let (outer, _) = shield_points(0, 0, size);
        fill_polygon(dc, &outer, Rgb(0, 0, 0));
        SelectObject(dc, old);
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: HBITMAP(mask.0 .0),
            hbmColor: HBITMAP(color.0 .0),
        };
        CreateIconIndirect(&ii).ok()
    }
}

fn state_colors(s: EngineState) -> (Rgb, Rgb) {
    match s {
        EngineState::Stopped => (theme::NEUTRAL, theme::NEUTRAL_SOFT),
        EngineState::Starting | EngineState::Stopping => (theme::WARNING, theme::WARNING_SOFT),
        EngineState::Running => (theme::SUCCESS, theme::SUCCESS_SOFT),
        EngineState::Failed => (theme::ERROR, theme::ERROR_SOFT),
    }
}

/// Badge colours: LIVE is green, static facts are neutral, missing data amber.
fn badge_colors(v: &Value) -> (Rgb, Rgb) {
    match v {
        Value::Real {
            source: Source::Engine,
            ..
        } => (theme::SUCCESS, theme::SUCCESS_SOFT),
        Value::Real { .. } => (theme::TEXT_MUTED, theme::NEUTRAL_SOFT),
        Value::Unavailable { .. } => (theme::WARNING, theme::WARNING_SOFT),
    }
}

fn series_colors(s: Series) -> (Rgb, Rgb) {
    match s {
        Series::Download => (theme::DOWNLOAD, theme::PRIMARY_SOFT),
        Series::Upload => (theme::UPLOAD, Rgb(0x15, 0x22, 0x36)),
    }
}

pub(super) struct Painter<'a> {
    pub hdc: HDC,
    pub dpi: u32,
    pub fonts: &'a Fonts,
}

impl Painter<'_> {
    pub fn s(&self, dip: i32) -> i32 {
        scale(dip, self.dpi)
    }

    /// Line width for icons and graphs.
    fn stroke_w(&self) -> i32 {
        (scale(3, self.dpi) / 2).max(1)
    }

    pub fn fill(&self, r: Rect, c: Rgb) {
        let b = brush(c);
        // SAFETY: valid DC and brush; RECT lives for the call.
        unsafe { FillRect(self.hdc, &to_rect(r), HBRUSH(b.0 .0)) };
    }

    /// Filled rounded rectangle without an outline.
    pub fn round(&self, r: Rect, c: Rgb, radius: i32) {
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
    pub fn round_outline(&self, r: Rect, fill: Rgb, border: Rgb, radius: i32) {
        let b = brush(fill);
        let p = pen(border, 1);
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
    pub fn ring(&self, r: Rect, c: Rgb, radius: i32) {
        let p = pen(c, self.s(2));
        let d = self.s(radius) * 2;
        // SAFETY: valid DC; selections restored before the pen drops.
        unsafe {
            let old_b = SelectObject(self.hdc, GetStockObject(NULL_BRUSH));
            let old_p = SelectObject(self.hdc, p.0);
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

    pub fn dot(&self, cx: i32, cy: i32, radius: i32, c: Rgb) {
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

    /// Status dot with a soft halo.
    pub fn halo_dot(&self, cx: i32, cy: i32, c: Rgb, soft: Rgb) {
        self.dot(cx, cy, self.s(7), soft);
        self.dot(cx, cy, self.s(4), c);
    }

    pub fn text(&self, s: &str, r: Rect, font: &Gdi, c: Rgb, flags: DRAW_TEXT_FORMAT) {
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
    pub fn measure(&self, s: &str, font: &Gdi) -> i32 {
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

    pub fn line(&self, s: &str, r: Rect, font: &Gdi, c: Rgb) {
        self.text(
            s,
            r,
            font,
            c,
            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
    }

    fn line_right(&self, s: &str, r: Rect, font: &Gdi, c: Rgb) {
        self.text(
            s,
            r,
            font,
            c,
            DT_RIGHT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
    }

    /// Pill badge, right-aligned at `right`, centred on `cy`. Returns its width.
    pub fn badge(&self, label: &str, right: i32, cy: i32, fg: Rgb, bg: Rgb) -> i32 {
        let w = self.measure(label, &self.fonts.small) + self.s(14);
        let h = self.s(20);
        let r = Rect::new(right - w, cy - h / 2, w, h);
        self.round(r, bg, 10);
        self.text(
            label,
            r,
            &self.fonts.small,
            fg,
            DT_CENTER | DT_SINGLELINE | DT_VCENTER,
        );
        w
    }

    /// Polyline in the icon/graph stroke width.
    fn stroke(&self, pts: &[POINT], c: Rgb, width: i32) {
        let p = pen(c, width);
        // SAFETY: valid DC; `pts` outlives the call; pen restored.
        unsafe {
            let old = SelectObject(self.hdc, p.0);
            let _ = Polyline(self.hdc, pts);
            SelectObject(self.hdc, old);
        }
    }

    fn outline_rect(&self, l: i32, t: i32, r: i32, b: i32, c: Rgb) {
        let p = pen(c, self.stroke_w());
        // SAFETY: valid DC; selections restored.
        unsafe {
            let old_b = SelectObject(self.hdc, GetStockObject(NULL_BRUSH));
            let old_p = SelectObject(self.hdc, p.0);
            let _ = Rectangle(self.hdc, l, t, r, b);
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    fn outline_ellipse(&self, l: i32, t: i32, r: i32, b: i32, c: Rgb) {
        let p = pen(c, self.stroke_w());
        // SAFETY: valid DC; selections restored.
        unsafe {
            let old_b = SelectObject(self.hdc, GetStockObject(NULL_BRUSH));
            let old_p = SelectObject(self.hdc, p.0);
            let _ = Ellipse(self.hdc, l, t, r, b);
            SelectObject(self.hdc, old_p);
            SelectObject(self.hdc, old_b);
        }
    }

    /// 16-DIP line icon with its top-left at (x, y).
    pub fn icon(&self, icon: Icon, x: i32, y: i32, c: Rgb) {
        let size = self.s(theme::ICON);
        let u = |v: i32| v * size / 16;
        let p = |px: i32, py: i32| POINT {
            x: x + u(px),
            y: y + u(py),
        };
        let w = self.stroke_w();
        let line = |pts: &[POINT]| self.stroke(pts, c, w);
        match icon {
            Icon::Dashboard => {
                for (l, t) in [(1, 1), (9, 1), (1, 9), (9, 9)] {
                    self.outline_rect(x + u(l), y + u(t), x + u(l + 6), y + u(t + 6), c);
                }
            }
            Icon::Network => {
                line(&[p(5, 2), p(5, 14)]);
                line(&[p(2, 11), p(5, 14), p(8, 11)]);
                line(&[p(11, 14), p(11, 2)]);
                line(&[p(8, 5), p(11, 2), p(14, 5)]);
            }
            Icon::Statistics => {
                self.outline_rect(x + u(1), y + u(9), x + u(5), y + u(15), c);
                self.outline_rect(x + u(6), y + u(5), x + u(10), y + u(15), c);
                self.outline_rect(x + u(11), y + u(1), x + u(15), y + u(15), c);
            }
            Icon::Logs => {
                self.outline_rect(x + u(2), y + u(1), x + u(14), y + u(15), c);
                for ly in [5, 8, 11] {
                    line(&[p(5, ly), p(11, ly)]);
                }
            }
            Icon::Settings => {
                for (ly, kx) in [(4, 5), (8, 11), (12, 7)] {
                    line(&[p(1, ly), p(15, ly)]);
                    self.dot(x + u(kx), y + u(ly), self.s(2), c);
                }
            }
            Icon::Download => {
                line(&[p(8, 1), p(8, 11)]);
                line(&[p(4, 7), p(8, 11), p(12, 7)]);
                line(&[p(3, 14), p(13, 14)]);
            }
            Icon::Upload => {
                line(&[p(8, 11), p(8, 1)]);
                line(&[p(4, 5), p(8, 1), p(12, 5)]);
                line(&[p(3, 14), p(13, 14)]);
            }
            Icon::Cpu => {
                self.outline_rect(x + u(4), y + u(4), x + u(12), y + u(12), c);
                self.outline_rect(x + u(6), y + u(6), x + u(10), y + u(10), c);
                for k in [6, 10] {
                    line(&[p(k, 1), p(k, 4)]);
                    line(&[p(k, 12), p(k, 15)]);
                    line(&[p(1, k), p(4, k)]);
                    line(&[p(12, k), p(15, k)]);
                }
            }
            Icon::Memory => {
                self.outline_rect(x + u(1), y + u(4), x + u(15), y + u(11), c);
                for k in [4, 7, 10, 13] {
                    line(&[p(k, 11), p(k, 14)]);
                }
            }
            Icon::Shield => {
                let (outer, _) = shield_points(x + u(1), y, u(14));
                let mut closed = [POINT::default(); 7];
                closed[..6].copy_from_slice(&outer);
                closed[6] = outer[0];
                line(&closed);
            }
            Icon::Interface => {
                self.outline_ellipse(x + u(1), y + u(1), x + u(15), y + u(15), c);
                self.outline_ellipse(x + u(5), y + u(1), x + u(11), y + u(15), c);
                line(&[p(1, 8), p(15, 8)]);
            }
            Icon::Clock => {
                self.outline_ellipse(x + u(1), y + u(1), x + u(15), y + u(15), c);
                line(&[p(8, 4), p(8, 8), p(11, 10)]);
            }
            Icon::Peak => {
                line(&[p(1, 14), p(5, 9), p(8, 11), p(14, 3)]);
                line(&[p(10, 3), p(14, 3), p(14, 7)]);
            }
            Icon::Average => {
                line(&[p(1, 11), p(4, 5), p(8, 11), p(12, 5), p(15, 11)]);
                line(&[p(1, 8), p(15, 8)]);
            }
            Icon::File => {
                line(&[p(3, 1), p(10, 1), p(13, 4), p(13, 15), p(3, 15), p(3, 1)]);
                line(&[p(10, 1), p(10, 4), p(13, 4)]);
            }
        }
    }

    pub fn shield(&self, x: i32, y: i32, size: i32) {
        draw_shield(self.hdc, x, y, size);
    }
}

// ── Components ───────────────────────────────────────────────────────────────

impl Painter<'_> {
    fn card(&self, card: &Card, r: Rect) {
        self.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
        let inner = r.inset(self.s(16));
        let (bfg, bbg) = badge_colors(&card.value);
        let badge_w = self.badge(
            card.value.badge(),
            inner.right(),
            inner.y + self.s(9),
            bfg,
            bbg,
        );
        let live = card.value.is_real();
        let icon_c = if live {
            theme::PRIMARY
        } else {
            theme::TEXT_MUTED
        };
        self.icon(card.icon, inner.x, inner.y + self.s(1), icon_c);
        let tx = inner.x + self.s(24);
        self.line(
            card.title,
            Rect::new(
                tx,
                inner.y,
                (inner.right() - badge_w - self.s(8) - tx).max(0),
                self.s(18),
            ),
            &self.fonts.body_bold,
            theme::TEXT_MUTED,
        );
        let value_c = if live {
            theme::TEXT
        } else {
            theme::TEXT_DISABLED
        };
        self.line(
            card.value.text(),
            Rect::new(inner.x, inner.y + self.s(24), inner.w, self.s(30)),
            &self.fonts.value,
            value_c,
        );
        self.line(
            &card.detail,
            Rect::new(inner.x, inner.y + self.s(56), inner.w, self.s(18)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
    }

    fn strip(&self, items: &[StripItem], r: Rect) {
        self.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
        if items.is_empty() {
            return;
        }
        let n = items.len() as i32;
        let seg = r.w / n;
        for (i, it) in items.iter().enumerate() {
            let x = r.x + seg * i as i32;
            if i > 0 {
                self.fill(
                    Rect::new(x, r.y + self.s(14), 1, r.h - self.s(28)),
                    theme::BORDER,
                );
            }
            let inner = Rect::new(
                x + self.s(16),
                r.y + self.s(12),
                seg - self.s(24),
                r.h - self.s(24),
            );
            self.icon(it.icon, inner.x, inner.y + self.s(1), theme::TEXT_MUTED);
            self.line(
                it.label,
                Rect::new(
                    inner.x + self.s(22),
                    inner.y,
                    inner.w - self.s(22),
                    self.s(18),
                ),
                &self.fonts.small,
                theme::TEXT_MUTED,
            );
            let (text, font, c) = match &it.value {
                Value::Real { text, .. } => (text.as_str(), &self.fonts.body_bold, theme::TEXT),
                Value::Unavailable { reason } => {
                    (reason.as_str(), &self.fonts.small, theme::TEXT_DISABLED)
                }
            };
            self.line(
                text,
                Rect::new(inner.x, inner.y + self.s(20), inner.w, self.s(20)),
                font,
                c,
            );
        }
    }

    fn tile(&self, tile: &Tile, r: Rect, history: &crate::model::History) {
        self.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
        let inner = r.inset(self.s(16));
        let (series_c, series_soft) = series_colors(tile.series);
        let (bfg, bbg) = badge_colors(&tile.value);
        self.badge(
            tile.value.badge(),
            inner.right(),
            inner.y + self.s(9),
            bfg,
            bbg,
        );
        self.icon(tile.icon, inner.x, inner.y + self.s(1), series_c);
        self.line(
            tile.title,
            Rect::new(inner.x + self.s(24), inner.y, inner.w / 2, self.s(18)),
            &self.fonts.body_bold,
            theme::TEXT_MUTED,
        );
        let live = tile.value.is_real();
        self.line(
            tile.value.text(),
            Rect::new(inner.x, inner.y + self.s(22), inner.w, self.s(34)),
            &self.fonts.title,
            if live {
                theme::TEXT
            } else {
                theme::TEXT_DISABLED
            },
        );
        self.line(
            &tile.detail,
            Rect::new(inner.x, inner.y + self.s(56), inner.w, self.s(18)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
        // Graph: last HISTORY_LEN samples, newest on the right.
        let g = Rect::new(
            inner.x,
            inner.y + self.s(78),
            inner.w,
            inner.bottom() - (inner.y + self.s(78)),
        );
        self.fill(Rect::new(g.x, g.bottom() - 1, g.w, 1), theme::BORDER);
        if !live || history.len() < 2 || g.h < self.s(8) {
            return;
        }
        let max = history.max(tile.series).max(1024) as f64;
        let step = g.w as f64 / (HISTORY_LEN - 1) as f64;
        let n = history.len();
        let mut pts = [POINT::default(); HISTORY_LEN + 2];
        for (i, v) in history.iter(tile.series).enumerate() {
            let slot = HISTORY_LEN - n + i;
            pts[i] = POINT {
                x: g.x + (slot as f64 * step).round() as i32,
                y: g.bottom() - 1 - ((v as f64 / max) * (g.h - 2) as f64).round() as i32,
            };
        }
        // Area under the line, closed along the baseline.
        pts[n] = POINT {
            x: pts[n - 1].x,
            y: g.bottom() - 1,
        };
        pts[n + 1] = POINT {
            x: pts[0].x,
            y: g.bottom() - 1,
        };
        fill_polygon(self.hdc, &pts[..n + 2], series_soft);
        self.stroke(&pts[..n], series_c, self.s(2));
    }

    fn table(&self, t: &Table, r: Rect, visible: usize) {
        self.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
        let pad = self.s(16);
        let cols: Vec<(i32, u16, u8)> = t
            .columns
            .iter()
            .map(|c| (c.min_w, c.weight, c.priority))
            .collect();
        let placed = place_columns(&cols, r.x + pad, r.w - 2 * pad, self.dpi);
        let head_h = self.s(theme::TABLE_HEADER_H);
        let row_h = self.s(theme::ROW_H);
        let mut y = r.y + self.s(4);
        let cell_pad = self.s(6);
        for pc in &placed {
            let col = &t.columns[pc.index];
            let cr = Rect::new(pc.x + cell_pad, y, pc.w - 2 * cell_pad, head_h);
            match col.align {
                Align::Left => self.line(col.title, cr, &self.fonts.small, theme::TEXT_MUTED),
                Align::Right => {
                    self.line_right(col.title, cr, &self.fonts.small, theme::TEXT_MUTED)
                }
            }
        }
        y += head_h;
        self.fill(Rect::new(r.x + pad, y, r.w - 2 * pad, 1), theme::BORDER);

        if t.rows.is_empty() {
            let msg = t.empty.as_deref().unwrap_or("Nothing to show.");
            self.line(
                msg,
                Rect::new(r.x + pad + cell_pad, y, r.w - 2 * pad, row_h),
                &self.fonts.body,
                theme::TEXT_MUTED,
            );
            return;
        }
        let overflow = t.rows.len() > visible;
        let shown = if overflow {
            visible.saturating_sub(1)
        } else {
            visible
        };
        for (i, row) in t.rows.iter().take(shown).enumerate() {
            if i > 0 {
                self.fill(Rect::new(r.x + pad, y, r.w - 2 * pad, 1), theme::BORDER);
            }
            let c = if row.muted {
                theme::TEXT_MUTED
            } else {
                theme::TEXT
            };
            for (k, pc) in placed.iter().enumerate() {
                let mut cr = Rect::new(pc.x + cell_pad, y, pc.w - 2 * cell_pad, row_h);
                if k == 0 && row.status != RowStatus::None {
                    let (dc, soft) = match row.status {
                        RowStatus::Connected => (theme::SUCCESS, theme::SUCCESS_SOFT),
                        RowStatus::Virtual => (theme::NEUTRAL, theme::NEUTRAL_SOFT),
                        _ => (theme::TEXT_DISABLED, theme::CARD),
                    };
                    self.halo_dot(cr.x + self.s(6), y + row_h / 2, dc, soft);
                    cr = Rect::new(cr.x + self.s(18), cr.y, (cr.w - self.s(18)).max(0), cr.h);
                }
                let text = row.cells.get(pc.index).map(String::as_str).unwrap_or("");
                match t.columns[pc.index].align {
                    Align::Left => self.line(text, cr, &self.fonts.body, c),
                    Align::Right => self.line_right(text, cr, &self.fonts.body, c),
                }
            }
            y += row_h;
        }
        if overflow {
            self.fill(Rect::new(r.x + pad, y, r.w - 2 * pad, 1), theme::BORDER);
            let more = t.rows.len() - shown;
            self.line(
                &format!("+{more} more (enlarge the window to see all)"),
                Rect::new(r.x + pad + cell_pad, y, r.w - 2 * pad, row_h),
                &self.fonts.small,
                theme::TEXT_MUTED,
            );
        }
    }
}

// ── Window painting ──────────────────────────────────────────────────────────

impl WindowState {
    pub(super) fn paint(&self, hdc: HDC) {
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
        p.fill(
            Rect::new(l.sidebar.right() - 1, 0, 1, l.sidebar.h),
            theme::BORDER,
        );
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
        let (sc, ssoft) = state_colors(self.ui.engine);
        p.halo_dot(f.x + s(26), f.y + s(18), sc, ssoft);
        p.line(
            &format!("Engine: {}", self.ui.engine),
            Rect::new(f.x + s(40), f.y + s(8), f.w - s(48), s(20)),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );
        p.line(
            crate::model::privilege_value(self.info.elevated).text(),
            Rect::new(f.x + s(40), f.y + s(28), f.w - s(48), s(20)),
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
                NoticeKind::Info => (theme::PRIMARY_SOFT, theme::PRIMARY),
                NoticeKind::Warning => (theme::WARNING_SOFT, theme::WARNING),
                NoticeKind::Error => (theme::ERROR_SOFT, theme::ERROR),
            };
            p.round(r, bg, 8);
            p.round(Rect::new(r.x, r.y + s(8), s(3), r.h - s(16)), fg, 2);
            p.line(
                &n.message,
                Rect::new(r.x + s(16), r.y, r.w - s(24), r.h),
                &self.fonts.body,
                theme::TEXT,
            );
        }

        if let Some(r) = l.engine_panel {
            self.paint_engine_panel(&p, r);
        }
        for (tile, r) in self.view.tiles.iter().zip(&l.tiles) {
            p.tile(tile, *r, &self.ui.history);
        }
        for (card, r) in self.view.cards.iter().zip(&l.cards) {
            p.card(card, *r);
        }
        if let Some(r) = l.strip {
            p.strip(&self.view.strip, r);
        }
        if let (Some(r), Some(t)) = (l.table, &self.view.table) {
            p.table(t, r, l.table_rows_visible);
        }
        if let (Some(r), Some(note)) = (l.note, &self.view.note) {
            p.text(
                note,
                r,
                &self.fonts.small,
                theme::TEXT_MUTED,
                DT_LEFT | DT_WORDBREAK,
            );
        }
    }

    fn paint_engine_panel(&self, p: &Painter, r: Rect) {
        let s = |d| p.s(d);
        p.round_outline(r, theme::CARD, theme::BORDER, theme::RADIUS);
        let btn_left = self.layout.start_btn.map_or(r.right(), |b| b.x);
        let text_w = (btn_left - r.x - s(36)).max(0);

        p.shield(r.x + s(20), r.y + s(12), s(16));
        p.line(
            ENGINE_TITLE,
            Rect::new(r.x + s(44), r.y + s(10), (text_w - s(24)).max(0), s(20)),
            &self.fonts.body_bold,
            theme::TEXT,
        );

        // State pill + uptime.
        let (c, soft) = state_colors(self.ui.engine);
        let label = if self.closing && !self.ui.shutdown_complete {
            "Shutting down…"
        } else {
            self.ui.engine.label()
        };
        let pill_h = s(28);
        let pill_w = p.measure(label, &self.fonts.body_bold) + s(40);
        let pill = Rect::new(r.x + s(20), r.y + s(36), pill_w, pill_h);
        p.round(pill, soft, 14);
        p.halo_dot(pill.x + s(15), pill.y + pill_h / 2, c, soft);
        p.dot(pill.x + s(15), pill.y + pill_h / 2, s(4), c);
        p.line(
            label,
            Rect::new(pill.x + s(28), pill.y, pill_w - s(32), pill_h),
            &self.fonts.body_bold,
            c,
        );
        let sub = match (self.ui.engine, self.ui.status.running_for) {
            (EngineState::Running, Some(d)) => format!("Running for {}", format_duration(d)),
            (EngineState::Stopped, _) => "Ready to start".to_owned(),
            (EngineState::Failed, _) => "See the message above; Start retries".to_owned(),
            _ => String::new(),
        };
        p.line(
            &sub,
            Rect::new(
                pill.right() + s(12),
                pill.y,
                (btn_left - pill.right() - s(24)).max(0),
                pill_h,
            ),
            &self.fonts.small,
            theme::TEXT_MUTED,
        );

        // Full-width scope line below the buttons (must stay readable).
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

    pub(super) fn draw_item(&self, dis: &DRAWITEMSTRUCT) {
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
        let state = dis.itemState.0;
        let pressed = state & ODS_SELECTED.0 != 0;
        let disabled = state & ODS_DISABLED.0 != 0;
        let focus = state & ODS_FOCUS.0 != 0 && state & ODS_NOFOCUSRECT.0 == 0;
        let hovered = HOVERED.with(|h| h.get()) == dis.hwndItem.0 as isize && !disabled;
        let id = dis.CtlID as u16;

        if (ID_NAV_BASE..ID_NAV_BASE + 5).contains(&id) {
            let page = Page::from_index((id - ID_NAV_BASE) as usize).unwrap_or(Page::Dashboard);
            let active = page == self.ui.page;
            p.fill(r, theme::SIDEBAR);
            let bg = match (pressed, active, hovered) {
                (true, _, _) => Some(theme::CARD_HOVER),
                (false, true, _) => Some(theme::PRIMARY_SOFT),
                (false, false, true) => Some(theme::SIDEBAR_HOVER),
                _ => None,
            };
            let pill = Rect::new(r.x + p.s(8), r.y, r.w - p.s(16), r.h);
            if let Some(bg) = bg {
                p.round(pill, bg, 8);
            }
            if active {
                p.round(
                    Rect::new(pill.x, pill.y + p.s(10), p.s(3), pill.h - p.s(20)),
                    theme::PRIMARY,
                    2,
                );
            }
            let (ic, tc, font) = if active {
                (theme::PRIMARY, theme::TEXT, &self.fonts.body_bold)
            } else if hovered {
                (theme::TEXT, theme::TEXT, &self.fonts.body)
            } else {
                (theme::TEXT_MUTED, theme::TEXT_MUTED, &self.fonts.body)
            };
            let ix = pill.x + p.s(14);
            p.icon(page.icon(), ix, r.y + (r.h - p.s(theme::ICON)) / 2, ic);
            p.line(
                page.label(),
                Rect::new(ix + p.s(26), r.y, pill.right() - ix - p.s(30), r.h),
                font,
                tc,
            );
            if focus {
                p.ring(pill, theme::PRIMARY, 8);
            }
            return;
        }

        let primary = match id {
            ID_START => true,
            ID_STOP => false,
            _ => return,
        };
        let label = if primary { "Start" } else { "Stop" };
        p.fill(r, theme::CARD);
        let (bg, border, fg) = match (primary, disabled, pressed, hovered) {
            (_, true, _, _) => (theme::CARD, theme::BORDER, theme::TEXT_DISABLED),
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
            (true, false, false, false) => (theme::PRIMARY, theme::PRIMARY, theme::ON_PRIMARY),
            (false, false, true, _) => (theme::BORDER, theme::BORDER_STRONG, theme::TEXT),
            (false, false, false, true) => (theme::CARD_HOVER, theme::TEXT_MUTED, theme::TEXT),
            (false, false, false, false) => (theme::CARD, theme::BORDER_STRONG, theme::TEXT),
        };
        p.round_outline(r, bg, border, 8);
        // Glyph (play triangle / stop square) + label, centred together.
        let gs = p.s(10);
        let lw = p.measure(label, &self.fonts.body_bold);
        let total = gs + p.s(8) + lw;
        let gx = r.x + (r.w - total) / 2;
        let gy = r.y + (r.h - gs) / 2;
        if primary {
            fill_polygon(
                p.hdc,
                &[
                    POINT { x: gx, y: gy },
                    POINT {
                        x: gx + gs,
                        y: gy + gs / 2,
                    },
                    POINT { x: gx, y: gy + gs },
                ],
                fg,
            );
        } else {
            p.fill(Rect::new(gx, gy, gs, gs), fg);
        }
        p.line(
            label,
            Rect::new(gx + gs + p.s(8), r.y, lw + p.s(4), r.h),
            &self.fonts.body_bold,
            fg,
        );
        if focus {
            p.ring(
                r.inset(p.s(2)),
                if primary { theme::TEXT } else { theme::PRIMARY },
                6,
            );
        }
    }
}
