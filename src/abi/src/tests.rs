extern crate std;

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

// The snapshot rows, with each constant's own NAME beside it - through `stringify!`, so the name
// cannot drift from the constant it labels the way a hand-written string could.
macro_rules! named {
	($(($name:ident, $value:expr)),* $(,)?) => { &[$(($name, $value, stringify!($name))),*] };
}

// Every syscall number, snapshotted. A new call appends; an existing one never moves.
const SYSCALLS: &[(u64, u64, &str)] = named![
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
	(SYS_PROCESS_GROUP_STATS, 75),
	(SYS_CHANNEL_SEND_CAPS, 67),
	(SYS_CHANNEL_RECV_CAPS, 68),
	(SYS_WAITSET_CREATE, 69),
	(SYS_WAITSET_ADD, 70),
	(SYS_WAITSET_REMOVE, 71),
	(SYS_WAITSET_WAIT, 72),
	(SYS_RANDOM_INSECURE, 73),
	(SYS_DEVICE_QUIESCED, 74),
];

// Every `pub const SYS_*` the crate declares, read out of its own source at compile time.
//
// THE SNAPSHOT ABOVE CANNOT NOTICE WHAT IT WAS NEVER TOLD ABOUT, and that is not a hypothetical:
// `SYS_DEVICE_QUIESCED = 74` was added by P02M0133, the table still ended at 73, and the guard meant
// to catch exactly this - `assert_eq!(SYSCALLS.len(), 73)` - was comparing the list against its own
// length. The suite stayed green over a syscall the kernel dispatches and the runtime wraps.
//
// So the two halves are now derived from different things on purpose. Correctness stays with the
// hand-written snapshot, because a frozen copy is the only thing that can catch a number MOVING - a
// list generated from the constants would move with them and prove nothing. COMPLETENESS comes from
// the source text, which nobody has to remember to update because there is nothing to update.
//
// `include_str!` is compile-time and dependency-free, which is what makes this affordable in a crate
// whose whole point is being small.
//
// NAMES, AND A REFUSAL FOR ANYTHING IT CANNOT READ. This parsed the VALUE too, and ended every line
// it did not understand with `continue` - so a declaration written in an unfamiliar shape was
// treated as though it were not a declaration:
//
//   pub const SYS_FOO: u64 = 0x4a;      // not a decimal literal, silently skipped
//   pub const RIGHT_FOO: u32 = 1u32 << 12;  // the suffix defeats `strip_prefix("1 << ")`
//   pub const ERR_FOO: i64 = -14 - 1;   // not a bare literal, skipped
//
// None of those exists in the crate today, which is exactly why it mattered: the mechanism built to
// make a missing declaration impossible had a hole through which one could arrive silently, and the
// failure mode is the quiet one - the test keeps passing while what it checks stops being checked.
//
// Two changes close it. The question is COMPLETENESS, which is about names, so the value is not
// parsed at all - the snapshot beside it already checks values, and parsing each one twice, once by
// the compiler and once by a string parser, is what created the shapes above. And a line that begins
// like a declaration and cannot be read is a PANIC rather than a skip: "I saw something claiming to
// be a declaration and could not read it" is a different and much safer question than "did I find
// one".
fn declared_names(prefix: &str) -> alloc::vec::Vec<alloc::string::String> {
	// EVERY MODULE, for the same reason `declared_repr_c_structs` reads every module: a completeness
	// question answered from one file is a completeness question about one file.
	let mut source = alloc::string::String::new();
	for (_, text) in crate_sources() {
		source.push_str(&text);
		source.push('\n');
	}
	let mut out: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
	for line in source.lines() {
		let Some(rest) = line.strip_prefix("pub const ") else { continue };
		let Some(rest) = rest.strip_prefix(prefix) else { continue };
		let Some((name, _)) = rest.split_once(':') else {
			panic!("`pub const {prefix}` declaration this test cannot read - it names no type: {line}");
		};
		let name = name.trim();
		assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'), "`pub const {prefix}` declaration this test cannot read - `{name}` is not a constant name: {line}");
		out.push(alloc::format!("{prefix}{name}"));
	}
	out
}

#[test]
fn the_syscall_numbers_are_what_they_were() {
	// 39 is deliberately absent: a retired call's number is not reused, because a stale binary
	// calling it must get "no such syscall" rather than somebody else's handler.
	let mut seen: [bool; 128] = [false; 128];
	for &(number, expected, _) in SYSCALLS {
		assert_eq!(number, expected, "a syscall number moved");
		let slot = number as usize;
		assert!(slot < seen.len(), "syscall {number} is past the table this test can check");
		assert!(!seen[slot], "two syscalls share number {number}");
		seen[slot] = true;
	}
	assert!(!seen[39], "39 is a retired number and must stay retired");
}

#[test]
fn the_snapshot_names_every_syscall_the_crate_declares() {
	// BY NAME. The values are the snapshot's business and it checks them above; completeness is a
	// question about which declarations exist, and a name is something a line-oriented parser can
	// extract without knowing the grammar of an expression.
	let mut declared = declared_names("SYS_");
	declared.sort();
	let mut snapshot: alloc::vec::Vec<alloc::string::String> = SYSCALLS.iter().map(|&(_, _, name)| alloc::string::String::from(name)).collect();
	snapshot.sort();
	// Declaration order is not numeric order - `SYS_DEVICE_QUIESCED` is declared two hundred lines
	// above a syscall with a lower number - so both sides are sorted before they are compared.
	assert_eq!(declared, snapshot, "the crate declares {} `pub const SYS_*` and the snapshot names {}; a syscall was added or removed without updating `SYSCALLS`", declared.len(), snapshot.len());
}

#[test]
fn every_right_is_one_bit_and_rights_all_is_their_union() {
	// `RIGHTS_ALL` was a hand-written 0xfff beside twelve individually defined bits. A thirteenth
	// right added without touching the literal is a right that `RIGHTS_ALL` silently does not grant.
	//
	// AND THE BIT IS SNAPSHOTTED BESIDE THE NAME, which it was not. This used `named_only!`, so the
	// four properties checked here - one bit each, no collision, the union is `RIGHTS_ALL`, no name
	// missing - were ALL true of a list in which two rights had swapped values. Both are one bit,
	// they do not collide, the union is unchanged and the names are the same set. Userspace passes
	// raw rights bits across the syscall boundary, so a swap is a program that asks for read and is
	// granted write; this is the same class the syscall-number snapshot exists to close, and the
	// claim that the frozen snapshot checks values was true for `SYS_*` and `ERR_*` and not for
	// these.
	const RIGHTS: &[(u32, u32, &str)] = named![
		(RIGHT_READ, 1 << 0),
		(RIGHT_WRITE, 1 << 1),
		(RIGHT_EXECUTE, 1 << 2),
		(RIGHT_MAP, 1 << 3),
		(RIGHT_SEND, 1 << 4),
		(RIGHT_RECEIVE, 1 << 5),
		(RIGHT_DUPLICATE, 1 << 6),
		(RIGHT_TRANSFER, 1 << 7),
		(RIGHT_REVOKE, 1 << 8),
		(RIGHT_GET_INFO, 1 << 9),
		(RIGHT_MANAGE, 1 << 10),
		(RIGHT_WAIT, 1 << 11),
	];
	let mut union = 0u32;
	for &(bit, expected, name) in RIGHTS {
		assert_eq!(bit, expected, "right {name} is {bit:#x} and the snapshot froze it at {expected:#x}; a rights bit is an ABI value userspace passes raw across the syscall boundary");
		assert_eq!(bit.count_ones(), 1, "right {name} is not a single bit");
		assert_eq!(union & bit, 0, "right {name} collides with one already defined");
		union |= bit;
	}
	assert_eq!(union, RIGHTS_ALL, "RIGHTS_ALL is not the union of the rights that exist");

	// AND THE LIST IS EVERY RIGHT, which is the half that was asserted rather than checked. A
	// thirteenth right added without touching this list would have left the union above correct over
	// twelve bits and `RIGHTS_ALL` correct over thirteen, and nothing would have said so.
	let mut declared = declared_names("RIGHT_");
	declared.sort();
	let mut snapshot: alloc::vec::Vec<alloc::string::String> = RIGHTS.iter().map(|&(_, _, name)| alloc::string::String::from(name)).collect();
	snapshot.sort();
	assert_eq!(declared, snapshot, "the crate declares {} `pub const RIGHT_*` and this list names {}; a right was added or removed without updating it", declared.len(), snapshot.len());
}

#[test]
fn every_wire_stable_numeric_family_is_frozen_and_complete() {
	// THE DECISION, made rather than deferred: these families ARE wire-stable and get the same
	// snapshot `SYS_*` and `ERR_*` have.
	//
	// The question was whether to freeze them or to write down that they are not stable.
	// `OBJECT_TYPE_*` settles it - `abi` documents it as a stable ABI code, and userspace matches on
	// the raw number that `SYS_OBJECT_INFO_GET` returns - and once one family is in, the argument
	// for the rest is identical: every one of these crosses the syscall boundary as a bare integer
	// that a userspace program compares against a constant it was compiled with.
	//
	// NOTHING IS VERSIONED BEFORE THE FIRST RELEASE, so today a changed object-type code is a
	// rebuild rather than a break. That is the reason to do this NOW and not the reason to skip it:
	// after the release, two codes that moved through a green suite are a compatibility break
	// nobody can undo. The cost while it is cheap is one test.
	//
	// Each family is checked for VALUES (the snapshot below, which is the only thing that can catch
	// a code moving) and for COMPLETENESS (`declared_names`, which is what catches one added without
	// being frozen). A generated list would move with the constants and prove neither.
	const OBJECT_TYPES: &[(u64, u64, &str)] = named![
		(OBJECT_TYPE_DOMAIN, 0),
		(OBJECT_TYPE_PROCESS, 1),
		(OBJECT_TYPE_THREAD, 2),
		(OBJECT_TYPE_ADDRESS_SPACE, 3),
		(OBJECT_TYPE_MEMORY_OBJECT, 4),
		(OBJECT_TYPE_CHANNEL, 5),
		(OBJECT_TYPE_EVENT, 6),
		(OBJECT_TYPE_TIMER, 7),
		(OBJECT_TYPE_INTERRUPT, 8),
		(OBJECT_TYPE_DEVICE_MEMORY, 9),
		(OBJECT_TYPE_DMA_BUFFER, 10),
		(OBJECT_TYPE_PROCESS_GROUP, 11),
		(OBJECT_TYPE_PRIVILEGE, 12),
		(OBJECT_TYPE_WAIT_SET, 13),
	];
	const PROC_STATES: &[(u64, u64, &str)] = named![(PROC_STATE_RUNNING, 0), (PROC_STATE_STOPPED, 1), (PROC_STATE_FAILED, 2)];
	// The POSIX numbers, deliberately: a program written against `kill -9` means nine.
	const SIGNALS: &[(u64, u64, &str)] = named![(SIG_INT, 2), (SIG_KILL, 9), (SIG_TERM, 15), (SIG_CONT, 18), (SIG_STOP, 19)];
	const PROPERTIES: &[(u64, u64, &str)] = named![(PROP_NAME, 0), (PROP_MEMORY_LIMIT, 1), (PROP_HANDLE_LIMIT, 2), (PROP_THREAD_LIMIT, 3), (PROP_DMA_LIMIT, 4), (PROP_IPC_QUEUE_LIMIT, 5), (PROP_STACK_LIMIT, 6),];
	const POWER: &[(u64, u64, &str)] = named![(POWER_REBOOT, 0), (POWER_OFF, 1)];
	const MEMMAP: &[(u32, u32, &str)] = named![
		(MEMMAP_USABLE, 0),
		(MEMMAP_RESERVED, 1),
		(MEMMAP_ACPI_RECLAIMABLE, 2),
		(MEMMAP_ACPI_NVS, 3),
		(MEMMAP_BAD, 4),
		(MEMMAP_BOOTLOADER, 5),
		(MEMMAP_KERNEL, 6),
		(MEMMAP_FRAMEBUFFER, 7),
	];
	const IRQ_KINDS: &[(u32, u32, &str)] = named![(IRQ_KIND_FIXED, 0), (IRQ_KIND_MSI, 1)];

	// Written twice, once per width, rather than through a generic: this crate is `no_std` and
	// dependency-free by policy, and a trait bound to save four lines is not worth a `num` import.
	fn check_u64(family: &str, rows: &[(u64, u64, &str)]) {
		let mut seen: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
		for &(value, expected, name) in rows {
			assert_eq!(value, expected, "{name} is {value} and the snapshot froze it at {expected}; userspace compares this code raw across the syscall boundary");
			assert!(!seen.contains(&value), "{family}: {name} reuses code {value}");
			seen.push(value);
		}
	}
	fn check_u32(family: &str, rows: &[(u32, u32, &str)]) {
		let mut seen: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
		for &(value, expected, name) in rows {
			assert_eq!(value, expected, "{name} is {value} and the snapshot froze it at {expected}; userspace compares this code raw across the syscall boundary");
			assert!(!seen.contains(&value), "{family}: {name} reuses code {value}");
			seen.push(value);
		}
	}
	fn complete_u64(prefix: &str, rows: &[(u64, u64, &str)]) {
		let mut declared = declared_names(prefix);
		declared.sort();
		let mut snapshot: alloc::vec::Vec<alloc::string::String> = rows.iter().map(|&(_, _, name)| alloc::string::String::from(name)).collect();
		snapshot.sort();
		assert_eq!(declared, snapshot, "the crate declares {} `pub const {prefix}` and the snapshot names {}; one was added or removed without freezing it", declared.len(), snapshot.len());
	}
	fn complete_u32(prefix: &str, rows: &[(u32, u32, &str)]) {
		let mut declared = declared_names(prefix);
		declared.sort();
		let mut snapshot: alloc::vec::Vec<alloc::string::String> = rows.iter().map(|&(_, _, name)| alloc::string::String::from(name)).collect();
		snapshot.sort();
		assert_eq!(declared, snapshot, "the crate declares {} `pub const {prefix}` and the snapshot names {}; one was added or removed without freezing it", declared.len(), snapshot.len());
	}

	check_u64("OBJECT_TYPE", OBJECT_TYPES);
	complete_u64("OBJECT_TYPE_", OBJECT_TYPES);
	check_u64("PROC_STATE", PROC_STATES);
	complete_u64("PROC_STATE_", PROC_STATES);
	check_u64("SIG", SIGNALS);
	complete_u64("SIG_", SIGNALS);
	check_u64("PROP", PROPERTIES);
	complete_u64("PROP_", PROPERTIES);
	check_u64("POWER", POWER);
	complete_u64("POWER_", POWER);
	check_u32("MEMMAP", MEMMAP);
	complete_u32("MEMMAP_", MEMMAP);
	check_u32("IRQ_KIND", IRQ_KINDS);
	complete_u32("IRQ_KIND_", IRQ_KINDS);
}

#[test]
fn the_error_codes_are_what_they_were() {
	// Negative, distinct, contiguous from -1, and never renumbered: a caller matching on -6 has to
	// keep meaning INVALID.
	const ERRORS: &[(i64, i64, &str)] = named![
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
		(ERR_INTERRUPTED, -14),
	];
	for (index, &(code, expected, _)) in ERRORS.iter().enumerate() {
		assert_eq!(code, expected, "an error code moved");
		assert_eq!(code, -(index as i64 + 1), "the error codes are no longer contiguous from -1");
	}

	// And the snapshot names every error the crate declares - the same completeness check the
	// syscalls have and this family did not.
	let mut declared = declared_names("ERR_");
	declared.sort();
	let mut snapshot: alloc::vec::Vec<alloc::string::String> = ERRORS.iter().map(|&(_, _, name)| alloc::string::String::from(name)).collect();
	snapshot.sort();
	assert_eq!(declared, snapshot, "the crate declares {} `pub const ERR_*` and the snapshot names {}; an error was added or removed without updating it", declared.len(), snapshot.len());
}

// The fields a `repr(C)` struct declares, in order, read out of the crate's own source.
//
// The layout test's comment promised "the offset of every field" and asserted seven of DeviceInfo's
// twelve. `notify_offset`, `notify_multiplier` and `isr_offset` are three adjacent `u32`s and none
// of them was named, so swapping any two passed the size check, the alignment check and every
// offset the test made - and a driver would program the wrong virtio structure.
//
// Listing the missing ones would leave the same hole open for the next field somebody adds. The
// list has to be checked against the struct instead, and the struct's own source is the only thing
// that knows.
fn declared_fields(name: &str) -> alloc::vec::Vec<alloc::string::String> {
	const SOURCE: &str = include_str!("lib.rs");
	let mut out: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
	let header = alloc::format!("pub struct {name} {{");
	let Some((_, body)) = SOURCE.split_once(header.as_str()) else { return out };
	for line in body.lines() {
		let line = line.trim();
		if line == "}" {
			break;
		}
		let Some(rest) = line.strip_prefix("pub ") else { continue };
		let Some((field, _)) = rest.split_once(':') else { continue };
		out.push(alloc::string::String::from(field.trim()));
	}
	out
}

// Size, alignment, and the offset of every field IN DECLARATION ORDER - checked against the source,
// so the list cannot quietly stop being every field.
macro_rules! assert_layout {
	($covered:ident, $ty:ident, $size:expr, $align:expr, $($field:ident => $offset:expr),+ $(,)?) => {{
		use core::mem::{align_of, offset_of, size_of, size_of_val};
		assert_eq!((size_of::<$ty>(), align_of::<$ty>()), ($size, $align), concat!(stringify!($ty), " changed size or alignment"));
		$(assert_eq!(offset_of!($ty, $field), $offset, concat!(stringify!($ty), ".", stringify!($field), " moved"));)+
		let named: alloc::vec::Vec<alloc::string::String> = alloc::vec![$(alloc::string::String::from(stringify!($field))),+];
		assert_eq!(declared_fields(stringify!($ty)), named, concat!(stringify!($ty), ": the assertions above are not every field in declaration order"));
		// NO IMPLICIT PADDING: every byte of the struct belongs to a named field.
		//
		// THE PROPERTY, not the instance. `DeviceInfo` had four unnamed bytes, they were named, and
		// the change that landed next created six more at the other end - and the assertions above
		// FIRED on that change, on the size, and the belief they corrected was about where the new
		// fields would fit. "Size, alignment and every field offset are what they were" and "every
		// byte belongs to a named field" are different properties, and the first was satisfied by a
		// change that broke the second. These structs are copied to userspace with
		// `size_of::<T>()` from values built on the kernel stack, and Rust does not promise that
		// padding in an otherwise initialised value is initialised.
		let probe = <$ty>::default();
		let mut occupied = 0usize;
		$(occupied += size_of_val(&probe.$field);)+
		assert_eq!(occupied, size_of::<$ty>(), concat!(stringify!($ty), ": its fields occupy fewer bytes than the struct, so `repr(C)` inserted padding - name it, the way `_pad0` is named, or userspace receives whatever the kernel stack held there"));
		$covered.push(alloc::string::String::from(stringify!($ty)));
	}};
}

// Every `.rs` file this crate is built from, read at test time.
//
// `include_str!("lib.rs")` and nothing else is what the parser below used to read, so the
// completeness argument it supports was about ONE FILE while claiming to be about the crate. That
// is not hypothetical here: `src/abi` already has three modules - `lib.rs`, `log.rs` and
// `bootstrap.rs` - and today every `repr(C)` struct happens to live in `lib.rs`, which is the only
// reason the count matched. A marshalled struct added to either of the others would have been
// invisible to the test whose whole purpose is that no marshalled struct is invisible.
//
// Read from the filesystem rather than through `include_str!` because the set of files is the
// question: a macro that has to name each file cannot notice a new one. `CARGO_MANIFEST_DIR` is
// resolved at compile time and the directory is walked at run time, so adding a module needs no
// edit here.
fn crate_sources() -> alloc::vec::Vec<(alloc::string::String, alloc::string::String)> {
	let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
	let mut out: alloc::vec::Vec<(alloc::string::String, alloc::string::String)> = alloc::vec::Vec::new();
	for entry in std::fs::read_dir(root).expect("this crate's own source directory is readable") {
		let path = entry.expect("a readable directory entry").path();
		if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
			continue;
		}
		let name = alloc::string::String::from(path.file_name().and_then(|name| name.to_str()).unwrap_or(""));
		let source = std::fs::read_to_string(&path).expect("a readable source file");
		out.push((name, source));
	}
	assert!(out.len() >= 3, "this crate has at least lib.rs, log.rs and bootstrap.rs; the scan found {}", out.len());
	out.sort();
	out
}

// The `repr(C)` structs one source file declares.
//
// Nine structs, nine layout assertions, and nothing that said the two numbers must match. This is
// what makes the no-implicit-padding property cover the CRATE rather than the nine structs somebody
// listed.
//
// AND THE SPELLING IS A RULE, not a coincidence the parser happens to match. It compared against the
// exact string `#[repr(C)]`, so `#[repr(C, align(8))]` or `#[repr(C, packed)]` would have been read
// as no marker at all and the struct behind it would have left the covered set silently. Both
// answers to that were defensible - accept every `#[repr(C..)]` form, or state that an ABI struct
// carries exactly `#[repr(C)]` - and this is the second one, made explicit: a struct whose layout is
// modified by `packed` or `align` has a different contract with userspace and must be a decision
// somebody makes rather than a spelling this test skips.
//
// AND IT FAILS CLOSED. The old version cleared `marked` on any line it did not recognise, so
//
//     #[repr(C)]
//     #[cfg(target_arch = "x86_64")]
//     pub struct Foo { .. }
//
// was not refused - `Foo` simply never entered the list the completeness test compares against, and
// a struct nobody checks is precisely what this machinery exists to make impossible. An
// unrecognised line between a `#[repr(..)]` and its declaration is now a panic naming the file and
// the line, the same way an unreadable `#[repr(..)]` spelling already was. A `#[repr(u8)]` on an
// ENUM is not this test's business and ends the run quietly; anything else is a human decision.
//
// A PARSER THAT DECIDES WHAT GETS CHECKED IS ITSELF SOMETHING THAT HAS TO BE CHECKED, which is why
// this takes its source as an argument: `the_repr_c_parser_sees_what_it_claims_to_see` drives it
// over synthetic files covering every branch, including the two that used to be silent.
fn repr_c_structs_in(file: &str, source: &str) -> alloc::vec::Vec<alloc::string::String> {
	let mut out: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
	let mut marked: Option<alloc::string::String> = None;
	for (index, raw) in source.lines().enumerate() {
		let line = raw.trim();
		let number = index + 1;
		if line.starts_with("#[repr(") {
			marked = Some(alloc::string::String::from(line));
			continue;
		}
		let Some(repr) = marked.clone() else { continue };
		// A `#[derive(..)]`, a comment or a blank line between the marker and the declaration.
		if line.starts_with("#[derive") || line.starts_with("//") || line.is_empty() {
			continue;
		}
		if let Some(rest) = line.strip_prefix("pub struct ") {
			assert_eq!(repr, "#[repr(C)]", "{file}:{number}: this crate's marshalled types carry exactly `#[repr(C)]`; `{repr}` changes the layout userspace is compiled against and has to be a decision rather than a spelling this test does not recognise");
			// `;` and `(` as well as `{` and `<`: a unit struct and a tuple struct are structs, and
			// splitting on the brace alone read `pub struct Spaced;` as a type named `Spaced;`.
			// Found by the parser's own test, which is what that test is for.
			let name = rest.split(|c: char| c == ' ' || c == '{' || c == '<' || c == ';' || c == '(').next().unwrap_or("");
			assert!(!name.is_empty(), "{file}:{number}: a `pub struct` whose name this test cannot read: {line}");
			out.push(alloc::string::String::from(name));
			marked = None;
			continue;
		}
		// A representation on an ENUM or a UNION fixes a discriminant or a layout this test says
		// nothing about, and `#[repr(u8)]` on an enum is ordinary. Ends the run without a verdict.
		if line.starts_with("pub enum ") || line.starts_with("enum ") || line.starts_with("pub union ") || line.starts_with("union ") {
			marked = None;
			continue;
		}
		// FAIL CLOSED. Anything else between a representation and its declaration - a `cfg`, an
		// `allow`, a visibility this test does not read, a doc attribute - would have dropped the
		// declaration out of the covered set in silence.
		panic!("{file}:{number}: `{repr}` is followed by a line this test cannot read, so whatever it marks would silently leave the covered set: {line}");
	}
	out
}

// Every `repr(C)` struct the crate declares, across every module.
fn declared_repr_c_structs() -> alloc::vec::Vec<alloc::string::String> {
	let mut out: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
	for (file, source) in crate_sources() {
		out.extend(repr_c_structs_in(&file, &source));
	}
	out
}

#[test]
fn the_repr_c_parser_sees_what_it_claims_to_see() {
	// The completeness test is only as good as the parser that feeds it, and both of the ways this
	// parser used to lose a struct were silent - which is the failure mode that survives a green
	// suite indefinitely.
	assert_eq!(repr_c_structs_in("t.rs", "#[repr(C)]\npub struct Plain { a: u32 }\n"), alloc::vec!["Plain"], "the ordinary shape");
	assert_eq!(repr_c_structs_in("t.rs", "#[repr(C)]\n#[derive(Clone, Copy)]\n// a comment\n\npub struct Spaced;\n"), alloc::vec!["Spaced"], "derives, comments and blank lines sit between the marker and the declaration");
	assert_eq!(repr_c_structs_in("t.rs", "#[repr(u8)]\npub enum Kind { A }\n"), alloc::vec::Vec::<alloc::string::String>::new(), "a representation on an enum is not this test's business");
	assert_eq!(repr_c_structs_in("t.rs", "pub struct Unmarked;\n"), alloc::vec::Vec::<alloc::string::String>::new(), "a struct with no representation is not marshalled");
	assert_eq!(repr_c_structs_in("t.rs", "#[repr(C)]\npub struct One;\n#[repr(C)]\npub struct Two;\n"), alloc::vec!["One", "Two"], "the marker does not leak from one declaration to the next");

	// THE TWO SILENT LOSSES, now loud.
	let cfg_between = std::panic::catch_unwind(|| repr_c_structs_in("t.rs", "#[repr(C)]\n#[cfg(target_arch = \"x86_64\")]\npub struct Hidden { a: u32 }\n"));
	assert!(cfg_between.is_err(), "an attribute the parser does not know sits between the marker and the declaration, and the struct would leave the covered set in silence");

	let modified_layout = std::panic::catch_unwind(|| repr_c_structs_in("t.rs", "#[repr(C, packed)]\npub struct Packed { a: u32 }\n"));
	assert!(modified_layout.is_err(), "a representation that modifies the layout is a decision rather than a spelling this test skips");
}

#[test]
fn every_marshalled_struct_has_the_layout_it_had() {
	// The offsets are the point: a size that happens to stay the same while two fields swap places
	// is the change this catches and a size check does not.
	//
	// `covered` collects what was asserted, so the crate's own source can say whether that is all of
	// them - the same completeness argument the syscall snapshot is built on.
	let mut covered: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
	assert_layout!(
		covered, DeviceInfo, 48, 8,
		device_type => 0,
		// The explicit padding must stay where the implicit padding was.
		_pad0 => 4,
		bar_len => 8,
		common_offset => 16,
		notify_offset => 20,
		notify_multiplier => 24,
		isr_offset => 28,

		device_offset => 32,
		bus => 36,
		dev => 37,
		func => 38,
		// The three the standards identity added. They took the struct from 40 bytes to 48 - there
		// was no tail padding to use, since 40 is already 8-aligned - and nothing BEFORE them moved,
		// which is the half of the old claim here that was true. The struct's own comment corrected
		// the other half and this copy of it was left behind.
		class => 39,
		subclass => 40,
		prog_if => 41,
		// And the six bytes the growth created, named rather than implicit: this struct is copied
		// to userspace with `size_of::<T>()` from a value built on the kernel stack.
		_pad1 => 42,
	);

	assert_layout!(
		covered, Framebuffer, 24, 4,
		width => 0,
		height => 4,
		pitch => 8,
		bytes_per_pixel => 12,
		red_shift => 16,
		red_size => 17,
		green_shift => 18,
		green_size => 19,
		blue_shift => 20,
		blue_size => 21,
		_pad => 22,
	);

	assert_layout!(covered, ObjectInfo, 32, 8, koid => 0, object_type => 8, rights => 16, generation => 20, size => 24);

	assert_layout!(
		covered, ProcessStats, 56, 8,
		messages_sent => 0,
		messages_received => 8,
		handle_count => 16,
		memory_bytes => 24,
		state => 32,
		completion => 40,
		completion_valid => 48,
	);

	assert_layout!(
		covered, DomainStats, 104, 8,
		memory_used => 0,
		memory_peak => 8,
		memory_limit => 16,
		handles_used => 24,
		handles_limit => 32,
		threads_used => 40,
		threads_limit => 48,
		ipc_used => 56,
		ipc_limit => 64,
		dma_used => 72,
		dma_limit => 80,
		stack_used => 88,
		stack_limit => 96,
	);

	assert_layout!(covered, MemoryStats, 32, 8, total_frames => 0, free_frames => 8, heap_total => 16, heap_free => 24);
	assert_layout!(covered, MemmapRegion, 24, 8, base => 0, length => 8, kind => 16, _pad => 20);
	assert_layout!(covered, IrqInfo, 16, 4, vector => 0, kind => 4, bound => 8, device => 12);
	assert_layout!(
		covered, PciInfo, 12, 2,
		vendor => 0,
		device => 2,
		class => 4,
		subclass => 5,
		prog_if => 6,
		bus => 7,
		dev => 8,
		func => 9,
		_pad => 10,
	);

	// AND EVERY `repr(C)` STRUCT THE CRATE DECLARES IS ONE OF THEM. Nine structs, nine assertions,
	// and nothing said the two numbers had to match - so a tenth marshalled struct would have had no
	// layout assertion and no padding check, and the properties above would have covered the nine
	// somebody remembered.
	let mut declared = declared_repr_c_structs();
	declared.sort();
	let mut asserted = covered;
	asserted.sort();
	assert_eq!(declared, asserted, "the crate declares {} `repr(C)` structs and {} have layout assertions; a marshalled struct was added without one", declared.len(), asserted.len());
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
fn the_writer_cannot_build_what_the_reader_calls_invalid() {
	// The SSOT crate produced archives its own reader refused. `Package::parse` required a canonical
	// name and `build_package` checked only `is_empty()` and a length, so two shapes got through:
	// duplicate names, and a NUL inside one - where `b"foo\0bar"` was written as one thing and read
	// back as `foo` with non-zero padding after it, and `b"foo\0"` was written as distinct from
	// `b"foo"` and read as exactly that.
	assert!(bootstrap::build_package(&[(b"foo", b"A"), (b"foo", b"B")]).is_none(), "two entries of one name");
	assert!(bootstrap::build_package(&[(b"foo\0bar", b"A")]).is_none(), "a NUL inside a name");
	assert!(bootstrap::build_package(&[(b"foo\0", b"A")]).is_none(), "a name the reader would parse as a different one");
	assert!(bootstrap::build_package(&[(b"", b"A")]).is_none(), "no name at all");

	// And whatever it DOES build, the reader accepts - which is the property the two halves owe
	// each other.
	let built = bootstrap::build_package(&[(b"a", b"one"), (b"b", b"two")]).expect("an ordinary archive");
	assert!(Package::parse(&built).is_some(), "the writer's output is the reader's input");

	// The reserved header word is reserved, which nothing checked - so a writer could fill it with
	// anything and it would not be reserved at all.
	let mut forged = built.clone();
	forged[12] = 1;
	assert!(Package::parse(&forged).is_none(), "a reserved field with something in it");

	// And the reader has a ceiling of its OWN - not the bootstrap writer's, which is a different
	// bound for a different archive. Conflating them refused the system volume package, which has
	// 146 entries against the bootstrap set's handful, and the guest reported it as a missing file.
	let mut oversized = built.clone();
	oversized[8..12].copy_from_slice(&((MAX_PACKAGE_ENTRIES + 1) as u32).to_le_bytes());
	assert!(Package::parse(&oversized).is_none(), "more entries than either half accepts");
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

	// The per-file and per-archive ceilings, PROVED BY CALLING THE RULE rather than by conjuring an
	// argument that cannot legally exist.
	//
	// This used to be `from_raw_parts(1 as *const u8, MAX_FILE_BYTES + 1)`, on the reasoning that a
	// slice which only has its length read need not have memory behind it. Constructing the
	// reference is itself the contract - the range must be valid, initialised and owned - so the
	// `unsafe` block was undefined behaviour before `build_package` saw it, in the suite whose
	// subject is what the ABI guarantees.
	use crate::bootstrap::{MAX_FILE_BYTES, MAX_TOTAL_BYTES, entry_fits};
	assert!(entry_fits(4, MAX_FILE_BYTES, 0), "a file exactly at the limit fits");
	assert!(!entry_fits(4, MAX_FILE_BYTES + 1, 0), "a file larger than the format's limit does not");
	assert!(!entry_fits(crate::PKG_NAME_LEN + 1, 4, 0), "nor does a name that does not fit the entry");
	assert!(entry_fits(4, 1, MAX_TOTAL_BYTES - 1), "an archive exactly at its total fits");
	assert!(!entry_fits(4, 2, MAX_TOTAL_BYTES - 1), "and one byte past it does not");
	assert!(!entry_fits(4, 1, usize::MAX), "a total that would wrap is refused rather than wrapped");
}

#[test]
fn a_bootstrap_path_has_a_grammar_at_the_parser_that_calls_itself_strict() {
	use crate::bootstrap::{MAX_PATH_BYTES, parse_list, valid_bootstrap_path};

	// THE SIDE OF A ROW THAT WAS ONLY CHECKED FOR BEING NON-EMPTY. The name got the package-name
	// rule and the path got nothing, in the file whose comment calls it the strict parser - so a
	// list containing a NUL, a control byte, `//`, a leading separator or `..` parsed here and was
	// refused later, differently, by whichever filesystem backend the recovery path selected. The
	// diagnostic for that is "could not assemble the package", which names nothing.
	assert!(valid_bootstrap_path(b"libexec/storage_service.lsexe"), "the shape every real list uses");
	assert!(valid_bootstrap_path(b"etc/bootstrap.list"));
	assert!(valid_bootstrap_path(b"single"));

	for bad in [
		&b""[..],
		b"/libexec/x",
		b"libexec/x/",
		b"libexec//x",
		b"./x",
		b"../x",
		b"libexec/../etc/x",
		b"libexec/.",
		b"libexec\\x",
		b"libexec/\x00x",
		b"libexec/\x1fx",
		b"libexec/\x7fx",
	] {
		assert!(!valid_bootstrap_path(bad), "refused: {:?}", core::str::from_utf8(bad));
	}

	let overlong: alloc::vec::Vec<u8> = alloc::vec![b'x'; MAX_PATH_BYTES + 1];
	assert!(!valid_bootstrap_path(&overlong), "and a path past the bound");

	// And the whole-list parser applies it, so the grammar is not merely available.
	assert!(parse_list(b"a.lsexe libexec/a.lsexe\n").is_some(), "a well-formed row");
	assert!(parse_list(b"a.lsexe libexec/../etc/a.lsexe\n").is_none(), "a row whose path escapes is refused HERE");
	assert!(parse_list(b"a.lsexe /libexec/a.lsexe\n").is_none(), "and so is one that is not relative");

	// THE REAL LIST STILL PARSES, which is the half a grammar can get wrong in the other direction.
	let real = b"device_manager.lsexe libexec/device_manager.lsexe\nstorage_service.lsexe libexec/storage_service.lsexe\n";
	assert_eq!(parse_list(real).map(|rows| rows.len()), Some(2), "the shape the build actually writes");
}
