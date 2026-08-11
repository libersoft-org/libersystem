use super::log::{LogRecord, Severity, encode, render_cbor, render_json, render_text};
use super::*;

#[test]
fn executable_alias_collision_is_exactly_one_suffix_level() {
	assert!(executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe"));
	assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe.lsexe"));
	assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"drivers/ping.lsexe.lsexe"));
}

#[test]
fn log_record_roundtrip_and_renders() {
	let fields: [(&[u8], &[u8]); 2] = [(b"event", b"online"), (b"files", b"2")];
	let mut wire: [u8; 128] = [0u8; 128];
	let n: usize = encode(42, Severity::Info, b"storage_service", &fields, &mut wire).expect("encode fits");
	let rec: LogRecord<'_> = LogRecord::parse(&wire[..n]).expect("parse round-trips");
	assert_eq!(rec.ts(), 42);
	assert_eq!(rec.severity(), Severity::Info);
	assert_eq!(rec.source(), b"storage_service");
	assert_eq!(rec.field_count(), 2);
	let mut fields = rec.fields();
	assert_eq!(fields.next(), Some((&b"event"[..], &b"online"[..])));
	assert_eq!(fields.next(), Some((&b"files"[..], &b"2"[..])));
	assert_eq!(fields.next(), None);

	let mut text: [u8; 128] = [0u8; 128];
	let text_len: usize = render_text(&rec, &mut text).expect("text fits");
	assert_eq!(&text[..text_len], b"[42] INFO storage_service: event=online files=2");

	let mut json: [u8; 256] = [0u8; 256];
	let json_len: usize = render_json(&rec, &mut json).expect("json fits");
	// A LIST of fields, which is what the wire format and `log.lsidl` both say and what the
	// generated codec has always rendered. It was an object here, so two fields with the same key
	// became whatever the consumer does with a duplicate.
	assert_eq!(&json[..json_len], br#"{"ts":42,"severity":"INFO","source":"storage_service","fields":[{"key":"event","value":"online"},{"key":"files","value":"2"}]}"#);

	let mut cbor: [u8; 128] = [0u8; 128];
	let cbor_len: usize = render_cbor(&rec, &mut cbor).expect("cbor fits");
	assert_eq!(cbor[0], 0xa4, "CBOR record is a 4-entry map");
	assert!(cbor[..cbor_len].windows(b"storage_service".len()).any(|window: &[u8]| window == b"storage_service"));
}

// THE ABI'S OWN INVARIANTS.
//
// `repr(C)` fixes a layout for one compiler run; it does not stop somebody inserting a syscall
// number in the middle of the table, widening a field, or reordering a struct - and because the
// kernel and userspace are built from this same crate, every one of those is green everywhere. A
// test that compares two halves of one build cannot see it. These assertions fail when a VALUE
// changes, which is the only thing that can.
//
// Nothing here is versioned before the first release, so this is not a compatibility promise. It is
// the machinery that will make one possible, and in the meantime it is what turns "somebody
// reordered a struct" from a silent success into a failing test.

// Every syscall number, snapshotted. A new call appends; an existing one never moves.
const SYSCALLS: &[(u64, u64)] = &[
	(SYS_DEBUG_NOOP, 0),
	(SYS_CLOCK_GET, 1),
	(SYS_DEBUG_WRITE, 2),
	(SYS_MEMORY_OBJECT_CREATE, 3),
	(SYS_MEMORY_MAP, 4),
	(SYS_MEMORY_UNMAP, 5),
	(SYS_HANDLE_DUPLICATE, 6),
	(SYS_HANDLE_CLOSE, 7),
	(SYS_CHANNEL_CREATE, 8),
	(SYS_CHANNEL_SEND, 9),
	(SYS_CHANNEL_RECV, 10),
	(SYS_EVENT_CREATE, 11),
	(SYS_EVENT_SIGNAL, 12),
	(SYS_EVENT_POLL, 13),
	(SYS_TIMER_CREATE, 14),
	(SYS_TIMER_SET, 15),
	(SYS_TIMER_POLL, 16),
	(SYS_USER_EXIT, 17),
	(SYS_FAULT_INFO_GET, 18),
	(SYS_DOMAIN_CREATE, 19),
	(SYS_DOMAIN_KILL, 20),
	(SYS_YIELD, 21),
	(SYS_OBJECT_INFO_GET, 22),
	(SYS_WAIT, 23),
	(SYS_DMA_BUFFER_CREATE, 24),
	(SYS_DEVICE_MEMORY_MAP, 25),
	(SYS_RANDOM_GET, 26),
	(SYS_INTERRUPT_BIND, 27),
	(SYS_OBJECT_PROPERTY_SET, 28),
	(SYS_PROCESS_CREATE, 29),
	(SYS_PROCESS_LOAD, 30),
	(SYS_THREAD_CREATE, 31),
	(SYS_THREAD_START, 32),
	(SYS_CONSOLE_ATTACH, 33),
	(SYS_DEVICE_COUNT, 34),
	(SYS_DEVICE_INFO, 35),
	(SYS_DEVICE_ACQUIRE, 36),
	(SYS_DMA_BUFFER_MAP, 37),
	(SYS_DMA_BUFFER_PHYS, 38),
	(SYS_INTERRUPT_ACK, 40),
	(SYS_CONSOLE_FEED, 41),
	(SYS_WAIT_ANY, 42),
	(SYS_CLOCK_RTC, 43),
	(SYS_FRAMEBUFFER_MAP, 44),
	(SYS_PROCESS_SIGNAL, 45),
	(SYS_DEVICE_MSIX_ACQUIRE, 46),
	(SYS_SYSTEM_POWER, 47),
	(SYS_CONSOLE_READLOG, 48),
	(SYS_CLOCK_MONO_NS, 49),
	(SYS_SIGNAL_CATCH, 50),
	(SYS_SIGNAL_TAKE, 51),
	(SYS_PROCESS_STATS_GET, 52),
	(SYS_DOMAIN_STATS_GET, 53),
	(SYS_CPU_INFO, 54),
	(SYS_MEMORY_STATS, 55),
	(SYS_MEMMAP_GET, 56),
	(SYS_IRQ_INFO, 57),
	(SYS_PCI_INFO, 58),
	(SYS_CHANNEL_PEEK, 59),
	(SYS_ABI_CHECK, 60),
	(SYS_CPU_NAME, 61),
	(SYS_DMA_BUFFER_UNMAP, 62),
	(SYS_PROCESS_LOAD_MODULE, 63),
	(SYS_BOOT_PROFILE, 64),
	(SYS_PROCESS_GROUP_CREATE, 65),
	(SYS_PROCESS_GROUP_SIGNAL, 66),
	(SYS_CHANNEL_SEND_CAPS, 67),
	(SYS_CHANNEL_RECV_CAPS, 68),
	(SYS_WAITSET_CREATE, 69),
	(SYS_WAITSET_ADD, 70),
	(SYS_WAITSET_REMOVE, 71),
	(SYS_WAITSET_WAIT, 72),
	(SYS_RANDOM_INSECURE, 73),
];

#[test]
fn the_syscall_numbers_are_what_they_were() {
	// 39 is deliberately absent: a retired call's number is not reused, because a stale binary
	// calling it must get "no such syscall" rather than somebody else's handler.
	let mut seen: [bool; 128] = [false; 128];
	for &(number, expected) in SYSCALLS {
		assert_eq!(number, expected, "a syscall number moved");
		let slot = number as usize;
		assert!(slot < seen.len(), "syscall {number} is past the table this test can check");
		assert!(!seen[slot], "two syscalls share number {number}");
		seen[slot] = true;
	}
	assert_eq!(SYSCALLS.len(), 73, "a syscall was added or removed without updating the snapshot");
	assert!(!seen[39], "39 is a retired number and must stay retired");
}

#[test]
fn every_right_is_one_bit_and_rights_all_is_their_union() {
	// `RIGHTS_ALL` was a hand-written 0xfff beside twelve individually defined bits. A thirteenth
	// right added without touching the literal is a right that `RIGHTS_ALL` silently does not grant.
	const RIGHTS: &[(u32, &str)] = &[
		(RIGHT_READ, "READ"),
		(RIGHT_WRITE, "WRITE"),
		(RIGHT_EXECUTE, "EXECUTE"),
		(RIGHT_MAP, "MAP"),
		(RIGHT_SEND, "SEND"),
		(RIGHT_RECEIVE, "RECEIVE"),
		(RIGHT_DUPLICATE, "DUPLICATE"),
		(RIGHT_TRANSFER, "TRANSFER"),
		(RIGHT_REVOKE, "REVOKE"),
		(RIGHT_GET_INFO, "GET_INFO"),
		(RIGHT_MANAGE, "MANAGE"),
		(RIGHT_WAIT, "WAIT"),
	];
	let mut union = 0u32;
	for &(bit, name) in RIGHTS {
		assert_eq!(bit.count_ones(), 1, "right {name} is not a single bit");
		assert_eq!(union & bit, 0, "right {name} collides with one already defined");
		union |= bit;
	}
	assert_eq!(union, RIGHTS_ALL, "RIGHTS_ALL is not the union of the rights that exist");
}

#[test]
fn the_error_codes_are_what_they_were() {
	// Negative, distinct, contiguous from -1, and never renumbered: a caller matching on -6 has to
	// keep meaning INVALID.
	const ERRORS: &[(i64, i64)] = &[
		(ERR_BAD_SYSCALL, -1),
		(ERR_NO_THREAD, -2),
		(ERR_NO_MEMORY, -3),
		(ERR_BAD_HANDLE, -4),
		(ERR_ACCESS_DENIED, -5),
		(ERR_INVALID, -6),
		(ERR_NOT_MAPPED, -7),
		(ERR_WOULD_BLOCK, -8),
		(ERR_PEER_CLOSED, -9),
		(ERR_RESOURCE_EXHAUSTED, -10),
		(ERR_TIMED_OUT, -11),
		(ERR_ABI_MISMATCH, -12),
		(ERR_UNSUPPORTED, -13),
	];
	for (index, &(code, expected)) in ERRORS.iter().enumerate() {
		assert_eq!(code, expected, "an error code moved");
		assert_eq!(code, -(index as i64 + 1), "the error codes are no longer contiguous from -1");
	}
}

#[test]
fn every_marshalled_struct_has_the_layout_it_had() {
	// Size, alignment and the offset of every field. The offsets are the point: a size that happens
	// to stay the same while two fields swap places is the change this catches and a size check
	// does not.
	use core::mem::{align_of, offset_of, size_of};

	assert_eq!((size_of::<DeviceInfo>(), align_of::<DeviceInfo>()), (40, 8));
	assert_eq!(offset_of!(DeviceInfo, device_type), 0);
	assert_eq!(offset_of!(DeviceInfo, _pad0), 4, "the explicit padding must stay where the implicit padding was");
	assert_eq!(offset_of!(DeviceInfo, bar_len), 8);
	assert_eq!(offset_of!(DeviceInfo, common_offset), 16);
	assert_eq!(offset_of!(DeviceInfo, device_offset), 32);
	assert_eq!(offset_of!(DeviceInfo, bus), 36);
	assert_eq!(offset_of!(DeviceInfo, _pad), 39);

	assert_eq!((size_of::<Framebuffer>(), align_of::<Framebuffer>()), (24, 4));
	assert_eq!(offset_of!(Framebuffer, width), 0);
	assert_eq!(offset_of!(Framebuffer, bytes_per_pixel), 12);
	assert_eq!(offset_of!(Framebuffer, red_shift), 16);
	assert_eq!(offset_of!(Framebuffer, _pad), 22);

	assert_eq!((size_of::<ObjectInfo>(), align_of::<ObjectInfo>()), (32, 8));
	assert_eq!(offset_of!(ObjectInfo, koid), 0);
	assert_eq!(offset_of!(ObjectInfo, object_type), 8);
	assert_eq!(offset_of!(ObjectInfo, rights), 16);
	assert_eq!(offset_of!(ObjectInfo, generation), 20);
	assert_eq!(offset_of!(ObjectInfo, size), 24);

	assert_eq!((size_of::<ProcessStats>(), align_of::<ProcessStats>()), (56, 8));
	assert_eq!(offset_of!(ProcessStats, messages_sent), 0);
	assert_eq!(offset_of!(ProcessStats, state), 32);
	assert_eq!(offset_of!(ProcessStats, completion), 40);
	assert_eq!(offset_of!(ProcessStats, completion_valid), 48);

	assert_eq!((size_of::<DomainStats>(), align_of::<DomainStats>()), (104, 8));
	assert_eq!(offset_of!(DomainStats, memory_used), 0);
	assert_eq!(offset_of!(DomainStats, stack_limit), 96);

	assert_eq!((size_of::<MemoryStats>(), align_of::<MemoryStats>()), (32, 8));
	assert_eq!(offset_of!(MemoryStats, total_frames), 0);
	assert_eq!(offset_of!(MemoryStats, heap_free), 24);

	assert_eq!((size_of::<MemmapRegion>(), align_of::<MemmapRegion>()), (24, 8));
	assert_eq!(offset_of!(MemmapRegion, base), 0);
	assert_eq!(offset_of!(MemmapRegion, length), 8);
	assert_eq!(offset_of!(MemmapRegion, kind), 16);
	assert_eq!(offset_of!(MemmapRegion, _pad), 20);

	assert_eq!((size_of::<IrqInfo>(), align_of::<IrqInfo>()), (16, 4));
	assert_eq!(offset_of!(IrqInfo, vector), 0);
	assert_eq!(offset_of!(IrqInfo, device), 12);

	assert_eq!((size_of::<PciInfo>(), align_of::<PciInfo>()), (12, 2));
	assert_eq!(offset_of!(PciInfo, vendor), 0);
	assert_eq!(offset_of!(PciInfo, device), 2);
	assert_eq!(offset_of!(PciInfo, class), 4);
	assert_eq!(offset_of!(PciInfo, func), 9);
	assert_eq!(offset_of!(PciInfo, _pad), 10);
}

#[test]
fn a_record_that_does_not_match_its_own_declaration_does_not_parse() {
	// `parse` validated the fixed prefix and took the rest of the slice as "fields", leaving the
	// walking to an iterator that stops early and reports nothing. So a record declaring one field
	// over an empty region parsed successfully - and after `Some(LogRecord)` every renderer is
	// entitled to assume the grammar holds. CBOR in particular announced a map of `field_count`
	// pairs and then emitted however many it got, which is not CBOR.
	let fields: [(&[u8], &[u8]); 1] = [(b"k", b"v")];
	let mut wire: [u8; 64] = [0u8; 64];
	let n: usize = encode(1, Severity::Info, b"src", &fields, &mut wire).expect("encode");
	assert!(LogRecord::parse(&wire[..n]).is_some(), "the fixture must be a record, or nothing below means anything");

	// One field declared, none present.
	let mut empty: [u8; 64] = wire;
	let count_at: usize = 8 + 1 + 2 + 3;
	assert_eq!(u16::from_le_bytes([empty[count_at], empty[count_at + 1]]), 1);
	assert!(LogRecord::parse(&empty[..count_at + 2]).is_none(), "a record declaring a field it does not carry");

	// Two declared, one present.
	empty[count_at..count_at + 2].copy_from_slice(&2u16.to_le_bytes());
	assert!(LogRecord::parse(&empty[..n]).is_none(), "a count larger than the fields present");

	// A field whose length runs past the record.
	let mut torn: [u8; 64] = wire;
	torn[count_at + 2..count_at + 4].copy_from_slice(&999u16.to_le_bytes());
	assert!(LogRecord::parse(&torn[..n]).is_none(), "a key longer than the buffer holding it");

	// CANONICAL: trailing bytes make it a different record, so it is not this one.
	let mut trailing: [u8; 64] = wire;
	trailing[n] = 0;
	assert!(LogRecord::parse(&trailing[..n + 1]).is_none(), "a record plus a zero byte is not the same record");
	trailing[n] = 0xAB;
	assert!(LogRecord::parse(&trailing[..n + 1]).is_none(), "a record plus junk is not the same record");
}

#[test]
fn the_wire_format_carries_strings_because_the_schema_says_so() {
	// `log.lsidl` declares source, key and value as `string` and its generated codec carries
	// `String`; this side took `&[u8]` and validated nothing. So `source = b"\xff"` was valid input
	// that produced invalid JSON - a raw 0xff inside a string literal - and invalid CBOR, whose
	// major type 3 is defined as UTF-8 text. Two implementations of one format disagreeing about
	// what the format accepts is the thing this crate exists to prevent.
	let mut out: [u8; 64] = [0u8; 64];
	let good: [(&[u8], &[u8]); 1] = [(b"k", b"v")];
	assert!(encode(1, Severity::Info, b"src", &good, &mut out).is_some());
	assert!(encode(1, Severity::Info, b"\xff", &good, &mut out).is_none(), "a source that is not text");
	let bad_key: [(&[u8], &[u8]); 1] = [(b"\xff", b"v")];
	assert!(encode(1, Severity::Info, b"src", &bad_key, &mut out).is_none(), "a key that is not text");
	let bad_val: [(&[u8], &[u8]); 1] = [(b"k", b"\xc3\x28")];
	assert!(encode(1, Severity::Info, b"src", &bad_val, &mut out).is_none(), "a value that is not text");

	// And parse refuses what encode would not have produced, because bytes arrive from elsewhere.
	let n: usize = encode(1, Severity::Info, b"src", &good, &mut out).expect("encode");
	let mut forged: [u8; 64] = out;
	forged[11] = 0xff; // the first byte of "src"
	assert!(LogRecord::parse(&forged[..n]).is_none(), "a record off the wire whose source is not text");
}

#[test]
fn a_log_entry_cannot_forge_a_log_entry() {
	// `render_text` copied source, key and value through unescaped, so a value containing a newline
	// and a plausible prefix appeared in the log as a SECOND RECORD - a component writing its own
	// audit trail - and an ESC sequence reached the terminal of whoever was reading it. The JSON
	// renderer escaped its controls; the human one, which is the one a person reads, did not.
	let fields: [(&[u8], &[u8]); 1] = [(b"note", b"ok\n[99] FATAL kernel: forged=yes\x1b[31m")];
	let mut wire: [u8; 128] = [0u8; 128];
	let n: usize = encode(7, Severity::Info, b"app", &fields, &mut wire).expect("encode");
	let rec: LogRecord<'_> = LogRecord::parse(&wire[..n]).expect("parse");

	let mut text: [u8; 256] = [0u8; 256];
	let len: usize = render_text(&rec, &mut text).expect("render");
	let line: &[u8] = &text[..len];
	assert!(!line.contains(&b'\n'), "a newline survived into the human line: {:?}", core::str::from_utf8(line));
	assert!(!line.contains(&0x1b), "an escape sequence reached the terminal");
	assert!(line.windows(4).any(|w| w == b"\\x1b"), "the escape must still be readable as what it was");
	assert!(line.windows(2).any(|w| w == b"\\n"), "and so must the newline");
}

#[test]
fn the_rendered_forms_agree_with_the_wire_about_what_a_field_list_is() {
	// The wire format is a LIST of fields and `log.lsidl` says `list<field>`, both of which permit
	// a key to repeat. The JSON renderer emitted an object, so two `error` fields became whatever
	// the consumer does with a duplicate key. The generated codec has always rendered
	// `[{"key":..,"value":..}]`; this is the two implementations agreeing.
	let fields: [(&[u8], &[u8]); 2] = [(b"error", b"first"), (b"error", b"second")];
	let mut wire: [u8; 128] = [0u8; 128];
	let n: usize = encode(1, Severity::Warn, b"app", &fields, &mut wire).expect("encode");
	let rec: LogRecord<'_> = LogRecord::parse(&wire[..n]).expect("parse");

	let mut json: [u8; 256] = [0u8; 256];
	let len: usize = render_json(&rec, &mut json).expect("render");
	assert_eq!(&json[..len], br#"{"ts":1,"severity":"WARN","source":"app","fields":[{"key":"error","value":"first"},{"key":"error","value":"second"}]}"#);

	// CBOR: an array of two-entry maps, for the same reason.
	let mut cbor: [u8; 256] = [0u8; 256];
	let len: usize = render_cbor(&rec, &mut cbor).expect("render");
	let body: &[u8] = &cbor[..len];
	assert_eq!(body[0], 0xa4, "the record is a 4-entry map");
	let at: usize = body.windows(6).position(|w| w == b"fields").expect("the fields key") + 6;
	assert_eq!(body[at], 0x82, "the fields are an array of two");
	assert_eq!(body[at + 1], 0xa2, "each field is a two-entry map");
}

#[test]
fn an_encode_that_does_not_fit_writes_nothing() {
	// `Buf::bytes` wrote byte by byte and returned None when it ran out, leaving the output partly
	// overwritten - not a safety problem, and "None on overflow" invites a caller to assume nothing
	// was written. Now that assumption is true.
	let fields: [(&[u8], &[u8]); 1] = [(b"key", b"value")];
	let mut out: [u8; 32] = [0xEEu8; 32];
	assert!(encode(1, Severity::Info, b"a-source-that-does-not-fit", &fields, &mut out).is_none());
	assert!(out.iter().all(|&b| b == 0xEE), "a refused encode left bytes behind: {out:?}");
}

// ---- The bootstrap list and the archive built from it -------------------------------------

#[test]
fn a_bootstrap_list_is_read_the_same_way_however_it_was_written() {
	use crate::bootstrap::{Row, parse_list};

	let named = |rows: &[Row<'_>]| -> alloc::vec::Vec<alloc::vec::Vec<u8>> { rows.iter().map(|row| [row.name, b" ", row.path].concat()).collect() };

	// A CRLF file left `\r` on the end of every path, so every read of every program failed on a
	// list edited on the wrong machine - a boot that dies with nothing to say which line did it.
	let crlf = parse_list(b"init bin/init\r\nshell bin/shell\r\n").expect("a CRLF list is a list");
	assert_eq!(named(&crlf), [b"init bin/init".to_vec(), b"shell bin/shell".to_vec()]);

	// A tab is a separator, repeated spaces are not part of the path, and a blank or commented
	// line is not a row.
	let mixed = parse_list(b"# the bootstrap set\ninit\tbin/init\n\n  shell   bin/shell  \n").expect("tabs, comments and padding");
	assert_eq!(named(&mixed), [b"init bin/init".to_vec(), b"shell bin/shell".to_vec()]);

	// And the refusals: a row with no separator, an empty name, an empty path, a duplicate name,
	// a name too long for the entry field, and a list with no rows at all.
	assert!(parse_list(b"init\n").is_none(), "a row with no path");
	assert!(parse_list(b" bin/init\n").is_none(), "an empty name");
	assert!(parse_list(b"init \n").is_none(), "an empty path");
	assert!(parse_list(b"init bin/init\ninit bin/other\n").is_none(), "a duplicate name");
	assert!(parse_list(&[&[b'n'; crate::PKG_NAME_LEN + 1][..], b" bin/init\n"].concat()).is_none(), "a name that does not fit the entry");
	assert!(parse_list(b"\n# nothing but a comment\n").is_none(), "a list with no rows");
}

#[test]
fn a_built_package_reads_back_as_the_files_that_went_into_it() {
	use crate::Package;
	use crate::bootstrap::build_package;

	let entries: [(&[u8], &[u8]); 2] = [(b"init", b"the init program"), (b"shell", b"the shell")];
	let archive = build_package(&entries).expect("a package");
	let parsed = Package::parse(&archive).expect("the writer and the reader agree");
	assert_eq!(parsed.len(), 2);
	assert_eq!(parsed.lookup(b"init"), Some(&b"the init program"[..]));
	assert_eq!(parsed.lookup(b"shell"), Some(&b"the shell"[..]));
}

#[test]
fn a_package_that_would_describe_itself_wrongly_is_not_built() {
	use crate::bootstrap::{MAX_ENTRIES, build_package};

	// The count, every offset and every length are `u32`. Unchecked `as u32` meant a large enough
	// set produced an archive whose table pointed somewhere other than its data - and the kernel
	// would then read whatever happened to be there as a program. None of these can be reached with
	// real files on a real medium; that is exactly why the arithmetic was never noticed.
	assert!(build_package(&[]).is_none(), "an empty package is not a package");
	assert!(build_package(&[(b"", b"body")]).is_none(), "an entry with no name");
	assert!(build_package(&[(&[b'n'; crate::PKG_NAME_LEN + 1], b"body")]).is_none(), "a name that does not fit the entry");

	let many: alloc::vec::Vec<(&[u8], &[u8])> = (0..MAX_ENTRIES + 1).map(|_| (&b"name"[..], &b"body"[..])).collect();
	assert!(build_package(&many).is_none(), "more entries than the format is allowed to carry");

	// A blob larger than the per-file limit, without allocating one: the limit is checked from the
	// slice's length, so a slice that claims the length is enough to prove the check runs.
	let huge = unsafe { core::slice::from_raw_parts(1 as *const u8, crate::bootstrap::MAX_FILE_BYTES + 1) };
	assert!(build_package(&[(b"init", huge)]).is_none(), "a file larger than the format's limit");
}
