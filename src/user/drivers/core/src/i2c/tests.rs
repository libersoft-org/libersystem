use super::*;
use crate::smbus::{Transaction, compose};

#[test]
fn the_out_header_carries_the_address_shifted_and_the_flags() {
	assert_eq!(header(0x50, 0), [0xA0, 0, 0, 0, 0, 0, 0, 0]);
	assert_eq!(header(0x50, FLAG_READ), [0xA0, 0, 0, 0, 2, 0, 0, 0]);
	assert_eq!(header(0x7F, FLAG_FAIL_NEXT | FLAG_READ), [0xFE, 0, 0, 0, 3, 0, 0, 0]);
}

#[test]
fn a_write_then_a_read_is_two_requests_and_the_first_fails_the_next() {
	let plan = plain(0x2C, 2, 30).expect("a write-then-read plans");
	assert_eq!(plan.count(), 2);
	assert_eq!(plan.requests[0], Some(Request { header: header(0x2C, FLAG_FAIL_NEXT), buffer: Buffer::Write(2) }));
	assert_eq!(plan.requests[1], Some(Request { header: header(0x2C, FLAG_READ), buffer: Buffer::Read(30) }));
	assert_eq!(plain(0x2C, 4, 0).expect("a write").requests, [Some(Request { header: header(0x2C, 0), buffer: Buffer::Write(4) }), None]);
	assert_eq!(plain(0x2C, 0, 4).expect("a read").requests, [Some(Request { header: header(0x2C, FLAG_READ), buffer: Buffer::Read(4) }), None]);
	assert_eq!(plain(0x2C, MAX_TRANSFER + 1, 0), None, "a message over the bound");
	assert_eq!(plain(0x2C, 0, MAX_TRANSFER + 1), None);
	assert_eq!(plain(0x2C, 0, 0), None, "nothing at all is a quick command, asked for as one");
}

#[test]
fn every_composed_smbus_shape_maps_onto_its_requests() {
	let quick = compose(0x50, Transaction::Quick { read: true }, false).expect("composes");
	assert_eq!(smbus(0x50, &quick).requests, [Some(Request { header: header(0x50, FLAG_READ), buffer: Buffer::None }), None], "a quick command is a zero-length request");
	let write = compose(0x50, Transaction::WriteByteData { command: 1, value: 2 }, true).expect("composes");
	assert_eq!(smbus(0x50, &write).requests, [Some(Request { header: header(0x50, 0), buffer: Buffer::Write(3) }), None], "command, value and PEC");
	let receive = compose(0x50, Transaction::ReceiveByte, false).expect("composes");
	assert_eq!(smbus(0x50, &receive).requests, [Some(Request { header: header(0x50, FLAG_READ), buffer: Buffer::Read(1) }), None]);
	let word = compose(0x50, Transaction::ReadWordData { command: 9 }, true).expect("composes");
	assert_eq!(smbus(0x50, &word).requests, [Some(Request { header: header(0x50, FLAG_FAIL_NEXT), buffer: Buffer::Write(1) }), Some(Request { header: header(0x50, FLAG_READ), buffer: Buffer::Read(3) })], "the command, then two bytes and the PEC");
}
