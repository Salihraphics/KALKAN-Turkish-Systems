use super::*;
use crate::theme::{MIN_CLIENT_H, MIN_CLIENT_W};

const DASH: Content = Content {
    engine_panel: true,
    notice: true,
    tiles: 0,
    cards: 4,
    strip: true,
    table_rows: None,
    note: false,
};
const NETWORK: Content = Content {
    engine_panel: false,
    notice: false,
    tiles: 2,
    cards: 0,
    strip: false,
    table_rows: Some(12),
    note: true,
};
const STATS: Content = Content {
    engine_panel: false,
    notice: false,
    tiles: 0,
    cards: 6,
    strip: true,
    table_rows: None,
    note: false,
};
const SETTINGS: Content = Content {
    engine_panel: false,
    notice: false,
    tiles: 0,
    cards: 1,
    strip: false,
    table_rows: Some(6),
    note: true,
};
const LOGS: Content = Content {
    engine_panel: false,
    notice: false,
    tiles: 0,
    cards: 3,
    strip: false,
    table_rows: None,
    note: true,
};
const PAGES: [Content; 5] = [DASH, NETWORK, STATS, SETTINGS, LOGS];

fn all_rects(l: &Layout) -> Vec<Rect> {
    let mut v = vec![l.header];
    v.extend(l.notice);
    v.extend(l.engine_panel);
    v.extend(l.tiles.iter().copied());
    v.extend(l.cards.iter().copied());
    v.extend(l.strip);
    v.extend(l.table);
    v.extend(l.note);
    v
}

#[test]
fn columns_grow_with_width_and_are_balanced() {
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
    let w3 = 3 * theme::CARD_MIN_W + 2 * theme::GAP;
    assert_eq!(columns_for(w3, 4, 96), 2, "4 cards: 2x2 rather than 3+1");
    assert_eq!(columns_for(w3, 3, 96), 3);
    assert_eq!(columns_for(w3, 6, 96), 3, "6 cards: 3x2");
    assert_eq!(columns_for(2000, 2, 96), 2, "never more columns than items");
    assert_eq!(columns_for(2000, 0, 96), 0);
}

#[test]
fn every_page_fits_at_minimum_size_at_common_scalings() {
    for dpi in [96, 120, 144, 168, 192] {
        let (w, h) = (scale(MIN_CLIENT_W, dpi), scale(MIN_CLIENT_H, dpi));
        for (i, c) in PAGES.iter().enumerate() {
            let l = compute(w, h, dpi, *c);
            assert!(
                l.content_bottom() <= h,
                "dpi {dpi} page {i}: {} > {h}",
                l.content_bottom()
            );
            assert!(
                l.nav[4].bottom() <= l.sidebar_footer.y,
                "dpi {dpi}: sidebar overlaps footer"
            );
        }
    }
}

#[test]
fn content_never_overlaps_sidebar_or_leaves_the_window() {
    for dpi in [96, 120, 144, 192] {
        for w in [MIN_CLIENT_W, 1040, 1920] {
            let (w, h) = (scale(w, dpi), scale(MIN_CLIENT_H, dpi));
            for c in PAGES {
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
fn blocks_do_not_overlap_each_other() {
    for c in PAGES {
        let l = compute(1040, 680, 96, c);
        let rects = all_rects(&l);
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                assert!(!a.intersects(b), "{a:?} {b:?} in {c:?}");
            }
        }
    }
}

#[test]
fn long_tables_are_clipped_to_the_window() {
    let many = Content {
        table_rows: Some(100),
        ..NETWORK
    };
    let l = compute(scale(MIN_CLIENT_W, 96), scale(MIN_CLIENT_H, 96), 96, many);
    assert!(
        l.table_rows_visible >= 3 && l.table_rows_visible < 100,
        "{}",
        l.table_rows_visible
    );
    assert!(l.content_bottom() <= l.client.bottom());
    let few = compute(
        1040,
        680,
        96,
        Content {
            table_rows: Some(2),
            ..NETWORK
        },
    );
    assert_eq!(few.table_rows_visible, 2);
    let empty = compute(
        1040,
        680,
        96,
        Content {
            table_rows: Some(0),
            ..NETWORK
        },
    );
    assert_eq!(empty.table_rows_visible, 0);
    assert!(
        empty.table.unwrap().h > 0,
        "room for the empty-state message"
    );
}

#[test]
fn buttons_sit_in_the_panel_above_the_full_width_scope_line() {
    for dpi in [96, 120, 144, 192] {
        for w in [MIN_CLIENT_W, 1920] {
            let l = compute(scale(w, dpi), scale(MIN_CLIENT_H, dpi), dpi, DASH);
            let p = l.engine_panel.unwrap();
            let (a, b) = (l.start_btn.unwrap(), l.stop_btn.unwrap());
            assert!(p.contains(&a) && p.contains(&b) && !a.intersects(&b));
            let scope_top = p.y + scale(theme::PANEL_SCOPE_Y, dpi);
            assert!(
                a.bottom() <= scope_top && b.bottom() <= scope_top,
                "dpi {dpi}"
            );
            assert!(scope_top + scale(20, dpi) <= p.bottom());
        }
    }
}

#[test]
fn layout_scales_with_dpi() {
    let a = compute(1000, 700, 96, DASH);
    let b = compute(2000, 1400, 192, DASH);
    assert_eq!(b.sidebar.w, 2 * a.sidebar.w);
    assert_eq!(b.engine_panel.unwrap().h, 2 * a.engine_panel.unwrap().h);
    assert_eq!(a.columns, b.columns);
}

#[test]
fn degenerate_sizes_do_not_panic() {
    for (w, h) in [(0, 0), (1, 1), (-5, -5), (50, 2000)] {
        for c in PAGES {
            let _ = compute(w, h, 96, c);
        }
    }
}

#[test]
fn optional_sections_absent_when_not_requested() {
    let l = compute(1040, 680, 96, Content::default());
    assert!(l.notice.is_none() && l.engine_panel.is_none() && l.table.is_none());
    assert!(l.cards.is_empty() && l.tiles.is_empty() && l.strip.is_none() && l.start_btn.is_none());
}

#[test]
fn columns_fill_width_and_drop_low_priority_when_narrow() {
    // (min_w, weight, priority): name, type, link, down, up, received, sent
    let cols = [
        (120, 4, 0),
        (64, 2, 3),
        (72, 2, 4),
        (80, 2, 0),
        (80, 2, 0),
        (80, 2, 1),
        (80, 2, 2),
    ];
    let wide = place_columns(&cols, 10, 900, 96);
    assert_eq!(wide.len(), 7);
    assert_eq!(wide[0].x, 10);
    assert_eq!(
        wide.last().map(|c| c.x + c.w),
        Some(910),
        "fills the width exactly"
    );
    for w in wide.windows(2) {
        assert_eq!(w[0].x + w[0].w, w[1].x, "contiguous");
    }
    let narrow = place_columns(&cols, 0, 430, 96);
    let kept: Vec<_> = narrow.iter().map(|c| c.index).collect();
    assert_eq!(
        kept,
        vec![0, 3, 4, 5],
        "link, type, then sent dropped first"
    );
    let mid = place_columns(&cols, 0, 470, 96);
    assert_eq!(mid.len(), 5, "at 470 px only link and type need to go");
    let tiny = place_columns(&cols, 0, 50, 96);
    let kept: Vec<_> = tiny.iter().map(|c| c.index).collect();
    assert_eq!(kept, vec![0, 3, 4], "essential columns are never dropped");
}

#[test]
fn nav_buttons_leave_the_sidebar_border_visible() {
    for dpi in [96, 144] {
        let l = compute(scale(1040, dpi), scale(680, dpi), dpi, DASH);
        for n in l.nav {
            assert_eq!(n.right(), l.sidebar.right() - 1, "dpi {dpi}");
        }
    }
}
