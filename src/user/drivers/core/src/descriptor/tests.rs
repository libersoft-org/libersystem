// DRV-012's four negatives, each held against the decision that closes it.
use super::{DescriptorFault, Walk, check_transfer, check_type};

// A minimal well-formed configuration: the configuration record, one interface, one endpoint.
fn configuration() -> [u8; 25] {
	[
		9,
		0x02,
		25,
		0,
		1,
		1,
		0,
		0x80,
		50, // configuration: length, type, total 25
		9,
		0x04,
		0,
		0,
		2,
		0x08,
		0x06,
		0x50,
		0, // interface: class 8, subclass 6, protocol 0x50
		7,
		0x05,
		0x81,
		0x02,
		0,
		2,
		0, // endpoint: address 0x81, bulk, 512 bytes
	]
}

#[test]
// THE PAGE IS REUSED BETWEEN TRANSFERS, so the bytes past a short answer are the PREVIOUS
// descriptor's - and the walk reads them as records. A device that claims two hundred bytes and
// sends nine is the case, and the claim was believed over the bytes that were actually there.
fn a_transfer_shorter_than_the_descriptor_it_claims_is_refused() {
	assert_eq!(check_transfer(25, 25), Ok(25));
	assert_eq!(check_transfer(25, 64), Ok(25), "a device may send more than it claims; the claim bounds the walk");
	assert_eq!(check_transfer(200, 9), Err(DescriptorFault::ShortTransfer { declared: 200, received: 9 }));
	assert_eq!(check_transfer(u16::MAX, 0), Err(DescriptorFault::ShortTransfer { declared: u16::MAX, received: 0 }));
}

#[test]
// A DEVICE THAT ANSWERS A CONFIGURATION REQUEST WITH A STRING DESCRIPTOR is answered by a walk over a
// string, and every field the walk reads is at an offset that means something else.
fn a_descriptor_of_the_wrong_type_is_refused() {
	assert_eq!(check_type(0x02, 0x02), Ok(()));
	assert_eq!(check_type(0x02, 0x03), Err(DescriptorFault::Type { expected: 0x02, got: 0x03 }));
}

#[test]
// A RECORD WHOSE `bLength` IS TWO IS A LEGAL RECORD, and the class byte the interface walk wants is
// not in it. The old walk read offset five of it anyway, which is the next record's header - or the
// tail of the previous transfer.
fn a_field_past_a_records_own_length_is_refused_rather_than_read() {
	let bytes = configuration();
	let mut walk = Walk::new(&bytes);
	let configuration_record = walk.next().expect("the configuration record");
	assert_eq!(configuration_record.kind, 0x02);
	let interface = walk.next().expect("the interface record");
	assert_eq!(interface.field(5), Ok(0x08), "the class, which is inside this record");
	assert_eq!(interface.field(6), Ok(0x06));
	assert_eq!(interface.field(7), Ok(0x50));
	assert_eq!(interface.field(9), Err(DescriptorFault::ShortRecord { length: 9, needed: 10 }), "one byte past the record is past it");
	let endpoint = walk.next().expect("the endpoint record");
	assert_eq!(endpoint.field16(4), Ok(512), "the maximum packet size, read as two bytes inside the record");
	assert_eq!(endpoint.field(7), Err(DescriptorFault::ShortRecord { length: 7, needed: 8 }));
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), None, "a walk that ended at the end of the buffer ended cleanly");

	// THE SHORT RECORD IN PRACTICE: an interface header whose length says two. Reading its class
	// byte used to produce whatever followed it.
	let short = [9u8, 0x02, 13, 0, 1, 1, 0, 0x80, 50, 2, 0x04, 0x08, 0x06];
	let mut walk = Walk::new(&short);
	walk.next().expect("the configuration record");
	let stunted = walk.next().expect("the two-byte interface record");
	assert_eq!(stunted.kind, 0x04);
	assert_eq!(stunted.field(5), Err(DescriptorFault::ShortRecord { length: 2, needed: 6 }));
}

#[test]
// A RECORD WHOSE DECLARED LENGTH RUNS PAST THE TRANSFER ends the walk: the length field is what says
// where the next record begins, so a record that cannot be believed takes the rest with it. A length
// below two is the other half - it would advance the walk by nothing and loop for ever.
fn a_record_that_runs_past_the_transfer_or_declares_nothing_ends_the_walk() {
	let overrun = [9u8, 0x02, 25, 0, 1, 1, 0, 0x80, 50, 40, 0x04, 0, 0];
	let mut walk = Walk::new(&overrun);
	walk.next().expect("the configuration record");
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), Some(DescriptorFault::Overrun));

	let malformed = [9u8, 0x02, 25, 0, 1, 1, 0, 0x80, 50, 1, 0x04, 0, 0];
	let mut walk = Walk::new(&malformed);
	walk.next().expect("the configuration record");
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), Some(DescriptorFault::Malformed));

	// A zero-length record is the same fault and not an infinite loop, which is what it was.
	let zero = [9u8, 0x02, 25, 0, 1, 1, 0, 0x80, 50, 0, 0x04];
	let mut walk = Walk::new(&zero);
	walk.next().expect("the configuration record");
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), Some(DescriptorFault::Malformed));
}
