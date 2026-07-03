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

	fn list(&mut self, _path: String) -> Result<Vec<FileInfo>, Error> {
		Ok(Vec::new())
	}

	fn write(&mut self, path: String, data: crate::codec::Buffer) -> Result<(), Error> {
		// the buffer handle must have travelled out-of-band (set_handle -> request_handle
		// -> take_handle); prove it by succeeding only when it arrives intact.
		if path.is_empty() || data.handle != 0xBEEF || data.len != 5 { Err(Error::Invalid) } else { Ok(()) }
	}

	fn remove(&mut self, path: String) -> Result<(), Error> {
		if path.is_empty() { Err(Error::NotFound) } else { Ok(()) }
	}

	fn snap_create(&mut self, name: String) -> Result<(), Error> {
		if name.is_empty() { Err(Error::Invalid) } else { Ok(()) }
	}

	fn snap_list(&mut self) -> Result<Vec<SnapshotInfo>, Error> {
		Ok(alloc::vec![SnapshotInfo { name: String::from("backup"), generation: 7 }])
	}

	fn snap_delete(&mut self, name: String) -> Result<(), Error> {
		if name.is_empty() { Err(Error::NotFound) } else { Ok(()) }
	}

	fn snap_open(&mut self, snapshot: String, path: String) -> Result<OpenResult, Error> {
		// the file handle must travel out-of-band, exactly like `open`.
		if snapshot.is_empty() || path.is_empty() { Err(Error::NotFound) } else { Ok(OpenResult { file: 0xCAFE, size: 42 }) }
	}

	fn mkdir(&mut self, path: String) -> Result<(), Error> {
		if path.is_empty() { Err(Error::Invalid) } else { Ok(()) }
	}

	fn rmdir(&mut self, path: String) -> Result<(), Error> {
		if path.is_empty() { Err(Error::NotFound) } else { Ok(()) }
	}

	fn capacity(&mut self) -> Result<u64, Error> {
		Ok(0x100000)
	}

	fn status(&mut self) -> Result<VolumeStatus, Error> {
		Ok(VolumeStatus { label: String::from("system"), total_bytes: 0x100000, free_bytes: 0x80000, compression: false, read_only: false })
	}

	fn set_compression(&mut self, _enabled: bool) -> Result<(), Error> {
		Ok(())
	}

	fn fsck(&mut self) -> Result<FsckReport, Error> {
		Ok(FsckReport { checksum_failures: 1, damaged: alloc::vec![String::from("docs/broken.txt")] })
	}

	fn restore(&mut self, path: String, _snapshot: String) -> Result<(), Error> {
		if path.is_empty() { Err(Error::NotFound) } else { Ok(()) }
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
fn write_buffer_handle_crosses_out_of_band() {
	let mut client = volume::Client::new(VolLoopback { service: VolStub });
	let data = crate::codec::Buffer { handle: 0xBEEF, len: 5 };
	// the buffer handle (0xBEEF) crosses out-of-band; the length travels in-stream.
	assert_eq!(client.write("/x", &data), Some(Ok(())));
	// a missing path is rejected.
	assert_eq!(client.write("", &data), Some(Err(Error::Invalid)));
}

#[test]
fn remove_round_trips() {
	let mut client = volume::Client::new(VolLoopback { service: VolStub });
	assert_eq!(client.remove("/x"), Some(Ok(())));
	assert_eq!(client.remove(""), Some(Err(Error::NotFound)));
}

#[test]
fn oversized_reply_degrades_to_a_typed_error() {
	// A reply that outgrows the caller's buffer must not vanish (the client would
	// block forever waiting for it): the dispatch overflow fallback rewrites it as
	// a typed `again` error, which always fits. snap_list's Ok reply (a one-entry
	// snapshot list) needs more than the 6 bytes offered here; the fallback -
	// [corr u32][err tag][again] - is exactly 6.
	let mut request = Vec::new();
	request.extend_from_slice(&volume::OP_SNAP_LIST.to_le_bytes());
	request.extend_from_slice(&7u32.to_le_bytes()); // correlation id
	let mut out = [0u8; 6];
	let mut reply_handle = 0u64;
	let n = volume::dispatch(&mut VolStub, &request, 0, &mut out, &mut reply_handle).expect("the fallback reply should be produced");
	assert_eq!(&out[..4], &7u32.to_le_bytes(), "the fallback keeps the correlation id");
	assert_eq!(out[4], 0, "the fallback is the error arm");
	assert_eq!(Error::decode(&out[5..n]), Some(Error::Again), "the fallback error is `again`");
	// with room to spare the same call succeeds normally.
	let mut big = [0u8; 256];
	let n = volume::dispatch(&mut VolStub, &request, 0, &mut big, &mut reply_handle).expect("the real reply fits");
	assert_eq!(big[4], 1, "the roomy reply is the ok arm");
	assert!(n > 6, "the ok reply carries the snapshot list");
}

#[test]
fn mkdir_rmdir_round_trip() {
	let mut client = volume::Client::new(VolLoopback { service: VolStub });
	assert_eq!(client.mkdir("/d"), Some(Ok(())));
	assert_eq!(client.mkdir(""), Some(Err(Error::Invalid)));
	assert_eq!(client.rmdir("/d"), Some(Ok(())));
	assert_eq!(client.rmdir(""), Some(Err(Error::NotFound)));
}

#[test]
fn snapshot_ops_round_trip() {
	let mut client = volume::Client::new(VolLoopback { service: VolStub });
	// create / delete return unit results.
	assert_eq!(client.snap_create("backup"), Some(Ok(())));
	assert_eq!(client.snap_create(""), Some(Err(Error::Invalid)));
	assert_eq!(client.snap_delete("backup"), Some(Ok(())));
	assert_eq!(client.snap_delete(""), Some(Err(Error::NotFound)));
	// list carries the snapshot records in-stream.
	assert_eq!(client.snap_list(), Some(Ok(alloc::vec![SnapshotInfo { name: String::from("backup"), generation: 7 }])));
	// snap-open returns the file handle out-of-band, just like open.
	assert_eq!(client.snap_open("backup", "/x"), Some(Ok(OpenResult { file: 0xCAFE, size: 42 })));
	assert_eq!(client.snap_open("backup", ""), Some(Err(Error::NotFound)));
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

// CBOR primitive heads use the canonical shortest argument encoding (RFC 8949):
// inline for n < 24, then 1/2/4/8 big-endian bytes.
#[test]
fn cbor_uint_uses_shortest_head() {
	use crate::codec::cbor;
	let mut v = Vec::new();
	cbor::uint(&mut v, 23);
	assert_eq!(v, [0x17]);
	v.clear();
	cbor::uint(&mut v, 24);
	assert_eq!(v, [0x18, 24]);
	v.clear();
	cbor::uint(&mut v, 255);
	assert_eq!(v, [0x18, 255]);
	v.clear();
	cbor::uint(&mut v, 256);
	assert_eq!(v, [0x19, 0x01, 0x00]);
	v.clear();
	cbor::uint(&mut v, 65535);
	assert_eq!(v, [0x19, 0xff, 0xff]);
	v.clear();
	cbor::uint(&mut v, 65536);
	assert_eq!(v, [0x1a, 0x00, 0x01, 0x00, 0x00]);
}

// Negative integers are major type 1 over `-1 - v`.
#[test]
fn cbor_int_encodes_negatives() {
	use crate::codec::cbor;
	let mut v = Vec::new();
	cbor::int(&mut v, -1);
	assert_eq!(v, [0x20]);
	v.clear();
	cbor::int(&mut v, -24);
	assert_eq!(v, [0x37]);
	v.clear();
	cbor::int(&mut v, -25);
	assert_eq!(v, [0x38, 24]);
	v.clear();
	cbor::int(&mut v, 9);
	assert_eq!(v, [0x09]);
}

// Simple values: booleans and null map to fixed major-type-7 bytes.
#[test]
fn cbor_simple_values() {
	use crate::codec::cbor;
	let mut v = Vec::new();
	cbor::boolean(&mut v, false);
	cbor::boolean(&mut v, true);
	cbor::null(&mut v);
	assert_eq!(v, [0xf4, 0xf5, 0xf6]);
}

// An enum case renders as a CBOR text string of its kebab-case name.
#[test]
fn severity_renders_cbor() {
	assert_eq!(Severity::Info.to_cbor(), [0x64, b'i', b'n', b'f', b'o']);
}

#[test]
fn error_renders_cbor_kebab() {
	assert_eq!(Error::NotFound.to_cbor(), [0x69, b'n', b'o', b't', b'-', b'f', b'o', b'u', b'n', b'd']);
}

// A record renders as a definite-length CBOR map keyed by field name, mirroring
// the JSON object structure.
#[test]
fn field_renders_cbor_map() {
	let f = Field { key: String::from("k"), value: String::from("v") };
	assert_eq!(f.to_cbor(), [0xa2, 0x63, b'k', b'e', b'y', 0x61, b'k', 0x65, b'v', b'a', b'l', b'u', b'e', 0x61, b'v']);
}

#[test]
fn entry_renders_cbor_map() {
	let e = Entry { timestamp: 42, severity: Severity::Info, source: String::from("kernel"), fields: Vec::new() };
	let mut want = Vec::new();
	want.push(0xa4); // map(4)
	want.push(0x69);
	want.extend_from_slice(b"timestamp");
	want.extend_from_slice(&[0x18, 42]); // uint 42
	want.push(0x68);
	want.extend_from_slice(b"severity");
	want.push(0x64);
	want.extend_from_slice(b"info");
	want.push(0x66);
	want.extend_from_slice(b"source");
	want.push(0x66);
	want.extend_from_slice(b"kernel");
	want.push(0x66);
	want.extend_from_slice(b"fields");
	want.push(0x80); // array(0)
	assert_eq!(e.to_cbor(), want);
}

// Options collapse to their value or `null`, and kebab-case keys are preserved.
#[test]
fn query_renders_cbor_with_options() {
	let q = Query { since: Some(5), min_severity: None, source: Some(String::from("svc")), limit: 9 };
	let mut want = Vec::new();
	want.push(0xa4); // map(4)
	want.push(0x65);
	want.extend_from_slice(b"since");
	want.push(0x05); // uint 5
	want.push(0x6c);
	want.extend_from_slice(b"min-severity");
	want.push(0xf6); // null
	want.push(0x66);
	want.extend_from_slice(b"source");
	want.push(0x63);
	want.extend_from_slice(b"svc");
	want.push(0x65);
	want.extend_from_slice(b"limit");
	want.push(0x09); // uint 9
	assert_eq!(q.to_cbor(), want);
}

// The System Graph round-trips through its binary wire form: a component carrying
// its kind, state, dependency edges and counters, plus a trace span, survives an
// encode/decode unchanged.
#[test]
fn graph_round_trips() {
	let g = Graph { components: alloc::vec![Component { name: String::from("log-service"), kind: ComponentKind::Service, state: ComponentState::Running, deps: Vec::new(), counters: Counters { messages_sent: 7, messages_received: 3, handles: 5, memory_bytes: 8192, restarts: 0, watchdog_trips: 0, last_failure: String::new() } }, Component { name: String::from("device-manager"), kind: ComponentKind::Service, state: ComponentState::Stopped, deps: alloc::vec![String::from("log-service")], counters: Counters { messages_sent: 1, messages_received: 1, handles: 2, memory_bytes: 4096, restarts: 1, watchdog_trips: 1, last_failure: String::from("hung") } },], spans: alloc::vec![TraceSpan { name: String::from("device.list"), duration_ns: 1234 }] };
	let bytes = g.encode_vec();
	assert_eq!(Graph::decode(&bytes), Some(g));
}

// A component renders to a CBOR map with kebab-case keys, its kind and state as text
// enums, its deps as an array, and its counters as a nested map.
#[test]
fn component_renders_cbor_map() {
	let c = Component { name: String::from("net"), kind: ComponentKind::Driver, state: ComponentState::Failed, deps: alloc::vec![String::from("device-manager")], counters: Counters { messages_sent: 0, messages_received: 0, handles: 1, memory_bytes: 0, restarts: 2, watchdog_trips: 0, last_failure: String::new() } };
	let mut want = Vec::new();
	want.push(0xa5); // map(5)
	want.push(0x64);
	want.extend_from_slice(b"name");
	want.push(0x63);
	want.extend_from_slice(b"net");
	want.push(0x64);
	want.extend_from_slice(b"kind");
	want.push(0x66);
	want.extend_from_slice(b"driver");
	want.push(0x65);
	want.extend_from_slice(b"state");
	want.push(0x66);
	want.extend_from_slice(b"failed");
	want.push(0x64);
	want.extend_from_slice(b"deps");
	want.push(0x81); // array(1)
	want.push(0x6e);
	want.extend_from_slice(b"device-manager");
	want.push(0x68);
	want.extend_from_slice(b"counters");
	want.push(0xa7); // map(7)
	want.push(0x6d);
	want.extend_from_slice(b"messages-sent");
	want.push(0x00); // uint 0
	want.push(0x71);
	want.extend_from_slice(b"messages-received");
	want.push(0x00); // uint 0
	want.push(0x67);
	want.extend_from_slice(b"handles");
	want.push(0x01); // uint 1
	want.push(0x6c);
	want.extend_from_slice(b"memory-bytes");
	want.push(0x00); // uint 0
	want.push(0x68);
	want.extend_from_slice(b"restarts");
	want.push(0x02); // uint 2
	want.push(0x6e);
	want.extend_from_slice(b"watchdog-trips");
	want.push(0x00); // uint 0
	want.push(0x6c);
	want.extend_from_slice(b"last-failure");
	want.push(0x60); // text(0)
	assert_eq!(c.to_cbor(), want);
}
