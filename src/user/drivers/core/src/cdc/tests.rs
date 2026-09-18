use super::*;
use alloc::vec::Vec;

// An ECM configuration built the way QEMU's `usb-net` builds one: a communications interface with
// the header, union and Ethernet functional descriptors and an interrupt endpoint, then a data
// interface with alternate zero carrying NO endpoints and alternate one carrying the bulk pair.
fn ecm_config() -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	// Configuration: length 9, type 2, total (filled later), interfaces 2, value 1.
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0x80, 50]);
	// Interface 0, alternate 0, 1 endpoint, class 2 (communications), subclass 6 (ECM).
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ECM, 0, 0]);
	// Header functional descriptor.
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_HEADER, 0x10, 0x01]);
	// Union: control 0, subordinate 1.
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 1]);
	// Ethernet: MAC string index 4, statistics 0, max segment 1514, filters 0, power filters 0.
	out.extend_from_slice(&[13, DT_CS_INTERFACE, FN_ETHERNET, 4, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0]);
	// The notification endpoint: interrupt IN.
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x03, 16, 0, 16]);
	// Interface 1, alternate 0, NO endpoints - the "not carrying traffic" setting.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 0, CLASS_CDC_DATA, 0, 0, 0]);
	// Interface 1, alternate 1, the bulk pair.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 1, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn an_ecm_configuration_names_its_two_interfaces_and_its_bulk_pair() {
	let bound = bind(&ecm_config()).expect("a plain ECM configuration binds");
	assert_eq!(bound.model, Model::Ecm);
	assert_eq!(bound.config_value, 1);
	assert_eq!(bound.control_interface, 0);
	assert_eq!(bound.data_interface, 1);
	assert_eq!(bound.bulk_in, 0x81);
	assert_eq!(bound.bulk_out, 0x02);
	assert_eq!(bound.bulk_in_packet, 512);
	assert_eq!(bound.mac_string, 4);
	assert_eq!(bound.max_segment, 1514);
}

#[test]
fn the_endpoints_are_taken_from_the_alternate_setting_that_has_them() {
	// THE DEFECT THIS EXISTS FOR: alternate zero of a CDC data interface has no endpoints at all,
	// and a driver that configures it waits on pipes that do not exist. The answer must be the
	// setting the endpoints are in, which is never zero.
	let bound = bind(&ecm_config()).expect("binds");
	assert_eq!(bound.data_alternate, 1, "the bulk pair is in alternate one, and that is what must be selected");
}

#[test]
fn the_lowest_numbered_setting_that_carries_a_pair_wins_whatever_order_they_appear_in() {
	// A device may publish several settings that all carry a bulk pair - they differ in buffer size
	// rather than in function - and it may publish them in any order. Taking the FIRST one seen is
	// what makes the choice depend on the device's descriptor order instead of on its numbering,
	// which is the kind of difference that shows up on one vendor's adapter and no other.
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0x80, 50]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ECM, 0, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 1]);
	out.extend_from_slice(&[13, DT_CS_INTERFACE, FN_ETHERNET, 4, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0]);
	// Alternate zero: no endpoints at all, which is what "not carrying traffic" looks like.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 0, CLASS_CDC_DATA, 0, 0, 0]);
	// Then alternate TWO, and only then alternate one - both with a pair.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 2, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x85, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x06, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 1, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	let bound = bind(&out).expect("binds");
	assert_eq!(bound.data_alternate, 1, "the lowest-numbered setting with a pair is the one to select");
	assert_eq!(bound.bulk_in, 0x81, "and its endpoints, not the ones of the setting that came first");
	assert_eq!(bound.bulk_out, 0x02);
}

#[test]
fn an_ncm_configuration_is_the_same_walk_with_a_different_model() {
	let mut config = ecm_config();
	// Turn the communications interface's subclass into NCM.
	let at = 9 + 6;
	config[at] = SUBCLASS_NCM;
	let bound = bind(&config).expect("an NCM configuration binds the same way");
	assert_eq!(bound.model, Model::Ncm);
	assert_eq!(bound.bulk_in, 0x81);
}

#[test]
fn a_configuration_with_no_union_is_refused() {
	let config = ecm_config();
	let mut without: Vec<u8> = Vec::new();
	let mut at = 0;
	while at < config.len() {
		let len = config[at] as usize;
		let record = &config[at..at + len];
		if !(record[1] == DT_CS_INTERFACE && record[2] == FN_UNION) {
			without.extend_from_slice(record);
		}
		at += len;
	}
	let total = without.len() as u16;
	without[2..4].copy_from_slice(&total.to_le_bytes());
	assert_eq!(bind(&without), Err(NotBindable::NoUnion));
}

#[test]
fn a_union_naming_an_interface_that_is_not_there_is_refused() {
	// The device describes one data interface and its union names another. Binding the one that IS
	// there would attach the driver to the wrong half of a composite device.
	let mut config = ecm_config();
	let union_at = 9 + 9 + 5;
	assert_eq!(config[union_at + 1], DT_CS_INTERFACE);
	assert_eq!(config[union_at + 2], FN_UNION);
	config[union_at + 4] = 7;
	assert_eq!(bind(&config), Err(NotBindable::NoDataInterface));
}

#[test]
fn a_configuration_with_no_network_interface_is_refused() {
	let mut config = ecm_config();
	config[9 + 6] = 0x02; // an ACM subclass, which is not a network model
	assert_eq!(bind(&config), Err(NotBindable::NoNetworkInterface));
}

#[test]
fn a_record_running_past_the_transfer_is_refused_rather_than_walked() {
	let mut config = ecm_config();
	let last = config.len() - 7;
	config[last] = 200;
	assert_eq!(bind(&config), Err(NotBindable::Malformed));
}

#[test]
fn a_mac_string_decodes_to_six_bytes() {
	let mut bytes = alloc::vec![26u8, 0x03];
	for c in b"0A1B2C3D4E5F" {
		bytes.push(*c);
		bytes.push(0);
	}
	assert_eq!(mac_from_string(&bytes), Some([0x0A, 0x1B, 0x2C, 0x3D, 0x4E, 0x5F]));
}

#[test]
fn a_mac_string_of_the_wrong_length_or_with_a_non_hex_digit_is_refused() {
	let mut short = alloc::vec![10u8, 0x03];
	for c in b"0A1B" {
		short.push(*c);
		short.push(0);
	}
	assert_eq!(mac_from_string(&short), None);

	let mut bad = alloc::vec![26u8, 0x03];
	for c in b"0A1B2C3D4E5G" {
		bad.push(*c);
		bad.push(0);
	}
	assert_eq!(mac_from_string(&bad), None);

	// A character outside ASCII is not a hex digit however it renders.
	let mut wide = alloc::vec![26u8, 0x03];
	for c in b"0A1B2C3D4E5F" {
		wide.push(*c);
		wide.push(1);
	}
	assert_eq!(mac_from_string(&wide), None);
}

// ---------------------------------------------------------------------------------------------
// NCM blocks
// ---------------------------------------------------------------------------------------------

// One block with `frames` datagrams in one pointer.
fn ncm_block_of(frames: &[&[u8]]) -> Vec<u8> {
	let table = NDP16_MIN_LEN + frames.len() * 4;
	let mut out = alloc::vec![0u8; NTH16_LEN + table];
	let mut offsets: Vec<(u16, u16)> = Vec::new();
	for frame in frames {
		offsets.push((out.len() as u16, frame.len() as u16));
		out.extend_from_slice(frame);
	}
	let total = out.len() as u16;
	out[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
	out[4..6].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
	out[6..8].copy_from_slice(&7u16.to_le_bytes());
	out[8..10].copy_from_slice(&total.to_le_bytes());
	out[10..12].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
	let ndp = NTH16_LEN;
	out[ndp..ndp + 4].copy_from_slice(&NDP16_SIGNATURE.to_le_bytes());
	out[ndp + 4..ndp + 6].copy_from_slice(&(table as u16).to_le_bytes());
	out[ndp + 6..ndp + 8].copy_from_slice(&0u16.to_le_bytes());
	for (i, (at, len)) in offsets.iter().enumerate() {
		let entry = ndp + 8 + i * 4;
		out[entry..entry + 2].copy_from_slice(&at.to_le_bytes());
		out[entry + 2..entry + 4].copy_from_slice(&len.to_le_bytes());
	}
	out
}

#[test]
fn a_block_yields_the_datagrams_its_table_names() {
	let block = ncm_block_of(&[b"first", b"second!"]);
	let header = nth16(&block).expect("the header parses");
	assert_eq!(header.sequence, 7);
	let found: Vec<Datagram> = Datagrams::new(&block, header).collect();
	assert_eq!(found.len(), 2);
	assert_eq!(&block[found[0].at as usize..found[0].at as usize + found[0].len as usize], b"first");
	assert_eq!(&block[found[1].at as usize..found[1].at as usize + found[1].len as usize], b"second!");
}

#[test]
fn a_block_claiming_more_than_arrived_is_refused() {
	let mut block = ncm_block_of(&[b"frame"]);
	let claimed = (block.len() + 64) as u16;
	block[8..10].copy_from_slice(&claimed.to_le_bytes());
	assert_eq!(nth16(&block), Err(BlockFault::Length));
}

#[test]
fn a_table_pointer_inside_the_header_is_refused() {
	let mut block = ncm_block_of(&[b"frame"]);
	block[10..12].copy_from_slice(&4u16.to_le_bytes());
	assert_eq!(nth16(&block), Err(BlockFault::Pointer));
}

#[test]
fn a_table_pointer_past_the_block_is_refused() {
	let mut block = ncm_block_of(&[b"frame"]);
	let past = (block.len() - 4) as u16;
	block[10..12].copy_from_slice(&past.to_le_bytes());
	assert_eq!(nth16(&block), Err(BlockFault::Pointer));
}

#[test]
fn a_datagram_running_past_the_block_is_refused() {
	// The entry claims a length that reaches past what arrived, which is the read this module
	// exists to stop.
	let mut block = ncm_block_of(&[b"frame"]);
	let entry = NTH16_LEN + 8;
	let over = (block.len() as u16) + 1;
	block[entry + 2..entry + 4].copy_from_slice(&over.to_le_bytes());
	let header = nth16(&block).expect("the header still parses");
	let mut walk = Datagrams::new(&block, header);
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), Some(BlockFault::Datagram));
}

#[test]
fn a_pointer_chain_that_loops_ends_rather_than_spinning() {
	// A pointer naming itself. Nothing about the bytes is otherwise wrong, so the only thing that
	// stops this is the bound.
	let mut block = ncm_block_of(&[b"frame"]);
	let ndp = NTH16_LEN;
	block[ndp + 6..ndp + 8].copy_from_slice(&(ndp as u16).to_le_bytes());
	let header = nth16(&block).expect("the header parses");
	let mut walk = Datagrams::new(&block, header);
	let found: Vec<Datagram> = walk.by_ref().collect();
	assert!(found.len() < 64, "a self-naming pointer must not produce datagrams without end");
	assert_eq!(walk.fault(), Some(BlockFault::Chain));
}

#[test]
fn a_crc_variant_pointer_is_refused_by_name() {
	let mut block = ncm_block_of(&[b"frame"]);
	let ndp = NTH16_LEN;
	block[ndp..ndp + 4].copy_from_slice(&NDP16_CRC_SIGNATURE.to_le_bytes());
	let header = nth16(&block).expect("the header parses");
	let mut walk = Datagrams::new(&block, header);
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), Some(BlockFault::Signature));
}

#[test]
fn a_block_with_no_datagrams_is_not_an_error() {
	let mut block = ncm_block_of(&[]);
	// A device with nothing to send may name no table at all.
	block[10..12].copy_from_slice(&0u16.to_le_bytes());
	let header = nth16(&block).expect("the header parses");
	let mut walk = Datagrams::new(&block, header);
	assert_eq!(walk.next(), None);
	assert_eq!(walk.fault(), None);
}

#[test]
fn a_block_that_is_not_ncm_is_refused() {
	let mut block = ncm_block_of(&[b"frame"]);
	block[0] = b'X';
	assert_eq!(nth16(&block), Err(BlockFault::Signature));
	assert_eq!(nth16(&block[..4]), Err(BlockFault::Short));
}

#[test]
fn a_built_block_reads_back_as_the_frame_it_carries() {
	// The two halves of this module against each other: what the transmit side builds is what the
	// receive side reads, which is the property a device on the other end depends on.
	let frame: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(7)).collect();
	let mut out = [0u8; 256];
	let len = ncm_block(3, &frame, &mut out).expect("the block fits");
	let header = nth16(&out[..len]).expect("the built header parses");
	assert_eq!(header.sequence, 3);
	let found: Vec<Datagram> = Datagrams::new(&out[..len], header).collect();
	assert_eq!(found.len(), 1);
	assert_eq!(&out[found[0].at as usize..found[0].at as usize + found[0].len as usize], &frame[..]);
}

#[test]
fn a_block_that_does_not_fit_is_refused_rather_than_truncated() {
	let frame = [0u8; 64];
	let mut out = [0u8; 32];
	assert_eq!(ncm_block(0, &frame, &mut out), None);
}

// An ACM configuration built the way QEMU's `usb-serial` builds one: a communications interface of
// the ACM subclass carrying the header, call-management, ACM and union functional descriptors and an
// interrupt endpoint, then a data interface with the bulk pair.
//
// A SECOND FUNCTION IN FRONT OF IT, because that is the case the union descriptor exists for: an
// interface that is NOT the adapter's data interface sits between the two, so a driver that binds
// "the interface after the communications one" binds the wrong one and takes a bulk pair another
// function is using.
fn acm_config(with_acm_descriptor: bool, capabilities: u8) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 3, 7, 0, 0x80, 50]);
	// interface 0: communications, ACM
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ACM, 1, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_HEADER, 0x10, 0x01]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_CALL_MANAGEMENT, 0x00, 2]);
	if with_acm_descriptor {
		out.extend_from_slice(&[4, DT_CS_INTERFACE, FN_ACM, capabilities]);
	}
	// The union names interface 2, NOT the interface that follows this one.
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 2]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x82, 0x03, 0x10, 0x00, 0xFF]);
	// interface 1: another function entirely, with a bulk pair of its own.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 2, 0xFF, 0xFF, 0xFF, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x03, 0x02, 0x00, 0x02, 0]);
	// interface 2: the adapter's data interface.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 2, 0, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x01, 0x02, 0x00, 0x02, 0]);
	out[2] = out.len() as u8;
	out[3] = (out.len() >> 8) as u8;
	out
}

#[test]
fn the_union_names_the_data_interface_and_the_next_one_is_not_it() {
	// On a composite device the interface after the communications one belongs to something else,
	// and binding it takes a bulk pair another function is using - which is not a refusal anywhere:
	// both halves then read from endpoints the other is driving.
	let binding = bind_acm(&acm_config(true, 0x02)).expect("a bindable ACM adapter");
	assert_eq!(binding.control_interface, 0, "the communications interface");
	assert_eq!(binding.data_interface, 2, "the interface the UNION names, not interface 1");
	assert_eq!(binding.bulk_in, 0x81, "and its bulk pair, not the other function's 0x83");
	assert_eq!(binding.bulk_out, 0x01);
}

#[test]
fn the_notification_endpoint_is_the_communications_interfaces_own() {
	// An interrupt endpoint taken from whichever interface carried one is a HID function's on a
	// composite adapter - and what arrives on it is not a serial state.
	let binding = bind_acm(&acm_config(true, 0x02)).expect("a bindable ACM adapter");
	assert_eq!(binding.notify_in, 0x82);
	assert_eq!(binding.notify_packet, 0x10);
}

#[test]
fn a_device_with_no_acm_descriptor_supports_no_line_coding_and_is_still_a_byte_stream() {
	// An MCU link has no UART behind the USB. It is a perfectly good byte stream that STALLS every
	// line-coding request, and a driver that asks first never finds that out the hard way.
	let binding = bind_acm(&acm_config(false, 0)).expect("a device with no ACM descriptor still binds");
	assert_eq!(binding.capabilities, 0, "nothing published means nothing supported");
	assert!(!binding.supports_line_coding());
	let with = bind_acm(&acm_config(true, 0x02)).expect("a bindable ACM adapter");
	assert!(with.supports_line_coding(), "bit one is the line-coding capability");
	let other_bits = bind_acm(&acm_config(true, 0x0D)).expect("a bindable ACM adapter");
	assert!(!other_bits.supports_line_coding(), "and the other capability bits are not it");
}

#[test]
fn a_configuration_with_no_union_names_nothing_to_bind() {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 2, 7, 0, 0x80, 50]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 0, CLASS_COMMUNICATIONS, SUBCLASS_ACM, 1, 0]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x01, 0x02, 0x00, 0x02, 0]);
	out[2] = out.len() as u8;
	assert_eq!(bind_acm(&out), Err(NotBindable::NoUnion), "nothing says which data interface is the adapter's");
}

#[test]
fn the_stop_bits_are_an_enumeration_and_the_data_bits_are_a_count() {
	// TWO CONVENTIONS IN SEVEN BYTES. Writing the number you mean into the stop-bits byte asks for
	// one and a HALF stop bits when you mean one - accepted by some devices, which then frame every
	// byte differently, and that is a link that works until the first byte of the second frame.
	let coding = LineCoding::default_8n1();
	let wire = coding.encode();
	assert_eq!(&wire[..4], &115_200u32.to_le_bytes(), "the rate is little-endian");
	assert_eq!(wire[4], 0, "ONE stop bit is zero, not one");
	assert_eq!(wire[5], 0, "no parity");
	assert_eq!(wire[6], 8, "and the data bits are the count they say");
	assert_eq!(LineCoding { rate: 9600, stop_bits: StopBits::Two, parity: Parity::Even, data_bits: 7 }.encode()[4], 2, "two stop bits is two");
	assert_eq!(LineCoding { rate: 9600, stop_bits: StopBits::OneAndAHalf, parity: Parity::None, data_bits: 8 }.encode()[4], 1, "and one and a half is one");
}

#[test]
fn a_line_coding_answer_outside_the_enumeration_is_refused_rather_than_rounded() {
	// Three stop bits is not a setting, and reading it as "two, probably" is a driver deciding what
	// a device meant.
	let good = LineCoding::default_8n1().encode();
	assert_eq!(LineCoding::decode(&good), Some(LineCoding::default_8n1()));
	let mut stop = good;
	stop[4] = 3;
	assert_eq!(LineCoding::decode(&stop), None, "a stop-bits value the structure does not define");
	let mut parity = good;
	parity[5] = 9;
	assert_eq!(LineCoding::decode(&parity), None);
	let mut bits = good;
	bits[6] = 9;
	assert_eq!(LineCoding::decode(&bits), None, "nine data bits is not a width this structure carries");
	assert_eq!(LineCoding::decode(&good[..LINE_CODING_LEN - 1]), None, "and an answer too short to be one");
}

#[test]
fn the_control_lines_are_a_bitmap_in_the_setup_packet() {
	// DTR is bit zero and RTS is bit one, and the request carries NO data stage: a driver that sends
	// them as bytes sends a transfer the device did not ask for.
	assert_eq!(control_lines(false, false), 0);
	assert_eq!(control_lines(true, false), 1, "DTR is bit zero");
	assert_eq!(control_lines(false, true), 2, "RTS is bit one");
	assert_eq!(control_lines(true, true), 3);
}
