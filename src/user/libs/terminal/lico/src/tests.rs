use super::*;
use std::vec::Vec;

struct Writer {
	bytes: Vec<u8>,
	fail_on_call: Option<usize>,
	calls: usize,
}

impl Writer {
	fn new() -> Writer {
		Writer { bytes: Vec::new(), fail_on_call: None, calls: 0 }
	}
}

impl TerminalWriter for Writer {
	fn write(&mut self, bytes: &[u8]) -> bool {
		self.calls += 1;
		if self.fail_on_call == Some(self.calls) {
			return false;
		}
		self.bytes.extend_from_slice(bytes);
		true
	}
}

#[test]
fn terminal_session_enters_and_restores_the_complete_tui_contract() {
	let mut writer = Writer::new();
	let mut session = TerminalSession::new(TerminalOptions::tui());
	assert!(session.enter(&mut writer));
	assert!(session.is_active());
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?9001h\x1b[?9002l\x1b[?1002h\x1b[?1006h\x1b[?2004h");
	assert!(session.enter(&mut writer));
	assert!(session.restore(&mut writer));
	assert!(!session.is_active());
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?9001h\x1b[?9002l\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?9001l\x1b[?9002h\x1b[?25h\x1b[?1049l");
	assert!(session.restore(&mut writer));
}

#[test]
fn terminal_session_attempts_cleanup_after_a_failed_enter() {
	let mut writer = Writer { bytes: Vec::new(), fail_on_call: Some(3), calls: 0 };
	let mut session = TerminalSession::new(TerminalOptions::tui());
	assert!(!session.enter(&mut writer));
	assert!(!session.is_active());
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?9001l\x1b[?9002h\x1b[?25h\x1b[?1049l");
	assert_eq!(writer.calls, 10);
}

#[test]
fn terminal_guard_restores_modes_when_the_application_leaves_scope() {
	let mut writer = Writer::new();
	{
		let mut terminal = TerminalGuard::enter(&mut writer, TerminalOptions::tui()).expect("terminal modes enter");
		assert!(terminal.is_active());
		assert!(terminal.writer().write(b"frame"));
	}
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?9001h\x1b[?9002l\x1b[?1002h\x1b[?1006h\x1b[?2004hframe\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?9001l\x1b[?9002h\x1b[?25h\x1b[?1049l");
}

#[test]
fn control_messages_require_exact_known_shapes() {
	assert_eq!(decode_control(b"WINSIZE\x18\x00\x50\x00"), Some(TerminalControl::InitialSize(TerminalSize::new(24, 80))));
	assert_eq!(decode_control(b"RESIZE\x32\x00\x78\x00"), Some(TerminalControl::Resized(TerminalSize::new(50, 120))));
	assert_eq!(decode_control(b"RESIZE\x32\x00\x78\x00x"), None);
	assert_eq!(WINSIZE_REQUEST, b"GET_WINSIZE");
}

#[test]
fn input_decoder_handles_navigation_functions_and_sgr_pointer_reports() {
	let mut input = InputDecoder::new();
	assert_eq!(input.feed(0x1b), None);
	assert_eq!(input.feed(b'['), None);
	assert_eq!(input.feed(b'A'), Some(InputEvent::Key(Key::ArrowUp)));
	for byte in b"\x1b[15~" {
		if *byte != b'~' {
			assert_eq!(input.feed(*byte), None);
		}
	}
	assert_eq!(input.feed(b'~'), Some(InputEvent::Key(Key::Function(5))));
	let mut event = None;
	for &byte in b"\x1b[<0;12;3M" {
		event = input.feed(byte).or(event);
	}
	assert_eq!(event, Some(InputEvent::Pointer(PointerEvent { code: 0, column: 12, row: 3, pressed: true })));
}

#[test]
fn input_decoder_rejects_unbounded_or_malformed_csi_sequences() {
	let mut input = InputDecoder::new();
	let mut event = None;
	for &byte in b"\x1b[999999999999~" {
		event = input.feed(byte).or(event);
	}
	assert_eq!(event, Some(InputEvent::InvalidSequence));
	assert_eq!(input.feed(b'x'), Some(InputEvent::Key(Key::Byte(b'x'))));
}

#[test]
fn text_decoder_preserves_chunk_boundaries_and_recovers_from_malformed_utf8() {
	let mut decoder = TextDecoder::new();
	let mut out = [0u32; 4];
	let first = decoder.decode(&[0xe2, 0x82], &mut out);
	assert_eq!(first, DecodedText { consumed: 2, produced: 0 });
	let second = decoder.decode(&[0xac], &mut out);
	assert_eq!(second, DecodedText { consumed: 1, produced: 1 });
	assert_eq!(out[0], 0x20ac);
	assert!(decoder.is_idle());
	let malformed = decoder.decode(&[0xe2, b'A'], &mut out);
	assert_eq!(malformed, DecodedText { consumed: 2, produced: 2 });
	assert_eq!(&out[..2], &[REPLACEMENT_CHARACTER, b'A' as u32]);
	assert_eq!(decoder.decode(&[0xe2], &mut out), DecodedText { consumed: 1, produced: 0 });
	assert_eq!(decoder.finish(), Some(REPLACEMENT_CHARACTER));
}

#[test]
fn display_line_uses_shared_tab_control_and_utf8_rules() {
	let mut output = Vec::new();
	let cells = append_display_line(b"a\tb\x01\xe2\x82", 8, 4, &mut output).expect("display buffer reserves");
	assert_eq!(cells, 7);
	assert_eq!(output, b"a   b.\xef\xbf\xbd");
}

#[test]
fn key_bindings_and_widget_state_stay_bounded_and_deterministic() {
	#[derive(Clone, Copy, Debug, Eq, PartialEq)]
	enum Action {
		View,
		Edit,
	}
	let bindings = [Binding::new(Key::Function(3), Action::View), Binding::new(Key::Function(4), Action::Edit)];
	assert_eq!(dispatch_key(&bindings, Key::Function(3)), Some(&Action::View));
	assert_eq!(dispatch_key(&bindings, Key::Function(5)), None);
	let mut focus = Focus::new(2);
	assert_eq!(focus.active(), Some(0));
	assert_eq!(focus.previous(), Some(1));
	assert_eq!(focus.next(), Some(0));
	assert!(!focus.select(2));
	let mut menu = MenuState::new(3);
	assert!(!menu.is_open());
	assert!(menu.toggle());
	menu.close();
	assert!(!menu.is_open());
	let mut dialog = DialogState::new();
	dialog.show(DialogKind::Confirm, 2);
	assert_eq!(dialog.kind(), DialogKind::Confirm);
	dialog.close();
	assert_eq!(dialog.kind(), DialogKind::None);
	let mut progress = Progress::new(Some(2), Some(100));
	progress.start();
	progress.advance(1, 40);
	progress.pause();
	progress.resume();
	progress.advance(1, 80);
	progress.complete();
	assert_eq!(progress.state, OperationState::Complete);
	assert_eq!(progress.byte_fraction_milli(), Some(1000));
}

#[test]
fn file_type_detection_prefers_magic_and_keeps_unknown_binary_safe() {
	assert_eq!(detect_file_type(b"screen.rs", b"\x7fELF", false), FileType::Executable);
	assert_eq!(detect_file_type(b"screen.rs", b"fn main() {}\n", false), FileType::Rust);
	assert_eq!(detect_file_type(b"theme.TOML", b"", false), FileType::Toml);
	assert_eq!(detect_file_type(b"sound.bin", b"OggS\0", false), FileType::Audio);
	assert_eq!(detect_file_type(b"blob", b"\0\x01\x02", false), FileType::Binary);
	assert_eq!(detect_file_type(b"any", b"", true), FileType::Directory);
}

#[test]
fn text_buffer_edits_and_moves_without_exceeding_its_bound() {
	let mut text = TextBuffer::from_bytes(b"one\ntwo\n", 12).expect("initial text fits");
	assert_eq!(text.line_at(0), b"one");
	assert_eq!(text.line_at(4), b"two");
	assert!(!text.is_dirty());
	assert!(text.move_end());
	assert_eq!(text.cursor(), 3);
	assert!(text.move_down());
	assert_eq!(text.cursor(), 7);
	assert!(text.move_home());
	assert_eq!(text.cursor(), 4);
	assert!(!text.is_dirty());
	text.insert(b'T').expect("insert fits");
	assert_eq!(text.bytes(), b"one\nTtwo\n");
	assert!(text.delete_before());
	assert_eq!(text.bytes(), b"one\ntwo\n");
	assert!(text.delete_at());
	assert_eq!(text.bytes(), b"one\nwo\n");
	assert!(text.is_dirty());
	assert_eq!(text.insert(b'x'), Ok(()));
	assert_eq!(text.insert(b'y'), Ok(()));
	assert_eq!(text.insert(b'z'), Ok(()));
	assert_eq!(text.insert(b'1'), Ok(()));
	assert_eq!(text.insert(b'2'), Ok(()));
	assert_eq!(text.insert(b'3'), Err(TextBufferError::TooLarge));
}

const RUST_SYNTAX: &[u8] = br#"lico-syntax 1
name rust
glob *.rs
first-line #!
style plain
style comment
style string
style keyword
max-nesting 4
context root plain
context comment comment
context string string
line root // comment
open root /* comment comment
close comment */
open root \" string string
close string \"
escape string \\
keyword root fn keyword
"#;

const SHELL_SYNTAX: &[u8] = b"lico-syntax 1\nname shell\nglob *.sh\nfirst-line #!\nstyle plain\nmax-nesting 1\ncontext root plain\n";

#[test]
fn syntax_descriptor_selects_deterministically_and_highlights_across_lines() {
	let rust = parse_descriptor(RUST_SYNTAX).unwrap();
	let shell = parse_descriptor(SHELL_SYNTAX).unwrap();
	let descriptors = [shell, rust];
	let selected = select_descriptor(&descriptors, b"main.rs", b"#!/bin/sh").unwrap();
	assert_eq!(selected.kind, SyntaxMatchKind::Filename);
	assert_eq!(selected.descriptor.name(), "rust");
	let selected = select_descriptor(&descriptors, b"profile", b"#!/bin/sh").unwrap();
	assert_eq!(selected.kind, SyntaxMatchKind::FirstLine);
	assert_eq!(selected.descriptor.name(), "rust");

	let descriptor = &descriptors[1];
	assert_eq!(descriptor.style_name(3), Some("keyword"));
	let mut state = descriptor.initial_state();
	let mut spans = [TokenSpan { start: 0, end: 0, style: 0 }; 8];
	let first = descriptor.highlight_line(&mut state, b"fn main() { /*", &mut spans);
	assert!(!first.truncated && !first.nesting_limited);
	assert_eq!(state.depth(), 2);
	assert!(spans[..first.spans].iter().any(|span| span.start == 0 && span.end == 2 && span.style == 3));
	let second = descriptor.highlight_line(&mut state, b"still */ fn", &mut spans);
	assert_eq!(state.depth(), 1);
	assert!(spans[..second.spans].iter().any(|span| span.style == 1));
	assert!(spans[..second.spans].iter().any(|span| span.style == 3));
}

#[test]
fn syntax_descriptor_rejects_hostile_or_ambiguous_rules_before_highlighting() {
	assert!(matches!(parse_descriptor(b"lico-syntax 2\n"), Err(SyntaxError::UnsupportedVersion)));
	assert!(matches!(parse_descriptor(b"lico-syntax 1\nname bad\nglob *.bad\nstyle plain\nmax-nesting 9\ncontext root plain\n"), Err(SyntaxError::InvalidNesting)));
	assert!(matches!(parse_descriptor(b"lico-syntax 1\nname bad\nglob *.bad\nstyle plain\nstyle comment\nmax-nesting 1\ncontext root plain\nline root // comment\nkeyword root // plain\n"), Err(SyntaxError::ConflictingRule)));
	assert!(matches!(parse_descriptor(b"lico-syntax 1\nname bad\nglob *.bad\nstyle plain\nmax-nesting 1\ncontext root plain\nopen missing /* root plain\n"), Err(SyntaxError::UnknownContext)));
	assert!(matches!(parse_descriptor(b"lico-syntax 1\n\xff"), Err(SyntaxError::InvalidUtf8)));
}
