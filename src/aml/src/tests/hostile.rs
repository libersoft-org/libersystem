//! HOSTILE AML: firmware that loops for ever, recurses without end, nests past the bounds, reaches a region outside
//! the policy, hands `Load` a bad table, writes a GPIO line or `Unload`s. Each is aborted and named; the namespace
//! and the next evaluation are unharmed.

use std::vec;

use super::build::*;
use super::{eval, failure, integer, load};
use crate::error::{Bound, Error, Limits};
use crate::interp::Aml;
use crate::object::Space;

#[test]
fn an_endless_loop_is_aborted_at_the_loop_bound() {
	let body = cat(&[method("SPIN", 0, while_(int(1), vec![0xA3])), method("OKAY", 0, ret(int(1)))]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\SPIN", &[])), Error::Bound(Bound::Loop));
	assert_eq!(integer(eval(&mut aml, &mut model, "\\OKAY", &[])), 1, "the next evaluation runs");
}

#[test]
fn a_loop_that_sleeps_is_aborted_at_the_time_bound() {
	let body = method("NAP_", 0, while_(int(1), sleep(int(100))));
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\NAP_", &[])), Error::Bound(Bound::Time));
}

#[test]
fn deep_recursion_is_aborted_at_the_call_bound() {
	let body = method("DEEP", 1, ret(super::conformance::method_call("DEEP", &[add(arg(0), int(1), vec![0x00])])));
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\DEEP", &[0])), Error::Bound(Bound::Calls));
}

#[test]
fn a_package_past_the_depth_bound_is_refused() {
	let mut nested = int(1);
	for _ in 0..20 {
		nested = package(&[nested]);
	}
	let body = method("NEST", 0, ret(nested));
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\NEST", &[])), Error::Bound(Bound::PackageDepth));
}

#[test]
fn an_allocation_past_the_memory_bound_is_refused() {
	let body = method("HUGE", 0, ret(buffer_of(0x1000_0000)));
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\HUGE", &[])), Error::Bound(Bound::Memory));
	// Growing a string in a loop reaches the bound too.
	let grow = method("GROW", 0, cat(&[store(string("0123456789abcdef0123456789abcdef"), local(0)), while_(int(1), concat(local(0), local(0), local(0)))]));
	let (mut aml, mut model) = load(&grow);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\GROW", &[])), Error::Bound(Bound::Memory));
}

#[test]
fn opcodes_past_the_step_bound_are_aborted() {
	let mut limits = Limits::default();
	limits.steps = 1000;
	limits.loop_iterations = u64::MAX;
	let mut aml = Aml::new(limits);
	let mut model = super::Model::default();
	aml.load(&table("DSDT", 2, &method("BUSY", 0, while_(int(1), vec![0xA3]))), &mut model).unwrap();
	let node = aml.lookup("\\BUSY").unwrap();
	assert_eq!(failure(aml.evaluate(node, &[], &mut model)), Error::Bound(Bound::Steps));
}

#[test]
fn a_region_outside_the_policy_is_refused_by_the_host_and_reported() {
	let body = cat(&[
		op_region("DMA0", 1, int(0x00), int(0x10)),
		field("DMA0", 0x01, &[named_field("CH0A", 8)]),
		op_region("POST", 1, int(0x80), int(1)),
		field("POST", 0x01, &[named_field("CODE", 8)]),
		method("DMAW", 0, store(int(0x55), name("CH0A"))),
		method("POSW", 0, store(int(0x42), name("CODE"))),
	]);
	let (mut aml, mut model) = load(&body);
	// THE ISA DMA CONTROLLER'S REGISTERS ARE THE KERNEL'S; the POST-code port is not.
	model.refused.push((Space::SystemIo, 0x00, 0x20));
	match eval(&mut aml, &mut model, "\\DMAW", &[]) {
		Err(Error::Region(crate::host::HostError::Refused(_))) => {}
		other => panic!("{other:?}"),
	}
	eval(&mut aml, &mut model, "\\POSW", &[]).expect("port 0x80 is mintable");
	assert_eq!(model.io[&0x80], 0x42);
}

#[test]
fn a_field_past_its_region_is_refused_before_any_access() {
	let body = cat(&[op_region("TINY", 0, int(0x100), int(2)), field("TINY", 0x03, &[named_field("WIDE", 32)]), method("READ", 0, ret(name("WIDE")))]);
	let (mut aml, mut model) = load(&body);
	assert!(matches!(eval(&mut aml, &mut model, "\\READ", &[]), Err(Error::Refused(_))));
	assert!(model.accesses.is_empty());
}

#[test]
fn a_bad_table_given_to_load_is_refused_and_the_namespace_unchanged() {
	let mut bad = table("SSDT", 2, &method("\\BADM", 0, ret(int(1))));
	bad[9] = bad[9].wrapping_add(1);
	let wrong_signature = table("FACP", 2, &[]);
	let body = cat(&[
		name_obj("TBL0", buffer(&bad)),
		name_obj("TBL1", buffer(&wrong_signature)),
		method("LDBD", 0, load_op(name("TBL0"), local(0))),
		method("LDWR", 0, load_op(name("TBL1"), local(0))),
	]);
	let (mut aml, mut model) = load(&body);
	assert!(matches!(eval(&mut aml, &mut model, "\\LDBD", &[]), Err(Error::BadTable(_))));
	assert!(matches!(eval(&mut aml, &mut model, "\\LDWR", &[]), Err(Error::BadTable(_))));
	assert!(aml.lookup("\\BADM").is_none());
	assert_eq!(aml.tables(), 1);
}

#[test]
fn unload_is_refused() {
	let body = method("UNLD", 0, cat(&[store(int(1), local(0)), unload(local(0))]));
	let (mut aml, mut model) = load(&body);
	assert!(matches!(eval(&mut aml, &mut model, "\\UNLD", &[]), Err(Error::Refused(_))));
}

#[test]
fn unknown_opcodes_malformed_lengths_and_missing_names_are_named() {
	let body = cat(&[method("UNKN", 0, vec![0x02]), method("MISS", 0, ret(name("\\NOPE")))]);
	let (mut aml, mut model) = load(&body);
	assert!(matches!(eval(&mut aml, &mut model, "\\UNKN", &[]), Err(Error::UnknownOpcode(0x02, _))));
	assert!(matches!(eval(&mut aml, &mut model, "\\MISS", &[]), Err(Error::NotFound(_))));
	// A package length that runs past the table.
	let mut broken = method("BRKN", 0, ret(int(1)));
	broken[1] = 0x3F;
	let mut aml = Aml::new(Limits::default());
	let mut model = super::Model::default();
	assert!(matches!(aml.load(&table("DSDT", 2, &broken), &mut model), Err(Error::Malformed(..))));
	// A truncated table header, a checksum that does not sum.
	assert!(matches!(aml.load(&[b'S', b'S', b'D', b'T'], &mut model), Err(Error::BadTable(_))));
}

#[test]
fn fatal_ends_the_evaluation_and_reaches_the_host() {
	let body = method("FATL", 0, fatal(0xA0, 0x1234, int(7)));
	let (mut aml, mut model) = load(&body);
	assert_eq!(failure(eval(&mut aml, &mut model, "\\FATL", &[])), Error::Fatal { kind: 0xA0, code: 0x1234, argument: 7 });
	assert_eq!(model.fatal, Some((0xA0, 0x1234, 7)));
}

#[test]
fn a_name_declared_twice_at_table_level_keeps_the_first_and_is_reported() {
	let body = cat(&[name_obj("DUPL", int(1)), name_obj("DUPL", int(2))]);
	let (mut aml, mut model) = load(&body);
	assert_eq!(integer(eval(&mut aml, &mut model, "\\DUPL", &[])), 1);
	assert_eq!(aml.warnings.len(), 1);
}

#[test]
fn a_wait_nothing_can_end_is_refused_rather_than_hung() {
	let body = cat(&[event("EVT0"), method("HANG", 0, ret(wait(name("EVT0"), int(0xFFFF))))]);
	let (mut aml, mut model) = load(&body);
	assert!(matches!(eval(&mut aml, &mut model, "\\HANG", &[]), Err(Error::Refused(_))));
}
