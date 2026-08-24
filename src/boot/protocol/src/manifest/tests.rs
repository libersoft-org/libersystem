use super::*;

// A manifest a test can vary one field of. Everything here is the smallest thing that is still a
// manifest: two rows, because one row cannot be out of order.
fn header() -> Header<'static> {
	Header { key_id: 7, product: b"LiberSystem", arch: ARCH_X86_64, source_kind: SOURCE_SYSTEM_VOLUME, release: b"0.0.1", volume_uuid: [0x11; 16] }
}

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
	assert_eq!(m.volume_uuid, [0x11; 16]);
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
