use super::*;
use core::sync::atomic::Ordering;

// The plain binary search, no cache - what resolving a codepoint cost before the
// cache, kept here as the reference the fast path must match.
fn bsearch(cp: u32) -> &'static [u8] {
	let table = &FONT[4..4 + FONT_COUNT * 4];
	let mut lo: usize = 0;
	let mut hi: usize = FONT_COUNT;
	while lo < hi {
		let mid = (lo + hi) / 2;
		let entry = u32::from_le_bytes([table[mid * 4], table[mid * 4 + 1], table[mid * 4 + 2], table[mid * 4 + 3]]);
		if entry == cp {
			let at = FONT_GLYPHS_BASE + mid * FONT_H;
			return &FONT[at..at + FONT_H];
		}
		if entry < cp {
			lo = mid + 1;
		} else {
			hi = mid;
		}
	}
	bsearch(b'?' as u32)
}

#[test]
fn ascii_cache_matches_the_search_and_eliminates_repaint_probes() {
	// correctness: every codepoint in the cached range (and a spread above it) resolves
	// to the same glyph through the cache as through the plain binary search, so the
	// fast path is a pure optimization with no behavioral change. (One test, not two, so
	// the shared probe counter is never raced by a parallel test.)
	for cp in 0u32..0x120 {
		assert_eq!(glyph_bitmap(cp), bsearch(cp), "glyph for U+{cp:04X} differs");
	}
	// the measurement: a full 80x25 screen of ASCII text repaints with zero
	// binary-search probes - the whole hot range is served by the compile-time cache, so
	// the cache pays (before it, every one of those ~2000 cells cost a log2(3000)
	// search). A codepoint above the cache still probes, so the search path stays intact
	// where the cache does not reach.
	GLYPH_PROBES.store(0, Ordering::Relaxed);
	for _ in 0..25 {
		for cp in 0x20u32..0x70 {
			let _ = glyph_bitmap(cp);
		}
	}
	assert_eq!(GLYPH_PROBES.load(Ordering::Relaxed), 0, "an ASCII repaint must not binary-search");
	let _ = glyph_bitmap(0x2500); // a box-drawing codepoint above the cache
	assert!(GLYPH_PROBES.load(Ordering::Relaxed) > 0, "a codepoint above the cache still searches");
}
