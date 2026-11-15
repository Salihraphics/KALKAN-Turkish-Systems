//! KALKAN visual identity: colours and metrics.
//!
//! Sizes are in device-independent pixels (DIP, 96 DPI = 1.0) and are scaled
//! by the window DPI at layout time.

/// An sRGB colour. Converted to a Win32 `COLORREF` with [`Rgb::colorref`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `0x00BBGGRR`, the Win32 `COLORREF` layout.
    pub const fn colorref(self) -> u32 {
        (self.0 as u32) | ((self.1 as u32) << 8) | ((self.2 as u32) << 16)
    }
}

// ── Palette (dark theme) ─────────────────────────────────────────────────────
pub const BG: Rgb = Rgb(0x14, 0x17, 0x1C);
pub const SIDEBAR: Rgb = Rgb(0x0D, 0x0F, 0x13);
pub const SIDEBAR_ACTIVE: Rgb = Rgb(0x1E, 0x23, 0x2B);
pub const SIDEBAR_PRESSED: Rgb = Rgb(0x26, 0x2C, 0x36);
pub const CARD: Rgb = Rgb(0x1C, 0x20, 0x27);
pub const CARD_BORDER: Rgb = Rgb(0x2A, 0x30, 0x3A);
pub const TEXT: Rgb = Rgb(0xE6, 0xE8, 0xEB);
pub const TEXT_MUTED: Rgb = Rgb(0x8A, 0x93, 0xA0);
pub const TEXT_DISABLED: Rgb = Rgb(0x55, 0x5C, 0x67);
/// KALKAN brand accent ("shield blue").
pub const ACCENT: Rgb = Rgb(0x3B, 0x82, 0xF6);
pub const ACCENT_PRESSED: Rgb = Rgb(0x25, 0x63, 0xEB);
pub const OK: Rgb = Rgb(0x22, 0xC5, 0x5E);
pub const WARN: Rgb = Rgb(0xF5, 0x9E, 0x0B);
pub const DANGER: Rgb = Rgb(0xEF, 0x44, 0x44);
pub const NEUTRAL: Rgb = Rgb(0x6B, 0x72, 0x80);
pub const NOTICE_INFO_BG: Rgb = Rgb(0x17, 0x25, 0x3D);
pub const NOTICE_ERROR_BG: Rgb = Rgb(0x3B, 0x17, 0x1A);

// ── Metrics (DIP) ────────────────────────────────────────────────────────────
pub const SIDEBAR_W: i32 = 208;
pub const LOGO_H: i32 = 88;
pub const NAV_H: i32 = 40;
pub const NAV_GAP: i32 = 4;
pub const SIDEBAR_FOOTER_H: i32 = 64;
pub const PAD: i32 = 24;
pub const GAP: i32 = 16;
pub const HEADER_H: i32 = 64;
pub const NOTICE_H: i32 = 40;
pub const PANEL_H: i32 = 88;
pub const CARD_H: i32 = 112;
pub const CARD_MIN_W: i32 = 200;
pub const MAX_COLUMNS: usize = 4;
pub const ROW_H: i32 = 32;
pub const NOTE_H: i32 = 56;
pub const BTN_W: i32 = 104;
pub const BTN_H: i32 = 36;
pub const RADIUS: i32 = 10;

/// Minimum client area. Every page's content fits at this size.
pub const MIN_CLIENT_W: i32 = 760;
pub const MIN_CLIENT_H: i32 = 560;
/// Initial client area.
pub const DEFAULT_CLIENT_W: i32 = 1040;
pub const DEFAULT_CLIENT_H: i32 = 660;

// ── Type scale (points) ──────────────────────────────────────────────────────
pub const FONT_FACE: &str = "Segoe UI";
pub const FONT_TITLE_PT: i32 = 20;
pub const FONT_VALUE_PT: i32 = 16;
pub const FONT_BRAND_PT: i32 = 14;
pub const FONT_BODY_PT: i32 = 10;
pub const FONT_SMALL_PT: i32 = 9;

/// Scale a DIP length to physical pixels for `dpi`.
pub fn scale(dip: i32, dpi: u32) -> i32 {
    // Round half away from zero; DIP values are non-negative in practice.
    ((dip as i64 * dpi as i64 + 48) / 96) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_layout() {
        assert_eq!(Rgb(0x11, 0x22, 0x33).colorref(), 0x0033_2211);
    }

    #[test]
    fn scaling() {
        assert_eq!(scale(100, 96), 100);
        assert_eq!(scale(100, 144), 150);
        assert_eq!(scale(100, 192), 200);
        assert_eq!(scale(1, 120), 1);
    }

    // Compile-time check: the default window is never below the minimum.
    const _: () = assert!(MIN_CLIENT_W <= DEFAULT_CLIENT_W && MIN_CLIENT_H <= DEFAULT_CLIENT_H);
}
