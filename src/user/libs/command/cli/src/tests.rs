// Host tests for the shared command-line primitives.
//
// Everything here is a case a tool will meet: a pattern somebody typed, a size with a unit, a
// stream that ends without a newline, a line longer than the tool will hold. The hostile ones are
// the point - a matcher is handed filenames, and a filename is not a promise.

use super::*;

#[test]
fn sizes_carry_binary_units_and_refuse_decimal_ones() {
	assert_eq!(parse_size(b"0"), Some(0));
	assert_eq!(parse_size(b"512"), Some(512));
	assert_eq!(parse_size(b"1K"), Some(1024));
	assert_eq!(parse_size(b"1KiB"), Some(1024));
	assert_eq!(parse_size(b"2M"), Some(2 * 1024 * 1024));
	assert_eq!(parse_size(b"3G"), Some(3 * 1024 * 1024 * 1024));
	assert_eq!(parse_size(b" 4T "), Some(4u64 << 40));
	// The decimal spellings are REFUSED rather than read as binary: a caller who wrote `KB` meant
	// a thousand, and answering with 1024 is the mistake this avoids.
	assert_eq!(parse_size(b"1KB"), None);
	assert_eq!(parse_size(b"1MB"), None);
	// Not a size at all.
	assert_eq!(parse_size(b""), None);
	assert_eq!(parse_size(b"K"), None);
	assert_eq!(parse_size(b"-1"), None);
	assert_eq!(parse_size(b"12x"), None);
	// Overflow is refused rather than wrapped.
	assert_eq!(parse_size(b"18446744073709551615"), Some(u64::MAX));
	assert_eq!(parse_size(b"18446744073709551616"), None);
	assert_eq!(parse_size(b"17179869184G"), None);
}

#[test]
fn ranges_are_inclusive_and_refuse_what_cannot_be_meant() {
	assert_eq!(parse_range(b"3", false), Some(Range { start: 3, end: 3 }));
	assert_eq!(parse_range(b"2-5", false), Some(Range { start: 2, end: 5 }));
	assert_eq!(parse_range(b"4-", false), Some(Range { start: 4, end: u64::MAX }));
	assert_eq!(parse_range(b"-4", false), Some(Range { start: 1, end: 4 }));
	assert_eq!(parse_range(b"-4", true), Some(Range { start: 0, end: 4 }));
	// A reversed range is a mistake, not an instruction to reorder.
	assert_eq!(parse_range(b"5-2", false), None);
	// Where the caller counts from one, zero is not a position.
	assert_eq!(parse_range(b"0", false), None);
	assert_eq!(parse_range(b"0-3", true), Some(Range { start: 0, end: 3 }));
	assert_eq!(parse_range(b"", false), None);
	assert_eq!(parse_range(b"-", false), None);
	assert_eq!(parse_range(b"1-2-3", false), None);

	let list = parse_ranges(b"1,3-5,8-", false).expect("a well-formed list parses");
	assert_eq!(list.len(), 3);
	assert!(list[1].contains(4));
	assert!(!list[1].contains(6));
	// ONE BAD PART FAILS THE WHOLE LIST: cutting different fields than you were asked to, quietly,
	// is worse than refusing.
	assert_eq!(parse_ranges(b"1,x,3", false), None);
	assert_eq!(parse_ranges(b"1,5-2", false), None);
}

#[test]
fn globs_match_literals_wildcards_and_classes() {
	assert!(glob_match(b"hello", b"hello"));
	assert!(!glob_match(b"hello", b"hellp"));
	assert!(glob_match(b"*", b""));
	assert!(glob_match(b"*", b"anything"));
	assert!(glob_match(b"*.txt", b"notes.txt"));
	assert!(!glob_match(b"*.txt", b"notes.txtx"));
	assert!(glob_match(b"a*b*c", b"abc"));
	assert!(glob_match(b"a*b*c", b"axxbyyc"));
	assert!(!glob_match(b"a*b*c", b"axxbyy"));
	assert!(glob_match(b"?", b"x"));
	assert!(!glob_match(b"?", b""));
	assert!(!glob_match(b"?", b"xy"));
	assert!(glob_match(b"log.?", b"log.1"));
	assert!(glob_match(b"[abc]at", b"cat"));
	assert!(!glob_match(b"[abc]at", b"hat"));
	assert!(glob_match(b"[a-z]9", b"q9"));
	assert!(!glob_match(b"[a-z]9", b"Q9"));
	assert!(glob_match(b"[!a-z]9", b"Q9"));
	assert!(!glob_match(b"[!a-z]9", b"q9"));
	assert!(glob_match(b"[]]", b"]"));
	assert!(glob_match(b"[a-]", b"-"));
	// A malformed class matches NOTHING rather than being read as a literal bracket.
	assert!(!glob_match(b"[abc", b"[abc"));
	assert!(!glob_match(b"[]", b"[]"));
	assert!(!glob_match(b"[!]", b"x"));
	// A reversed range inside a class matches nothing, like a reversed range outside one.
	assert!(!glob_match(b"[z-a]", b"m"));
}

#[test]
fn a_pattern_of_asterisks_costs_comparisons_rather_than_stack() {
	// The recursive form of the matcher overflows here. Forty asterisks against a name that ends
	// in the wrong byte is the textbook input, and a tool is handed patterns by whoever types one.
	let pattern: alloc::vec::Vec<u8> = core::iter::repeat_n(b'*', 40).chain(b"end".iter().copied()).collect();
	let name: alloc::vec::Vec<u8> = core::iter::repeat_n(b'a', 4096).collect();
	assert!(!glob_match(&pattern, &name));
	let matching: alloc::vec::Vec<u8> = name.iter().copied().chain(b"end".iter().copied()).collect();
	assert!(glob_match(&pattern, &matching));
}

#[test]
fn arguments_are_classified_without_being_interpreted() {
	assert_eq!(classify(b"--json"), Arg::Long(b"json", None));
	assert_eq!(classify(b"--count=12"), Arg::Long(b"count", Some(b"12")));
	assert_eq!(classify(b"--count="), Arg::Long(b"count", Some(b"")));
	assert_eq!(classify(b"-n"), Arg::Short(b'n'));
	assert_eq!(classify(b"-abc"), Arg::Short(b'a'));
	assert_eq!(short_cluster(b"-abc"), b"abc");
	assert_eq!(short_cluster(b"--long"), b"");
	assert_eq!(classify(b"--"), Arg::Separator);
	// A bare `-` is conventionally standard input, so it is a VALUE rather than an option with no
	// letter - a tool that read it as an option would refuse the pipeline form.
	assert_eq!(classify(b"-"), Arg::Value(b"-"));
	assert_eq!(classify(b"vol://system/x"), Arg::Value(b"vol://system/x"));
}

#[test]
fn lines_are_split_across_chunk_boundaries_and_bounded_individually() {
	let mut lines = Lines::new(SliceSource::new(b"one\ntwo\nthree", 2), 64);
	assert_eq!(lines.next_line(), LineOutcome::Line);
	assert_eq!(lines.line(), b"one");
	assert_eq!(lines.next_line(), LineOutcome::Line);
	assert_eq!(lines.line(), b"two");
	// A trailing line without a newline is still a line.
	assert_eq!(lines.next_line(), LineOutcome::Line);
	assert_eq!(lines.line(), b"three");
	assert_eq!(lines.next_line(), LineOutcome::End);

	// An empty stream is zero lines, not one empty one.
	let mut empty = Lines::new(SliceSource::new(b"", 4), 64);
	assert_eq!(empty.next_line(), LineOutcome::End);

	// Empty lines are lines.
	let mut blanks = Lines::new(SliceSource::new(b"\n\n", 1), 64);
	assert_eq!(blanks.next_line(), LineOutcome::Line);
	assert_eq!(blanks.line(), b"");
	assert_eq!(blanks.next_line(), LineOutcome::Line);
	assert_eq!(blanks.next_line(), LineOutcome::End);

	// ONE LINE IS BOUNDED. A file with no newline in it is otherwise a way to grow a tool's memory
	// by handing it a file, and the answer is an error rather than a silent split.
	let long: alloc::vec::Vec<u8> = core::iter::repeat_n(b'x', 100).collect();
	let mut bounded = Lines::new(SliceSource::new(&long, 8), 16);
	assert_eq!(bounded.next_line(), LineOutcome::TooLong);
}

#[test]
fn the_last_lines_ring_holds_its_window_rather_than_the_input() {
	let mut ring = LastLines::new(3);
	for n in 0..10u8 {
		assert!(ring.push(&[b'0' + n]));
	}
	let held: alloc::vec::Vec<alloc::vec::Vec<u8>> = ring.lines().map(|line| line.to_vec()).collect();
	assert_eq!(held, alloc::vec![b"7".to_vec(), b"8".to_vec(), b"9".to_vec()], "the last three, oldest first");

	// Fewer lines than the window is every line, in order.
	let mut short = LastLines::new(5);
	assert!(short.push(b"a"));
	assert!(short.push(b"b"));
	let held: alloc::vec::Vec<alloc::vec::Vec<u8>> = short.lines().map(|line| line.to_vec()).collect();
	assert_eq!(held, alloc::vec![b"a".to_vec(), b"b".to_vec()]);

	// A window of nothing holds nothing and says so by succeeding.
	let mut none = LastLines::new(0);
	assert!(none.push(b"ignored"));
	assert_eq!(none.lines().count(), 0);
}

#[test]
fn the_line_buffer_holds_bytes_flat_and_sorts_an_index() {
	let mut buffer = LineBuffer::new(4, 64);
	for line in [&b"pear"[..], b"apple", b"fig"] {
		assert_eq!(buffer.push(line), Ok(()));
	}
	assert_eq!(buffer.len(), 3);
	assert_eq!(buffer.line(1), b"apple");
	buffer.sort_by(|a, b| a.cmp(b));
	let held: alloc::vec::Vec<alloc::vec::Vec<u8>> = buffer.lines().map(|line| line.to_vec()).collect();
	assert_eq!(held, alloc::vec![b"apple".to_vec(), b"fig".to_vec(), b"pear".to_vec()]);

	// STABLE: equal keys keep the order they arrived in, which is what makes sorting by a second
	// key meaningful rather than arbitrary.
	let mut stable = LineBuffer::new(8, 64);
	for line in [&b"b 1"[..], b"a 2", b"b 3", b"a 4"] {
		assert_eq!(stable.push(line), Ok(()));
	}
	stable.sort_by(|a, b| a[..1].cmp(&b[..1]));
	let held: alloc::vec::Vec<alloc::vec::Vec<u8>> = stable.lines().map(|line| line.to_vec()).collect();
	assert_eq!(held, alloc::vec![b"a 2".to_vec(), b"a 4".to_vec(), b"b 1".to_vec(), b"b 3".to_vec()]);

	// BOTH CEILINGS REFUSE rather than grow, and say which one was reached.
	let mut counted = LineBuffer::new(2, 64);
	assert_eq!(counted.push(b"one"), Ok(()));
	assert_eq!(counted.push(b"two"), Ok(()));
	assert_eq!(counted.push(b"three"), Err(HoldError::Full));
	let mut sized = LineBuffer::new(8, 4);
	assert_eq!(sized.push(b"abcd"), Ok(()));
	assert_eq!(sized.push(b"e"), Err(HoldError::Full));
}
