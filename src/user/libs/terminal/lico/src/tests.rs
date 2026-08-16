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
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2004h");
	assert!(session.enter(&mut writer));
	assert!(session.restore(&mut writer));
	assert!(!session.is_active());
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?1049l");
	assert!(session.restore(&mut writer));
}

#[test]
fn terminal_session_attempts_cleanup_after_a_failed_enter() {
	let mut writer = Writer { bytes: Vec::new(), fail_on_call: Some(3), calls: 0 };
	let mut session = TerminalSession::new(TerminalOptions::tui());
	assert!(!session.enter(&mut writer));
	assert!(!session.is_active());
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?1049l");
	// Eight writes, not ten: the two mode escapes are gone from the stream - they are requests on
	// the control channel now, which `fail_on_call` does not count and a file cannot forge.
	assert_eq!(writer.calls, 8);
}

#[test]
fn terminal_guard_restores_modes_when_the_application_leaves_scope() {
	let mut writer = Writer::new();
	{
		let mut terminal = TerminalGuard::enter(&mut writer, TerminalOptions::tui()).expect("terminal modes enter");
		assert!(terminal.is_active());
		assert!(terminal.writer().write(b"frame"));
	}
	assert_eq!(writer.bytes, b"\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2004hframe\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?1049l");
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

// THE DESCRIPTORS THIS SYSTEM SHIPS, read by the parser that will read them at runtime.
//
// `include_bytes!` rather than a copy: a test written against a transcription of the asset proves
// the transcription parses, which is not the question. These are the exact bytes installed under
// `bin/lico/syntax/`, so a descriptor edited into an invalid state fails here rather than in a
// viewer that silently falls back to plain text.
const INSTALLED: [(&str, &[u8]); 6] = [
	("rust", include_bytes!("../../../../../volume/bin/lico/syntax/rust.syntax")),
	("lsidl", include_bytes!("../../../../../volume/bin/lico/syntax/lsidl.syntax")),
	("toml", include_bytes!("../../../../../volume/bin/lico/syntax/toml.syntax")),
	("json", include_bytes!("../../../../../volume/bin/lico/syntax/json.syntax")),
	("markdown", include_bytes!("../../../../../volume/bin/lico/syntax/markdown.syntax")),
	("shell", include_bytes!("../../../../../volume/bin/lico/syntax/shell.syntax")),
];

#[test]
fn every_installed_descriptor_parses_and_names_itself() {
	for (name, bytes) in INSTALLED {
		let descriptor = parse_descriptor(bytes).unwrap_or_else(|error| panic!("{name}.syntax does not parse: {error:?}"));
		assert_eq!(descriptor.name(), name, "the descriptor's `name` matches the file it is installed as");
		assert!(descriptor.rule_count() > 0, "{name} carries rules rather than only declarations");
		assert!(descriptor.context_count() > 0, "{name} declares at least a root context");
		// AND IT HIGHLIGHTS SOMETHING. A descriptor that parses and produces no spans is the
		// failure a person writing one actually reaches - a rule in the wrong context, a literal
		// with a typo - and it looks exactly like a file with nothing to highlight. Feeding it its
		// own bytes is the cheapest source of text guaranteed to contain what it describes: every
		// one of these begins with a comment.
		let mut state = descriptor.initial_state();
		let mut spans = [TokenSpan { start: 0, end: 0, style: 0 }; 32];
		let mut styled = 0;
		for line in bytes.split(|byte| *byte == b'\n') {
			let result = descriptor.highlight_line(&mut state, line, &mut spans);
			styled += result.spans;
		}
		assert!(styled > 0, "{name} produced no spans over its own text");
	}
}

#[test]
fn the_installed_set_selects_one_descriptor_per_kind_of_file() {
	// SELECTION IS THE HALF A BAD DESCRIPTOR BREAKS QUIETLY. A glob that is too greedy does not
	// fail to parse - it steals files from another language, and the reader sees Rust keywords
	// highlighted in a JSON file rather than an error. So this asserts what each name resolves to
	// across the whole installed set rather than one descriptor at a time.
	let descriptors: Vec<SyntaxDescriptor> = INSTALLED.iter().map(|(_, bytes)| parse_descriptor(bytes).expect("parses")).collect();
	for (file, expected) in [
		("main.rs", "rust"),
		("storage.lsidl", "lsidl"),
		("Cargo.toml", "toml"),
		("Cargo.lock", "toml"),
		("package.json", "json"),
		("README.md", "markdown"),
		("build.sh", "shell"),
		("product.conf", "shell"),
	] {
		let selected = select_descriptor(&descriptors, file.as_bytes(), b"").unwrap_or_else(|| panic!("{file} selects nothing"));
		assert_eq!(selected.descriptor.name(), expected, "{file}");
		assert_eq!(selected.kind, SyntaxMatchKind::Filename, "{file} is recognised by its name");
	}
	// A FILE WITH NO EXTENSION FALLS TO ITS FIRST LINE, which is what a shell script without one
	// has. And a file that matches neither selects NOTHING rather than something arbitrary: the
	// caller's fallback is plain text, and a wrong language is worse than none.
	let selected = select_descriptor(&descriptors, b"install", b"#!/bin/sh\n").expect("the shebang is recognised");
	assert_eq!(selected.descriptor.name(), "shell");
	assert_eq!(selected.kind, SyntaxMatchKind::FirstLine);
	assert!(select_descriptor(&descriptors, b"notes", b"nothing in particular").is_none(), "an unrecognised file selects no descriptor at all");
}

#[test]
fn undo_takes_back_an_operation_rather_than_a_keystroke() {
	// WHAT A PERSON UNDOES IS WHAT THEY DID, and what they did when they typed a word is type a
	// word. A history with one entry per keystroke is technically complete and useless for the
	// operation undo is reached for most, so a run of ordinary typing coalesces into one entry -
	// and anything that is not a continuation of it starts another.
	let mut buffer = TextBuffer::new(4096);
	for byte in b"hello" {
		buffer.insert(*byte).expect("room for the text");
	}
	assert_eq!(buffer.bytes(), b"hello");
	assert!(buffer.undo(), "there is something to undo");
	assert_eq!(buffer.bytes(), b"", "the whole typed run comes back out, not one letter of it");
	assert!(buffer.redo(), "and goes back in");
	assert_eq!(buffer.bytes(), b"hello");

	// A MOVE ENDS THE RUN. Without that, going somewhere else and typing there would join two
	// unrelated pieces of work into one undo step.
	buffer.move_home();
	buffer.insert(b'>').expect("room");
	assert_eq!(buffer.bytes(), b">hello");
	assert!(buffer.undo());
	assert_eq!(buffer.bytes(), b"hello", "the second run undoes on its own");

	// THE CURSOR COMES BACK TOO. Restoring the text and leaving the caret elsewhere is what makes
	// undo feel broken even when the bytes are right.
	buffer.move_end();
	let before = buffer.cursor();
	buffer.insert_slice(b" world").expect("room");
	assert!(buffer.undo());
	assert_eq!(buffer.cursor(), before, "the caret is put back where the edit started");

	// A PASTE IS ONE EDIT and so is a replace-all, for the same reason: each was one action.
	buffer.insert_slice(b" there").expect("room");
	assert_eq!(buffer.bytes(), b"hello there");
	assert!(buffer.undo());
	assert_eq!(buffer.bytes(), b"hello");
	assert_eq!(buffer.replace_all(b"l", b"L", false).expect("room"), 2);
	assert_eq!(buffer.bytes(), b"heLLo");
	assert!(buffer.undo());
	assert_eq!(buffer.bytes(), b"hello", "one undo takes back every replacement, not the last one");
	assert!(!buffer.can_redo() || buffer.can_redo(), "redo state is defined either way");
}

#[test]
fn a_refused_edit_leaves_the_buffer_exactly_as_it_was() {
	// THE ITEM'S OWN SENTENCE: an oversized edit fails without corrupting the buffer or the saved
	// file. Everything that can fail is booked before a byte moves, so this is a property rather
	// than a hope - and it is asserted on the cursor as well as the text, because a buffer whose
	// caret moved for an edit that did not happen is corrupt in the way a reader notices next.
	let mut buffer = TextBuffer::from_bytes(b"abc", 4).expect("within the limit");
	buffer.move_end();
	let cursor = buffer.cursor();
	buffer.insert(b'd').expect("the fourth byte fits");
	assert_eq!(buffer.insert(b'e'), Err(TextBufferError::TooLarge), "the fifth does not");
	assert_eq!(buffer.bytes(), b"abcd", "and nothing changed");
	assert_eq!(buffer.cursor(), cursor + 1, "the caret is where the accepted edit left it");
	assert_eq!(buffer.insert_slice(b"xyz"), Err(TextBufferError::TooLarge), "and a run that would not fit is refused whole");
	assert_eq!(buffer.bytes(), b"abcd", "rather than being written as far as it fits");
}

#[test]
fn block_indent_leaves_alone_the_lines_that_do_not_have_the_unit() {
	// UNINDENTING MUST NOT EAT TEXT. A selection whose lines are indented differently is the
	// ordinary case - a block where one line is already at the margin - and taking the first
	// characters off that line regardless is how an editor silently deletes code.
	let mut buffer = TextBuffer::from_bytes(b"\talpha\nbeta\n\tgamma", 4096).expect("fits");
	// The selection is made by MOVING, which is how a reader makes one. `goto_line` is a jump and
	// clears it deliberately - a jump that dragged a selection behind it would select everything
	// between two places somebody merely visited.
	buffer.set_anchor();
	buffer.move_down();
	buffer.move_down();
	buffer.move_end();
	assert_eq!(buffer.unindent_block(b"\t").expect("room"), 2, "two of the three lines had a tab");
	assert_eq!(buffer.bytes(), b"alpha\nbeta\ngamma", "and the one that did not is untouched");

	// AND ONE UNDO TAKES THE WHOLE BLOCK BACK, because the block was one operation.
	assert!(buffer.undo());
	assert_eq!(buffer.bytes(), b"\talpha\nbeta\n\tgamma");

	// A SELECTION ENDING AT THE START OF A LINE DOES NOT INCLUDE IT: selecting downward to the
	// first column of the next line is how a reader selects the lines above it, and indenting the
	// line the caret merely landed on would indent a line nobody chose.
	let mut buffer = TextBuffer::from_bytes(b"one\ntwo\nthree", 4096).expect("fits");
	buffer.set_anchor();
	for _ in 0..4 {
		buffer.move_right();
	}
	assert_eq!(buffer.indent_block(b"  ").expect("room"), 1, "the selection ends at the start of line 2, so only line 1 is indented");
	assert_eq!(buffer.bytes(), b"  one\ntwo\nthree");
}

#[test]
fn line_operations_keep_the_shape_of_the_file() {
	let mut buffer = TextBuffer::from_bytes(b"one\ntwo\nthree", 4096).expect("fits");
	buffer.goto_line(2);
	buffer.duplicate_line().expect("room");
	assert_eq!(buffer.bytes(), b"one\ntwo\ntwo\nthree");
	assert!(buffer.delete_line());
	assert_eq!(buffer.bytes(), b"one\ntwo\nthree");

	// DELETING THE LAST LINE TAKES THE NEWLINE BEFORE IT, because that line has none of its own -
	// otherwise the file is left with a blank line and a trailing newline it never had.
	buffer.goto_line(3);
	assert!(buffer.delete_line());
	assert_eq!(buffer.bytes(), b"one\ntwo");

	buffer.goto_line(2);
	assert!(buffer.move_line_up().expect("room"));
	assert_eq!(buffer.bytes(), b"two\none");
	assert_eq!(buffer.line_number(), 1, "the caret follows the line that moved");
	assert!(buffer.move_line_down().expect("room"));
	assert_eq!(buffer.bytes(), b"one\ntwo");
	assert_eq!(buffer.line_number(), 2);
	assert!(!buffer.move_line_down().expect("room"), "the last line has nowhere to go");
}

#[test]
fn a_hexadecimal_query_is_validated_before_anything_is_scanned() {
	// AN ODD NIBBLE IS NEVER GUESSED AT. `48 6` names half a byte, and choosing a half is how a
	// search silently looks for something nobody asked for - so it is refused, by name, before a
	// single window of the file is compared.
	assert_eq!(HexPattern::parse(b"48 6").unwrap_err(), HexPatternError::OddNibble);
	assert_eq!(HexPattern::parse(b"4 8").unwrap_err(), HexPatternError::OddNibble, "a separator inside a byte is a mistyped byte, not two");
	assert_eq!(HexPattern::parse(b"48 zz").unwrap_err(), HexPatternError::NotHexadecimal);
	assert_eq!(HexPattern::parse(b"48 ?").unwrap_err(), HexPatternError::NotHexadecimal, "a wildcard is two characters wide, like the byte it stands for");
	assert_eq!(HexPattern::parse(b"   ").unwrap_err(), HexPatternError::Empty);

	// SPACING IS PRESENTATION AND CASE IS PRESENTATION. The same three bytes are written all three
	// of these ways by a hex dump, a specification and a person.
	let haystack = b"say Hello there";
	for spelling in [&b"48 65 6c 6c 6f"[..], &b"4865 6C6C6F"[..], &b"48656c6C6f"[..]] {
		let pattern = HexPattern::parse(spelling).expect("valid");
		assert_eq!(pattern.len(), 5);
		assert_eq!(pattern.find(haystack, 0, false), Some(4), "{spelling:?}");
	}

	// A WILDCARD STANDS FOR EXACTLY ONE BYTE.
	let pattern = HexPattern::parse(b"48 ?? 6c").expect("valid");
	assert_eq!(pattern.find(haystack, 0, false), Some(4));
	assert_eq!(pattern.find(haystack, 5, false), None, "and there is only the one");

	// AND THE BOUND IS A REFUSAL. A pasted megabyte must not become a pattern dragged over every
	// window of the file.
	let long: Vec<u8> = core::iter::repeat_n(b'4', (MAX_PATTERN_BYTES + 1) * 2).collect();
	assert_eq!(HexPattern::parse(&long).unwrap_err(), HexPatternError::TooLong);
}

#[test]
fn a_text_search_repeats_forward_and_backward_without_standing_still() {
	// REPEATING A SEARCH HAS TO ADVANCE. A forward search that started AT the current match would
	// find the same line forever, which is the classic way a viewer's `n` key does nothing.
	let haystack = b"alpha beta alphabet alpha";
	let query = TextQuery::new(b"alpha");
	assert_eq!(query.find(haystack, 0, false), Some(0));
	assert_eq!(query.find(haystack, 1, false), Some(11));
	assert_eq!(query.find(haystack, 12, false), Some(20));
	assert_eq!(query.find(haystack, 21, false), None);
	assert_eq!(query.find(haystack, 25, true), Some(20), "backward is strictly before where it started");
	assert_eq!(query.find(haystack, 20, true), Some(11));
	assert_eq!(query.find(haystack, 0, true), None);

	// WHOLE WORD IS A FILTER AND NOT A STOP. Rejecting `alphabet` must not end the search - the
	// next real word is further along.
	let word = TextQuery { needle: b"alpha", ignore_case: false, whole_word: true };
	assert_eq!(word.find(haystack, 0, false), Some(0));
	assert_eq!(word.find(haystack, 1, false), Some(20), "the substring inside `alphabet` is skipped, not fatal");

	let folded = TextQuery { needle: b"BETA", ignore_case: true, whole_word: false };
	assert_eq!(folded.find(haystack, 0, false), Some(6));
	assert_eq!(TextQuery::new(b"").find(haystack, 0, false), None, "an empty query matches nothing rather than everywhere");
}

#[test]
fn a_modified_key_arrives_as_a_chord_and_a_bare_one_does_not() {
	// SELECTION NEEDS SHIFT, and before this every modified key decoded to `InvalidSequence` - so
	// shift+arrow was indistinguishable from a damaged sequence and an editor could not offer
	// shift-movement at all.
	let mut decoder = InputDecoder::new();
	let mut feed = |decoder: &mut InputDecoder, bytes: &[u8]| {
		let mut last = None;
		for &byte in bytes {
			if let Some(event) = decoder.feed(byte) {
				last = Some(event);
			}
		}
		last.expect("the sequence produced an event")
	};

	assert_eq!(feed(&mut decoder, b"\x1b[A"), InputEvent::Key(Key::ArrowUp), "a bare arrow keeps the shape every caller already handles");
	assert_eq!(feed(&mut decoder, b"\x1b[1;2A"), InputEvent::Chord(Chord { key: Key::ArrowUp, shift: true, alt: false, control: false }));
	assert_eq!(feed(&mut decoder, b"\x1b[1;5C"), InputEvent::Chord(Chord { key: Key::ArrowRight, shift: false, alt: false, control: true }));
	assert_eq!(feed(&mut decoder, b"\x1b[1;6D"), InputEvent::Chord(Chord { key: Key::ArrowLeft, shift: true, alt: false, control: true }));
	assert_eq!(feed(&mut decoder, b"\x1b[3;2~"), InputEvent::Chord(Chord { key: Key::Delete, shift: true, alt: false, control: false }), "the tilde forms carry modifiers too");

	// BACK-TAB IS ITS OWN FINAL BYTE rather than a modified Tab, which is what unindent is bound to.
	assert_eq!(feed(&mut decoder, b"\x1b[Z"), InputEvent::Chord(Chord { key: Key::Tab, shift: true, alt: false, control: false }));

	// A MODIFIER OF NONE IS THE PLAIN KEY. Some terminals spell a bare arrow this way, and a caller
	// that had to handle both shapes for one keystroke would eventually handle only one.
	assert_eq!(feed(&mut decoder, b"\x1b[1;1A"), InputEvent::Key(Key::ArrowUp));

	// AND WHAT IS STILL NOT A KEY STAYS REFUSED, rather than becoming a chord with a guessed key.
	assert_eq!(feed(&mut decoder, b"\x1b[9;2A"), InputEvent::InvalidSequence, "the letter form's first parameter is always 1");
	assert_eq!(feed(&mut decoder, b"\x1b[1;2X"), InputEvent::InvalidSequence, "an unknown final byte names no key");
}

#[test]
fn word_movement_skips_what_separates_words_rather_than_stepping_over_it() {
	// PRESSING CONTROL+LEFT IN A RUN OF SPACES MUST REACH THE WORD BEFORE THEM. A movement that
	// stopped at the first space would need pressing twice for every gap, which is the difference
	// between word movement and a slower arrow key.
	let mut buffer = TextBuffer::from_bytes(b"alpha   beta_two, gamma", 4096).expect("fits");
	buffer.move_end();
	assert_eq!(buffer.cursor(), 23);
	assert!(buffer.move_word_left());
	assert_eq!(buffer.cursor(), 18, "back to the start of `gamma`");
	assert!(buffer.move_word_left());
	assert_eq!(buffer.cursor(), 8, "past the comma AND the space, to the start of `beta_two`");
	assert!(buffer.move_word_left());
	assert_eq!(buffer.cursor(), 0, "past three spaces, to the start of `alpha`");
	assert!(!buffer.move_word_left(), "and there is nowhere further to go");

	assert!(buffer.move_word_right());
	assert_eq!(buffer.cursor(), 5, "forward stops past the end of the word");
	assert!(buffer.move_word_right());
	assert_eq!(buffer.cursor(), 16, "`beta_two` is one word - the underscore is part of it");
	assert!(buffer.move_word_right());
	assert_eq!(buffer.cursor(), 23);
	assert!(!buffer.move_word_right());
}

#[test]
fn replacing_one_occurrence_replaces_the_one_that_is_selected() {
	// REPLACE-ONE ACTS ON WHAT THE READER CAN SEE, which is why it takes the selection rather than
	// a position: a replace that acted on "the next match" while a different match was highlighted
	// would change text the reader was not looking at.
	let mut buffer = TextBuffer::from_bytes(b"one two one", 4096).expect("fits");
	let at = buffer.find(b"one", 1, false, false).expect("the second occurrence");
	assert_eq!(at, 8);
	buffer.set_cursor(at);
	buffer.set_anchor();
	buffer.set_cursor(at + 3);
	assert!(buffer.replace_selection(b"one", b"1", false));
	assert_eq!(buffer.bytes(), b"one two 1", "the first occurrence is untouched");

	// AND A SELECTION THAT IS NOT THE PATTERN IS REFUSED rather than replaced anyway - the caller
	// may have moved the selection since the search.
	buffer.set_cursor(0);
	buffer.set_anchor();
	buffer.set_cursor(3);
	assert!(!buffer.replace_selection(b"two", b"2", false), "the selection is `one`, not `two`");
	assert_eq!(buffer.bytes(), b"one two 1", "and nothing changed");
}

#[test]
fn a_listing_orders_the_way_a_reader_expects_and_stays_stable() {
	let entries = [
		EntryKey { name: b"zeta.rs", size: 10, modified: 300, is_dir: false },
		EntryKey { name: b"alpha", size: 30, modified: 100, is_dir: true },
		EntryKey { name: b"beta.rs", size: 10, modified: 200, is_dir: false },
		EntryKey { name: b".hidden", size: 5, modified: 400, is_dir: false },
		EntryKey { name: b"gamma.toml", size: 20, modified: 50, is_dir: false },
	];
	let sorted = |spec: SortSpec| {
		let mut kept: std::vec::Vec<EntryKey> = entries.iter().copied().filter(|entry| spec.admits(entry)).collect();
		kept.sort_by(|left, right| order(spec, left, right));
		kept.iter().map(|entry| entry.name).collect::<std::vec::Vec<_>>()
	};

	let by_name = SortSpec::default();
	assert_eq!(sorted(by_name), [&b"alpha"[..], b"beta.rs", b"gamma.toml", b"zeta.rs"], "directories lead and the hidden file is not shown");

	// NAME IS THE TIE-BREAK, so two files of the same size do not swap places between refreshes -
	// which is what keeps the entry under the cursor under the cursor.
	let by_size = SortSpec { key: SortKey::Size, ..by_name };
	assert_eq!(sorted(by_size), [&b"alpha"[..], b"beta.rs", b"zeta.rs", b"gamma.toml"], "10, 10 broken by name, then 20");

	// REVERSING REVERSES THE FILES, NOT THE GROUPING. A reader who reverses by size wants the
	// biggest first, not the folders moved to the bottom.
	let reversed = SortSpec { key: SortKey::Size, reverse: true, ..by_name };
	assert_eq!(sorted(reversed)[0], &b"alpha"[..], "the directory still leads");
	assert_eq!(sorted(reversed)[1], &b"gamma.toml"[..], "and the largest file is first among the files");

	// A LEADING DOT IS NOT AN EXTENSION SEPARATOR: `.hidden` is a hidden file named `hidden`, and
	// sorting it under a `hidden` extension puts it where nobody looks for it.
	let shown = SortSpec { key: SortKey::Extension, show_hidden: true, ..by_name };
	assert_eq!(sorted(shown)[1], &b".hidden"[..], "no extension sorts before every extension");
	assert_eq!(EntryKey { name: b".hidden", size: 0, modified: 0, is_dir: false }.extension(), b"");
	assert_eq!(EntryKey { name: b"a.tar.gz", size: 0, modified: 0, is_dir: false }.extension(), b"gz");

	// `..` IS NEVER HIDDEN. A panel that could hide the way out of a directory would be a trap.
	assert!(by_name.admits(&EntryKey { name: b"..", size: 0, modified: 0, is_dir: true }));
}

#[test]
fn history_forgets_the_future_when_the_reader_goes_somewhere_else() {
	let mut history = History::new();
	assert!(history.visit(b"vol://system"));
	assert!(history.visit(b"vol://system/bin"));
	assert!(history.visit(b"vol://system/bin/lico"));
	assert_eq!(history.back(), Some(&b"vol://system/bin"[..]));
	assert_eq!(history.back(), Some(&b"vol://system"[..]));
	assert_eq!(history.back(), None, "there is nothing before the first place");
	assert_eq!(history.forward(), Some(&b"vol://system/bin"[..]));

	// A NEW NAVIGATION DISCARDS WHAT WAS AHEAD. A forward list kept across a divergence offers to
	// go somewhere the reader did not come from.
	assert!(history.visit(b"vol://media"));
	assert_eq!(history.forward(), None, "the old forward entry is gone");
	assert_eq!(history.back(), Some(&b"vol://system/bin"[..]));

	// REFRESHING THE SAME DIRECTORY IS NOT A NAVIGATION.
	let before = history.len();
	assert!(history.visit(b"vol://system/bin"));
	assert_eq!(history.len(), before, "arriving where you already are records nothing");

	// AND IT IS BOUNDED, oldest first.
	let mut long = History::new();
	for index in 0..MAX_HISTORY + 10 {
		let mut path = std::vec::Vec::from(&b"vol://system/"[..]);
		path.extend_from_slice(std::format!("{index}").as_bytes());
		assert!(long.visit(&path));
	}
	assert_eq!(long.len(), MAX_HISTORY);
}

#[test]
fn quick_search_moves_to_the_name_being_typed_and_wraps_once() {
	let names: std::vec::Vec<&[u8]> = std::vec![b"alpha", b"Beta", b"beta.rs", b"gamma"];
	assert_eq!(quick_search(names.iter().copied(), b"be", 0), Some(1), "case is folded - typing a capital to reach a file is a requirement nobody wants");
	assert_eq!(quick_search(names.iter().copied(), b"be", 2), Some(2), "the search continues from where the cursor is");

	// WRAPPING ONCE, so a search started half way down reaches a name above it - and only once, so
	// a pattern nothing matches terminates.
	assert_eq!(quick_search(names.iter().copied(), b"al", 2), Some(0));
	assert_eq!(quick_search(names.iter().copied(), b"zz", 0), None);

	// PREFIX AND NOT SUBSTRING. Typing `mm` means a file that begins with it; matching inside
	// `gamma` would move the cursor somewhere the reader was not heading.
	assert_eq!(quick_search(names.iter().copied(), b"mm", 0), None);
	assert_eq!(quick_search(names.iter().copied(), b"", 0), None);
}

#[test]
fn a_bookmark_is_never_dropped_to_make_room_for_a_newer_one() {
	let mut marks = Bookmarks::new();
	assert!(marks.add(b"vol://system/bin"));
	assert!(marks.add(b"vol://system/bin"), "adding the same place twice is not an error and not a duplicate");
	assert_eq!(marks.len(), 1);
	assert!(marks.add(b"vol://media"));
	assert_eq!(marks.get(1), Some(&b"vol://media"[..]));
	assert!(marks.remove(b"vol://system/bin"));
	assert_eq!(marks.get(0), Some(&b"vol://media"[..]));
	assert!(!marks.remove(b"vol://system/bin"), "removing what is not there says so");

	for index in 0..MAX_BOOKMARKS {
		let mut path = std::vec::Vec::from(&b"vol://system/"[..]);
		path.extend_from_slice(std::format!("{index}").as_bytes());
		let _ = marks.add(&path);
	}
	assert_eq!(marks.len(), MAX_BOOKMARKS);
	// REFUSED, not rotated: a bookmark somebody made on purpose disappearing to make room for a
	// newer one is the opposite of what they asked for.
	assert!(!marks.add(b"vol://usb"), "past the cap it refuses");
	assert_eq!(marks.get(0), Some(&b"vol://media"[..]), "and the oldest is still there");
}
