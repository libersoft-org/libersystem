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
