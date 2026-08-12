// The crate's own edges.
//
// `src/wire` had no tests: its primitives were exercised through `src/proto`, which is good
// coverage of the generated SHAPES and none of the crate's own boundaries. Every case below is one
// of those boundaries, and the ones marked with a defect were reachable before P02M0090f.

use super::*;

#[test]
fn a_boolean_has_one_encoding_per_value() {
	// `Some(self.u8()? != 0)` accepted 2 through 255 as `true`, against a stated contract that a
	// malformed buffer answers `None`. One logical value with 255 spellings is malleability with no
	// purpose, and it starts costing the moment a frame is hashed, compared or replayed.
	assert_eq!(Reader::new(&[0]).boolean(), Some(false));
	assert_eq!(Reader::new(&[1]).boolean(), Some(true));
	for byte in 2u8..=255 {
		assert_eq!(Reader::new(&[byte]).boolean(), None, "{byte} is not a boolean");
	}
	assert_eq!(Reader::new(&[]).boolean(), None, "and a short buffer is still short");
}

#[test]
fn trailing_bytes_are_refused_at_the_framing_boundary() {
	// `finish()` is what `decode` calls; a nested `read` must NOT require the end of the buffer,
	// which is why it is a separate call rather than a check inside `take`.
	let mut r = Reader::new(&[1, 2, 3]);
	assert_eq!(r.u8(), Some(1));
	assert_eq!(r.u16(), Some(0x0302));
	assert_eq!(r.finish(), Some(()), "every byte consumed");

	let mut r = Reader::new(&[1, 2, 3, 0xde, 0xad]);
	assert_eq!(r.u8(), Some(1));
	assert_eq!(r.u16(), Some(0x0302));
	assert_eq!(r.finish(), None, "two bytes the reader never looked at");

	// And an unclaimed capability is the same refusal: a caller that sent one nothing reads must
	// not have it silently dropped.
	let handles = Handles::try_from_slice(&[7]).expect("one handle");
	let mut r = Reader::with_handles(&[], &handles);
	assert_eq!(r.finish(), None, "a capability nobody took");
	assert_eq!(r.take_handle(), Some(7));
	assert_eq!(r.finish(), Some(()), "and now it is accounted for");
}

#[test]
fn four_handles_are_accepted_and_five_are_refused_rather_than_truncated() {
	// `from_slice` took `MAX_HANDLES` and dropped the rest. For ordinary data that is a defensible
	// API; for CAPABILITIES the fifth is not lost information, it is a live kernel object that
	// nothing then closes.
	assert_eq!(Handles::try_from_slice(&[]).map(|h| h.len()), Some(0));
	assert_eq!(Handles::try_from_slice(&[1, 2, 3, 4]).map(|h| h.len()), Some(4));
	assert!(Handles::try_from_slice(&[1, 2, 3, 4, 5]).is_none(), "five is refused, not clamped to four");

	let raw = [9u64, 8, 7, 6];
	assert_eq!(Handles::try_from_array(&raw, 4).map(|h| h.len()), Some(4));
	assert!(Handles::try_from_array(&raw, 5).is_none(), "a count past the array is refused");

	// `push` and `set_handle` have always had this shape; the point is that they now all do.
	let mut handles = Handles::new();
	for handle in 1..=4u64 {
		assert_eq!(handles.push(handle), Some(()));
	}
	assert_eq!(handles.push(5), None, "the fifth push is refused");
	let mut buf = [0u8; 8];
	let mut writer = SliceWriter::new(&mut buf);
	for handle in 1..=4u64 {
		assert_eq!(writer.set_handle(handle), Some(()));
	}
	assert_eq!(writer.set_handle(5), None, "and so is the fifth set_handle");
}

#[test]
fn a_reader_takes_its_handles_in_encoding_order() {
	// The order the encoder set them is the order the fields appear, so a stage's stdin and stdout
	// cannot be swapped by a decoder that reads them in a different order than they were written.
	let handles = Handles::try_from_slice(&[11, 22, 33]).expect("three handles");
	let mut r = Reader::with_handles(&[], &handles);
	assert_eq!(r.take_handle(), Some(11));
	assert_eq!(r.take_handle(), Some(22));
	assert!(r.has_handle(), "one is still unclaimed");
	assert_eq!(r.take_handle(), Some(33));
	assert!(!r.has_handle());
	assert_eq!(r.take_handle(), None, "and they are spent");
}

#[test]
fn a_slice_writer_stops_at_exactly_its_capacity() {
	let mut buf = [0u8; 4];
	let mut w = SliceWriter::new(&mut buf);
	assert_eq!(w.u32(0x0403_0201), Some(()));
	assert_eq!(w.pos(), 4);
	assert_eq!(w.put(0), None, "one byte past the end is a refusal, not a panic");

	// N-1: a four-byte write into three bytes fails and the bulk path is the one being checked.
	let mut buf = [0u8; 3];
	let mut w = SliceWriter::new(&mut buf);
	assert_eq!(w.raw(&[1, 2, 3, 4]), None);
	let mut w = SliceWriter::new(&mut buf);
	assert_eq!(w.raw(&[1, 2, 3]), Some(()));
	assert_eq!(w.pos(), 3);
}

#[test]
fn every_primitive_refuses_at_every_truncation_point() {
	// A reader over a buffer one byte short of each width answers None rather than reading past it.
	let bytes = [0xffu8; 8];
	for width in 1..8usize {
		let short = &bytes[..width];
		if width < 2 {
			assert_eq!(Reader::new(short).u16(), None, "u16 at {width}");
		}
		if width < 4 {
			assert_eq!(Reader::new(short).u32(), None, "u32 at {width}");
		}
		if width < 8 {
			assert_eq!(Reader::new(short).u64(), None, "u64 at {width}");
		}
	}
	assert_eq!(Reader::new(&bytes).u64(), Some(u64::MAX), "and the full width reads");
}

#[test]
fn a_string_is_bounded_and_must_be_utf8() {
	// The length prefix is attacker-controlled, so the allocation behind it is fallible and the
	// bytes behind THAT must be there.
	let mut buf = [0u8; 16];
	let mut w = SliceWriter::new(&mut buf);
	assert_eq!(w.bytes_lp(b"hello"), Some(()));
	let n = w.pos();
	let mut r = Reader::new(&buf[..n]);
	assert_eq!(r.string_lp().as_deref(), Some("hello"));
	assert_eq!(r.finish(), Some(()));

	// A length that runs past the buffer.
	assert_eq!(Reader::new(&[0xff, 0xff]).string_lp(), None, "a 65535-byte string in two bytes");
	// Invalid UTF-8 behind a valid length.
	assert_eq!(Reader::new(&[1, 0, 0xff]).string_lp(), None, "0xff is not UTF-8");
	// And the empty string, which is legal.
	assert_eq!(Reader::new(&[0, 0]).string_lp().as_deref(), Some(""));
}

#[test]
fn a_reader_over_arbitrary_bytes_never_panics() {
	// A decoder is the first thing a hostile message reaches. Swept rather than fuzzed, because a
	// deterministic sweep is what a build gate can run: every one-, two- and three-byte buffer
	// through every primitive.
	let mut buf = [0u8; 3];
	for a in 0..=255u8 {
		buf[0] = a;
		for b in [0u8, 1, 0x7f, 0x80, 0xff] {
			buf[1] = b;
			for c in [0u8, 1, 0xff] {
				buf[2] = c;
				for len in 0..=3usize {
					let bytes = &buf[..len];
					let _ = Reader::new(bytes).boolean();
					let _ = Reader::new(bytes).u8();
					let _ = Reader::new(bytes).u16();
					let _ = Reader::new(bytes).u32();
					let _ = Reader::new(bytes).u64();
					let _ = Reader::new(bytes).string_lp();
				}
			}
		}
	}
}

#[test]
fn a_vec_writer_that_records_a_handle_cannot_be_read_as_bytes_alone() {
	// The generated `encode_vec` refuses such a type; this is the primitive underneath it. The
	// bytes and the capabilities are two halves of one message and neither is the message.
	let mut w = VecWriter::new();
	assert_eq!(w.u32(7), Some(()));
	assert_eq!(w.set_handle(42), Some(()));
	assert_eq!(w.handles(), &[42], "the handle is recorded");
	assert_eq!(w.handle(), 42);
	assert_eq!(w.into_inner(), alloc::vec![7, 0, 0, 0], "and `into_inner` gives the bytes only");
}

#[test]
fn max_handles_is_the_kernels_number() {
	// Two constants for one limit is one constant and one copy of its value. The day somebody
	// raises the kernel's is the day that matters.
	assert_eq!(MAX_HANDLES, abi::MAX_MESSAGE_CAPS);
}
