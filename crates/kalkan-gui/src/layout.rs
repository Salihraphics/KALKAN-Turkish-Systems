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
    /// Large metric tiles with graphs.
    pub tiles: usize,
    pub cards: usize,
    /// One-row information strip.
    pub strip: bool,
    /// `Some(n)`: a table with `n` data rows (0 = empty-state message).
    pub table_rows: Option<usize>,
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
    pub tiles: Vec<Rect>,
    pub columns: usize,
    pub cards: Vec<Rect>,
    pub strip: Option<Rect>,
    pub table: Option<Rect>,
    /// Data rows that fit in `table` (the rest are summarised as "+N more").
    pub table_rows_visible: usize,
    pub note: Option<Rect>,
}

/// Number of grid columns that fit in `content_w` physical pixels, balanced
/// so the last row is not left with a single orphan (4 → 2×2, not 3 + 1).
pub fn columns_for(content_w: i32, items: usize, dpi: u32) -> usize {
    if items == 0 {
        return 0;
    }
    let min_w = scale(theme::CARD_MIN_W, dpi);
    let gap = scale(theme::GAP, dpi);
    let fit = ((content_w + gap) / (min_w + gap)).max(1) as usize;
    let mut cols = fit.min(theme::MAX_COLUMNS).min(items);
    let rows = items.div_ceil(cols);
    while cols > 1 && items.div_ceil(cols - 1) == rows {
        cols -= 1;
    }
    cols
}

/// Lay `n` equal cells out in a grid starting at `y`; returns the cells and
/// the y just below the grid (including the trailing gap).
fn grid(content: Rect, y: i32, n: usize, cell_h: i32, dpi: u32) -> (Vec<Rect>, usize, i32) {
    let cols = columns_for(content.w, n, dpi);
    if cols == 0 {
        return (Vec::new(), 0, y);
    }
    let gap = scale(theme::GAP, dpi);
    let c = cols as i32;
    let cw = (content.w - gap * (c - 1)) / c;
    let cells = (0..n as i32)
        .map(|i| {
            Rect::new(
                content.x + (i % c) * (cw + gap),
                y + (i / c) * (cell_h + gap),
                cw,
                cell_h,
            )
        })
        .collect();
    let rows = (n as i32 + c - 1) / c;
    (cells, cols, y + rows * (cell_h + gap))
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
        // Leave the sidebar's 1 px right border visible next to the buttons.
        *r = Rect::new(0, y, (sidebar.w - 1).max(0), s(theme::NAV_H));
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
        let by = panel.y + (s(theme::PANEL_SCOPE_Y) - bh) / 2;
        let stop = Rect::new(panel.right() - s(20) - bw, by, bw, bh);
        let start = Rect::new(stop.x - s(12) - bw, by, bw, bh);
        (Some(panel), Some(start), Some(stop))
    } else {
        (None, None, None)
    };

    let (tiles, _, ny) = grid(content, y, c.tiles, s(theme::TILE_H), dpi);
    y = ny;
    let (cards, columns, ny) = grid(content, y, c.cards, s(theme::CARD_H), dpi);
    y = ny;

    let strip = c.strip.then(|| {
        let r = Rect::new(content.x, y, content.w, s(theme::STRIP_H));
        y = r.bottom() + gap;
        r
    });

    let note_h = if c.note { s(theme::NOTE_H) + gap } else { 0 };
    let (table, table_rows_visible) = match c.table_rows {
        Some(rows) => {
            let row_h = s(theme::ROW_H);
            let head = s(theme::TABLE_HEADER_H) + s(8);
            // Rows that fit above the note, at least one (or the empty row).
            let room = content.bottom() - note_h - y - head;
            let fit = (room / row_h).max(1) as usize;
            let wanted = rows.max(1);
            let shown = wanted.min(fit);
            let r = Rect::new(content.x, y, content.w, head + shown as i32 * row_h);
            y = r.bottom() + gap;
            (Some(r), if rows == 0 { 0 } else { shown })
        }
        None => (None, 0),
    };

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
        tiles,
        columns,
        cards,
        strip,
        table,
        table_rows_visible,
        note,
    }
}

impl Layout {
    /// Lowest point used by page content.
    pub fn content_bottom(&self) -> i32 {
        [
            self.note,
            self.table,
            self.strip,
            self.cards.last().copied(),
            self.tiles.last().copied(),
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

/// A column chosen for display: index into the model's columns, x and width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedColumn {
    pub index: usize,
    pub x: i32,
    pub w: i32,
}

/// Fit table columns into `width` pixels: drop the highest `priority` columns
/// until the minimum widths fit, then share the spare width by `weight`.
/// `cols` is `(min_w_dip, weight, priority)`.
pub fn place_columns(cols: &[(i32, u16, u8)], x: i32, width: i32, dpi: u32) -> Vec<PlacedColumn> {
    let mut keep: Vec<usize> = (0..cols.len()).collect();
    let min_total = |k: &[usize]| k.iter().map(|&i| scale(cols[i].0, dpi)).sum::<i32>();
    while min_total(&keep) > width && keep.len() > 1 {
        // Remove the highest-priority (least important) remaining column.
        let (pos, _) = keep
            .iter()
            .enumerate()
            .max_by_key(|(p, &i)| (cols[i].2, *p))
            .unwrap();
        if cols[keep[pos]].2 == 0 {
            break; // only essential columns left
        }
        keep.remove(pos);
    }
    let spare = (width - min_total(&keep)).max(0);
    let weights: i32 = keep.iter().map(|&i| cols[i].1 as i32).sum::<i32>().max(1);
    let mut cx = x;
    let mut out = Vec::with_capacity(keep.len());
    for (n, &i) in keep.iter().enumerate() {
        let mut w = scale(cols[i].0, dpi) + spare * cols[i].1 as i32 / weights;
        if n == keep.len() - 1 {
            w = (x + width - cx).max(0); // absorb rounding
        }
        out.push(PlacedColumn { index: i, x: cx, w });
        cx += w;
    }
    out
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
