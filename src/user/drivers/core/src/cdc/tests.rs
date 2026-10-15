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
	assert_eq!((bound.notification, bound.notification_interval), (Some(0x83), 16), "the notification endpoint and its interval");
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
	let len = ncm_block(3, &frame, &gadget_layout(), &mut out).expect("the block fits");
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
	assert_eq!(ncm_block(0, &frame, &gadget_layout(), &mut out), None);
	// AND THE LAYOUT'S OWN BOUND, which is the device's: a frame whose block is longer than the device
	// takes is not sent in a block it would drop whole.
	let mut page = [0u8; 4096];
	let layout = gadget_layout();
	assert_eq!(ncm_block(0, &alloc::vec![0x55u8; layout.most], &layout, &mut page), None);
	assert_eq!(ncm_block(0, &[], &layout, &mut page), None, "an empty datagram is the table's terminator, not a frame");
}

// ---------------------------------------------------------------------------------------------
// NCM parameters and the blocks built and read under them
// ---------------------------------------------------------------------------------------------

// `GET_NTB_PARAMETERS` as the host kernel's own `usb_f_ncm` answers it: both block formats, 16 KiB
// each way, datagrams on four-byte boundaries and tables on four-byte boundaries, no datagram limit.
// This is the device the harness's gadget oracle binds, so it is the shape these tests start from.
fn gadget_parameters() -> [u8; NTB_PARAMETERS_LEN] {
	let mut out = [0u8; NTB_PARAMETERS_LEN];
	out[0..2].copy_from_slice(&(NTB_PARAMETERS_LEN as u16).to_le_bytes());
	out[2..4].copy_from_slice(&(NTB_FORMATS_16 | NTB_FORMATS_32).to_le_bytes());
	out[4..8].copy_from_slice(&16384u32.to_le_bytes());
	out[8..10].copy_from_slice(&4u16.to_le_bytes()); // IN divisor
	out[10..12].copy_from_slice(&0u16.to_le_bytes()); // IN remainder
	out[12..14].copy_from_slice(&4u16.to_le_bytes()); // IN alignment
	out[16..20].copy_from_slice(&16384u32.to_le_bytes());
	out[20..22].copy_from_slice(&4u16.to_le_bytes()); // OUT divisor
	out[22..24].copy_from_slice(&0u16.to_le_bytes()); // OUT remainder
	out[24..26].copy_from_slice(&4u16.to_le_bytes()); // OUT alignment
	out[26..28].copy_from_slice(&0u16.to_le_bytes()); // no datagram limit
	out
}

// The layout the driver derives for that device: 512-byte packets, a 1514-byte longest frame and a
// one-page buffer - the three numbers `usb_net` passes.
fn gadget_layout() -> NtbOut {
	let parameters = ntb_parameters(&gadget_parameters()).expect("the gadget's parameters are ones this host frames for");
	NtbOut::new(&parameters, 512, 1514, 4096).expect("and a full-size frame fits a page")
}

// One received block shaped the way `usb_f_ncm` builds one: the header, each datagram on the next
// four-byte boundary, and the datagram table LAST, after them - the opposite order from the blocks
// this host builds, which is why the table's position is never assumed.
fn gadget_block(frames: &[&[u8]]) -> Vec<u8> {
	let mut out = alloc::vec![0u8; NTH16_LEN];
	let mut entries: Vec<(u16, u16)> = Vec::new();
	for frame in frames {
		while out.len() % 4 != 0 {
			out.push(0);
		}
		entries.push((out.len() as u16, frame.len() as u16));
		out.extend_from_slice(frame);
	}
	while out.len() % 4 != 0 {
		out.push(0);
	}
	let ndp = out.len();
	out.extend_from_slice(&NDP16_SIGNATURE.to_le_bytes());
	out.extend_from_slice(&((8 + 4 * (entries.len() + 1)) as u16).to_le_bytes());
	out.extend_from_slice(&0u16.to_le_bytes());
	for (at, len) in &entries {
		out.extend_from_slice(&at.to_le_bytes());
		out.extend_from_slice(&len.to_le_bytes());
	}
	out.extend_from_slice(&[0u8; 4]);
	let total = out.len() as u16;
	out[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
	out[4..6].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
	out[8..10].copy_from_slice(&total.to_le_bytes());
	out[10..12].copy_from_slice(&(ndp as u16).to_le_bytes());
	out
}

// An Ethernet frame of `len` bytes whose every byte says which frame it is and where in it.
fn numbered_frame(seed: u8, len: usize) -> Vec<u8> {
	(0..len).map(|i| seed.wrapping_add((i as u8).wrapping_mul(31))).collect()
}

#[test]
fn the_gadget_s_ntb_parameters_are_read_field_by_field() {
	let parameters = ntb_parameters(&gadget_parameters()).expect("a well-formed answer is read");
	assert_eq!(parameters.formats, NTB_FORMATS_16 | NTB_FORMATS_32);
	assert_eq!(parameters.in_max, 16384, "the IN maximum is the four bytes at offset four");
	assert_eq!(parameters.out_max, 16384, "and the OUT maximum the four at sixteen, past the IN half's padding");
	assert_eq!((parameters.out_divisor, parameters.out_remainder, parameters.out_alignment), (4, 0, 4));
	assert_eq!(parameters.out_datagrams, 0);
	// THE OUT HALF IS READ FROM ITS OWN OFFSETS AND NOT THE IN HALF'S, which is the mistake a structure
	// with two halves of the same shape invites: a device whose two halves differ shows which was read.
	let mut skewed = gadget_parameters();
	skewed[20..22].copy_from_slice(&8u16.to_le_bytes());
	skewed[22..24].copy_from_slice(&2u16.to_le_bytes());
	skewed[24..26].copy_from_slice(&16u16.to_le_bytes());
	let parameters = ntb_parameters(&skewed).expect("still well-formed");
	assert_eq!((parameters.out_divisor, parameters.out_remainder, parameters.out_alignment), (8, 2, 16));
}

#[test]
fn ntb_parameters_outside_their_bounds_are_refused_rather_than_rounded() {
	let good = gadget_parameters();
	assert_eq!(ntb_parameters(&good[..NTB_PARAMETERS_LEN - 1]), Err(NtbRefusal::Short), "an answer shorter than the structure");
	let mut claims_less = good;
	claims_less[0..2].copy_from_slice(&16u16.to_le_bytes());
	assert_eq!(ntb_parameters(&claims_less), Err(NtbRefusal::Short), "and one whose own length says its fields are not all there");

	let mut only_32 = good;
	only_32[2..4].copy_from_slice(&NTB_FORMATS_32.to_le_bytes());
	assert_eq!(ntb_parameters(&only_32), Err(NtbRefusal::No16Bit), "16-bit blocks are the one format every function must offer");

	let mut small_in = good;
	small_in[4..8].copy_from_slice(&(NTB_MIN_IN_SIZE - 1).to_le_bytes());
	assert_eq!(ntb_parameters(&small_in), Err(NtbRefusal::InSize), "an IN maximum under the floor");
	small_in[4..8].copy_from_slice(&NTB_MIN_IN_SIZE.to_le_bytes());
	assert!(ntb_parameters(&small_in).is_ok(), "and the floor itself is a maximum a device may offer");

	let mut divisor = good;
	divisor[20..22].copy_from_slice(&0u16.to_le_bytes());
	assert_eq!(ntb_parameters(&divisor), Err(NtbRefusal::Divisor), "a zero divisor is a division by zero in the layout");
	divisor[20..22].copy_from_slice(&4u16.to_le_bytes());
	divisor[22..24].copy_from_slice(&4u16.to_le_bytes());
	assert_eq!(ntb_parameters(&divisor), Err(NtbRefusal::Divisor), "a remainder of four modulo four is an offset that cannot exist");

	for alignment in [0u16, 2, 6, 12] {
		let mut aligned = good;
		aligned[24..26].copy_from_slice(&alignment.to_le_bytes());
		assert_eq!(ntb_parameters(&aligned), Err(NtbRefusal::Alignment), "a table alignment of {alignment} is not a power of two of at least four");
	}

	// AND THE LAYOUT'S BOUND, which no single field states: the longest frame the link carries has to
	// fit one block inside the device's maximum and the host's page both.
	let parameters = ntb_parameters(&good).expect("well-formed");
	assert_eq!(NtbOut::new(&NtbParameters { out_max: 1500, ..parameters }, 512, 1514, 4096), Err(NtbRefusal::OutSize), "a maximum too small for one full-size frame");
	assert_eq!(NtbOut::new(&NtbParameters { out_alignment: 4096, ..parameters }, 512, 1514, 4096), Err(NtbRefusal::OutSize), "a table aligned past the page leaves no room for any frame");
	assert_eq!(NtbOut::new(&parameters, 512, 1514, 1024), Err(NtbRefusal::OutSize), "and a buffer too small for the frame is refused, not overrun");
	assert_eq!(NtbOut::new(&NtbParameters { out_divisor: 0, ..parameters }, 512, 1514, 4096), Err(NtbRefusal::Divisor), "a layout is never computed from a refused value");
	assert_eq!(NtbOut::new(&NtbParameters { out_alignment: 0, ..parameters }, 512, 1514, 4096), Err(NtbRefusal::Alignment));
}

#[test]
fn a_device_offering_more_than_a_page_is_told_a_page_and_one_offering_less_is_not_asked() {
	// The receive is posted for exactly the size in force: a device that would send sixteen kilobytes
	// is told a page, and one whose own maximum fits is left at its default and posted for that.
	assert_eq!(ntb_input_size(16384, 4096), Some((4096, Some(4096))));
	assert_eq!(ntb_input_size(4096, 4096), Some((4096, None)));
	assert_eq!(ntb_input_size(2048, 4096), Some((2048, None)), "posted for the device's maximum, not the page");
	assert_eq!(ntb_input_size(16384, NTB_MIN_IN_SIZE - 1), None, "a buffer below the floor is not one a host may offer");
}

#[test]
fn an_outgoing_frame_s_block_is_read_back_by_the_production_parser_exactly() {
	// WHAT GOES DOWN THE BULK OUT ENDPOINT, READ BY THE SAME CODE THAT READS WHAT COMES BACK. Every
	// size from the shortest frame to the longest, in the gadget's layout and in one that moves the
	// table and the datagram off their natural places, so a builder that ignored either is caught.
	let gadget = gadget_layout();
	let parameters = ntb_parameters(&gadget_parameters()).expect("well-formed");
	let odd = NtbOut::new(&NtbParameters { out_divisor: 8, out_remainder: 2, out_alignment: 16, ..parameters }, 512, 1514, 4096).expect("fits");
	assert_eq!((odd.table, odd.payload % 8), (16, 2), "the table on the alignment and the datagram on the remainder");
	let mut page = [0u8; 4096];
	let mut found: Vec<Datagram> = Vec::new();
	for layout in [gadget, odd] {
		for (sequence, len) in [(0u16, ETHERNET_HEADER), (1, 60), (2, 483), (3, 1000), (4, 1514)] {
			let frame = numbered_frame(sequence as u8, len);
			let total = ncm_block(sequence, &frame, &layout, &mut page).expect("the frame fits a block");
			let header = ncm_frames(&page[..total], 1514, &mut found).expect("the production parser takes the block");
			assert_eq!(header.sequence, sequence);
			assert_eq!(header.block_length as usize, total, "the block says its own length");
			assert_eq!(header.ndp_index as usize, layout.table, "the table where the layout put it");
			assert_eq!(found.len(), 1, "one datagram a block");
			assert_eq!(found[0].at as usize % if layout == odd { 8 } else { 4 }, if layout == odd { 2 } else { 0 }, "the datagram where the device asked for it");
			assert_eq!(&page[found[0].at as usize..found[0].at as usize + found[0].len as usize], &frame[..], "and exactly the frame, {len} bytes");
		}
	}
}

#[test]
fn a_block_that_is_a_whole_number_of_packets_carries_one_more_byte() {
	// THE DEVICE CANNOT SEE A TRANSFER END ON A PACKET BOUNDARY short of its own maximum, so the next
	// block would be read as the rest of this one. A frame whose block lands exactly on 512 gets a pad
	// byte inside the block's length - and the frame read back is still exactly the frame.
	let layout = gadget_layout();
	let len = 512 - layout.payload;
	let frame = numbered_frame(9, len);
	let mut page = [0u8; 4096];
	let total = ncm_block(7, &frame, &layout, &mut page).expect("fits");
	assert_eq!(total, 513, "a 512-byte block is sent as 513");
	let mut found: Vec<Datagram> = Vec::new();
	let header = ncm_frames(&page[..total], 1514, &mut found).expect("the padded block parses");
	assert_eq!(header.block_length, 513);
	assert_eq!(found.len(), 1);
	assert_eq!(&page[found[0].at as usize..found[0].at as usize + len], &frame[..]);
	// One byte either side needs nothing.
	assert_eq!(layout.length(len - 1), Some(511));
	assert_eq!(layout.length(len + 1), Some(513));
	// AND A BLOCK OF EXACTLY THE DEVICE'S MAXIMUM IS ITS OWN END, so it is not padded past it.
	let parameters = ntb_parameters(&gadget_parameters()).expect("well-formed");
	let tight = NtbOut::new(&NtbParameters { out_max: 2048, ..parameters }, 512, 1514, 4096).expect("fits");
	assert_eq!(tight.length(2048 - tight.payload), Some(2048));
}

#[test]
fn a_received_block_with_several_datagrams_splits_into_exactly_those_frames() {
	// SEVERAL FRAMES IN ONE BLOCK IS WHAT NCM IS FOR, and the receive side's whole job is to hand up
	// each of them as a frame of its own - in the order the table names them, each exactly its bytes.
	let frames: Vec<Vec<u8>> = [60usize, 1514, 98, 14, 342].iter().enumerate().map(|(i, &len)| numbered_frame(i as u8 * 40, len)).collect();
	let refs: Vec<&[u8]> = frames.iter().map(|frame| &frame[..]).collect();
	let block = gadget_block(&refs);
	let mut found: Vec<Datagram> = Vec::new();
	ncm_frames(&block, 1514, &mut found).expect("a well-formed block of five frames");
	assert_eq!(found.len(), frames.len(), "one datagram per frame, none merged and none dropped");
	for (datagram, frame) in found.iter().zip(&frames) {
		assert_eq!(&block[datagram.at as usize..datagram.at as usize + datagram.len as usize], &frame[..]);
	}
	// AND THE TRANSFER MAY BE LONGER THAN THE BLOCK - a device that cannot send a zero-length packet
	// adds a byte - which is not part of any frame.
	let mut longer = block.clone();
	longer.push(0);
	assert_eq!(ncm_frames(&longer, 1514, &mut found).map(|_| found.len()), Ok(frames.len()));
	// A block naming no table carries nothing, and that is not a refusal.
	let mut empty = gadget_block(&[]);
	empty[10..12].copy_from_slice(&0u16.to_le_bytes());
	assert_eq!(ncm_frames(&empty, 1514, &mut found).map(|_| found.len()), Ok(0));
}

#[test]
fn a_malformed_block_is_refused_whole_and_nothing_in_it_is_delivered() {
	// EVERY CASE HAS GOOD DATAGRAMS BEFORE THE BAD ONE, which is what makes "whole" testable: a parser
	// that handed up what it had walked before the refusal passes every test that only looks at the
	// answer, and fails this one, which looks at what was left in the list.
	let good: Vec<Vec<u8>> = (0..3).map(|i| numbered_frame(i, 64)).collect();
	let mut frames: Vec<&[u8]> = good.iter().map(|frame| &frame[..]).collect();
	let short = numbered_frame(7, ETHERNET_HEADER - 1);
	frames.push(&short);
	let mut found: Vec<Datagram> = alloc::vec![Datagram { at: 1, len: 1 }];
	assert_eq!(ncm_frames(&gadget_block(&frames), 1514, &mut found), Err(BlockFault::Frame), "a datagram shorter than an Ethernet header");
	assert!(found.is_empty(), "and nothing of the block is left to deliver, not even the three good frames before it");

	frames.pop();
	let long = numbered_frame(8, 1515);
	frames.push(&long);
	assert_eq!(ncm_frames(&gadget_block(&frames), 1514, &mut found), Err(BlockFault::Frame), "a datagram longer than the link's longest frame");
	assert!(found.is_empty());

	// A datagram inside the header, which is the header.
	frames.pop();
	let mut block = gadget_block(&frames);
	let ndp = u16::from_le_bytes([block[10], block[11]]) as usize;
	let last = ndp + 8 + 2 * 4;
	block[last..last + 2].copy_from_slice(&0u16.to_le_bytes());
	block[last + 2..last + 4].copy_from_slice(&(ETHERNET_HEADER as u16).to_le_bytes());
	assert_eq!(ncm_frames(&block, 1514, &mut found), Err(BlockFault::Frame), "a datagram at offset zero");
	assert!(found.is_empty());

	// THE SAME BYTES NAMED OVER AND OVER: every entry is inside the block and frame-sized, and together
	// they are more than the block - one block multiplied into frames by its own table. Three 600-byte
	// frames, with the second and third entries widened to 1100 bytes from the first frame's offset:
	// 2800 bytes named in a block of 1836.
	let one = numbered_frame(3, 600);
	let mut block = gadget_block(&[&one, &one, &one]);
	assert_eq!(block.len(), 1836);
	let ndp = u16::from_le_bytes([block[10], block[11]]) as usize;
	let first_at = u16::from_le_bytes([block[ndp + 8], block[ndp + 9]]);
	for entry in 1..3 {
		let at = ndp + 8 + entry * 4;
		block[at..at + 2].copy_from_slice(&first_at.to_le_bytes());
		block[at + 2..at + 4].copy_from_slice(&1100u16.to_le_bytes());
	}
	assert_eq!(ncm_frames(&block, 1514, &mut found), Err(BlockFault::Frame));
	assert!(found.is_empty());

	// AND THE TABLE'S OWN REFUSALS STILL REFUSE THE WHOLE BLOCK: a datagram running past the block
	// after two good ones, a CRC-variant table, and a block claiming more than arrived.
	let mut block = gadget_block(&frames);
	let ndp = u16::from_le_bytes([block[10], block[11]]) as usize;
	let third = ndp + 8 + 2 * 4;
	let past = (block.len() as u16).to_le_bytes();
	block[third + 2..third + 4].copy_from_slice(&past);
	assert_eq!(ncm_frames(&block, 1514, &mut found), Err(BlockFault::Datagram));
	assert!(found.is_empty(), "the two good datagrams before the overrun are not delivered either");
	let mut crc = gadget_block(&frames);
	crc[ndp..ndp + 4].copy_from_slice(&NDP16_CRC_SIGNATURE.to_le_bytes());
	assert_eq!(ncm_frames(&crc, 1514, &mut found), Err(BlockFault::Signature));
	let whole = gadget_block(&frames);
	assert_eq!(ncm_frames(&whole[..whole.len() - 1], 1514, &mut found), Err(BlockFault::Length), "a transfer shorter than the block it carries");
	assert!(found.is_empty());
}

// The shape of a Linux composite gadget carrying one `ncm` function: an association, the
// communications interface with its header, union, Ethernet and NCM functional descriptors and the
// notification endpoint, then the data interface's empty setting zero and its setting one with the
// bulk pair.
fn ncm_config(capabilities: u8) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 2, 1, 0, 0x80, 1]);
	out.extend_from_slice(&[8, 0x0B, 0, 2, CLASS_COMMUNICATIONS, SUBCLASS_NCM, 0, 0]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_NCM, 0, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_HEADER, 0x10, 0x01]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 1]);
	out.extend_from_slice(&[13, DT_CS_INTERFACE, FN_ETHERNET, 4, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0]);
	out.extend_from_slice(&[6, DT_CS_INTERFACE, FN_NCM, 0x00, 0x01, capabilities]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x83, 0x03, 16, 0, 9]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 0, CLASS_CDC_DATA, 0, 1, 0]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 1, 2, CLASS_CDC_DATA, 0, 1, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	let total = out.len() as u16;
	out[2..4].copy_from_slice(&total.to_le_bytes());
	out
}

#[test]
fn an_ncm_function_says_which_optional_requests_it_takes() {
	// THE PACKET FILTER IS ONE OF THEM. ECM requires it and NCM makes it optional, so a driver that
	// sent it to every NCM function would have a device that does not take it stall the request -
	// and one that never sent it would leave a device that does take it at whatever filter it had.
	let bound = bind(&ncm_config(0x11)).expect("the gadget's shape binds");
	assert_eq!(bound.model, Model::Ncm);
	assert_eq!(bound.ncm_capabilities, 0x11, "the bitmap after the NCM version");
	assert_ne!(bound.ncm_capabilities & NCM_CAN_FILTER, 0);
	assert_eq!((bound.data_interface, bound.data_alternate, bound.bulk_in, bound.bulk_out), (1, 1, 0x81, 0x02));
	assert_eq!(bound.notification, Some(0x83));
	assert_eq!(bind(&ncm_config(0x10)).map(|bound| bound.ncm_capabilities & NCM_CAN_FILTER), Ok(0), "a function that does not take the filter says so");
	assert_eq!(bind(&ecm_config()).map(|bound| bound.ncm_capabilities), Ok(0), "and an ECM configuration has no NCM descriptor at all");
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
	// PROTOCOL ONE IS THE AT-COMMAND SET, which is what an ordinary modem-shaped ACM port declares.
	acm_config_protocol(with_acm_descriptor, capabilities, 1)
}

fn acm_config_protocol(with_acm_descriptor: bool, capabilities: u8, protocol: u8) -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 3, 7, 0, 0x80, 50]);
	// interface 0: communications, ACM
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ACM, protocol, 0]);
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

// TWO SERIAL FUNCTIONS ON ONE DEVICE, laid out the way a real one is.
//
// These bytes are the shape a Linux composite gadget with two `acm` functions actually presents,
// read off the host with the fixture standing: interfaces 0 and 2 are the control halves, each with
// a union naming ITS OWN data interface - 1 and 3 - and each data interface carries its own bulk
// pair. Nothing about it is invented, which matters for a fixture whose whole job is to be the
// device the binder will meet.
fn acm_pair_config() -> Vec<u8> {
	let mut out: Vec<u8> = Vec::new();
	out.extend_from_slice(&[9, descriptor::DT_CONFIG, 0, 0, 4, 1, 0, 0x80, 50]);
	// Function zero: control interface 0, data interface 1.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 0, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ACM, 1, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_HEADER, 0x10, 0x01]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_CALL_MANAGEMENT, 0x00, 1]);
	out.extend_from_slice(&[4, DT_CS_INTERFACE, FN_ACM, 0x02]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 0, 1]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x85, 0x03, 0x0A, 0x00, 9]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 1, 0, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x81, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x02, 0x02, 0x00, 0x02, 0]);
	// Function one: control interface 2, data interface 3.
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 2, 0, 1, CLASS_COMMUNICATIONS, SUBCLASS_ACM, 1, 0]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_HEADER, 0x10, 0x01]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_CALL_MANAGEMENT, 0x00, 3]);
	out.extend_from_slice(&[4, DT_CS_INTERFACE, FN_ACM, 0x02]);
	out.extend_from_slice(&[5, DT_CS_INTERFACE, FN_UNION, 2, 3]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x8A, 0x03, 0x0A, 0x00, 9]);
	out.extend_from_slice(&[9, descriptor::DT_INTERFACE, 3, 0, 2, CLASS_CDC_DATA, 0, 0, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x86, 0x02, 0x00, 0x02, 0]);
	out.extend_from_slice(&[7, descriptor::DT_ENDPOINT, 0x07, 0x02, 0x00, 0x02, 0]);
	out[2] = out.len() as u8;
	out[3] = (out.len() >> 8) as u8;
	out
}

#[test]
fn each_serial_function_of_a_composite_device_binds_its_own_endpoints() {
	// ONE BEST BULK PAIR IN THE CONFIGURATION IS THE DEFECT A SECOND FUNCTION EXPOSES. The binder
	// kept a single lowest-alternate pair for the whole descriptor, which is right while there is
	// one data interface and wrong the moment there are two: function one's union names interface
	// three and the pair kept was interface one's, so the second port would either be refused as
	// "its union names a data interface that is not in the configuration" or - worse - be handed
	// the first port's endpoints, and the two byte streams would cross with nothing reporting it.
	let config = acm_pair_config();

	let first = bind_acm_nth(&config, 0).expect("the first serial function binds");
	assert_eq!(first.control_interface, 0);
	assert_eq!(first.data_interface, 1, "the interface ITS union names");
	assert_eq!((first.bulk_in, first.bulk_out), (0x81, 0x02));
	assert_eq!(first.notify_in, 0x85, "and its own notification endpoint");

	let second = bind_acm_nth(&config, 1).expect("the second serial function binds");
	assert_eq!(second.control_interface, 2);
	assert_eq!(second.data_interface, 3, "the interface ITS union names, not the lowest in the configuration");
	assert_eq!((second.bulk_in, second.bulk_out), (0x86, 0x07), "its own bulk pair, not the first function's");
	assert_eq!(second.notify_in, 0x8A, "and its own notification endpoint, not the first function's 0x85");

	assert_eq!(first.config_value, second.config_value, "both are in the same configuration, so both are reached by one SET_CONFIGURATION");
}

#[test]
fn a_configuration_runs_out_of_serial_functions_and_says_so_the_one_way() {
	// A caller walks 0, 1, 2... until it is told there are no more, and "no more" has to be the
	// same answer as "this configuration carries none at all" - otherwise every caller needs a
	// second way of asking the same question, and a device with one port would be read as an error
	// on the second look.
	assert_eq!(bind_acm_nth(&acm_pair_config(), 2), Err(NotBindable::NoNetworkInterface));
	assert_eq!(bind_acm_nth(&acm_config(true, 0x02), 1), Err(NotBindable::NoNetworkInterface));
}

#[test]
fn an_alternate_setting_of_a_control_interface_is_not_a_second_function() {
	// A control interface with two alternate settings declares the interface number twice, and a
	// binder counting DECLARATIONS rather than interfaces would report two serial ports on a device
	// that has one - the second of them with no union, so it would be refused as malformed.
	let mut config = acm_config(true, 0x02);
	// A second alternate setting of interface 0, carrying nothing.
	let mut extra: Vec<u8> = alloc::vec![9, descriptor::DT_INTERFACE, 0, 1, 0, CLASS_COMMUNICATIONS, SUBCLASS_ACM, 1, 0];
	config.append(&mut extra);
	let total = config.len();
	config[2] = total as u8;
	config[3] = (total >> 8) as u8;
	assert!(bind_acm_nth(&config, 0).is_ok(), "the one function still binds");
	assert_eq!(bind_acm_nth(&config, 1), Err(NotBindable::NoNetworkInterface), "and there is no second one");
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

#[test]
fn a_network_connection_notification_says_which_way_the_link_went() {
	let up = [0xA1, NOTIFY_NETWORK_CONNECTION, 1, 0, 0, 0, 0, 0];
	let down = [0xA1, NOTIFY_NETWORK_CONNECTION, 0, 0, 0, 0, 0, 0];
	assert_eq!(notification(&up), Notification::Link { up: true });
	assert_eq!(notification(&down), Notification::Link { up: false });
	// ANY NON-ZERO VALUE IS CONNECTED, which is what the specification says and not what a driver
	// comparing against one would do.
	assert_eq!(notification(&[0xA1, NOTIFY_NETWORK_CONNECTION, 2, 0, 0, 0, 0, 0]), Notification::Link { up: true });
}

#[test]
fn a_notification_this_driver_does_not_act_on_is_not_a_link_change() {
	// A driver that read every notification as a link transition reports the link going down when
	// the adapter said something else entirely - `RESPONSE_AVAILABLE`, for one.
	assert_eq!(notification(&[0xA1, 0x01, 0, 0, 0, 0, 0, 0]), Notification::Other(0x01));
	assert_eq!(notification(&[0xA1, 0x20, 0, 0, 0, 0, 0, 0]), Notification::Other(0x20));
}

#[test]
fn a_speed_change_shorter_than_it_claims_is_refused() {
	let mut whole = [0u8; NOTIFICATION_HEADER_LEN + 8];
	whole[1] = NOTIFY_CONNECTION_SPEED_CHANGE;
	whole[6] = 8;
	whole[8..12].copy_from_slice(&100_000_000u32.to_le_bytes());
	whole[12..16].copy_from_slice(&10_000_000u32.to_le_bytes());
	assert_eq!(notification(&whole), Notification::Speed { down: 100_000_000, up: 10_000_000 });

	// THE LENGTH FIELD IS THE DEVICE'S CLAIM AND THE SLICE IS WHAT ARRIVED. A header promising
	// eight bytes of rates in a transfer that carried none is not a slow link - reading them would
	// read past what the controller wrote.
	assert_eq!(notification(&whole[..NOTIFICATION_HEADER_LEN]), Notification::Malformed);
	let mut lying = whole;
	lying[6] = 2;
	assert_eq!(notification(&lying), Notification::Malformed, "and a claim shorter than the field itself is not a shorter answer");

	assert_eq!(notification(&[0u8; NOTIFICATION_HEADER_LEN - 1]), Notification::Malformed);
	assert_eq!(notification(&[]), Notification::Malformed);
}

// AN ETHERNET ADAPTER WEARING A SERIAL PORT'S IDENTITY IS NOT A SERIAL PORT.
//
// RNDIS declares class 2, subclass 2 - the same two bytes an ACM port declares - and a VENDOR
// protocol. Measured on QEMU's own `usb-net`, whose default mode is RNDIS: a binding that read only
// the class and the subclass took the device, and the boot reported no network provider at all with
// nothing anywhere saying why. The refusal is `NoNetworkInterface`, which is this decoder's "there
// is no interface here I bind" and is what lets the network models be offered it next.
#[test]
fn an_acm_shaped_rndis_control_interface_is_not_a_serial_port() {
	assert_eq!(bind_acm(&acm_config_protocol(true, 0x02, PROTOCOL_VENDOR)), Err(NotBindable::NoNetworkInterface));
	// AND EVERY PROTOCOL THE SUBCLASS ITSELF DEFINES STILL BINDS, which is what makes the line above
	// a statement about the vendor value rather than about protocols in general. Zero is "no class
	// protocol" - an MCU link with no command set at all - and one is the AT commands a modem takes.
	for protocol in [0u8, 1, 2, 6] {
		assert!(bind_acm(&acm_config_protocol(true, 0x02, protocol)).is_ok(), "protocol {protocol} is one this subclass defines");
	}
}
