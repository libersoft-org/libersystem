//! Host-runnable tests for the graphics-free terminal model (L2).
//!
//! They drive `Screen` with byte streams and check that `TextSink` serializes the grid to
//! the expected logical text - the model is exercised with no renderer, proving it is
//! graphics-independent.

use crate::{RawSink, Screen, TextSink};
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
	let mut s = Screen::new(8, 4);
	feed(&mut s, b"hello world\nbye");
	assert_eq!(dump(&s), b"hello world\nbye");
}

// Trailing spaces are trimmed and the blank bottom of the screen produces no trailing
// newlines.
#[test]
fn trims_trailing_spaces_and_blank_rows() {
	let mut s = Screen::new(10, 5);
	feed(&mut s, b"abc   \ndef");
	assert_eq!(dump(&s), b"abc\ndef");
}

// A blank screen serializes to nothing.
#[test]
fn blank_screen_is_empty() {
	let s = Screen::new(8, 4);
	assert_eq!(dump(&s), b"");
}

// A line that exactly fills the width and is then explicitly newlined is a hard break, not
// a soft wrap (the next line stays separate).
#[test]
fn exact_width_then_newline_is_hard_break() {
	let mut s = Screen::new(4, 4);
	feed(&mut s, b"abcd\nef");
	assert_eq!(dump(&s), b"abcd\nef");
}

// Content that scrolls off the top is kept in scrollback, and its soft-wrap flag travels
// with it so the dump still joins the wrapped line after the scroll.
#[test]
fn scrollback_preserves_soft_wrap() {
	let mut s = Screen::new(6, 3);
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
	let mut s = Screen::new(8, 4);
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

// The DEC private mouse-tracking modes (?1000 normal, ?1002 button-event, ?1003 any-event)
// and the SGR encoding (?1006) toggle the queryable mode the console reads to route pointer
// events; each turns off again with the matching `l`.
#[test]
fn mouse_modes_track_the_dec_private_toggles() {
	let mut s = Screen::new(8, 4);
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
	let mut s = Screen::new(8, 4);
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
	let mut s = Screen::new(8, 4);
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
	let mut s = Screen::new(8, 4);
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
	let mut s = Screen::new(8, 4);
	feed(&mut s, b"ab\ncd");
	s.selection_begin(0, 0);
	s.selection_extend(1, 1);
	assert_eq!(s.selection_text(), b"ab\ncd");
}

// Selection works over the scrollback view: scrolling up brings a scrolled-off line into
// the viewport, and a selection there copies that history line.
#[test]
fn selection_reaches_into_scrollback() {
	let mut s = Screen::new(8, 3);
	feed(&mut s, b"L0\nL1\nL2\nL3\nL4");
	// L0/L1 have scrolled into the history; page the view up to show them.
	s.scroll_view_up();
	s.selection_begin(0, 0);
	s.selection_extend(1, 0);
	assert_eq!(s.selection_text(), b"L0");
}
