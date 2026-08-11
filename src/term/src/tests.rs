//! Host-runnable tests for the graphics-free terminal model (L2).
//!
//! They drive `Screen` with byte streams and check that `TextSink` serializes the grid to
//! the expected logical text - the model is exercised with no renderer, proving it is
//! graphics-independent.

use crate::screen::SCROLLBACK_ROWS;
use crate::{Echo, EchoBuf, Ld, RawSink, Screen, TextSink};
use alloc::vec::Vec;

fn dump(screen: &Screen) -> Vec<u8> {
	let mut sink = TextSink::new();
	sink.capture(screen);
	sink.as_bytes().to_vec()
}

fn feed(screen: &mut Screen, bytes: &[u8]) {
	for &b in bytes {
		screen.put_byte(b);
	}
}

// A line longer than the grid auto-wraps; the text dump joins the wrapped rows back into
// one logical line and breaks only on the explicit newline.
#[test]
fn joins_soft_wraps_and_breaks_on_hard_newlines() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	feed(&mut s, b"hello world\nbye");
	assert_eq!(dump(&s), b"hello world\nbye");
}

// Trailing spaces are trimmed and the blank bottom of the screen produces no trailing
// newlines.
#[test]
fn trims_trailing_spaces_and_blank_rows() {
	let mut s = Screen::new(10, 5, SCROLLBACK_ROWS);
	feed(&mut s, b"abc   \ndef");
	assert_eq!(dump(&s), b"abc\ndef");
}

// A blank screen serializes to nothing.
#[test]
fn blank_screen_is_empty() {
	let s = Screen::new(8, 4, SCROLLBACK_ROWS);
	assert_eq!(dump(&s), b"");
}

// Unicode text survives the grid round trip: the UTF-8 stream decodes to codepoints, the
// cells record them (the renderer resolves them to unscii-16 glyphs), and the text dump
// re-encodes the same UTF-8 bytes - Czech diacritics included.
#[test]
fn unicode_round_trips_through_the_grid() {
	let mut s = Screen::new(40, 4, SCROLLBACK_ROWS);
	feed(&mut s, "příliš žluťoučký kůň\n€ ○ ─".as_bytes());
	assert_eq!(dump(&s), "příliš žluťoučký kůň\n€ ○ ─".as_bytes());
}

// A line that exactly fills the width and is then explicitly newlined is a hard break, not
// a soft wrap (the next line stays separate).
#[test]
fn exact_width_then_newline_is_hard_break() {
	let mut s = Screen::new(4, 4, SCROLLBACK_ROWS);
	feed(&mut s, b"abcd\nef");
	assert_eq!(dump(&s), b"abcd\nef");
}

// Content that scrolls off the top is kept in scrollback, and its soft-wrap flag travels
// with it so the dump still joins the wrapped line after the scroll.
#[test]
fn scrollback_preserves_soft_wrap() {
	let mut s = Screen::new(6, 3, SCROLLBACK_ROWS);
	// "abcdefghij" (10 chars) wraps across two rows; the following newlines scroll the
	// wrapped pair up into the scrollback before the dump is taken.
	feed(&mut s, b"abcdefghij\n1\n2\n3\n4");
	assert_eq!(dump(&s), b"abcdefghij\n1\n2\n3\n4");
}

// The L1 stream tap records the raw bytes verbatim - ANSI control codes included - alongside
// the L2 model: the console forks each output chunk into the `Screen` (which parses it into
// glyphs) and the `RawSink` (which keeps the exact stream a future ssh/`script` would forward).
#[test]
fn raw_sink_records_the_exact_stream() {
	let stream: &[u8] = b"\x1b[31mhi\x1b[0m\nbye";
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	let mut raw = RawSink::new();
	raw.feed(stream);
	feed(&mut s, stream);
	// L1: the tap holds the stream byte-for-byte, control codes and all.
	assert_eq!(raw.as_bytes(), stream);
	// L2: the model parsed the same stream into its glyphs (the SGR codes are consumed).
	assert_eq!(dump(&s), b"hi\nbye");
}

// A fresh tap is empty, fills as the stream is fed, and resets on `clear` (how the serial
// mirror drains itself each wake).
#[test]
fn raw_sink_clear_resets_the_capture() {
	let mut raw = RawSink::new();
	assert!(raw.is_empty());
	raw.feed(b"abc");
	assert!(!raw.is_empty());
	raw.clear();
	assert!(raw.is_empty());
	assert_eq!(raw.as_bytes(), b"");
}

// A bounded consumer drains the stream in slices: consume drops exactly the oldest
// bytes it took, keeps the rest in order, and an over-long consume just empties.
#[test]
fn raw_sink_consume_drops_only_the_oldest_bytes() {
	let mut raw = RawSink::new();
	raw.feed(b"hello world");
	raw.consume(6);
	assert_eq!(raw.as_bytes(), b"world");
	raw.feed(b"!");
	assert_eq!(raw.as_bytes(), b"world!");
	raw.consume(100);
	assert!(raw.is_empty());
}

// The DEC private mouse-tracking modes (?1000 normal, ?1002 button-event, ?1003 any-event)
// and the SGR encoding (?1006) toggle the queryable mode the console reads to route pointer
// events; each turns off again with the matching `l`.
#[test]
fn mouse_modes_track_the_dec_private_toggles() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	assert!(!s.mouse_tracking());
	feed(&mut s, b"\x1b[?1000h");
	assert!(s.mouse_tracking() && !s.mouse_report_motion());
	feed(&mut s, b"\x1b[?1002h");
	assert!(s.mouse_report_motion() && !s.mouse_any_motion());
	feed(&mut s, b"\x1b[?1003h");
	assert!(s.mouse_any_motion());
	feed(&mut s, b"\x1b[?1003l");
	assert!(!s.mouse_tracking());
	assert!(!s.mouse_sgr());
	feed(&mut s, b"\x1b[?1006h");
	assert!(s.mouse_sgr());
}

// Bracketed paste (?2004) toggles the flag the console reads to wrap a paste.
#[test]
fn bracketed_paste_toggles() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	assert!(!s.bracketed_paste());
	feed(&mut s, b"\x1b[?2004h");
	assert!(s.bracketed_paste());
	feed(&mut s, b"\x1b[?2004l");
	assert!(!s.bracketed_paste());
}

// OSC 52 sets the clipboard: the base64 payload is decoded to plain text and drained to the
// console, which holds the clipboard.
#[test]
fn osc_52_sets_the_clipboard() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	// "aGVsbG8=" is base64 for "hello".
	feed(&mut s, b"\x1b]52;c;aGVsbG8=\x07");
	assert_eq!(s.take_clipboard_set().as_deref(), Some(&b"hello"[..]));
	// drained once.
	assert_eq!(s.take_clipboard_set(), None);
}

// A click-drag selection over the live screen copies the selected glyphs, trailing spaces
// trimmed, rows joined by a newline; the selected cells render reversed.
#[test]
fn selection_copies_text_and_highlights_cells() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	feed(&mut s, b"hello");
	s.selection_begin(0, 0);
	s.selection_extend(4, 0);
	assert!(s.has_selection());
	assert_eq!(s.selection_text(), b"hello");
	// the selected cells are reversed, an unselected one is not.
	assert!(s.display_cell(0, 0).reverse);
	assert!(s.display_cell(4, 0).reverse);
	assert!(!s.display_cell(5, 0).reverse);
	s.selection_clear();
	assert!(!s.has_selection());
	assert!(!s.display_cell(0, 0).reverse);
}

// A selection spanning two rows takes each row's segment to its end, joining them with a
// newline (trailing blanks trimmed).
#[test]
fn selection_spans_rows() {
	let mut s = Screen::new(8, 4, SCROLLBACK_ROWS);
	feed(&mut s, b"ab\ncd");
	s.selection_begin(0, 0);
	s.selection_extend(1, 1);
	assert_eq!(s.selection_text(), b"ab\ncd");
}

// Selection works over the scrollback view: scrolling up brings a scrolled-off line into
// the viewport, and a selection there copies that history line.
#[test]
fn selection_reaches_into_scrollback() {
	let mut s = Screen::new(8, 3, SCROLLBACK_ROWS);
	feed(&mut s, b"L0\nL1\nL2\nL3\nL4");
	// L0/L1 have scrolled into the history; page the view up to show them.
	s.scroll_view_up();
	s.selection_begin(0, 0);
	s.selection_extend(1, 0);
	assert_eq!(s.selection_text(), b"L0");
}

// Drive the cooked line discipline: feed the initial bytes, then `tabs` Tab keys, all
// against `vocab`, and return the resulting edited line. There is no grid (`term` None) -
// only the buffer state matters here.
fn tab_complete(initial: &[u8], vocab: &[&[u8]], tabs: usize) -> Vec<u8> {
	let mut ld = Ld::new(8);
	let vocab: Vec<Vec<u8>> = vocab.iter().map(|v: &&[u8]| v.to_vec()).collect();
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	for &b in initial {
		ld.feed(b, &vocab, &mut echo);
	}
	for _ in 0..tabs {
		ld.feed(b'\t', &vocab, &mut echo);
	}
	ld.line[..ld.len].to_vec()
}

// A unique command-word match completes fully and is closed with a space (bash's builtins +
// $PATH completion), unchanged by the segment-aware rewrite.
#[test]
fn completes_a_unique_command_word() {
	assert_eq!(tab_complete(b"ec", &[b"echo", b"cat"], 1), b"echo ");
}

// Several command-word matches extend to their longest common prefix and stop (no space -
// the word is not finished yet).
#[test]
fn extends_a_command_word_to_the_common_prefix() {
	assert_eq!(tab_complete(b"l", &[b"lsblk", b"lscpu", b"cat"], 1), b"ls");
}

// A path argument completes the trailing path segment (after the last '/') against the
// directory's entries, leaving the rest of the line intact: `cat ./mot` -> `cat ./motd.txt `.
#[test]
fn completes_a_unique_path_argument() {
	assert_eq!(tab_complete(b"cat ./mot", &[b"motd.txt", b"hello.txt"], 1), b"cat ./motd.txt ");
}

// A bare argument (no slash) completes against the directory entries too - the segment is the
// whole token after the space.
#[test]
fn completes_a_bare_path_argument() {
	assert_eq!(tab_complete(b"cat mot", &[b"motd.txt"], 1), b"cat motd.txt ");
}

// A directory completion carries its trailing '/' and is NOT closed with a space, so the
// operator keeps typing the sub-path.
#[test]
fn a_directory_completion_stays_open() {
	assert_eq!(tab_complete(b"cd bi", &[b"bin/", b"boot/"], 1), b"cd bin/");
}

// Several path-argument matches extend only the trailing segment to their common prefix, not
// the whole token.
#[test]
fn extends_a_path_segment_to_the_common_prefix() {
	assert_eq!(tab_complete(b"cat ./f", &[b"foo.txt", b"foobar.txt"], 1), b"cat ./foo");
}

#[test]
fn a_geometry_the_renderer_cannot_address_is_refused_rather_than_panicking() {
	// `put_pixel` writes `bytes[i]` for `i in 0..bytes_per_pixel` over a `[u8; 4]`, so a
	// `bytes_per_pixel` of 5 was an index panic rather than a refusal; `channel` shifts a `u32`, so
	// a `red_shift` of 32 panicked in a debug build. Neither was checked anywhere, and both are
	// ordinary values for a mode line to carry wrongly - a display backend reporting a format this
	// renderer does not implement should get "no console", not a crash on the first glyph.
	let sane = crate::render::Geometry { width: 64, height: 32, pitch: 64 * 4, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 };
	let mut backing = alloc::vec![0u8; sane.pitch * sane.height];
	let addr = backing.as_mut_ptr() as u64;
	// SAFETY: `backing` is a real allocation of exactly the size the geometry describes and
	// outlives every `Raster` below.
	assert!(unsafe { crate::render::Raster::new(addr, &sane) }.is_some(), "the sane geometry must be accepted, or nothing below means anything");

	let refused = |what: &str, edit: &dyn Fn(&mut crate::render::Geometry)| {
		let mut g: crate::render::Geometry = sane.clone();
		edit(&mut g);
		assert!(unsafe { crate::render::Raster::new(addr, &g) }.is_none(), "{what}");
	};
	refused("five bytes per pixel writes past a u32", &|g| g.bytes_per_pixel = 5);
	refused("zero bytes per pixel writes nothing and divides nothing", &|g| g.bytes_per_pixel = 0);
	refused("a pitch shorter than a row overlaps the next one", &|g| g.pitch = 64 * 4 - 1);
	refused("a red shift past the word", &|g| g.red_shift = 32);
	refused("a green channel wider than a byte", &|g| g.green_size = 9);
	refused("a blue channel that ends past the word", &|g| {
		g.blue_shift = 28;
		g.blue_size = 8;
	});
}

#[test]
fn the_alternate_screen_keeps_its_own_soft_wrap_flags() {
	// One `wrap` vector served both cell buffers, so a full-screen program edited the PRIMARY
	// screen's soft-wrap metadata - and after it exited, `TextSink` joined rows that had nothing to
	// do with each other. The cell buffers were always two; the thing describing them was one.
	let mut screen = Screen::new(8, 4, 0);
	// Two logical lines, the first of them soft-wrapped by filling the row exactly.
	feed(&mut screen, b"aaaaaaaabbbb");
	let before = dump(&screen);
	assert_eq!(before, b"aaaaaaaabbbb", "the fixture must soft-wrap, or nothing below means anything");

	// Into the alternate screen, wrap something there, and back out.
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"xxxxxxxxyyyy");
	feed(&mut screen, b"\x1b[?1049l");
	assert_eq!(dump(&screen), before, "the primary screen's wrap flags must survive the alternate screen");
}

#[test]
fn erasing_a_row_clears_the_soft_wrap_it_described() {
	// `CSI J` and `CSI K` blank cells through `fill_cells`, which left `wrap` alone - so a
	// soft-wrap flag survived the complete erasure of the row it described, and the serializer
	// went on joining an empty row to its successor.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"aaaaaaaabbbb");
	assert_eq!(dump(&screen), b"aaaaaaaabbbb", "the fixture must soft-wrap");

	// Erase the whole screen and write two separate lines.
	feed(&mut screen, b"\x1b[2J\x1b[H");
	feed(&mut screen, b"one\r\ntwo");
	assert_eq!(dump(&screen), b"one\ntwo", "an erased row must not join the next one");
}

#[test]
fn a_full_line_keeps_its_caret_and_a_row_move_does_not_wrap() {
	// Pending wrap was encoded as a cursor one past the end - `put_glyph` ended with `col += 1`, so
	// a glyph in the last column left `col == cols` and only the NEXT glyph read that as a wrap.
	//
	// Two consequences, and both are visible to a person using the terminal.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"aaaaaaaa");
	// The renderer draws the caret only where `cursor_col() < cols`, so filling a line exactly made
	// it vanish.
	assert!(screen.cursor_col() < screen.cols(), "the caret must still be on the screen after a full line");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (7, 0), "and on the last cell it wrote");

	// A CSI that moves the ROW alone left the out-of-range column intact, so the next glyph both
	// wrapped and line-fed and landed a row below where it belonged.
	feed(&mut screen, b"\x1b[B");
	assert_eq!(screen.cursor_row(), 1);
	feed(&mut screen, b"X");
	assert_eq!(screen.cursor_row(), 1, "a glyph after a row move must land on that row");
	// Column 7, which is where the move left it - the row moved and the column did not.
	assert_eq!(dump(&screen), b"aaaaaaaa\n       X", "and in the column the move left it in");
}

#[test]
fn a_malformed_utf8_sequence_becomes_one_replacement_character() {
	// The decoder checked lead and continuation BIT PATTERNS and nothing else, so an overlong
	// encoding, a surrogate and anything above U+10FFFF all reached a cell - and then rendered as
	// `?`, because the font lookup rejected what the decoder had accepted. A truncated sequence was
	// worse: the partial was dropped in silence, so a stream that lost a byte lost a character with
	// no sign that it had.
	let malformed: [(&str, &[u8]); 5] = [
		("an overlong two-byte NUL", b"\xc0\x80"),
		("an overlong three-byte solidus", b"\xe0\x80\xaf"),
		("a surrogate half", b"\xed\xa0\x80"),
		("a codepoint above U+10FFFF", b"\xf7\xbf\xbf\xbf"),
		("a lone continuation byte", b"\x80"),
	];
	for (what, bytes) in malformed {
		let mut screen = Screen::new(8, 2, 0);
		feed(&mut screen, bytes);
		let text = dump(&screen);
		assert_eq!(text, "\u{fffd}".as_bytes(), "{what} must be one replacement character, got {text:?}");
	}

	// A truncated sequence is one replacement character AND the interrupting byte on its own.
	let mut screen = Screen::new(8, 2, 0);
	feed(&mut screen, b"\xe2\x82A");
	assert_eq!(dump(&screen), "\u{fffd}A".as_bytes(), "a truncated sequence must not vanish");

	// And a legal sequence still decodes, so the strictness is not simply refusing.
	let mut screen = Screen::new(8, 2, 0);
	feed(&mut screen, "€ěš".as_bytes());
	assert_eq!(dump(&screen), "€ěš".as_bytes());
}

#[test]
fn an_erase_mode_this_terminal_does_not_implement_erases_nothing() {
	// `erase_display` mapped 0 and 1 and sent everything else to "the whole screen". xterm's
	// `CSI 3 J` clears the SAVED lines and leaves the display alone, so the one sequence a program
	// uses to drop history wiped what the user was reading - and `CSI 99J`, a typo or a sequence
	// meant for another terminal, did the same.
	let mut screen = Screen::new(8, 3, 4);
	feed(&mut screen, b"one\r\ntwo\r\nsix");
	let visible = dump(&screen);
	assert_eq!(visible, b"one\ntwo\nsix");

	// An unknown mode leaves the screen exactly as it was.
	feed(&mut screen, b"\x1b[99J");
	assert_eq!(dump(&screen), visible, "an unimplemented erase mode must erase nothing");
	feed(&mut screen, b"\x1b[7K");
	assert_eq!(dump(&screen), visible, "and the same for an unimplemented line erase");

	// `CSI 3 J` drops the scrollback and leaves the display.
	feed(&mut screen, b"\x1b[3J");
	assert_eq!(dump(&screen), visible, "CSI 3 J clears the saved lines, not the screen");

	// And the modes that ARE implemented still erase.
	feed(&mut screen, b"\x1b[2J");
	assert_ne!(dump(&screen), visible, "CSI 2 J must still clear the screen");
}
