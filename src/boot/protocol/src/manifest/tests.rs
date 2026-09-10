use super::*;

// A manifest a test can vary one field of. Everything here is the smallest thing that is still a
// manifest: two rows, because one row cannot be out of order.
fn header() -> Header<'static> {
	Header { key_id: 7, product: b"LiberSystem", arch: ARCH_X86_64, source_kind: SOURCE_SYSTEM_VOLUME, release: b"0.0.1", security_generation: 9, purpose: PURPOSE_BOOT, volume_uuid: [0x11; 16], dma_mode: None }
}

// Where the version-3 header's fields sit for the fixture above, counted rather than searched for -
// a test that found a field by scanning for its value would not notice the field moving.
const GENERATION_AT: usize = MAGIC.len() + 2 + 4 + 1 + b"LiberSystem".len() + 1 + 1 + 1 + b"0.0.1".len();
const PURPOSE_AT: usize = GENERATION_AT + 8;
const UUID_AT: usize = PURPOSE_AT + 4;
const DMA_TAG_AT: usize = UUID_AT + 16;

fn rows() -> [Row<'static>; 2] {
	[
		Row { kind: KIND_KERNEL, path: b"boot/kernel", length: 4096, digest: [0xaa; 32] },
		Row { kind: KIND_PROGRAM, path: b"bin/init", length: 128, digest: [0xbb; 32] },
	]
}

// Encode, append a signature, and answer the whole record.
fn manifest(out: &mut [u8]) -> usize {
	let mut r = rows();
	let len = encode_payload(&header(), &mut r, out).expect("the manifest encodes");
	out[len..len + 64].copy_from_slice(&[0x5a; 64]);
	len + 64
}

#[test]
fn a_manifest_reads_back_what_was_written() {
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	let m = Manifest::decode(&buf[..n]).expect("it decodes");
	assert_eq!(m.alg, ALG_ED25519);
	assert_eq!(m.key_id, 7);
	assert_eq!(m.product, b"LiberSystem");
	assert_eq!(m.arch, ARCH_X86_64);
	assert_eq!(m.source_kind, SOURCE_SYSTEM_VOLUME);
	assert_eq!(m.release, b"0.0.1");
	assert_eq!(m.security_generation, 9);
	assert_eq!(m.purpose, PURPOSE_BOOT);
	assert_eq!(m.volume_uuid, [0x11; 16]);
	assert_eq!(m.dma_mode, None, "the fixture is a tag-0 manifest");
	assert_eq!(m.row_count(), 2);
	assert_eq!(m.signature(), [0x5a; 64]);
	// THE PAYLOAD IS EVERYTHING BUT THE SIGNATURE, exactly. A caller has no way to ask about less.
	assert_eq!(m.payload().len(), n - 64);
	assert_eq!(m.payload(), &buf[..n - 64]);
}

#[test]
fn the_rows_are_the_ones_that_were_written_in_canonical_order() {
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	let m = Manifest::decode(&buf[..n]).expect("it decodes");
	// Written kernel-then-program, and kernel is kind 1: the encoder sorts, so the order is the
	// format's rather than the caller's.
	let first = m.row(0).expect("a first row");
	assert_eq!(first.kind, KIND_KERNEL);
	assert_eq!(first.path, b"boot/kernel");
	assert_eq!(first.length, 4096);
	assert_eq!(first.digest, [0xaa; 32]);
	let second = m.row(1).expect("a second row");
	assert_eq!(second.kind, KIND_PROGRAM);
	assert_eq!(second.path, b"bin/init");
	assert!(m.row(2).is_none(), "and no third");
	// And a row is found by what names it rather than by where it sits.
	assert_eq!(m.find(KIND_PROGRAM, b"bin/init").expect("found").length, 128);
	assert!(m.find(KIND_KERNEL, b"bin/init").is_none(), "the kind is part of the name");
	assert!(m.find(KIND_PROGRAM, b"bin/other").is_none());
}

#[test]
fn anything_that_is_not_this_format_is_refused_before_it_is_read() {
	assert_eq!(Manifest::decode(b"").unwrap_err(), Refusal::NotAManifest);
	assert_eq!(Manifest::decode(b"liberboot-manifest 1\n").unwrap_err(), Refusal::NotAManifest, "the text format is not this one");
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	buf[3] ^= 1;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::NotAManifest, "one bit of the magic");
	// A record longer than this reader will look at is refused BEFORE anything is indexed.
	let huge = [0u8; MAX_MANIFEST_BYTES + 1];
	assert_eq!(Manifest::decode(&huge).unwrap_err(), Refusal::TooLarge);
}

#[test]
fn a_record_that_stops_early_is_truncated_rather_than_read_past() {
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	// Every prefix short of the whole thing, so no field's read is the one that happens to be safe.
	for cut in 0..n {
		let verdict = Manifest::decode(&buf[..cut]);
		assert!(verdict.is_err(), "a manifest cut at {cut} is not a manifest");
	}
	assert!(Manifest::decode(&buf[..n]).is_ok(), "and the whole one is");
}

#[test]
fn a_byte_after_the_signature_is_a_different_record() {
	// A manifest is exactly as long as it says it is. Trailing bytes are where a second, unsigned
	// record would live.
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	buf[n] = 0;
	assert_eq!(Manifest::decode(&buf[..n + 1]).unwrap_err(), Refusal::TrailingBytes);
}

#[test]
fn an_enum_value_this_version_does_not_define_is_refused() {
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	// The algorithm, at a fixed offset after the magic.
	let alg_at = MAGIC.len();
	buf[alg_at] = 2;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::UnknownValue, "an algorithm nothing here implements");
	buf[alg_at] = 1;
	// The architecture, after the product name.
	let arch_at = MAGIC.len() + 2 + 4 + 1 + b"LiberSystem".len();
	assert_eq!(buf[arch_at], ARCH_X86_64, "the fixture's architecture is where it is expected");
	buf[arch_at] = 9;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::UnknownValue, "an architecture this version does not name");
	buf[arch_at] = ARCH_X86_64;
	buf[arch_at + 1] = 9;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::UnknownValue, "a source kind this version does not name");
}

#[test]
fn rows_out_of_order_or_repeated_are_not_a_canonical_manifest() {
	// THE POINT OF CANONICAL ORDER: a manifest whose rows can be reordered has more than one byte
	// encoding, and a signature over one says nothing about the other. So the reader refuses any
	// encoding but the one the writer produces.
	let mut buf = [0u8; 1024];
	let mut r = [Row { kind: KIND_PROGRAM, path: b"bin/b", length: 1, digest: [1; 32] }, Row { kind: KIND_PROGRAM, path: b"bin/a", length: 2, digest: [2; 32] }];
	let len = encode_payload(&header(), &mut r, &mut buf).expect("encodes");
	buf[len..len + 64].copy_from_slice(&[0; 64]);
	let m = Manifest::decode(&buf[..len + 64]).expect("the writer produced the canonical order");
	assert_eq!(m.row(0).unwrap().path, b"bin/a", "sorted by the writer");

	// The same two rows, written by hand in the other order.
	let swapped = {
		let mut out = [0u8; 1024];
		let rows_at = len - (2 * (1 + 2 + 5 + 8 + 32));
		out[..rows_at].copy_from_slice(&buf[..rows_at]);
		let row = 1 + 2 + 5 + 8 + 32;
		out[rows_at..rows_at + row].copy_from_slice(&buf[rows_at + row..rows_at + 2 * row]);
		out[rows_at + row..rows_at + 2 * row].copy_from_slice(&buf[rows_at..rows_at + row]);
		out
	};
	assert_eq!(Manifest::decode(&swapped[..len + 64]).unwrap_err(), Refusal::NotCanonical);

	// And a repeat is refused by the writer, which is where it can still be fixed.
	let mut repeated = [Row { kind: KIND_PROGRAM, path: b"bin/a", length: 1, digest: [1; 32] }, Row { kind: KIND_PROGRAM, path: b"bin/a", length: 2, digest: [2; 32] }];
	assert_eq!(encode_payload(&header(), &mut repeated, &mut buf).unwrap_err(), Refusal::NotCanonical);
}

#[test]
fn a_path_that_names_something_outside_the_source_is_refused() {
	let mut buf = [0u8; 1024];
	for path in [b"/bin/init".as_slice(), b"../bin/init", b"bin/../init", b"bin//init", b"", b"bin/in\\\\it"] {
		let mut r = [Row { kind: KIND_PROGRAM, path, length: 1, digest: [0; 32] }];
		assert_eq!(encode_payload(&header(), &mut r, &mut buf).unwrap_err(), Refusal::InvalidPath, "{path:?} is not a path a manifest may name");
	}
	let long = [b'a'; MAX_PATH_BYTES + 1];
	let mut r = [Row { kind: KIND_PROGRAM, path: &long, length: 1, digest: [0; 32] }];
	assert_eq!(encode_payload(&header(), &mut r, &mut buf).unwrap_err(), Refusal::InvalidPath);
}

#[test]
fn a_writer_refuses_what_a_reader_would_have_to() {
	let mut buf = [0u8; 1024];
	let mut r = rows();
	// An architecture, a source kind and a row kind this version does not define.
	let mut bad = header();
	bad.arch = 9;
	assert_eq!(encode_payload(&bad, &mut r, &mut buf).unwrap_err(), Refusal::UnknownValue);
	let mut bad = header();
	bad.source_kind = 9;
	assert_eq!(encode_payload(&bad, &mut r, &mut buf).unwrap_err(), Refusal::UnknownValue);
	let mut odd = [Row { kind: 9, path: b"bin/init", length: 1, digest: [0; 32] }];
	assert_eq!(encode_payload(&header(), &mut odd, &mut buf).unwrap_err(), Refusal::UnknownValue);
	// A product or release name that is empty or too long.
	let mut bad = header();
	bad.product = b"";
	assert_eq!(encode_payload(&bad, &mut r, &mut buf).unwrap_err(), Refusal::InvalidPath);
	let long = [b'x'; MAX_NAME_BYTES + 1];
	let mut bad = header();
	bad.release = &long;
	assert_eq!(encode_payload(&bad, &mut r, &mut buf).unwrap_err(), Refusal::InvalidPath);
}

#[test]
fn a_buffer_too_short_produces_an_error_rather_than_a_shorter_manifest() {
	// The failure that would otherwise be silent: a writer that truncates makes a manifest missing
	// its last row, which reads perfectly and covers less than it says.
	let mut r = rows();
	let full = {
		let mut buf = [0u8; 1024];
		encode_payload(&header(), &mut r, &mut buf).expect("encodes")
	};
	for size in 0..full {
		let mut small = [0u8; 1024];
		let mut r = rows();
		assert_eq!(encode_payload(&header(), &mut r, &mut small[..size]).unwrap_err(), Refusal::Truncated, "a buffer of {size} bytes");
	}
}

// ------------------------------------------------------------ the version-3 header, byte by byte

#[test]
fn the_version_three_header_has_its_fields_at_fixed_offsets_and_a_one_byte_tag_zero_record() {
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	assert_eq!(&buf[..8], b"LBRMAN\x03\x00", "the magic carries the version");
	assert_eq!(u64::from_le_bytes(buf[GENERATION_AT..GENERATION_AT + 8].try_into().unwrap()), 9, "the generation sits after the release string");
	assert_eq!(u32::from_le_bytes(buf[PURPOSE_AT..PURPOSE_AT + 4].try_into().unwrap()), PURPOSE_BOOT, "then the purpose");
	assert_eq!(&buf[UUID_AT..UUID_AT + 16], &[0x11; 16], "then the uuid");
	// THE DMA RECORD IS EXACTLY ONE BYTE FOR TAG 0, and the row count follows it immediately.
	assert_eq!(buf[DMA_TAG_AT], DMA_MODE_ABSENT);
	assert_eq!(u16::from_le_bytes([buf[DMA_TAG_AT + 1], buf[DMA_TAG_AT + 2]]), 2, "the row count is one byte after a tag-0 record");
	let m = Manifest::decode(&buf[..n]).expect("decodes");
	assert_eq!(m.dma_mode, None);
	assert_eq!(m.row(0).expect("a row").path, b"boot/kernel", "and the rows are found where the tag put them");
}

#[test]
fn a_signed_mode_is_a_five_byte_record_and_round_trips_both_modes() {
	for mode in [crate::dma_mode::MODE_ENFORCING_REQUIRED, crate::dma_mode::MODE_NO_IOMMU] {
		let mut buf = [0u8; 1024];
		let mut r = rows();
		let mut h = header();
		h.dma_mode = Some(mode);
		let len = encode_payload(&h, &mut r, &mut buf).expect("encodes");
		buf[len..len + 64].copy_from_slice(&[0x5a; 64]);
		// THE RECORD IS EXACTLY FIVE BYTES FOR TAG 1: the tag, then the little-endian mode, then
		// the row count - four bytes further along than the tag-0 fixture puts it.
		assert_eq!(buf[DMA_TAG_AT], DMA_MODE_PRESENT);
		assert_eq!(u32::from_le_bytes(buf[DMA_TAG_AT + 1..DMA_TAG_AT + 5].try_into().unwrap()), mode);
		assert_eq!(u16::from_le_bytes([buf[DMA_TAG_AT + 5], buf[DMA_TAG_AT + 6]]), 2, "the row count is five bytes after a tag-1 record");
		let m = Manifest::decode(&buf[..len + 64]).expect("decodes");
		assert_eq!(m.dma_mode, Some(mode));
		assert_eq!(m.row(1).expect("a row").path, b"bin/init");
		assert_eq!(m.payload().len(), len, "the record is inside what the signature covers");
	}
}

#[test]
fn a_mode_or_a_tag_this_version_does_not_define_is_refused_by_writer_and_reader() {
	let mut buf = [0u8; 1024];
	for mode in [0u32, 3, 0xffff_ffff] {
		let mut r = rows();
		let mut h = header();
		h.dma_mode = Some(mode);
		assert_eq!(encode_payload(&h, &mut r, &mut buf).unwrap_err(), Refusal::UnknownValue, "mode {mode} is not one this version defines");
	}
	// Written by hand: a valid tag-1 record whose mode byte is then edited.
	let mut r = rows();
	let mut h = header();
	h.dma_mode = Some(crate::dma_mode::MODE_NO_IOMMU);
	let len = encode_payload(&h, &mut r, &mut buf).expect("encodes");
	buf[len..len + 64].copy_from_slice(&[0; 64]);
	for mode in [0u8, 3, 0xff] {
		let mut edited = buf;
		edited[DMA_TAG_AT + 1] = mode;
		assert_eq!(Manifest::decode(&edited[..len + 64]).unwrap_err(), Refusal::UnknownValue, "mode {mode}");
	}
	// And the tag itself: 2 is not a tag, whatever follows it.
	let mut edited = buf;
	edited[DMA_TAG_AT] = 2;
	assert_eq!(Manifest::decode(&edited[..len + 64]).unwrap_err(), Refusal::UnknownValue, "a tag this version does not define");
}

#[test]
fn a_dma_record_cut_short_is_truncated_rather_than_read_as_absent() {
	let mut buf = [0u8; 1024];
	let mut r = rows();
	let mut h = header();
	h.dma_mode = Some(crate::dma_mode::MODE_ENFORCING_REQUIRED);
	let len = encode_payload(&h, &mut r, &mut buf).expect("encodes");
	assert!(len > DMA_TAG_AT + 5, "the record sits inside the payload");
	// The record ends inside its own mode bytes: nothing after the tag is a manifest.
	for cut in DMA_TAG_AT + 1..DMA_TAG_AT + 5 {
		assert!(Manifest::decode(&buf[..cut]).is_err(), "a tag-1 record cut at {cut} is not a manifest");
	}
}

#[test]
fn surplus_bytes_after_a_tag_zero_record_break_the_row_grammar_rather_than_becoming_a_mode() {
	// A tag-0 record followed by four bytes that LOOK like a mode. The reader takes the tag at its
	// word - one byte - and reads those four as the start of the rows, which no longer parse. What
	// must not happen is those bytes being read as a mode.
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	let mut edited = [0u8; 1024];
	edited[..DMA_TAG_AT + 1].copy_from_slice(&buf[..DMA_TAG_AT + 1]);
	edited[DMA_TAG_AT + 1..DMA_TAG_AT + 5].copy_from_slice(&crate::dma_mode::MODE_NO_IOMMU.to_le_bytes());
	edited[DMA_TAG_AT + 5..n + 4].copy_from_slice(&buf[DMA_TAG_AT + 1..n]);
	let verdict = Manifest::decode(&edited[..n + 4]);
	assert!(verdict.is_err(), "the surplus bytes are a malformed row boundary, refused");
	if let Ok(m) = verdict {
		assert_ne!(m.dma_mode, Some(crate::dma_mode::MODE_NO_IOMMU));
	}
}

#[test]
fn a_legacy_version_is_refused_by_version_and_never_read_as_tag_zero() {
	// A version-2 record: the same bytes with the version byte turned back. It carries no tag, so
	// the reader must refuse it outright rather than parse its uuid's neighbour as one.
	let mut buf = [0u8; 1024];
	let n = manifest(&mut buf);
	buf[6] = 2;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::LegacyVersion);
	buf[6] = 1;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::LegacyVersion, "any other version of this magic is a legacy version");
	buf[6] = 4;
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::LegacyVersion, "and so is a later one this reader does not read");
	// A different product's magic is still not a manifest at all.
	buf[6] = 3;
	buf[0] = b'X';
	assert_eq!(Manifest::decode(&buf[..n]).unwrap_err(), Refusal::NotAManifest);
	// And the signature domain moved with the layout, so a version-2 signature can never be a
	// version-3 one even over identical bytes.
	assert_eq!(DOMAIN, b"libersystem-boot-manifest-v3\0");
}

#[test]
fn a_purpose_this_version_does_not_define_is_refused_by_writer_and_reader() {
	let mut buf = [0u8; 1024];
	let mut r = rows();
	let mut h = header();
	h.purpose = 3;
	assert_eq!(encode_payload(&h, &mut r, &mut buf).unwrap_err(), Refusal::UnknownValue);
	h.purpose = 0;
	assert_eq!(encode_payload(&h, &mut r, &mut buf).unwrap_err(), Refusal::UnknownValue);
	h.purpose = PURPOSE_RECOVERY;
	let len = encode_payload(&h, &mut r, &mut buf).expect("recovery is a purpose");
	buf[len..len + 64].copy_from_slice(&[0; 64]);
	assert_eq!(Manifest::decode(&buf[..len + 64]).expect("decodes").purpose, PURPOSE_RECOVERY);
	buf[PURPOSE_AT] = 3;
	assert_eq!(Manifest::decode(&buf[..len + 64]).unwrap_err(), Refusal::UnknownValue);
}

#[test]
fn the_generation_is_a_little_endian_u64_and_every_value_round_trips() {
	for generation in [0u64, 1, 0x1234_5678_9abc_def0, u64::MAX] {
		let mut buf = [0u8; 1024];
		let mut r = rows();
		let mut h = header();
		h.security_generation = generation;
		let len = encode_payload(&h, &mut r, &mut buf).expect("encodes");
		buf[len..len + 64].copy_from_slice(&[0; 64]);
		assert_eq!(&buf[GENERATION_AT..GENERATION_AT + 8], &generation.to_le_bytes());
		assert_eq!(Manifest::decode(&buf[..len + 64]).expect("decodes").security_generation, generation);
	}
}
