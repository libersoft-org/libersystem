//! THE CONFORMANCE SUITE: the specification's constructs, each asserted by what a method answers and what the
//! register model saw.

use std::string::String;
use std::vec;

use super::build::*;
use super::{eval, integer, load, load_revision};
use crate::object::{Object, Space};

#[test]
fn arithmetic_and_control_flow_answer_what_the_specification_says() {
	// Method (SUMN, 1) { Local0 = 0; Local1 = 0; While (Local1 < Arg0) { Local1++; Local0 += Local1 } Return (Local0) }
	let body = method("SUMN", 1, cat(&[store(int(0), local(0)), store(int(0), local(1)), while_(lless(local(1), arg(0)), cat(&[increment(local(1)), add(local(0), local(1), local(0))])), ret(local(0))]));
	// Method (PICK, 1) { If (Arg0 == 1) { Return (0x10) } Else { If (Arg0 > 5) { Return (0x20) } } Return (0x30) }
	let pick = method("PICK", 1, cat(&[if_(lequal(arg(0), int(1)), ret(int(0x10))), else_(if_(lgreater(arg(0), int(5)), ret(int(0x20)))), ret(int(0x30))]));
	// Method (BRK_) { Local0 = 0; While (One) { Local0++; If (Local0 == 7) { Break } } Return (Local0) }
	let brk_method = method("BRK_", 0, cat(&[store(int(0), local(0)), while_(int(1), cat(&[increment(local(0)), if_(lequal(local(0), int(7)), brk())])), ret(local(0))]));
	// Method (ARIT) { Divide (100, 7, Local1, Local0); Return (Local0 * 1000 + Local1 + (13 % 5) * 100000) }
	let arit = method(
		"ARIT",
		0,
		cat(&[
			divide(int(100), int(7), local(1), local(0)),
			multiply(local(0), int(1000), local(2)),
			add(local(2), local(1), local(2)),
			modulo(int(13), int(5), local(3)),
			multiply(local(3), int(100000), local(3)),
			add(local(2), local(3), local(2)),
			ret(local(2)),
		]),
	);
	let bits = method(
		"BITS",
		0,
		cat(&[
			shift_left(int(1), int(12), local(0)),
			find_set_left_bit(local(0), local(1)),
			not(int(0), local(2)),
			and(local(2), int(0xF0), local(2)),
			or(local(2), local(1), local(2)),
			ret(local(2)),
		]),
	);
	let (mut aml, mut model) = load(&cat(&[body, pick, brk_method, arit, bits]));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\SUMN", &[10])), 55);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\PICK", &[1])), 0x10);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\PICK", &[9])), 0x20);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\PICK", &[3])), 0x30);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\BRK_", &[])), 7);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\ARIT", &[])), 14 * 1000 + 2 + 3 * 100000);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\BITS", &[])), 0xF0 | 13);
}

#[test]
fn a_revision_one_dsdt_is_a_32_bit_namespace() {
	let body = method("WRAP", 0, cat(&[store(int(0xFFFF_FFFF), local(0)), increment(local(0)), ret(local(0))]));
	let (mut aml, mut model) = load_revision(&body, 1);
	assert!(!aml.int64);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\WRAP", &[])), 0, "an integer wraps at 32 bits");
	let ones = method("ONES", 0, ret(vec![0xFF]));
	let (mut aml, mut model) = load_revision(&ones, 1);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\ONES", &[])), 0xFFFF_FFFF);
	let (mut aml, mut model) = load_revision(&ones, 2);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\ONES", &[])), u64::MAX);
}

#[test]
fn strings_buffers_and_packages_convert_and_index_as_specified() {
	let body = cat(&[
		name_obj("PKG0", package(&[int(1), string("two"), buffer(&[3, 4]), package(&[int(5)])])),
		name_obj("BUF0", buffer(&[0x10, 0x20, 0x30, 0x40])),
		name_obj("STR0", string("ABCDEF")),
		// Index + DerefOf, SizeOf of each kind.
		method(
			"IDX_",
			0,
			cat(&[
				store(deref(index(name("PKG0"), int(0), vec![0x00])), local(0)),
				add(local(0), sizeof(name("PKG0")), local(0)),
				add(local(0), sizeof(name("BUF0")), local(0)),
				add(local(0), sizeof(name("STR0")), local(0)),
				ret(local(0)),
			]),
		),
		// A store through an Index reference changes the package itself.
		method("SETE", 0, cat(&[store(int(0x77), index(name("PKG0"), int(0), vec![0x00])), ret(deref(index(name("PKG0"), int(0), vec![0x00])))])),
		// A buffer byte through Index.
		method("SETB", 0, cat(&[store(int(0x99), index(name("BUF0"), int(2), vec![0x00])), ret(name("BUF0"))])),
		method("CAT_", 0, ret(concat(name("STR0"), string("GH"), vec![0x00]))),
		method("CATB", 0, ret(concat(buffer(&[1]), buffer(&[2]), vec![0x00]))),
		method("CATI", 0, ret(concat(int(1), int(2), vec![0x00]))),
		method("HEX_", 0, ret(to_hex_string(int(0x2A), vec![0x00]))),
		method("DEC_", 0, ret(to_decimal_string(int(42), vec![0x00]))),
		method("TINT", 0, cat(&[to_integer(string("0x1F"), local(0)), to_integer(string("123"), local(1)), add(local(0), local(1), local(0)), ret(local(0))])),
		method("TBUF", 0, ret(to_buffer(int(0x0102), vec![0x00]))),
		method("TSTR", 0, ret(to_string(buffer(&[b'h', b'i', 0, b'x']), vec![0xFF], vec![0x00]))),
		method("MID_", 0, ret(mid(name("STR0"), int(2), int(3), vec![0x00]))),
		method("MTCH", 0, ret(match_(package(&[int(3), int(8), int(12), int(20)]), 5, int(8), 3, int(20), int(0)))),
		method("SCMP", 0, ret(land(lless(string("ABC"), string("ABD")), lequal(buffer(&[1, 2]), buffer(&[1, 2]))))),
		method("IMPL", 0, cat(&[store(string("0x10"), local(0)), add(local(0), int(1), local(1)), ret(local(1))])),
	]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\IDX_", &[])), 1 + 4 + 4 + 6);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\SETE", &[])), 0x77);
	match eval(&mut aml, &mut model, "\\SETB", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![0x10, 0x20, 0x99, 0x40]),
		other => panic!("{other:?}"),
	}
	match eval(&mut aml, &mut model, "\\CAT_", &[]).unwrap() {
		Some(Object::String(text)) => assert_eq!(text, "ABCDEFGH"),
		other => panic!("{other:?}"),
	}
	match eval(&mut aml, &mut model, "\\CATB", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![1, 2]),
		other => panic!("{other:?}"),
	}
	match eval(&mut aml, &mut model, "\\CATI", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]),
		other => panic!("{other:?}"),
	}
	assert!(matches!(eval(&mut aml, &mut model, "\\HEX_", &[]).unwrap(), Some(Object::String(text)) if text == "0x2A"));
	assert!(matches!(eval(&mut aml, &mut model, "\\DEC_", &[]).unwrap(), Some(Object::String(text)) if text == "42"));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\TINT", &[])), 0x1F + 123);
	assert!(matches!(eval(&mut aml, &mut model, "\\TBUF", &[]).unwrap(), Some(Object::Buffer(bytes)) if bytes == vec![2, 1, 0, 0, 0, 0, 0, 0]));
	assert!(matches!(eval(&mut aml, &mut model, "\\TSTR", &[]).unwrap(), Some(Object::String(text)) if text == "hi"));
	assert!(matches!(eval(&mut aml, &mut model, "\\MID_", &[]).unwrap(), Some(Object::String(text)) if text == "CDE"));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\MTCH", &[])), 2, "the first element greater than 8 and less than 20");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\SCMP", &[])), u64::MAX);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\IMPL", &[])), 0x11, "a string operand is read as hexadecimal");
}

#[test]
fn a_store_converts_to_the_target_and_copy_object_replaces_it() {
	let body = cat(&[
		name_obj("INT0", int(5)),
		name_obj("BUF0", buffer(&[0, 0, 0])),
		name_obj("STR0", string("x")),
		method("STOR", 0, cat(&[store(string("0x21"), name("INT0")), store(int(0x0A0B0C0D), name("BUF0")), store(int(0x41), name("STR0")), ret(name("INT0"))])),
		method("COPY", 0, cat(&[copy_object(string("now a string"), name("INT0")), ret(object_type(name("INT0")))])),
		// A reference passed in an argument: a store to the argument goes through it.
		method("SETR", 1, store(int(0x55), arg(0))),
		name_obj("TEMP", int(0)),
		method("REF_", 0, cat(&[store(refof(name("TEMP")), local(1)), method_call("SETR", &[local(1)]), ret(name("TEMP"))])),
	]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\STOR", &[])), 0x21, "a string stored into an integer is converted");
	let buf = aml.lookup("\\BUF0").unwrap();
	assert!(matches!(&*aml.ns.object(buf).unwrap().borrow(), Object::Buffer(bytes) if *bytes == vec![0x0D, 0x0C, 0x0B]), "a buffer target keeps its length");
	let text = aml.lookup("\\STR0").unwrap();
	assert!(matches!(&*aml.ns.object(text).unwrap().borrow(), Object::String(value) if value == "0000000000000041"));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\COPY", &[])), crate::object::kind::STRING, "CopyObject replaced the integer");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\REF_", &[])), 0x55, "a store to an argument holding a reference goes through it");
}

pub fn method_call(n: &str, args: &[std::vec::Vec<u8>]) -> std::vec::Vec<u8> {
	let mut out = name(n);
	for a in args {
		out.extend_from_slice(a);
	}
	out
}

#[test]
fn methods_call_methods_and_their_own_names_go_when_they_return() {
	let body = cat(&[
		method("DBL_", 1, ret(multiply(arg(0), int(2), vec![0x00]))),
		method("QUAD", 1, ret(method_call("DBL_", &[method_call("DBL_", &[arg(0)])]))),
		method("FACT", 1, cat(&[if_(lless(arg(0), int(2)), ret(int(1))), ret(multiply(arg(0), method_call("FACT", &[subtract(arg(0), int(1), vec![0x00])]), vec![0x00]))])),
		// A method that declares a name of its own: called twice, it does not find the first call's.
		method("TMPN", 0, cat(&[name_obj("XTMP", int(3)), increment(name("XTMP")), ret(name("XTMP"))])),
	]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\QUAD", &[3])), 12);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\FACT", &[10])), 3_628_800);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\TMPN", &[])), 4);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\TMPN", &[])), 4, "the name was gone and made again");
	assert!(aml.lookup("\\TMPN.XTMP").is_none());
}

#[test]
fn names_resolve_by_the_search_rule_scopes_aliases_and_externals() {
	let body = cat(&[
		name_obj("GLOB", int(7)),
		scope("\\_SB", cat(&[device("DEV0", cat(&[name_obj("LOCL", int(1)), method("READ", 0, ret(add(name("GLOB"), name("LOCL"), vec![0x00])))])), alias("\\_SB.DEV0.LOCL", "ALIA")])),
		external("\\_SB.LATE", 8, 1),
		method("CHEK", 0, cat(&[if_(cond_refof(name("\\_SB.LATE"), vec![0x00]), ret(int(1))), ret(int(0))])),
		method("PARN", 0, ret(name("^GLOB"))),
	]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.DEV0.READ", &[])), 8, "GLOB found up the scope chain");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.ALIA", &[])), 1);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\CHEK", &[])), 0, "an External nothing defined is not there to CondRefOf");
	// An SSDT defines it: the External placeholder is replaced.
	let ssdt = table("SSDT", 2, &scope("\\_SB", method("LATE", 1, ret(add(arg(0), int(100), vec![0x00])))));
	aml.load(&ssdt, &mut model).expect("the SSDT loads");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\CHEK", &[])), 1);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.LATE", &[5])), 105);
}

#[test]
fn osi_os_and_rev_say_what_this_system_decided() {
	let body = cat(&[
		method("WIN_", 0, ret(method_call("\\_OSI", &[string("Windows 2015")]))),
		method("LNX_", 0, ret(method_call("\\_OSI", &[string("Linux")]))),
		method("DRWN", 0, ret(method_call("\\_OSI", &[string("Darwin")]))),
		method("FEAT", 0, ret(method_call("\\_OSI", &[string("Module Device")]))),
		method("OSNM", 0, ret(name("\\_OS"))),
		method("REVI", 0, ret(name("\\_REV"))),
	]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\WIN_", &[])), u64::MAX);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\LNX_", &[])), 0);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\DRWN", &[])), 0);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\FEAT", &[])), u64::MAX);
	assert!(matches!(eval(&mut aml, &mut model, "\\OSNM", &[]).unwrap(), Some(Object::String(text)) if text == "Microsoft Windows NT"));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\REVI", &[])), 2);
}

#[test]
fn memory_fields_are_reached_in_their_access_width_and_merged_by_their_update_rule() {
	let body = cat(&[
		op_region("RAM0", 0, int(0x1000), int(0x20)),
		// Field (RAM0, ByteAcc, NoLock, Preserve) { B0, 8, W1, 16, , 4, N4, 4, D2, 32 }
		field("RAM0", 0x01, &[named_field("B0__", 8), named_field("W1__", 16), reserved(4), named_field("N4__", 4), named_field("D2__", 32)]),
		// Field (RAM0, DWordAcc, NoLock, WriteAsOnes) { Offset (0x10), X1, 3, X2, 5 }
		field("RAM0", 0x03 | 0x20, &[reserved(0x80), named_field("X1__", 3), named_field("X2__", 5)]),
		method(
			"WRIT",
			0,
			cat(&[
				store(int(0xAB), name("B0__")),
				store(int(0x1234), name("W1__")),
				store(int(0x9), name("N4__")),
				store(int(0xDEADBEEF), name("D2__")),
				store(int(0x5), name("X1__")),
			]),
		),
		method("READ", 0, ret(add(name("W1__"), name("N4__"), vec![0x00]))),
	]);
	let (mut aml, mut model) = load(&body);
	model.memory.insert(0x1003, 0x0F);
	eval(&mut aml, &mut model, "\\WRIT", &[]).unwrap();
	assert_eq!(model.memory[&0x1000], 0xAB);
	assert_eq!(model.memory[&0x1001], 0x34);
	assert_eq!(model.memory[&0x1002], 0x12);
	assert_eq!(model.memory[&0x1003], 0x9F, "the high nibble written, the low nibble preserved");
	assert_eq!((model.memory[&0x1004], model.memory[&0x1007]), (0xEF, 0xDE));
	// The dword field unit written as ones outside X1: 0xFFFFFFF8 | 5.
	assert_eq!((model.memory[&0x1010], model.memory[&0x1011]), (0xFD, 0xFF));
	assert!(model.accesses.iter().any(|&(space, address, width, write)| space == Space::SystemMemory && address == 0x1010 && width == 32 && write), "a dword field is written as a dword");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\READ", &[])), 0x1234 + 0x9);
}

#[test]
fn index_bank_and_buffer_fields_reach_their_bits() {
	let body = cat(&[
		op_region("IOPT", 1, int(0x70), int(2)),
		field("IOPT", 0x01, &[named_field("INDX", 8), named_field("DATA", 8)]),
		// IndexField (INDX, DATA, ByteAcc, NoLock, Preserve) { Offset (0x10), CM10, 8, CM11, 8 }
		index_field("INDX", "DATA", 0x01, &[reserved(0x80), named_field("CM10", 8), named_field("CM11", 8)]),
		op_region("BNKR", 0, int(0x2000), int(4)),
		field("BNKR", 0x01, &[named_field("BANK", 8)]),
		op_region("BNKD", 0, int(0x3000), int(4)),
		bank_field("BNKD", "BANK", int(2), 0x01, &[named_field("BK20", 8)]),
		name_obj("BUF0", buffer(&[0, 0, 0, 0, 0, 0, 0, 0])),
		method(
			"BFLD",
			0,
			cat(&[
				create_dword_field(name("BUF0"), int(4), "DW04"),
				create_bit_field(name("BUF0"), int(3), "BIT3"),
				create_field(name("BUF0"), int(8), int(12), "F812"),
				store(int(0x11223344), name("DW04")),
				store(int(1), name("BIT3")),
				store(int(0xABC), name("F812")),
				ret(name("BUF0")),
			]),
		),
		method("CMOS", 0, cat(&[store(int(0x5A), name("CM11")), ret(name("CM10"))])),
		method("BNK_", 0, store(int(0x77), name("BK20"))),
	]);
	let (mut aml, mut model) = load(&body);
	model.io.insert(0x71, 0);
	match eval(&mut aml, &mut model, "\\BFLD", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![0x08, 0xBC, 0x0A, 0x00, 0x44, 0x33, 0x22, 0x11]),
		other => panic!("{other:?}"),
	}
	eval(&mut aml, &mut model, "\\CMOS", &[]).unwrap();
	// The index register was written with CM11's offset, then the data register with the value.
	let writes: std::vec::Vec<(u64, u8)> = model.accesses.iter().filter(|access| access.0 == Space::SystemIo && access.3).map(|access| (access.1, access.2)).collect();
	assert!(writes.contains(&(0x70, 8)) && writes.contains(&(0x71, 8)));
	eval(&mut aml, &mut model, "\\BNK_", &[]).unwrap();
	assert_eq!(model.memory[&0x2000], 2, "the bank was selected");
	assert_eq!(model.memory[&0x3000], 0x77);
}

#[test]
fn a_pci_config_region_reaches_the_function_its_device_names() {
	// Device (\_SB.PCI0) { _HID EISAID("PNP0A08"), _BBN 0 ; Device (S10) { _ADR 0x00020001; OperationRegion (PCFG, PCI_Config, 0, 0x100); Field ... { VID, 16, DID, 16 } } }
	let body = scope(
		"\\_SB",
		device(
			"PCI0",
			cat(&[
				name_obj("_HID", eisaid("PNP0A08")),
				name_obj("_BBN", int(0)),
				device(
					"S10_",
					cat(&[
						name_obj("_ADR", int(0x0002_0001)),
						op_region("PCFG", 2, int(0), int(0x100)),
						field("PCFG", 0x02, &[named_field("VID_", 16), named_field("DID_", 16)]),
						method("RDID", 0, ret(name("DID_"))),
					]),
				),
			]),
		),
	);
	let (mut aml, mut model) = load(&body);
	model.pci.insert((0, 0, 2, 1, 2), 0x34);
	model.pci.insert((0, 0, 2, 1, 3), 0x12);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.PCI0.S10_.RDID", &[])), 0x1234);
}

#[test]
fn notify_mutexes_events_and_the_global_lock() {
	let body = cat(&[
		scope("\\_SB", device("LID0", name_obj("_HID", eisaid("PNP0C0D")))),
		mutex("MUT0", 0),
		event("EVT0"),
		method("NTFY", 0, notify(name("\\_SB.LID0"), int(0x80))),
		method("LOCK", 0, cat(&[store(acquire(name("MUT0"), 0xFFFF), local(0)), release(name("MUT0")), ret(local(0))])),
		method("GLCK", 0, cat(&[store(acquire(name("\\_GL"), 10), local(0)), ret(local(0))])),
		method("GREL", 0, release(name("\\_GL"))),
		method(
			"EVNT",
			0,
			cat(&[
				signal(name("EVT0")),
				store(wait(name("EVT0"), int(5)), local(0)),
				store(wait(name("EVT0"), int(5)), local(1)),
				ret(add(local(0), and(local(1), int(1), vec![0x00]), vec![0x00])),
			]),
		),
	]);
	let (mut aml, mut model) = load(&body);
	eval(&mut aml, &mut model, "\\NTFY", &[]).unwrap();
	assert_eq!(model.notifies, vec![(String::from("\\_SB_.LID0"), 0x80)]);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\LOCK", &[])), 0, "Acquire answers False when it acquired");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\GLCK", &[])), 0);
	assert!(model.global_lock_held);
	eval(&mut aml, &mut model, "\\GREL", &[]).unwrap();
	assert!(!model.global_lock_held);
	model.global_lock_busy = true;
	assert_eq!(integer(eval(&mut aml, &mut model, "\\GLCK", &[])), u64::MAX, "the firmware holds it: the Acquire times out");
	// The first Wait finds the signal; the second waits its timeout and answers True (timed out).
	assert_eq!(integer(eval(&mut aml, &mut model, "\\EVNT", &[])), 1);
}

#[test]
fn a_table_loaded_by_load_table_and_one_by_load_join_the_namespace() {
	let extra = table("SSDT", 2, &method("\\XTRA", 0, ret(int(0x44))));
	let mut other = table("OEM1", 2, &method("\\OEM1", 0, ret(int(0x55))));
	other[10..16].copy_from_slice(b"LIBER ");
	let body = cat(&[
		name_obj("TBL0", buffer(&other)),
		method("LDTB", 0, ret(load_table("SSDT", "LIBER", "TESTTABL"))),
		method("LDBF", 0, cat(&[load_op(name("TBL0"), local(0)), ret(local(0))])),
	]);
	let (mut aml, mut model) = load(&body);
	model.tables.push(extra);
	assert!(integer(eval(&mut aml, &mut model, "\\LDTB", &[])) != 0, "a handle");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\XTRA", &[])), 0x44);
	assert!(integer(eval(&mut aml, &mut model, "\\LDBF", &[])) != 0);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\OEM1", &[])), 0x55);
	assert_eq!(aml.tables(), 3);
}

#[test]
fn the_walk_runs_sta_and_ini_in_order_and_skips_what_is_absent() {
	let body = scope(
		"\\_SB",
		cat(&[
			method("_INI", 0, store(string("SB._INI"), debug())),
			device(
				"PRES",
				cat(&[
					method("_STA", 0, ret(int(0x0F))),
					method("_INI", 0, store(string("PRES._INI"), debug())),
					device("KID1", method("_INI", 0, store(string("KID1._INI"), debug()))),
				]),
			),
			device("ABSN", cat(&[method("_STA", 0, ret(int(0))), device("KID2", method("_INI", 0, store(string("KID2._INI"), debug())))])),
			device("FUNC", cat(&[method("_STA", 0, ret(int(0x08))), device("KID3", method("_INI", 0, store(string("KID3._INI"), debug())))])),
		]),
	);
	let (mut aml, mut model) = load(&body);
	let walk = aml.initialize(&mut model);
	assert!(walk.failures.is_empty(), "{:?}", walk.failures);
	let found: std::vec::Vec<String> = walk.found.iter().map(|found| found.path.text()).collect();
	assert_eq!(found, vec!["\\_SB_.FUNC.KID3", "\\_SB_.PRES", "\\_SB_.PRES.KID1"]);
	assert_eq!(model.debug, vec!["SB._INI", "KID3._INI", "PRES._INI", "KID1._INI"], "a present device's _INI before its children's; an absent one's children skipped; a functioning one's walked without its own");
}

#[test]
fn identity_resources_and_device_properties_decode() {
	let crs = template(&[memory32_fixed(true, 0xFED4_0000, 0x1000), io(0x3F8, 8), irq_no_flags(&[4])]);
	let dsd = package(&[
		uuid(crate::dsd::DEVICE_PROPERTIES),
		package(&[
			package(&[string("compatible"), string("liber,fixture")]),
			package(&[string("reg"), int(0x50)]),
			package(&[string("gpios"), package(&[name("\\_SB.GPI0"), int(3), int(0), int(0)])]),
		]),
		uuid(crate::dsd::HIERARCHICAL_DATA),
		package(&[package(&[string("led@0"), name("LED0")])]),
	]);
	let led0 = package(&[uuid(crate::dsd::DEVICE_PROPERTIES), package(&[package(&[string("label"), string("status")])])]);
	let body = scope(
		"\\_SB",
		cat(&[
			device("GPI0", name_obj("_HID", string("LSFXGPIO"))),
			device(
				"FIX0",
				cat(&[
					name_obj("_HID", string("LSFX0001")),
					name_obj("_CID", package(&[eisaid("PNP0C02"), string("PRP0001")])),
					name_obj("_UID", int(0)),
					name_obj("_CRS", buffer(&crs)),
					name_obj("_DSD", dsd),
					name_obj("LED0", led0),
				]),
			),
		]),
	);
	let (mut aml, mut model) = load(&body);
	let node = aml.lookup("\\_SB.FIX0").unwrap();
	let identity = aml.identity(node, &mut model).unwrap();
	assert_eq!(identity.hid.as_deref(), Some("LSFX0001"));
	assert_eq!(identity.cids, vec![String::from("PNP0C02"), String::from("PRP0001")]);
	assert_eq!(identity.uid.as_deref(), Some("0"));
	let resources = aml.resources(node, &mut model).unwrap();
	assert_eq!(resources[0], crate::resource::Resource::Memory { base: 0xFED4_0000, length: 0x1000, writable: true });
	assert_eq!(resources[1], crate::resource::Resource::Io { base: 0x3F8, length: 8 });
	assert!(matches!(resources[2], crate::resource::Resource::Interrupt { line: 4, level: false, .. }));
	let properties = aml.properties(node, &mut model).unwrap();
	assert_eq!(properties.get("compatible"), Some(&crate::dsd::Value::String(String::from("liber,fixture"))));
	assert_eq!(properties.get("reg"), Some(&crate::dsd::Value::Integer(0x50)));
	match properties.get("gpios") {
		Some(crate::dsd::Value::Package(values)) => assert_eq!(values[0], crate::dsd::Value::Reference(String::from("\\_SB_.GPI0"))),
		other => panic!("{other:?}"),
	}
	assert_eq!(properties.children.len(), 1);
	assert_eq!(properties.children[0].0, "led@0");
	assert_eq!(properties.children[0].1.get("label"), Some(&crate::dsd::Value::String(String::from("status"))));
}

#[test]
fn osc_and_dsm_dispatch_by_uuid_and_function() {
	// _OSC: CreateDWordField (Arg3, 4, CDW2); CreateDWordField (Arg3, 8, CDW3); If (Arg0 == ToUUID(PCIe)) { CDW3 &= 0x1D } Else { CDW1 |= 4 }; Return (Arg3)
	let pcie = "33db4d5b-1ff7-401c-9657-7441c03dd766";
	let osc = method(
		"_OSC",
		4,
		cat(&[
			create_dword_field(arg(3), int(0), "CDW1"),
			create_dword_field(arg(3), int(8), "CDW3"),
			if_(lequal(arg(0), uuid(pcie)), and(name("CDW3"), int(0x1D), name("CDW3"))),
			else_(or(name("CDW1"), int(4), name("CDW1"))),
			ret(arg(3)),
		]),
	);
	let fixture = "4a5e8f45-1d6b-4a2c-9c35-5c8a6d2e7f10";
	let dsm = method(
		"_DSM",
		4,
		cat(&[
			if_(lequal(arg(0), uuid(fixture)), cat(&[if_(lequal(arg(2), int(0)), ret(buffer(&[0x03]))), if_(lequal(arg(2), int(1)), ret(add(deref(index(arg(3), int(0), vec![0x00])), int(1), vec![0x00])))])),
			ret(buffer(&[0])),
		]),
	);
	let body = scope("\\_SB", device("PCI0", cat(&[name_obj("_HID", eisaid("PNP0A08")), osc, dsm])));
	let (mut aml, mut model) = load(&body);
	let node = aml.lookup("\\_SB.PCI0").unwrap();
	let answered = aml.osc(node, pcie, 1, &[0, 0x1F, 0x1F], &mut model).unwrap().unwrap();
	assert_eq!(answered, vec![0, 0x1F, 0x1D], "control granted but for hot-plug's bit");
	let answered = aml.osc(node, "0811b06e-4a27-44f9-8d60-3cbbc22e7b48", 1, &[0, 0x1F], &mut model).unwrap().unwrap();
	assert_eq!(answered[0], 4, "an unknown UUID is answered as unrecognised");
	match aml.dsm(node, fixture, 1, 0, Object::Package(std::vec::Vec::new()), &mut model).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![3], "functions 0 and 1 supported"),
		other => panic!("{other:?}"),
	}
	let args = Object::Package(vec![crate::object::obj(Object::Integer(41))]);
	assert!(matches!(aml.dsm(node, fixture, 1, 1, args, &mut model).unwrap(), Some(Object::Integer(42))));
}

#[test]
fn a_serial_bus_field_reads_and_writes_its_device_and_a_gpio_field_reads_its_line() {
	let i2c_connection = i2c(0x50, 100_000, "\\_SB.I2C0");
	let gpio_connection = gpio(false, 0x01, &[7, 9], "\\_SB.GPI0");
	let body = cat(&[
		op_region("GSB0", 9, int(0), int(0x100)),
		// Field (GSB0, BufferAcc, NoLock, Preserve) { Connection (I2cSerialBusV2 (0x50, ...)), Offset (0x10), AccessAs (BufferAcc, AttribByte), REG1, 8 }
		field("GSB0", 0x05, &[connection(&i2c_connection), reserved(0x80), access_as(0x05, 0x06), named_field("REG1", 8)]),
		op_region("GPO0", 8, int(0), int(2)),
		field("GPO0", 0x01, &[connection(&gpio_connection), named_field("LIN0", 1), named_field("LIN1", 1)]),
		name_obj("BUFF", buffer(&[0, 1, 0x5A])),
		method("GSBW", 0, store(name("BUFF"), name("REG1"))),
		method("GSBR", 0, ret(name("REG1"))),
		method("GPIR", 0, ret(add(name("LIN0"), multiply(name("LIN1"), int(2), vec![0x00]), vec![0x00]))),
		method("GPIW", 0, store(int(1), name("LIN0"))),
	]);
	let (mut aml, mut model) = load(&body);
	eval(&mut aml, &mut model, "\\GSBW", &[]).unwrap();
	assert_eq!(model.i2c.get(&(0x50, 0x10)), Some(&0x5A), "the byte written at command 0x10 of the device at 0x50");
	model.i2c.insert((0x50, 0x10), 0xC3);
	match eval(&mut aml, &mut model, "\\GSBR", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![0, 1, 0xC3], "status, length, data"),
		other => panic!("{other:?}"),
	}
	model.gpio.insert(9, true);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\GPIR", &[])), 2, "line 9 high, line 7 low");
	let refused = eval(&mut aml, &mut model, "\\GPIW", &[]).unwrap_err();
	assert!(matches!(refused, crate::error::Error::Refused(ref why) if why.contains("GeneralPurposeIo")), "{refused:?}");
}

#[test]
fn resource_templates_concatenate_and_bcd_converts() {
	let body = cat(&[
		method("CRES", 0, ret(concat_res(buffer(&template(&[io(0x60, 1)])), buffer(&template(&[io(0x64, 1)])), vec![0x00]))),
		method("BCD_", 0, cat(&[vec![0x5B, 0x29], int(1234), local(0), vec![0x5B, 0x28], local(0), local(1), ret(add(multiply(local(0), int(0x10000), vec![0x00]), local(1), vec![0x00]))])),
	]);
	let (mut aml, mut model) = load(&body);
	match eval(&mut aml, &mut model, "\\CRES", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => {
			let decoded = crate::resource::decode(&bytes).unwrap();
			assert_eq!(decoded, vec![crate::resource::Resource::Io { base: 0x60, length: 1 }, crate::resource::Resource::Io { base: 0x64, length: 1 }]);
		}
		other => panic!("{other:?}"),
	}
	assert_eq!(integer(eval(&mut aml, &mut model, "\\BCD_", &[])), (0x1234 << 16) | 1234);
}

#[test]
fn run_reg_tells_the_owners_of_a_space_it_is_connected() {
	let body = scope(
		"\\_SB",
		cat(&[
			device(
				"EC0_",
				cat(&[
					name_obj("_HID", eisaid("PNP0C09")),
					name_obj("REGS", int(0)),
					op_region("ERAM", 3, int(0), int(0xFF)),
					method("_REG", 2, if_(lequal(arg(0), int(3)), store(arg(1), name("REGS")))),
				]),
			),
			device("OTHR", cat(&[op_region("SMEM", 0, int(0x100), int(4)), method("_REG", 2, store(string("wrong space"), debug()))])),
		]),
	);
	let (mut aml, mut model) = load(&body);
	let failed = aml.run_reg(Space::EmbeddedControl, true, &mut model);
	assert!(failed.is_empty(), "{failed:?}");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.EC0_.REGS", &[])), 1);
	assert!(model.debug.is_empty(), "a device with no region of that space is not told");
}

#[test]
fn processors_power_resources_and_thermal_zones_are_walked_as_their_kinds() {
	let body = cat(&[
		scope("\\_PR", processor("CPU0", 0, 0x410, 6, method("_STA", 0, ret(int(0x0F))))),
		power_resource("PRES", 0, 0, cat(&[method("_STA", 0, ret(int(1))), method("_ON_", 0, stall(int(10))), method("_OFF", 0, vec![0xA3])])),
		scope("\\_TZ", thermal_zone("TZ00", method("_TMP", 0, ret(int(3000))))),
		method("DOWN", 1, cat(&[store(arg(0), local(0)), decrement(local(0)), ret(lnot(local(0)))])),
	]);
	let (mut aml, mut model) = load(&body);
	let walk = aml.initialize(&mut model);
	let kinds: std::vec::Vec<(String, crate::devices::Kind)> = walk.found.iter().map(|found| (found.path.text(), found.kind)).collect();
	assert!(kinds.contains(&(String::from("\\PRES"), crate::devices::Kind::PowerResource)), "{kinds:?}");
	assert!(kinds.contains(&(String::from("\\_PR_.CPU0"), crate::devices::Kind::Processor)), "{kinds:?}");
	assert!(kinds.contains(&(String::from("\\_TZ_.TZ00"), crate::devices::Kind::ThermalZone)), "{kinds:?}");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_TZ.TZ00._TMP", &[])), 3000);
	eval(&mut aml, &mut model, "\\PRES._ON", &[]).unwrap();
	assert_eq!(integer(eval(&mut aml, &mut model, "\\DOWN", &[1])), u64::MAX, "1 - 1 is zero, and LNot of zero is True");
	assert_eq!(integer(eval(&mut aml, &mut model, "\\DOWN", &[5])), 0);
}

#[test]
fn a_serial_bus_field_in_the_bytes_protocol_moves_its_length() {
	let i2c_connection = i2c(0x50, 100_000, "\\_SB.I2C0");
	let body = cat(&[
		op_region("GSB0", 9, int(0), int(0x100)),
		field("GSB0", 0x05, &[connection(&i2c_connection), reserved(0x100), access_as_bytes(0x05, 0x0B, 3), named_field("BLK3", 8)]),
		name_obj("BUFF", buffer(&[0, 3, 0xA1, 0xA2, 0xA3])),
		method("WRIT", 0, store(name("BUFF"), name("BLK3"))),
		method("READ", 0, ret(name("BLK3"))),
	]);
	let (mut aml, mut model) = load(&body);
	eval(&mut aml, &mut model, "\\WRIT", &[]).unwrap();
	assert_eq!((model.i2c[&(0x50, 0x20)], model.i2c[&(0x50, 0x21)], model.i2c[&(0x50, 0x22)]), (0xA1, 0xA2, 0xA3));
	match eval(&mut aml, &mut model, "\\READ", &[]).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![0, 3, 0xA1, 0xA2, 0xA3]),
		other => panic!("{other:?}"),
	}
}

/// THE HARNESS EMITTER'S SAMPLE (`src/harness/aml_emitter.py --sample`), loaded as QEMU would hand it over: what the
/// fixture SSDTs are built with runs in this interpreter. The emitter's self-test regenerates the sample and fails
/// on any byte that differs, so the two cannot drift apart unnoticed.
#[test]
fn the_harness_emitters_sample_table_loads_and_runs() {
	let sample = include_bytes!("fixtures/emitter-sample.aml");
	let mut aml = crate::interp::Aml::new(crate::error::Limits::default());
	let mut model = super::Model::default();
	aml.load(&table("DSDT", 2, &[]), &mut model).unwrap();
	aml.load(sample, &mut model).expect("the emitter's SSDT loads");
	let node = aml.lookup("\\_SB.FIX0").unwrap();
	let identity = aml.identity(node, &mut model).unwrap();
	assert_eq!((identity.hid.as_deref(), identity.cids.clone(), identity.uid.as_deref()), (Some("LSFX0001"), vec![String::from("PRP0001")], Some("0")));
	model.memory.insert(0x2000, 5);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.FIX0.BUMP", &[3])), 8);
	eval(&mut aml, &mut model, "\\_SB.FIX0.SIGN", &[0x42]).unwrap();
	assert_eq!(model.memory[&0x2004], 0x42);
	assert_eq!(model.notifies, vec![(String::from("\\_SB_.FIX0"), 0x80)]);
	model.i2c.insert((0x50, 0), 0x3C);
	assert!(matches!(eval(&mut aml, &mut model, "\\_SB.FIX0.REG0", &[]).unwrap(), Some(Object::Buffer(bytes)) if bytes == vec![0, 1, 0x3C]));
	model.gpio.insert(4, true);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\_SB.FIX0.LIN4", &[])), 1);
	let resources = aml.resources(node, &mut model).unwrap();
	assert_eq!(resources.len(), 3);
	assert!(matches!(&resources[1], crate::resource::Resource::Gpio { interrupt: true, pins, .. } if *pins == vec![5]));
	assert!(matches!(&resources[2], crate::resource::Resource::I2c { address: 0x51, .. }));
	let properties = aml.properties(node, &mut model).unwrap();
	assert_eq!(properties.get("reg"), Some(&crate::dsd::Value::Integer(0x50)));
	assert_eq!(properties.children[0].1.get("label"), Some(&crate::dsd::Value::String(String::from("status"))));
	let args = Object::Package(vec![crate::object::obj(Object::Integer(9))]);
	assert!(matches!(aml.dsm(node, "4a5e8f45-1d6b-4a2c-9c35-5c8a6d2e7f10", 1, 1, args, &mut model).unwrap(), Some(Object::Integer(10))));
	match aml.dsm(node, "4a5e8f45-1d6b-4a2c-9c35-5c8a6d2e7f10", 1, 0, Object::Package(std::vec::Vec::new()), &mut model).unwrap() {
		Some(Object::Buffer(bytes)) => assert_eq!(bytes, vec![0b11]),
		other => panic!("{other:?}"),
	}
}
