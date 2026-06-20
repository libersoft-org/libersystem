//! Golden and round-trip tests for the generated `system` bindings.
//!
//! The golden test pins the `entry` encoding to the existing `abi::log` wire
//! layout, byte for byte, so the generated codec stays a drop-in replacement.

use crate::codec::{Sink, VecWriter};
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
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		let mut out = [0u8; 4096];
		let mut reply_handle = 0u64;
		let n = log::dispatch(&mut self.service, request, request_handle, &mut out, &mut reply_handle)?;
		Some((out[..n].to_vec(), reply_handle))
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

	fn tail(&mut self, _q: Query) -> Vec<Entry> {
		self.entries.clone()
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

#[test]
fn tail_stream_round_trip() {
	// Drive the generated stream helpers directly: a producer opens the stream
	// (decoding the request, calling the service), frames each item with a
	// sequence number, and a consumer reads the frames back.
	let mut service = MemLog::default();
	let e0 = Entry { timestamp: 1, severity: Severity::Info, source: String::from("a"), fields: Vec::new() };
	let e1 = Entry { timestamp: 2, severity: Severity::Warn, source: String::from("b"), fields: Vec::new() };
	service.entries = alloc::vec![e0.clone(), e1.clone()];

	// Encode a tail request the way the client does (op + corr + query).
	let q = Query { since: None, min_severity: None, source: None, limit: 0 };
	let mut writer = VecWriter::new();
	let w = &mut writer;
	w.u16(log::OP_TAIL).unwrap();
	w.u32(7).unwrap();
	q.write(w).unwrap();
	let request = writer.into_inner();

	let (corr, items) = log::tail_open(&mut service, &request).unwrap();
	assert_eq!(corr, 7);
	assert_eq!(items.len(), 2);

	let mut frame = [0u8; 256];
	for (seq, item) in items.iter().enumerate() {
		let n = log::tail_frame(seq as u32, item, &mut frame).unwrap();
		assert_eq!(log::tail_read(&frame[..n]), Some(item.clone()));
	}
}

// A volume stub whose `open` returns a non-zero handle value. The wire encodes
// only a u32 placeholder for the handle, so if the client recovers this value it
// must have travelled out-of-band (set_handle -> reply_handle -> take_handle).
struct VolStub;

impl volume::Service for VolStub {
	fn open(&mut self, o: OpenOpts) -> Result<OpenResult, Error> {
		if o.path.is_empty() { Err(Error::NotFound) } else { Ok(OpenResult { file: 0xCAFE, size: 42 }) }
	}
}

struct VolLoopback<S: volume::Service> {
	service: S,
}

impl<S: volume::Service> crate::codec::Transport for VolLoopback<S> {
	fn call(&mut self, request: &[u8], request_handle: u64) -> Option<(Vec<u8>, u64)> {
		let mut out = [0u8; 256];
		let mut reply_handle = 0u64;
		let n = volume::dispatch(&mut self.service, request, request_handle, &mut out, &mut reply_handle)?;
		Some((out[..n].to_vec(), reply_handle))
	}
}

#[test]
fn handle_return_crosses_out_of_band() {
	let mut client = volume::Client::new(VolLoopback { service: VolStub });
	let opts = OpenOpts { path: String::from("/x"), write: false, create: false };
	// the file handle (0xCAFE) crosses out-of-band; the size travels in the byte stream.
	assert_eq!(client.open(&opts), Some(Ok(OpenResult { file: 0xCAFE, size: 42 })));
	let empty = OpenOpts { path: String::new(), write: false, create: false };
	assert_eq!(client.open(&empty), Some(Err(Error::NotFound)));
}

#[test]
fn entry_renders_json() {
	let e = Entry { timestamp: 42, severity: Severity::Info, source: String::from("kernel"), fields: Vec::new() };
	assert_eq!(e.to_json(), "{\"timestamp\":42,\"severity\":\"info\",\"source\":\"kernel\",\"fields\":[]}");
}

#[test]
fn entry_renders_json_with_fields() {
	let e = Entry { timestamp: 1, severity: Severity::Warn, source: String::from("s"), fields: alloc::vec![Field { key: String::from("k"), value: String::from("v") }] };
	assert_eq!(e.to_json(), "{\"timestamp\":1,\"severity\":\"warn\",\"source\":\"s\",\"fields\":[{\"key\":\"k\",\"value\":\"v\"}]}");
}

#[test]
fn query_renders_json_with_options_and_kebab_keys() {
	let q = Query { since: Some(5), min_severity: None, source: Some(String::from("svc")), limit: 9 };
	assert_eq!(q.to_json(), "{\"since\":5,\"min-severity\":null,\"source\":\"svc\",\"limit\":9}");
}

#[test]
fn error_renders_json_kebab() {
	assert_eq!(Error::NotFound.to_json(), "\"not-found\"");
}

#[test]
fn entry_renders_text() {
	let e = Entry { timestamp: 42, severity: Severity::Info, source: String::from("kernel"), fields: Vec::new() };
	assert_eq!(e.to_text(), "{timestamp=42, severity=info, source=kernel, fields=[]}");
}

#[test]
fn entry_renders_text_with_fields() {
	let e = Entry { timestamp: 1, severity: Severity::Warn, source: String::from("s"), fields: alloc::vec![Field { key: String::from("k"), value: String::from("v") }] };
	assert_eq!(e.to_text(), "{timestamp=1, severity=warn, source=s, fields=[{key=k, value=v}]}");
}

#[test]
fn query_renders_text_with_option_none() {
	let q = Query { since: Some(5), min_severity: None, source: Some(String::from("svc")), limit: 9 };
	assert_eq!(q.to_text(), "{since=5, min-severity=-, source=svc, limit=9}");
}
