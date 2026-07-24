use super::{
	executable_aliases_ambiguous,
	log::{LogRecord, Severity, encode, render_cbor, render_json, render_text},
};

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
	assert_eq!(&json[..json_len], br#"{"ts":42,"severity":"INFO","source":"storage_service","fields":{"event":"online","files":"2"}}"#);

	let mut cbor: [u8; 128] = [0u8; 128];
	let cbor_len: usize = render_cbor(&rec, &mut cbor).expect("cbor fits");
	assert_eq!(cbor[0], 0xa4, "CBOR record is a 4-entry map");
	assert!(cbor[..cbor_len].windows(b"storage_service".len()).any(|window: &[u8]| window == b"storage_service"));
}
