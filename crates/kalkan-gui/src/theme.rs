//! KALKAN design tokens: colours, metrics and type scale.
//!
//! Every colour the GUI paints comes from this file. Sizes are in
//! device-independent pixels (DIP, 96 DPI = 1.0) and are scaled by the window
//! DPI at layout time.
//!
//! Brand: a restrained **teal** primary on a deep neutral dark theme. No
//! gradients, no shadows, no animations.

/// An sRGB colour. Converted to a Win32 `COLORREF` with [`Rgb::colorref`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `0x00BBGGRR`, the Win32 `COLORREF` layout.
    pub const fn colorref(self) -> u32 {
        (self.0 as u32) | ((self.1 as u32) << 8) | ((self.2 as u32) << 16)
    }

    /// WCAG relative luminance (0..1).
    pub fn luminance(self) -> f64 {
        let ch = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(self.0) + 0.7152 * ch(self.1) + 0.0722 * ch(self.2)
    }

    /// WCAG contrast ratio between two colours (1..21).
    pub fn contrast(self, other: Rgb) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

// ── Brand / primary ──────────────────────────────────────────────────────────
pub const PRIMARY: Rgb = Rgb(0x14, 0xB8, 0xA6);
pub const PRIMARY_HOVER: Rgb = Rgb(0x2D, 0xD4, 0xBF);
pub const PRIMARY_PRESSED: Rgb = Rgb(0x0F, 0x94, 0x88);
/// Tinted background for active navigation, info notices and badges.
pub const PRIMARY_SOFT: Rgb = Rgb(0x10, 0x2B, 0x2A);
/// Text on a PRIMARY fill (dark for contrast; white on teal is too weak).
pub const ON_PRIMARY: Rgb = Rgb(0x04, 0x1F, 0x1C);

// ── Surfaces ─────────────────────────────────────────────────────────────────
pub const BG: Rgb = Rgb(0x0F, 0x12, 0x16);
pub const SIDEBAR: Rgb = Rgb(0x0A, 0x0C, 0x0F);
pub const SIDEBAR_HOVER: Rgb = Rgb(0x15, 0x19, 0x1F);
pub const CARD: Rgb = Rgb(0x16, 0x1A, 0x20);
pub const CARD_HOVER: Rgb = Rgb(0x1D, 0x22, 0x2A);
pub const BORDER: Rgb = Rgb(0x26, 0x2C, 0x35);
pub const BORDER_STRONG: Rgb = Rgb(0x3A, 0x43, 0x50);

// ── Text ─────────────────────────────────────────────────────────────────────
pub const TEXT: Rgb = Rgb(0xE8, 0xEB, 0xEF);
pub const TEXT_MUTED: Rgb = Rgb(0x8B, 0x95, 0xA3);
pub const TEXT_DISABLED: Rgb = Rgb(0x4E, 0x56, 0x62);

// ── Status ───────────────────────────────────────────────────────────────────
pub const SUCCESS: Rgb = Rgb(0x34, 0xD3, 0x99);
pub const SUCCESS_SOFT: Rgb = Rgb(0x10, 0x2A, 0x21);
pub const WARNING: Rgb = Rgb(0xFB, 0xBF, 0x24);
pub const WARNING_SOFT: Rgb = Rgb(0x2E, 0x24, 0x12);
pub const ERROR: Rgb = Rgb(0xF8, 0x71, 0x71);
pub const ERROR_SOFT: Rgb = Rgb(0x33, 0x1A, 0x1C);
pub const NEUTRAL: Rgb = Rgb(0x6B, 0x72, 0x80);
pub const NEUTRAL_SOFT: Rgb = Rgb(0x1E, 0x22, 0x29);

// ── Data series ──────────────────────────────────────────────────────────────
pub const DOWNLOAD: Rgb = PRIMARY;
pub const UPLOAD: Rgb = Rgb(0x60, 0xA5, 0xFA);

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
pub const PANEL_H: i32 = 100;
/// Engine panel: top of the full-width scope line, below the buttons.
pub const PANEL_SCOPE_Y: i32 = 72;
pub const CARD_H: i32 = 104;
pub const CARD_MIN_W: i32 = 200;
pub const MAX_COLUMNS: usize = 4;
pub const STRIP_H: i32 = 64;
pub const TILE_H: i32 = 136;
pub const TABLE_HEADER_H: i32 = 32;
pub const ROW_H: i32 = 32;
pub const NOTE_H: i32 = 48;
pub const BTN_W: i32 = 104;
pub const BTN_H: i32 = 36;
pub const RADIUS: i32 = 10;
pub const ICON: i32 = 16;
/// Header action buttons (Logs, Settings).
pub const ACTION_W: i32 = 96;
/// Log viewer toolbar and rows.
pub const TOOLBAR_H: i32 = 36;
pub const CHIP_W: i32 = 76;
pub const LOG_STATUS_H: i32 = 28;
pub const LOG_ROW_H: i32 = 24;
/// Settings page.
pub const SETTINGS_STATUS_H: i32 = 64;
pub const SETTINGS_ROW_H: i32 = 48;
pub const SEGMENT_W: i32 = 50;
pub const NUMBER_W: i32 = 110;
pub const SWITCH_W: i32 = 44;

/// Minimum client area. Every page's content fits at this size.
pub const MIN_CLIENT_W: i32 = 760;
pub const MIN_CLIENT_H: i32 = 600;
/// Initial client area.
pub const DEFAULT_CLIENT_W: i32 = 1040;
pub const DEFAULT_CLIENT_H: i32 = 680;

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

    /// Text colours meet WCAG AA (4.5:1) on every surface they are drawn on.
    #[test]
    fn text_contrast_meets_wcag() {
        for surface in [BG, SIDEBAR, CARD, CARD_HOVER, PRIMARY_SOFT] {
            assert!(TEXT.contrast(surface) >= 4.5, "TEXT on {surface:?}");
            assert!(
                TEXT_MUTED.contrast(surface) >= 4.5,
                "TEXT_MUTED on {surface:?}"
            );
        }
        assert!(
            ON_PRIMARY.contrast(PRIMARY) >= 4.5,
            "button label on PRIMARY"
        );
        for (fg, bg) in [
            (SUCCESS, SUCCESS_SOFT),
            (WARNING, WARNING_SOFT),
            (ERROR, ERROR_SOFT),
            (PRIMARY, PRIMARY_SOFT),
            (UPLOAD, CARD),
        ] {
            assert!(fg.contrast(bg) >= 4.5, "{fg:?} on {bg:?}");
        }
    }

    #[test]
    fn disabled_text_is_visibly_dimmer() {
        assert!(TEXT_DISABLED.contrast(CARD) < TEXT_MUTED.contrast(CARD));
    }

    // Compile-time check: the default window is never below the minimum.
    const _: () = assert!(MIN_CLIENT_W <= DEFAULT_CLIENT_W && MIN_CLIENT_H <= DEFAULT_CLIENT_H);
}
