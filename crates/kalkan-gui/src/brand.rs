//! The KALKAN logo, embedded in the executable.
//!
//! `assets/kalkan.ico` holds the logo at 16–256 px (PNG-compressed entries).
//! This module only reads the ICO directory; turning an entry into an
//! `HICON` happens in the Win32 layer (`CreateIconFromResourceEx`), which
//! falls back to the drawn shield if that fails.

/// The icon file, compiled into the binary (no resource script needed).
pub const LOGO_ICO: &[u8] = include_bytes!("../../../assets/kalkan.ico");

/// One image inside an ICO file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IcoEntry<'a> {
    /// Width in pixels (an ICO width byte of 0 means 256).
    pub size: u32,
    /// The image bytes as `CreateIconFromResourceEx` expects them (PNG or
    /// DIB without the file header).
    pub data: &'a [u8],
}

/// Parse the ICO directory. Returns `None` for a malformed file (wrong
/// header, entries pointing outside the file).
pub fn ico_entries(ico: &[u8]) -> Option<Vec<IcoEntry<'_>>> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(ico.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(ico.get(o..o + 4)?.try_into().ok()?));
    if u16_at(0)? != 0 || u16_at(2)? != 1 {
        return None; // not an icon file
    }
    let count = u16_at(4)? as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = 6 + 16 * i;
        let w = *ico.get(e)?;
        let len = u32_at(e + 8)? as usize;
        let off = u32_at(e + 12)? as usize;
        let data = ico.get(off..off.checked_add(len)?)?;
        out.push(IcoEntry {
            size: if w == 0 { 256 } else { w as u32 },
            data,
        });
    }
    (!out.is_empty()).then_some(out)
}

/// The entry to scale for a `px`-sized icon: the smallest one at least that
/// big (downscaling stays sharp), else the largest.
pub fn best_entry<'a>(entries: &[IcoEntry<'a>], px: u32) -> Option<IcoEntry<'a>> {
    entries
        .iter()
        .filter(|e| e.size >= px)
        .min_by_key(|e| e.size)
        .or_else(|| entries.iter().max_by_key(|e| e.size))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_logo_has_all_sizes_as_png() {
        let entries = ico_entries(LOGO_ICO).expect("valid ICO");
        let sizes: Vec<u32> = entries.iter().map(|e| e.size).collect();
        // Exact sizes for the sidebar (32 DIP) and panel (16 DIP) marks at
        // 100/150/200 %, plus the usual shell sizes.
        for s in [16, 24, 32, 48, 64, 256] {
            assert!(sizes.contains(&s), "missing {s}px in {sizes:?}");
        }
        for e in entries {
            assert!(e.data.starts_with(b"\x89PNG\r\n\x1a\n"), "{}px", e.size);
        }
    }

    #[test]
    fn best_entry_prefers_the_smallest_entry_not_smaller_than_requested() {
        let entries = ico_entries(LOGO_ICO).unwrap();
        assert_eq!(best_entry(&entries, 32).unwrap().size, 32);
        assert_eq!(best_entry(&entries, 30).unwrap().size, 32);
        assert_eq!(best_entry(&entries, 49).unwrap().size, 64);
        assert_eq!(best_entry(&entries, 1000).unwrap().size, 256);
        assert_eq!(best_entry(&[], 16), None);
    }

    #[test]
    fn malformed_files_are_rejected() {
        assert_eq!(ico_entries(b""), None);
        assert_eq!(ico_entries(b"\x89PNG\r\n\x1a\n...."), None);
        // Header claims one entry whose data lies past the end.
        let mut bad = vec![0, 0, 1, 0, 1, 0];
        bad.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]);
        bad.extend_from_slice(&100u32.to_le_bytes());
        bad.extend_from_slice(&22u32.to_le_bytes());
        assert_eq!(ico_entries(&bad), None);
        // Zero entries.
        assert_eq!(ico_entries(&[0, 0, 1, 0, 0, 0]), None);
    }
}
