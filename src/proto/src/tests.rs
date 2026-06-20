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

// An in-memory loopback transport that dispatches a request straight into a
// Service and returns the encoded reply - the host stand-in for a channel.
struct Loopback<S: log::Service> {
	service: S,
}

impl<S: log::Service> crate::codec::Transport for Loopback<S> {
	fn call(&mut self, request: &[u8]) -> Option<Vec<u8>> {
		let mut out = [0u8; 4096];
		let n = log::dispatch(&mut self.service, request, &mut out)?;
		Some(out[..n].to_vec())
	}
}

// A trivial in-memory Log service for the round-trip test.
#[derive(Default)]
struct MemLog {
	entries: Vec<Entry>,
}

impl log::Service for MemLog {
	fn emit(&mut self, e: Entry) -> Result<(), Error> {
		self.entries.push(e);
		Ok(())
	}

	fn query(&mut self, _q: Query) -> Result<Vec<Entry>, Error> {
		Ok(self.entries.clone())
	}
}

#[test]
fn client_server_round_trip() {
	let mut client = log::Client::new(Loopback { service: MemLog::default() });
	let e = Entry { timestamp: 9, severity: Severity::Error, source: String::from("svc"), fields: Vec::new() };
	assert_eq!(client.emit(&e), Some(Ok(())));
	let q = Query { since: None, min_severity: None, source: None, limit: 0 };
	assert_eq!(client.query(&q), Some(Ok(alloc::vec![e])));
}
