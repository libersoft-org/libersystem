//! Golden and round-trip tests for the generated `system` bindings.
//!
//! The golden test pins the `entry` encoding to the existing `abi::log` wire
//! layout, byte for byte, so the generated codec stays a drop-in replacement.

use crate::system::*;
use alloc::string::String;
use alloc::vec::Vec;

#[test]
fn entry_matches_abi_log_layout() {
	let e = Entry { timestamp: 42, severity: Severity::Info, source: String::from("kernel"), fields: Vec::new() };
	let mut buf = [0u8; 64];
	let n = e.encode(&mut buf).expect("encode");
	let expected: &[u8] = &[
		0x2a, 0, 0, 0, 0, 0, 0, 0,    // timestamp = 42
		0x02, // severity = info
		0x06, 0x00, // source length = 6
		b'k', b'e', b'r', b'n', b'e', b'l', // "kernel"
		0x00, 0x00, // field count = 0
	];
	assert_eq!(&buf[..n], expected);
}

#[test]
fn entry_round_trips() {
	let e = Entry { timestamp: 7, severity: Severity::Warn, source: String::from("svc"), fields: alloc::vec![Field { key: String::from("k"), value: String::from("v") }] };
	let mut buf = [0u8; 128];
	let n = e.encode(&mut buf).unwrap();
	assert_eq!(Entry::decode(&buf[..n]).unwrap(), e);
}

#[test]
fn query_options_round_trip() {
	let q = Query { since: Some(100), min_severity: Some(Severity::Error), source: None, limit: 50 };
	let mut buf = [0u8; 128];
	let n = q.encode(&mut buf).unwrap();
	assert_eq!(Query::decode(&buf[..n]).unwrap(), q);
}

#[test]
fn error_round_trips() {
	for e in [Error::Denied, Error::NotFound, Error::Invalid, Error::Again, Error::Closed] {
		let mut buf = [0u8; 4];
		let n = e.encode(&mut buf).unwrap();
		assert_eq!(Error::decode(&buf[..n]).unwrap(), e);
	}
}

#[test]
fn severity_ordinals_match_abi_log() {
	assert_eq!(Severity::Trace as u8, 0);
	assert_eq!(Severity::Info as u8, 2);
	assert_eq!(Severity::Fatal as u8, 5);
}

#[test]
fn opcodes_are_stable() {
	assert_eq!(log::OP_EMIT, 1);
	assert_eq!(log::OP_QUERY, 2);
	assert_eq!(log::OP_TAIL, 3);
}

#[test]
fn encode_rejects_small_buffer() {
	let e = Entry { timestamp: 1, severity: Severity::Trace, source: String::from("x"), fields: Vec::new() };
	let mut buf = [0u8; 4];
	assert_eq!(e.encode(&mut buf), None);
}
