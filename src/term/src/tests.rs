//! Host-runnable tests for the graphics-free terminal model (L2).
//!
//! They drive `Screen` with byte streams and check that `TextSink` serializes the grid to
//! the expected logical text - the model is exercised with no renderer, proving it is
//! graphics-independent.

use crate::screen::{MAX_SCROLLBACK_BYTES, SCROLLBACK_ROWS};
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

	// A PARTIAL DRAIN REPEATED MANY TIMES, which is how the console uses it: the serial mirror is
	// drained in transmit-ring-sized pieces every frame. `drain(..n)` memmoved the remainder on each
	// one; a read offset does not. This asserts the visible half of that - the bytes stay correct
	// across hundreds of partial drains with feeds interleaved - since the copying itself is not
	// observable from a test.
	let mut raw = RawSink::new();
	let mut expected: Vec<u8> = Vec::new();
	for i in 0..500u32 {
		let chunk = [b'a' + (i % 26) as u8; 32];
		raw.feed(&chunk);
		expected.extend_from_slice(&chunk);
		raw.consume(16);
		expected.drain(..16);
		assert_eq!(raw.as_bytes(), &expected[..], "the stream survives a partial drain");
	}
	// And the front is actually reclaimed rather than accumulating for the life of the sink.
	raw.consume(expected.len());
	assert!(raw.is_empty());
	raw.feed(b"after");
	assert_eq!(raw.as_bytes(), b"after");
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
	// Disabling the HIGHEST mode falls back to the ones still enabled - it does not turn tracking
	// off. `?1002l` used to cancel a `?1000` the program never disabled and still believed it had.
	feed(&mut s, b"\x1b[?1003l");
	assert!(s.mouse_report_motion() && !s.mouse_any_motion(), "?1003l leaves the ?1002 that is still on");
	feed(&mut s, b"\x1b[?1002l");
	assert!(s.mouse_tracking() && !s.mouse_report_motion(), "?1002l leaves the ?1000 that is still on");
	feed(&mut s, b"\x1b[?1000l");
	assert!(!s.mouse_tracking(), "and the last one off is tracking off");
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
fn a_control_byte_that_moves_nothing_leaves_the_deferred_wrap_alone() {
	// `put_byte` cleared `pending_wrap` for EVERY control byte under 0x20 that was not ESC, plus
	// DEL, before dispatching it - hoisted "so a new one cannot forget". BEL moves nothing:
	// `0x07 => self.bell = true` and that is all. So the flag was cleared for a cursor that had not
	// moved, and the glyph waiting to wrap overwrote the last column instead.
	//
	// A shell that rings the bell on a completion miss at the end of a full line hits this.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678");
	feed(&mut screen, b"\x07");
	assert!(screen.take_bell(), "BEL still rings");
	feed(&mut screen, b"X");
	assert_eq!(screen.cursor_row(), 1, "the glyph after BEL wraps to the next row");
	assert_eq!(dump(&screen), b"12345678X", "and continues the soft-wrapped line");

	// The control bytes that DO assign the cursor still clear it, now from their own handlers
	// rather than from a blanket above them.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678\rX");
	assert_eq!(dump(&screen), b"X2345678", "CR moves the cursor, so no wrap is pending after it");

	// Backspace steps off the cell the last glyph filled, so the X lands one to its left - which is
	// the point: it lands somewhere, rather than being deferred to the next row.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678\x08X");
	assert_eq!(dump(&screen), b"123456X8", "and backspace does too");
}

#[test]
fn setting_the_scroll_region_clears_the_deferred_wrap_it_homed_away_from() {
	// DECSTBM ends with `col = 0; row = 0` and did not say so. `csi_dispatch` no longer clears the
	// flag on entry - that shortcut was removed so a non-moving CSI could not cancel a wrap - so
	// DECSTBM homed the cursor and left a wrap deferred against the column it had just left.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678");
	feed(&mut screen, b"\x1b[1;4r"); // scroll region, whole screen: homes the cursor
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (0, 0), "DECSTBM homes the cursor");
	feed(&mut screen, b"X");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (1, 0), "and the glyph lands at home");
	assert_eq!(dump(&screen), b"X2345678", "not a row below it");
}

#[test]
fn a_deferred_wrap_does_not_survive_a_buffer_switch() {
	// The flag describes a glyph in the last column of the LIVE buffer's current row. Switching
	// buffers changes which buffer that is, so it describes nothing afterwards. `enter_alt_buffer`
	// got this for free by homing; `leave_alt_buffer` only flipped the flag and marked the screen
	// dirty, so a wrap deferred against the alternate's last column carried into the primary and
	// fired at whatever column the primary's cursor was parked in.
	// The LIVE cursor is shared between the buffers, which is xterm and which the `?47` case in
	// `the_three_alternate_screen_modes_are_not_one_mode` pins deliberately - so this asserts what
	// the flag does, not where the cursor ends up.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"ab");
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"12345678"); // fill the alternate's first row: a wrap is now deferred
	feed(&mut screen, b"\x1b[?47l");
	assert_eq!(screen.cursor_row(), 0, "back on the primary, on its first row");
	feed(&mut screen, b"X");
	assert_eq!(screen.cursor_row(), 0, "the next glyph stays on it");
	assert_eq!(dump(&screen), b"ab     X", "the alternate's deferred wrap did not follow it here");

	// The other direction, which the homing already covered - asserted so the rule is pinned on
	// both sides rather than on the side that happened to be broken.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678");
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"X");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (1, 0), "entering homes and cancels the wrap");
}

#[test]
fn a_resize_during_the_alternate_screen_leaves_the_program_its_own_cursor() {
	// The cell half of this was fixed - the reflow reads the primary explicitly and copies the
	// alternate as the rectangle it is - and the cursor half was not. `cursor_at` was recorded only
	// when the primary was live, which is right; nothing took the other branch, so the reflow lost
	// the position, the "end of the content" fallback fired, and the result was assigned to `row`
	// and `col`, which DURING ALT ARE THE PROGRAM'S CURSOR. A window resize therefore teleported a
	// full-screen program's cursor to a position computed from the shell's scrollback.
	let mut screen = Screen::new(16, 6, 8);
	// Give the primary enough content that the fallback would land somewhere obviously wrong.
	feed(&mut screen, b"one\r\ntwo\r\nthree\r\nfour\r\n");
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"\x1b[3;9H"); // the program puts its cursor at row 2, column 8
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (8, 2));
	screen.resize(12, 6, 64, 64);
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (8, 2), "the program keeps its own cursor across a resize");

	// And it is CLAMPED when the new geometry cannot hold it, rather than left out of range.
	screen.resize(6, 3, 64, 64);
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (5, 2), "clamped into the smaller screen");
	assert!(screen.cursor_col() < screen.cols() && screen.cursor_row() < screen.rows());
}

#[test]
fn the_saved_cursor_is_remapped_by_a_resize_and_belongs_to_its_own_buffer() {
	// Two defects in one place. `?1049h` saved the primary's cursor, a resize reflowed the primary
	// and did not touch the save, and `restore_cursor` only clamped - so after a width change the
	// shell came back to a cell that was no longer where its prompt was. And the save was ONE field
	// shared by both buffers, so a full-screen program's own DECSC overwrote what `?1049h` stored.
	let mut screen = Screen::new(16, 6, 8);
	// A logical line long enough that a narrower screen re-wraps it: the saved position must travel
	// with its own cell, exactly as the live cursor does.
	feed(&mut screen, b"0123456789abcdef0123");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (4, 1), "the live cursor after a wrap");
	// Park the shell's cursor ON a written cell - the nineteenth character, row 1 column 2 - so the
	// reflow has a cell to follow. A position one past the last written cell is not remapped by any
	// of this; it takes the documented "end of the content" fallback, the same as the live cursor.
	feed(&mut screen, b"\x1b[2;3H");
	feed(&mut screen, b"\x1b[?1049h");
	// The program moves about and saves ITS cursor: a different slot, so the shell's is untouched.
	feed(&mut screen, b"\x1b[5;3H");
	feed(&mut screen, b"\x1b7");
	feed(&mut screen, b"\x1b[1;1H");
	screen.resize(10, 6, 64, 64);
	feed(&mut screen, b"\x1b8");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (2, 4), "the program's own save, clamped not reflowed");
	feed(&mut screen, b"\x1b[?1049l");
	// The saved cell is offset 18 of a 20-character logical line. At 16 columns that is row 1
	// column 2; at 10 columns it is row 1 column 8. The save followed the character, which is what
	// the live cursor has always done and what the save never did.
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (8, 1), "the shell's saved cursor followed its own cell through the reflow");
}

#[test]
fn each_buffer_keeps_its_own_saved_cursor() {
	// The save was one field. A program entering the alternate screen and issuing DECSC overwrote
	// whatever the shell had saved with ESC 7 before it started, so the shell's later DECRC landed
	// wherever the program had been.
	let mut screen = Screen::new(16, 6, 0);
	feed(&mut screen, b"\x1b[2;3H");
	feed(&mut screen, b"\x1b7"); // the shell saves its own cursor
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"\x1b[5;9H");
	feed(&mut screen, b"\x1b7"); // the program saves ITS cursor - a different slot
	feed(&mut screen, b"\x1b[?47l");
	feed(&mut screen, b"\x1b8");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (2, 1), "the shell restores what the shell saved");
}

#[test]
fn the_scrollback_depth_returns_to_the_configured_one_after_a_widen() {
	// `resize` passed the CURRENT capacity back into `geometry` as the request, and `geometry`
	// clamps a request to what the byte budget allows at the new width. That made the clamp monotone
	// over a terminal's life: widen once and the depth was permanently reduced, because narrowing
	// again re-clamped the already-reduced value.
	//
	// A `Cell` is a few bytes, so the budget bites at a scrollback of tens of thousands of rows -
	// which is what this asks for, rather than a plausible one, so the clamp is actually reached.
	let deep: usize = MAX_SCROLLBACK_BYTES; // certain to exceed the budget at any width
	let mut screen = Screen::new(80, 24, deep);
	let narrow: usize = screen.scrollback_capacity();

	screen.resize(400, 24, 1024, 1024);
	let wide: usize = screen.scrollback_capacity();
	assert!(wide < narrow, "a wider row costs more per line, so fewer lines fit the byte budget");

	screen.resize(80, 24, 1024, 1024);
	assert_eq!(screen.scrollback_capacity(), narrow, "and the depth comes back when the width does");
}

// A `Term` over a heap-backed framebuffer, so a test can drive the editor against a REAL local
// grid. Every line-editor test before this one passed `term: None`, which is exactly the half of
// the echo path where "a backspace always moves the caret" was false.
struct TestSurface {
	raster: crate::render::Raster,
	// The pixels the raster writes into. Held so the mapping outlives the raster.
	_pixels: Vec<u8>,
}

impl crate::render::Surface for TestSurface {
	fn raster(&self) -> &crate::render::Raster {
		&self.raster
	}
	fn present(&self, _x: u32, _y: u32, _w: u32, _h: u32) {}
}

fn test_term(cols: usize, rows: usize, scrollback: usize) -> crate::Term {
	use crate::render::{CELL_H, CELL_W, Geometry};
	let g = Geometry { width: cols * CELL_W, height: rows * CELL_H, pitch: cols * CELL_W * 4, bytes_per_pixel: 4, red_shift: 16, red_size: 8, green_shift: 8, green_size: 8, blue_shift: 0, blue_size: 8 };
	let mut pixels: Vec<u8> = alloc::vec![0u8; g.pitch * g.height];
	// SAFETY: the mapping is this vector, it is big enough for the geometry, and it is moved into
	// the surface below so it outlives the raster and nothing else references it.
	let raster = unsafe { crate::render::Raster::new(pixels.as_mut_ptr() as u64, &g) }.expect("test geometry");
	crate::Term::new(alloc::boxed::Box::new(TestSurface { raster, _pixels: pixels }), scrollback)
}

#[test]
fn home_on_a_line_longer_than_the_screen_leaves_the_caret_at_the_text() {
	// The reverse-wrap backspace steps onto the previous physical row only while there IS one.
	// Once a command line is long enough that its first row has scrolled into the scrollback, the
	// caret reaches the top-left and stops - and `Ld::home` emitted `columns(0, cursor)` backspaces
	// and set `cursor = 0` regardless. From then on the buffer and the caret disagreed by however
	// many backspaces the screen swallowed, and every later edit was drawn in the wrong place.
	//
	// 8x4 is 32 cells; the line is 50 characters, so its first rows are gone.
	let mut term = test_term(8, 4, 16);
	let mut ld = Ld::new(8);
	let vocab: Vec<Vec<u8>> = Vec::new();
	let line: Vec<u8> = (0..50u8).map(|i| b'a' + i % 26).collect();
	{
		let mut echo = Echo { term: Some(&mut term), ser: EchoBuf::new() };
		for &b in &line {
			ld.feed(b, &vocab, &mut echo);
		}
	}
	assert!(term.screen.caret_index() > 0, "the line filled the screen and scrolled it");

	// Home.
	{
		let mut echo = Echo { term: Some(&mut term), ser: EchoBuf::new() };
		ld.feed(0x01, &vocab, &mut echo); // Ctrl+A
	}
	assert_eq!((term.screen.cursor_col(), term.screen.cursor_row()), (0, 0), "the caret is at the start of the text, which is as far back as the screen goes");
	assert_eq!(term.screen.cell(0, 0).glyph, line[0] as u32, "and the text under it is the line's first character");

	// And the next insertion lands there rather than wherever the caret was left stranded.
	{
		let mut echo = Echo { term: Some(&mut term), ser: EchoBuf::new() };
		ld.feed(b'#', &vocab, &mut echo);
	}
	assert_eq!(term.screen.cell(0, 0).glyph, b'#' as u32, "the inserted character is drawn at the caret");
	assert_eq!(&ld.line[..ld.len][..3], b"#ab", "and it went in at the start of the buffer");
	assert_eq!((term.screen.cursor_col(), term.screen.cursor_row()), (1, 0), "with the caret just past it");
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

#[test]
fn narrowing_the_window_reflows_instead_of_destroying_text() {
	// `resize` copied the overlapping rectangle bottom-anchored and threw the rest away, so
	// narrowing the window DESTROYED the text right of the new width - and emptied the scrollback,
	// dropped the wrap metadata, reallocated the alternate screen and forced `alt_active` to false,
	// so a full-screen program was thrown back into the primary buffer by a host window resize.
	let mut screen = Screen::new(20, 4, 16);
	feed(&mut screen, b"the quick brown fox\r\njumps");
	assert_eq!(dump(&screen), b"the quick brown fox\njumps");

	// Narrower: the long line flows onto a second row rather than losing its tail.
	screen.resize(10, 4, 200, 200);
	assert_eq!(dump(&screen), b"the quick brown fox\njumps", "narrowing must reflow, not crop");

	// And wider again: the reflow is reversible, because the wrap flags say which rows are
	// continuations rather than the text being re-broken from scratch.
	screen.resize(20, 4, 200, 200);
	assert_eq!(dump(&screen), b"the quick brown fox\njumps");
}

#[test]
fn a_resize_does_not_throw_a_full_screen_program_out_of_the_alternate_buffer() {
	// `alt_active` was forced to `false` and the alternate buffer reallocated, so a host window
	// resize dropped a full-screen program back into the primary screen mid-session.
	// `TextSink` serializes the PRIMARY buffer and its scrollback - that is what a scrollback view
	// is - so the alternate screen is read through `cell`, which is what the renderer reads.
	let row_text = |screen: &Screen, row: usize, len: usize| -> alloc::vec::Vec<u8> { (0..len).map(|c| screen.cell(c, row).glyph as u8).collect() };
	let mut screen = Screen::new(20, 4, 16);
	feed(&mut screen, b"shell output here");
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"EDITOR");
	assert_eq!(row_text(&screen, 0, 6), b"EDITOR");

	screen.resize(16, 4, 200, 200);
	assert_eq!(row_text(&screen, 0, 6), b"EDITOR", "the alternate screen must survive a resize");

	// And leaving it still finds the primary screen's text - reflowed, not cropped.
	feed(&mut screen, b"\x1b[?1049l");
	assert_eq!(dump(&screen), b"shell output here");
}

#[test]
fn an_unflushed_scroll_queue_stops_growing_and_asks_for_a_repaint() {
	// The only drain is the renderer's flush, and ConsoleService flushes the foreground VT only -
	// so a program on VT 2 while the user watches VT 1 accumulated one `ScrollOp` per scrolled
	// line, forever, and switching to it replayed every one as a bulk pixel copy before the full
	// repaint that overwrote them all.
	let mut screen = Screen::new(8, 4, 0);
	for _ in 0..10_000 {
		feed(&mut screen, b"line\r\n");
	}
	let queued = screen.take_scrolls().len();
	assert!(queued <= 64, "the scroll queue grew to {queued} while nobody flushed it");
	// And the screen still holds what it should, so the bound is not losing content.
	feed(&mut screen, b"last");
	assert!(dump(&screen).ends_with(b"last"));
}

#[test]
fn cooked_mode_accepts_the_characters_the_screen_can_show() {
	// `Ld::feed` took `0x20..=0x7e` and dropped everything above, so this terminal rendered Czech
	// perfectly and could not accept one accented character as INPUT - and an unbracketed paste
	// arrives through the same path byte by byte, so `příliš žluťoučký kůň` silently lost every
	// non-ASCII byte on its way in.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	let typed = "příliš žluťoučký kůň";
	for &b in typed.as_bytes() {
		ld.feed(b, &[], &mut echo);
	}
	assert_eq!(&ld.line[..ld.len], typed.as_bytes(), "every byte the user typed must reach the line");

	// Backspace deletes a CHARACTER, not a byte: one press must remove the whole `ň`.
	ld.feed(0x7f, &[], &mut echo);
	let after = ld.line[..ld.len].to_vec();
	assert_eq!(after, "příliš žluťoučký ků".as_bytes(), "backspace must delete a whole character");
	assert!(core::str::from_utf8(&after).is_ok(), "and must never leave a fragment that is not text");
}

#[test]
fn a_csi_parameter_too_large_to_mean_anything_does_not_wrap_into_a_key() {
	// `csi_param` was a `u8` accumulated with wrapping arithmetic, so `CSI 259~` became 3 and
	// executed Delete - a key nobody pressed, from a sequence a terminal database or a mangled
	// paste can produce.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	for &b in b"abc" {
		ld.feed(b, &[], &mut echo);
	}
	ld.feed(0x01, &[], &mut echo); // Ctrl+A, to the start
	for &b in b"\x1b[259~" {
		ld.feed(b, &[], &mut echo);
	}
	assert_eq!(&ld.line[..ld.len], b"abc", "a parameter this terminal cannot mean must do nothing");

	// And the real Delete still works, so the widening is not simply refusing.
	for &b in b"\x1b[3~" {
		ld.feed(b, &[], &mut echo);
	}
	assert_eq!(&ld.line[..ld.len], b"bc");
}

#[test]
fn a_zero_history_bound_is_not_a_panic_waiting_for_the_first_line() {
	// `Ld::new(0)` is a legal public call and its first `commit` ran `history.remove(0)` on an
	// empty vector. The configuration path filters zero, which is protection by coincidence.
	let mut ld = Ld::new(0);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	for &b in b"one\r" {
		if ld.feed(b, &[], &mut echo) {
			ld.commit();
		}
	}
	for &b in b"two\r" {
		if ld.feed(b, &[], &mut echo) {
			ld.commit();
		}
	}
}

#[test]
fn a_full_line_recall_reaches_the_serial_mirror_whole() {
	// `EchoBuf` was 512 bytes and its `push` stopped there. `replace_line` - Ctrl+U, and every
	// history recall - echoes the tail, then three bytes per column to erase it, then the new line:
	// past 20 kB for a full line. The framebuffer echo is live and stayed correct, so the local
	// display looked right while the serial mirror and the PTY master got a truncated ESCAPE
	// STREAM - a half-written sequence is not a shorter update, it is a terminal in a state nobody
	// chose.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	// A long line, then Ctrl+U to erase it: the echo has to carry the whole erase.
	for _ in 0..1000 {
		ld.feed(b'x', &[], &mut echo);
	}
	let before = echo.ser.as_slice().len();
	ld.feed(0x15, &[], &mut echo);
	let after = echo.ser.as_slice().len();
	assert!(after > 512, "the echo of a 1000-column erase must not stop at 512 bytes (grew by {})", after - before);
	assert_eq!(&ld.line[..ld.len], b"", "and the line itself is erased");
}

#[test]
fn a_control_string_that_did_not_arrive_whole_is_not_acted_on() {
	// `osc_byte` dropped every byte past its 256-byte buffer and then dispatched whatever it had
	// kept, so an OSC 52 payload longer than the buffer set the clipboard to a PREFIX of the
	// intended text - which is worse than losing it, because it looks like a successful copy.
	let mut screen = Screen::new(8, 2, 0);
	let mut long: Vec<u8> = b"\x1b]52;c;".to_vec();
	long.extend(core::iter::repeat_n(b'A', 400));
	long.push(0x07);
	feed(&mut screen, &long);
	assert!(screen.take_clipboard_set().is_none(), "a truncated OSC 52 must set nothing");

	// A payload that fits is still honoured, so the refusal is not "OSC 52 never works".
	feed(&mut screen, b"\x1b]52;c;aGVsbG8=\x07");
	assert!(screen.take_clipboard_set().is_some(), "a complete OSC 52 must still set the clipboard");
}

#[test]
fn a_scroll_region_does_not_reach_the_scrollback() {
	// `scroll_up` pushed rows to history whenever `scroll_top == 0`, never checking that the region
	// reached the bottom - though the comment above it says a region scroll must not reach history.
	// So `CSI 1;3r` filled the scrollback with the top rows of a status pane.
	let mut screen = Screen::new(8, 6, 16);
	feed(&mut screen, b"\x1b[1;3r"); // a region covering rows 1-3 only
	feed(&mut screen, b"\x1b[H");
	for i in 0..10 {
		feed(&mut screen, alloc::format!("r{i}\r\n").as_bytes());
	}
	assert_eq!(screen.total_logical_rows() - screen.rows(), 0, "a region scroll must not push anything to history");

	// The whole screen still does, so the rule is not "never scroll back".
	let mut screen = Screen::new(8, 6, 16);
	for i in 0..10 {
		feed(&mut screen, alloc::format!("r{i}\r\n").as_bytes());
	}
	assert!(screen.total_logical_rows() - screen.rows() > 0, "a full-screen scroll must still reach history");
}

#[test]
fn ris_leaves_a_terminal_indistinguishable_from_a_fresh_one() {
	// RIS restored SGR, the cursor, the margins and the mouse modes, and left the OSC-modified
	// palette, the default colours, the cursor shape and the saved-cursor state exactly as the
	// previous program had them. A shell runs this after a program crashes precisely to get a
	// terminal it did not configure.
	let mut screen = Screen::new(8, 4, 4);
	feed(&mut screen, b"\x1b]4;1;#00ff00\x07"); // repaint palette entry 1
	feed(&mut screen, b"\x1b[3 q"); // a blinking underline cursor
	feed(&mut screen, b"\x1b7"); // save the cursor somewhere
	feed(&mut screen, b"\x1b[5;5H\x1b7");
	feed(&mut screen, b"\x1bc"); // RIS

	let fresh = Screen::new(8, 4, 4);
	assert_eq!((0..16).map(|i| screen.palette_color(i)).collect::<Vec<_>>(), (0..16).map(|i| fresh.palette_color(i)).collect::<Vec<_>>(), "the palette must come back");
	assert!(screen.cursor_shape() == fresh.cursor_shape(), "and the cursor shape");
	// A DECRC after the reset must not restore a position from before it.
	feed(&mut screen, b"\x1b8");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (0, 0), "the saved cursor must be reset too");
}

#[test]
fn the_three_alternate_screen_modes_are_not_one_mode() {
	// `?47`, `?1047` and `?1049` shared one code path with the same save/restore and clear. xterm
	// distinguishes them and ported full-screen programs depend on the difference: which one saves
	// the cursor, which one clears on the way out.
	let row_text = |screen: &Screen, len: usize| -> alloc::vec::Vec<u8> { (0..len).map(|c| screen.cell(c, 0).glyph as u8).collect() };

	// ?1049 saves and restores the cursor.
	let mut screen = Screen::new(16, 4, 0);
	feed(&mut screen, b"\x1b[2;5H"); // row 1, column 4
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"\x1b[4;9H"); // move about inside the alternate screen
	feed(&mut screen, b"\x1b[?1049l");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (4, 1), "?1049 restores the cursor it saved");

	// ?47 does not: the program is expected to have saved it itself.
	let mut screen = Screen::new(16, 4, 0);
	feed(&mut screen, b"\x1b[2;5H");
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"\x1b[4;9H");
	feed(&mut screen, b"\x1b[?47l");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (8, 3), "?47 leaves the cursor where the program left it");

	// ?1047 clears the alternate buffer on the way out, so the next entry starts blank; ?47 does not.
	let mut screen = Screen::new(16, 4, 0);
	feed(&mut screen, b"\x1b[?1047h");
	feed(&mut screen, b"LEFTOVER");
	feed(&mut screen, b"\x1b[?1047l");
	feed(&mut screen, b"\x1b[?1047h");
	assert_ne!(row_text(&screen, 8), b"LEFTOVER".to_vec(), "?1047 clears on exit");

	let mut screen = Screen::new(16, 4, 0);
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"LEFTOVER");
	feed(&mut screen, b"\x1b[?47l");
	feed(&mut screen, b"\x1b[?47h");
	assert_eq!(row_text(&screen, 8), b"LEFTOVER".to_vec(), "?47 does not");
}

#[test]
fn a_degenerate_geometry_is_a_small_screen_rather_than_a_panic() {
	// `Screen::new` multiplied without checking and `Term::new` divides a surface by the cell size,
	// so a surface smaller than 8x16 produced a 0x0 grid whose first `put_glyph` indexed an empty
	// `wrap` vector. The kernel checks for 0x0 after constructing one; a public API should not
	// require its callers to know that.
	let mut screen = Screen::new(0, 0, 0);
	assert!(screen.cols() >= 1 && screen.rows() >= 1, "a grid always has somewhere for the cursor to be");
	feed(&mut screen, b"abc\r\ndef");
	// `cell` is public and indexed without a check, where `set_cell` returns quietly.
	let _ = screen.cell(10_000, 10_000);

	// And an absurd configuration is clamped rather than allocated: the grid to the ceilings, the
	// scrollback to a byte budget that does not grow with the display's width.
	let big = Screen::new(usize::MAX, 24, usize::MAX);
	assert!(big.cols() <= crate::screen::MAX_COLS);
	let ring_rows = big.total_logical_rows() - big.rows();
	assert_eq!(ring_rows, 0, "an empty screen has an empty ring whatever it was sized for");
	let rows_budget = crate::screen::MAX_SCROLLBACK_BYTES / (big.cols() * core::mem::size_of::<crate::Cell>());
	assert!(rows_budget > 0 && rows_budget < usize::MAX, "the ring is sized by bytes, not by the number asked for");
}

#[test]
fn a_query_sequence_is_answered_instead_of_ignored() {
	// `csi_dispatch` handled no `n` (DSR/CPR) and no `c` (DA), so a program asking where the cursor
	// is waited forever - and the OSC 52 query case said outright that a write-back path was
	// missing. The model records what it owes and the console delivers it, which is the shape
	// `clipboard_set` and the tty mode requests already use.
	let mut screen = Screen::new(20, 6, 0);
	feed(&mut screen, b"\x1b[3;7H"); // row 3, column 7, one-based
	feed(&mut screen, b"\x1b[6n");
	assert_eq!(screen.take_reply(), b"\x1b[3;7R", "CPR reports the cursor, one-based");
	assert!(screen.take_reply().is_empty(), "and the reply is drained, not repeated");

	feed(&mut screen, b"\x1b[5n");
	assert_eq!(screen.take_reply(), b"\x1b[0n", "DSR reports the terminal is there");

	feed(&mut screen, b"\x1b[c");
	assert_eq!(screen.take_reply(), b"\x1b[?6c", "DA identifies the terminal");

	// A program that queries faster than the console drains must not grow the queue without limit.
	// Against the constant, not against a copy of it: the bound was raised to fit the clipboard
	// answer and a hard-coded 256 here would have had to be edited to match, which is how a test
	// stops pinning the property and starts pinning the number.
	for _ in 0..4000 {
		feed(&mut screen, b"\x1b[6n");
	}
	assert!(screen.take_reply().len() <= crate::screen::MAX_REPLY_BYTES, "an undrained reply queue is bounded");
}

#[test]
fn backspace_steps_back_over_a_soft_wrap() {
	// The line editor moves the cursor by repeating `\x08`, and the screen's backspace stopped at
	// column 0 - it never stepped onto the previous row. So a command long enough to wrap left
	// Home, Backspace and mid-line editing moving the editor's cursor while the caret stayed stuck
	// on the last physical row: buffer and screen diverged and every later edit drew in the wrong
	// place.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"aaaaaaaabb"); // wraps after eight
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (2, 1));
	for _ in 0..3 {
		feed(&mut screen, b"\x08");
	}
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (7, 0), "backspace must cross a soft wrap");

	// But NOT over a hard line break: a backspace at the start of a real line stops there.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"one\r\ntw");
	feed(&mut screen, b"\x08\x08\x08\x08");
	assert_eq!((screen.cursor_col(), screen.cursor_row()), (0, 1), "and must not cross a hard newline");
}

#[test]
fn a_sequence_that_moves_nothing_does_not_end_a_deferred_wrap() {
	// `csi_dispatch` cleared `pending_wrap` at its ENTRY, under a comment saying "a sequence that
	// moves or repositions the cursor ends any deferred wrap" - and most CSI sequences do not move
	// the cursor. SGR, DSR, DA, the cursor style and every mode set went through it.
	//
	// On an eight-column screen a full line, a colour change and one more character is what a
	// prompt does, and the character landed over the `8` instead of starting the next row.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678\x1b[31mX");
	assert_eq!(dump(&screen), b"12345678X", "the X begins the next row, joined to the first by the soft wrap");
	assert_eq!(screen.cell(7, 0).glyph, '8' as u32, "and it did not land on the 8");
	assert_eq!(screen.cell(0, 1).glyph, 'X' as u32, "it is at the start of row 1");

	// The same for the other non-moving sequences the entry-clear used to catch.
	for sequence in [&b"\x1b[0m"[..], b"\x1b[5n", b"\x1b[c", b"\x1b[1 q", b"\x1b[?25h"] {
		let mut screen = Screen::new(8, 4, 0);
		feed(&mut screen, b"12345678");
		feed(&mut screen, sequence);
		feed(&mut screen, b"X");
		assert_eq!(screen.cell(0, 1).glyph, 'X' as u32, "{sequence:?} moves nothing, so the deferred wrap survives it");
	}
}

#[test]
fn an_esc_cursor_move_after_a_full_line_feeds_once() {
	// The other direction. `ESC D` calls `line_feed`, which did not touch `pending_wrap` - so a
	// full line followed by `ESC D` followed by a glyph fed TWICE and the character landed two rows
	// down. `ESC M` and `ESC E` had the same shape.
	// IND keeps the COLUMN - it is a line feed, not a new line - so the X belongs at (7, 1). What
	// the defect produced was (7, 2): the ESC fed once and the surviving deferred wrap fed again.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678\x1bDX");
	assert_eq!(screen.cursor_row(), 1, "ESC D moved down one row, and the wrap it ended did not move another");
	assert_eq!(screen.cell(7, 1).glyph, 'X' as u32, "the X is one row down, not two");
	assert_eq!(screen.cell(0, 2).glyph, ' ' as u32, "and row 2 was never reached");

	// ESC E: carriage return plus line feed, so the same single move.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"12345678\x1bEX");
	assert_eq!(screen.cell(0, 1).glyph, 'X' as u32, "ESC E feeds once");

	// ESC M on row 1 goes back to row 0, and the deferred wrap must not then feed forward again.
	// RI keeps the column too, so this is (7, 0) rather than (7, 1) - which is where the surviving
	// wrap would have put it by feeding forward again after the ESC had moved back.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"\r\n12345678\x1bMX");
	assert_eq!(screen.cell(7, 0).glyph, 'X' as u32, "ESC M moved up, and the wrap did not survive it");
}

#[test]
fn a_resize_during_the_alternate_screen_leaves_the_primary_intact() {
	// `resize`'s reflow read the primary CELLS with the ALTERNATE screen's wrap flags, so it
	// reflowed the shell's scrollback according to a full-screen program's line breaks - and then
	// assigned both vectors unconditionally, leaving them swapped afterwards.
	let mut screen = Screen::new(8, 4, 8);
	feed(&mut screen, b"aaaaaaaabbbb\r\nsecond");
	let before = dump(&screen);
	assert_eq!(before, b"aaaaaaaabbbb\nsecond", "the fixture soft-wraps and then breaks hard");

	// The full-screen program's line structure is DELIBERATELY DIFFERENT from the primary's: two
	// short hard-broken lines against one soft-wrapped one. A reflow reading the wrong screen's
	// flags then breaks the primary's wrapped line in two, which is a difference the dump shows.
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"xx\r\nyy");
	// A resize while the full-screen program is up, then the program exits.
	screen.resize(8, 6, 8, 6);
	feed(&mut screen, b"\x1b[?1049l");
	assert_eq!(dump(&screen), before, "the primary screen comes back as it was, with its own line structure");
}

#[test]
fn a_reset_during_the_alternate_screen_returns_a_clean_primary_and_a_clean_alternate() {
	// RIS set `alt_active = false` directly rather than through `leave_alt_buffer`, which was the
	// only place that swapped the wrap vectors back - so a reset during alt left the primary live
	// with the alternate's flags, and the `clear()` that follows ran against them.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"aaaaaaaabbbb");
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"xxxxxxxxyyyy");
	feed(&mut screen, b"\x1bc");
	assert_eq!(dump(&screen), b"", "a reset leaves nothing on the primary screen");

	// And nothing on the alternate either: entering it again shows a blank screen rather than the
	// last program's picture.
	feed(&mut screen, b"\x1b[?47h");
	assert_eq!(dump(&screen), b"", "nor on the alternate screen the reset also cleared");
	feed(&mut screen, b"one\r\ntwo");
	assert_eq!(dump(&screen), b"one\ntwo", "and the alternate screen works normally afterwards");
}

#[test]
fn mode_47_preserves_wrap_flags_along_with_cells() {
	// `?47` deliberately preserves the alternate CELLS across leave and re-enter - that is what
	// lets a program leave and return without redrawing - while `enter_alt_buffer` cleared its wrap
	// flags on every entry. So the program got its characters back without their line structure,
	// and the serializer broke a soft-wrapped line in two.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"\x1b[?47h");
	feed(&mut screen, b"xxxxxxxxyyyy");
	let inside = dump(&screen);
	assert_eq!(inside, b"xxxxxxxxyyyy", "the alternate screen soft-wrapped");
	feed(&mut screen, b"\x1b[?47l");
	feed(&mut screen, b"\x1b[?47h");
	assert_eq!(dump(&screen), inside, "re-entering ?47 returns the cells AND the line structure");
}

#[test]
fn a_selection_over_the_alternate_screen_copies_what_is_shown() {
	// `global_glyph` read `self.primary` for live rows while `global_wrap` read the active flags,
	// so during alt they described different screens: a selection over a full-screen program
	// highlighted the alternate cells and copied the shell text underneath them. What is seen and
	// what is copied were different things.
	let mut screen = Screen::new(8, 4, 0);
	feed(&mut screen, b"shelltxt");
	feed(&mut screen, b"\x1b[?1049h");
	feed(&mut screen, b"program!");
	screen.selection_begin(0, 0);
	screen.selection_extend(7, 0);
	assert_eq!(screen.selection_text(), b"program!", "the selection copies the screen that is showing");
}

// Drive the editor and return what it echoed since the last call.
fn echoed(ld: &mut Ld, echo: &mut Echo, bytes: &[u8]) -> Vec<u8> {
	let before = echo.ser.as_slice().len();
	for &b in bytes {
		ld.feed(b, &[], echo);
	}
	echo.ser.as_slice()[before..].to_vec()
}

#[test]
fn the_line_editor_measures_in_cells_not_bytes() {
	// The decoder half was done - bytes at or above 0x80 reach the line, and Backspace, Delete,
	// Left and Right respect UTF-8 boundaries. The cursor ARITHMETIC did not follow: Home moved
	// left by a byte count and `replace_line` erased by one, so on a line containing any multi-byte
	// character both walked past the start of the line and into the prompt.
	//
	// Three characters, four bytes.
	let text: Vec<u8> = "\u{10d}aj".as_bytes().to_vec();
	assert_eq!(text.len(), 4, "the fixture must be multi-byte, or nothing below means anything");

	// Home from the end emits one backspace per CELL, which is three, not four.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	echoed(&mut ld, &mut echo, &text);
	assert_eq!(echoed(&mut ld, &mut echo, b"\x1b[H"), b"\x08\x08\x08", "Home steps back three cells for three characters");

	// Ctrl+U erases three cells, not four.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	echoed(&mut ld, &mut echo, &text);
	assert_eq!(echoed(&mut ld, &mut echo, b"\x15"), b"\x08 \x08\x08 \x08\x08 \x08", "Ctrl+U erases three cells, not four");
}

#[test]
fn a_multi_byte_character_reaches_the_screen_whole() {
	// The echo happened per BYTE, so inserting a multi-byte character in the middle of a line put
	// its lead byte on the display, then the whole suffix, and only then its continuation bytes - a
	// valid line in the editor's buffer and a replacement character on the screen.
	let mut ld = Ld::new(8);
	let mut echo = Echo { term: None, ser: EchoBuf::new() };
	echoed(&mut ld, &mut echo, b"ab");
	// Left once, so the insertion is mid-line.
	echoed(&mut ld, &mut echo, b"\x1b[D");
	let ch: Vec<u8> = "\u{10d}".as_bytes().to_vec();
	assert_eq!(ch.len(), 2);
	assert_eq!(echoed(&mut ld, &mut echo, &ch[..1]), b"", "a lead byte alone draws nothing - the character is not there yet");
	// Now the whole character, then the suffix, then one backspace per suffix CELL.
	let mut expected: Vec<u8> = Vec::new();
	expected.extend_from_slice(&ch);
	expected.push(b'b');
	expected.extend_from_slice(b"\x08");
	assert_eq!(echoed(&mut ld, &mut echo, &ch[1..]), expected, "the completed character and the suffix arrive together");
}

#[test]
fn typing_past_the_width_soft_wraps_and_keeps_scrolling() {
	// THE ORDINARY CASE, which the long-line test above does not reach: it types fifty characters
	// and then presses `Home`, and `Home` sets `self.cursor = 0` before moving, so the count that
	// `Ld::insert` steps back by is zero on a path where zero was already the answer.
	//
	// Typing at the END of a line - most typing - is exactly where the count is zero and the answer
	// is not. `move_left(0)` fed `caret_index()` back into an absolute CUP, and CUP cancels the
	// deferred wrap: the glyph landed where it would have landed anyway and `wrap[0]` was never set,
	// so a line the user typed continuously rendered as a hard newline.
	let mut term = test_term(8, 4, 16);
	let mut ld = Ld::new(8);
	let vocab: Vec<Vec<u8>> = Vec::new();
	{
		let mut echo = Echo { term: Some(&mut term), ser: EchoBuf::new() };
		for b in b"123456789" {
			ld.feed(*b, &vocab, &mut echo);
		}
	}
	assert!(term.screen.row_wrapped(0), "the first row is SOFT-wrapped: the ninth character continued the line rather than starting a new one");
	assert_eq!(term.screen.cell(0, 1).glyph, b'9' as u32, "and the ninth character is on the second row");
	assert_eq!((term.screen.cursor_col(), term.screen.cursor_row()), (1, 1), "with the caret just past it");

	// And past `rows * cols`, where the same defect stopped the screen scrolling: `target / cols`
	// was `rows`, the CUP arm clamped it to `rows - 1`, and every later character overwrote the
	// last row.
	{
		let mut echo = Echo { term: Some(&mut term), ser: EchoBuf::new() };
		for i in 0..40u8 {
			ld.feed(b'A' + i % 26, &vocab, &mut echo);
		}
	}
	assert_eq!(term.screen.cursor_row(), 3, "the caret is on the last row because the screen scrolled under it");
	let last = (0..8).map(|c| term.screen.cell(c, 3).glyph).collect::<Vec<u32>>();
	assert!(last.iter().all(|g| *g != b'1' as u32), "the last row holds the tail of the typing rather than a row overwritten in place");
	assert_eq!(ld.len, 49, "and every character went into the buffer");
}

#[test]
fn every_cursor_move_settles_the_deferred_wrap() {
	// THE SENTENCE MADE CHECKABLE. `cursor_moved`'s comment says "every path that actually assigns
	// `row` or `col` calls this, and nothing else does", and that claim has now been wrong twice -
	// `Screen::clear`, which `ConsoleService` calls when a VT is reused for a fresh shell, and
	// `restore_cursor`. Both left a wrap owed against a column the cursor had left, so the next
	// glyph started a row down.
	//
	// So this drives every public way to move the cursor, each from a state with the wrap owed, and
	// asserts the flag is settled afterwards. A new mover written against that sentence and not
	// honouring it fails here rather than in somebody's prompt.
	let moves: &[(&str, &[u8])] = &[
		("CUP", b"\x1b[1;1H"),
		("CUU", b"\x1b[1A"),
		("CUD", b"\x1b[1B"),
		("CUF", b"\x1b[1C"),
		("CUB", b"\x1b[1D"),
		("CNL", b"\x1b[1E"),
		("CPL", b"\x1b[1F"),
		("CHA", b"\x1b[1G"),
		("VPA", b"\x1b[1d"),
		("carriage return", b"\r"),
		("line feed", b"\n"),
		("backspace", b"\x08"),
		("tab", b"\t"),
		("index", b"\x1bD"),
		("reverse index", b"\x1bM"),
		("next line", b"\x1bE"),
		("DECSTBM", b"\x1b[1;4r"),
		("DECRC", b"\x1b8"),
		("alternate screen", b"\x1b[?1049h"),
		("reset", b"\x1bc"),
	];
	for (label, sequence) in moves {
		let mut term = test_term(8, 4, 16);
		// Fill the last column of row 0, which is the only way to owe a wrap.
		feed(&mut term.screen, b"12345678");
		assert!(term.screen.wrap_pending(), "{label}: the fixture must start with the wrap owed");
		feed(&mut term.screen, sequence);
		assert!(!term.screen.wrap_pending(), "{label} moves the cursor and must settle the deferred wrap with it");
	}

	// And `clear`, which is not a control sequence but is reached from production.
	let mut term = test_term(8, 4, 16);
	feed(&mut term.screen, b"12345678");
	assert!(term.screen.wrap_pending());
	term.screen.clear();
	assert!(!term.screen.wrap_pending(), "clear() homes the cursor, so it settles the wrap");
}
