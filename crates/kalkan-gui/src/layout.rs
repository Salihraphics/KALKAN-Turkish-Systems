//! Pure layout computation: client size + DPI → rectangles.
//!
//! No Win32 here, so the responsive behaviour is unit-tested on any OS.

use crate::theme::{self, scale};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
    /// Shrink by `d` on every side (never below zero size).
    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2 * d).max(0),
            (self.h - 2 * d).max(0),
        )
    }
    pub fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// What the current page needs space for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Content {
    pub engine_panel: bool,
    pub notice: bool,
    pub cards: usize,
    pub table_rows: usize,
    pub note: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub client: Rect,
    pub sidebar: Rect,
    pub logo: Rect,
    pub nav: [Rect; 5],
    pub sidebar_footer: Rect,
    /// Area right of the sidebar, inside padding.
    pub content: Rect,
    pub header: Rect,
    pub notice: Option<Rect>,
    pub engine_panel: Option<Rect>,
    pub start_btn: Option<Rect>,
    pub stop_btn: Option<Rect>,
    pub columns: usize,
    pub cards: Vec<Rect>,
    pub table: Option<Rect>,
    pub note: Option<Rect>,
}

/// Number of card columns that fit in `content_w` physical pixels.
pub fn columns_for(content_w: i32, cards: usize, dpi: u32) -> usize {
    if cards == 0 {
        return 0;
    }
    let min_w = scale(theme::CARD_MIN_W, dpi);
    let gap = scale(theme::GAP, dpi);
    let fit = ((content_w + gap) / (min_w + gap)).max(1) as usize;
    fit.min(theme::MAX_COLUMNS).min(cards)
}

pub fn compute(client_w: i32, client_h: i32, dpi: u32, c: Content) -> Layout {
    let s = |d| scale(d, dpi);
    let client = Rect::new(0, 0, client_w.max(0), client_h.max(0));

    // Sidebar
    let sidebar = Rect::new(0, 0, s(theme::SIDEBAR_W).min(client.w), client.h);
    let logo = Rect::new(0, 0, sidebar.w, s(theme::LOGO_H));
    let mut nav = [Rect::default(); 5];
    let mut y = logo.bottom();
    for r in &mut nav {
        *r = Rect::new(0, y, sidebar.w, s(theme::NAV_H));
        y += s(theme::NAV_H) + s(theme::NAV_GAP);
    }
    let footer_h = s(theme::SIDEBAR_FOOTER_H);
    let sidebar_footer = Rect::new(0, (client.h - footer_h).max(y), sidebar.w, footer_h);

    // Content column
    let pad = s(theme::PAD);
    let gap = s(theme::GAP);
    let cx = sidebar.right() + pad;
    let content = Rect::new(
        cx,
        pad,
        (client.w - cx - pad).max(0),
        (client.h - 2 * pad).max(0),
    );
    let header = Rect::new(content.x, content.y, content.w, s(theme::HEADER_H));
    let mut y = header.bottom() + gap;

    let notice = c.notice.then(|| {
        let r = Rect::new(content.x, y, content.w, s(theme::NOTICE_H));
        y = r.bottom() + gap;
        r
    });

    let (engine_panel, start_btn, stop_btn) = if c.engine_panel {
        let panel = Rect::new(content.x, y, content.w, s(theme::PANEL_H));
        y = panel.bottom() + gap;
        let (bw, bh) = (s(theme::BTN_W), s(theme::BTN_H));
        let by = panel.y + (panel.h - bh) / 2;
        let stop = Rect::new(panel.right() - pad - bw, by, bw, bh);
        let start = Rect::new(stop.x - s(12) - bw, by, bw, bh);
        (Some(panel), Some(start), Some(stop))
    } else {
        (None, None, None)
    };

    let columns = columns_for(content.w, c.cards, dpi);
    let mut cards = Vec::with_capacity(c.cards);
    if columns > 0 {
        let cols = columns as i32;
        let cw = (content.w - gap * (cols - 1)) / cols;
        let ch = s(theme::CARD_H);
        for i in 0..c.cards as i32 {
            let (col, row) = (i % cols, i / cols);
            cards.push(Rect::new(
                content.x + col * (cw + gap),
                y + row * (ch + gap),
                cw,
                ch,
            ));
        }
        let rows = (c.cards as i32 + cols - 1) / cols;
        y += rows * (ch + gap);
    }

    let table = (c.table_rows > 0).then(|| {
        let r = Rect::new(
            content.x,
            y,
            content.w,
            c.table_rows as i32 * s(theme::ROW_H) + s(8),
        );
        y = r.bottom() + gap;
        r
    });

    let note = c
        .note
        .then(|| Rect::new(content.x, y, content.w, s(theme::NOTE_H)));

    Layout {
        client,
        sidebar,
        logo,
        nav,
        sidebar_footer,
        content,
        header,
        notice,
        engine_panel,
        start_btn,
        stop_btn,
        columns,
        cards,
        table,
        note,
    }
}

impl Layout {
    /// Lowest point used by page content.
    pub fn content_bottom(&self) -> i32 {
        [
            self.note,
            self.table,
            self.cards.last().copied(),
            self.engine_panel,
            self.notice,
        ]
        .into_iter()
        .flatten()
        .map(|r| r.bottom())
        .max()
        .unwrap_or(self.header.bottom())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{MIN_CLIENT_H, MIN_CLIENT_W};

    const DASH: Content = Content {
        engine_panel: true,
        notice: true,
        cards: 4,
        table_rows: 0,
        note: false,
    };
    const SETTINGS: Content = Content {
        engine_panel: false,
        notice: false,
        cards: 1,
        table_rows: 6,
        note: true,
    };
    const LOGS: Content = Content {
        engine_panel: false,
        notice: false,
        cards: 4,
        table_rows: 0,
        note: true,
    };

    fn all_rects(l: &Layout) -> Vec<Rect> {
        let mut v = vec![l.header];
        v.extend(l.notice);
        v.extend(l.engine_panel);
        v.extend(l.cards.iter().copied());
        v.extend(l.table);
        v.extend(l.note);
        v
    }

    #[test]
    fn columns_shrink_with_width() {
        let mut prev = 0;
        for w in (200..2000).step_by(50) {
            let c = columns_for(w, 4, 96);
            assert!((1..=4).contains(&c));
            assert!(
                c >= prev,
                "columns must not decrease as width grows (w={w})"
            );
            prev = c;
        }
        assert_eq!(columns_for(300, 4, 96), 1);
        assert_eq!(columns_for(2000, 4, 96), 4);
        assert_eq!(columns_for(2000, 2, 96), 2, "never more columns than cards");
        assert_eq!(columns_for(2000, 0, 96), 0);
    }

    #[test]
    fn content_never_overlaps_sidebar_and_fits_width() {
        for dpi in [96, 120, 144, 192] {
            for w in [MIN_CLIENT_W, 1040, 1920] {
                let (w, h) = (scale(w, dpi), scale(MIN_CLIENT_H, dpi));
                for c in [DASH, SETTINGS, LOGS] {
                    let l = compute(w, h, dpi, c);
                    for r in all_rects(&l) {
                        assert!(!r.intersects(&l.sidebar), "dpi {dpi} w {w}: {r:?}");
                        assert!(r.right() <= l.client.right(), "dpi {dpi} w {w}: {r:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_page_fits_at_minimum_size() {
        for dpi in [96, 144, 192] {
            let (w, h) = (scale(MIN_CLIENT_W, dpi), scale(MIN_CLIENT_H, dpi));
            for c in [DASH, SETTINGS, LOGS] {
                let l = compute(w, h, dpi, c);
                assert!(
                    l.content_bottom() <= h,
                    "dpi {dpi} {c:?}: {} > {h}",
                    l.content_bottom()
                );
                assert!(l.nav[4].bottom() <= l.sidebar_footer.y);
            }
        }
    }

    #[test]
    fn cards_do_not_overlap_each_other() {
        let l = compute(1040, 660, 96, LOGS);
        for (i, a) in l.cards.iter().enumerate() {
            for b in &l.cards[i + 1..] {
                assert!(!a.intersects(b), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn buttons_sit_inside_engine_panel() {
        for w in [MIN_CLIENT_W, 1920] {
            let l = compute(w, 700, 96, DASH);
            let p = l.engine_panel.unwrap();
            assert!(p.contains(&l.start_btn.unwrap()) && p.contains(&l.stop_btn.unwrap()));
            assert!(!l.start_btn.unwrap().intersects(&l.stop_btn.unwrap()));
        }
    }

    #[test]
    fn layout_scales_with_dpi() {
        let a = compute(1000, 600, 96, DASH);
        let b = compute(2000, 1200, 192, DASH);
        assert_eq!(b.sidebar.w, 2 * a.sidebar.w);
        assert_eq!(b.engine_panel.unwrap().h, 2 * a.engine_panel.unwrap().h);
        assert_eq!(a.columns, b.columns);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (-5, -5), (50, 2000)] {
            let _ = compute(w, h, 96, DASH);
        }
    }

    #[test]
    fn optional_sections_absent_when_not_requested() {
        let l = compute(1040, 660, 96, Content::default());
        assert!(l.notice.is_none() && l.engine_panel.is_none() && l.table.is_none());
        assert!(l.cards.is_empty() && l.start_btn.is_none());
    }
}
