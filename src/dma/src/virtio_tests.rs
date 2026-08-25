// The wire, the negotiation, the statuses and the events - each one asked what it does when the
// answer is wrong.
//
// A CODEC WRITTEN AGAINST A SPECIFICATION AND NEVER RUN AGAINST A MALFORMED ANSWER is where drivers
// go wrong, and the device on the other end of this one is emulated by something this milestone's
// threat model does not trust. So roughly half of what follows feeds it rubbish.

use super::virtio_iommu::*;
use super::*;
use alloc::vec::Vec;

// A transport that answers with whatever the test needs, and records what it was asked.
struct Wire {
	answers: Vec<u8>,
	sent: Vec<Vec<u8>>,
	events: Vec<Vec<u8>>,
	broken: bool,
}

impl Wire {
	fn ok() -> Self {
		Self { answers: Vec::new(), sent: Vec::new(), events: Vec::new(), broken: false }
	}

	fn answering(status: u8) -> Self {
		Self { answers: alloc::vec![status], sent: Vec::new(), events: Vec::new(), broken: false }
	}
}

impl Transport for Wire {
	fn request(&mut self, request: &[u8], tail: &mut [u8]) -> Result<(), Fault> {
		self.sent.push(request.to_vec());
		if self.broken {
			return Err(Fault::Unconfirmed);
		}
		let status = if self.answers.is_empty() { S_OK } else { self.answers.remove(0) };
		tail[0] = status;
		Ok(())
	}

	fn take_event(&mut self, out: &mut [u8]) -> usize {
		if self.events.is_empty() {
			return 0;
		}
		let event = self.events.remove(0);
		let len = event.len().min(out.len());
		out[..len].copy_from_slice(&event[..len]);
		len
	}
}

fn config_bytes() -> Vec<u8> {
	let mut bytes = Vec::new();
	bytes.extend_from_slice(&0x1000u64.to_le_bytes()); // page size mask: 4 KiB only
	bytes.extend_from_slice(&0u64.to_le_bytes()); // input start
	bytes.extend_from_slice(&0xFFFF_FFFFu64.to_le_bytes()); // input end, inclusive
	bytes.extend_from_slice(&1u32.to_le_bytes()); // domain start
	bytes.extend_from_slice(&0xFFFFu32.to_le_bytes()); // domain end
	bytes.extend_from_slice(&0u32.to_le_bytes()); // probe size
	bytes.push(1); // bypass on, as the boot profile starts
	bytes.extend_from_slice(&[0, 0, 0]);
	bytes
}

fn backend(wire: Wire) -> VirtioIommu<Wire> {
	let config = Config::parse(&config_bytes()).expect("a valid config");
	let features = negotiate(REQUIRED | F_BYPASS_CONFIG).expect("negotiated");
	VirtioIommu::new(wire, config, features, Generation(1)).expect("a backend")
}

#[test]
fn negotiation_takes_what_it_implements_and_refuses_what_it_needs_and_is_not_offered() {
	let taken = negotiate(REQUIRED | F_BYPASS_CONFIG | F_MMIO).expect("the required set is there");
	assert_eq!(taken & F_MMIO, 0, "a feature this code does not implement is not acknowledged");
	assert_eq!(taken & F_BYPASS_CONFIG, F_BYPASS_CONFIG, "and one it does is");
	// MAP_UNMAP is the mechanism; without it there is nothing to enforce with.
	assert_eq!(negotiate(F_INPUT_RANGE | F_DOMAIN_RANGE), Err(Fault::Unconfirmed));
	assert_eq!(negotiate(0), Err(Fault::Unconfirmed));
}

#[test]
fn a_configuration_that_cannot_be_true_is_refused_rather_than_used() {
	assert!(Config::parse(&config_bytes()).is_ok());
	assert_eq!(Config::parse(&[0u8; 8]), Err(Fault::Malformed), "a short config is not a config");

	let mut backwards = config_bytes();
	backwards[16..24].copy_from_slice(&0u64.to_le_bytes());
	backwards[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
	assert_eq!(Config::parse(&backwards), Err(Fault::Malformed), "an input range that ends before it starts translates nothing");

	let mut no_pages = config_bytes();
	no_pages[0..8].copy_from_slice(&0u64.to_le_bytes());
	assert_eq!(Config::parse(&no_pages), Err(Fault::Malformed), "a device that supports no page size can map nothing");
}

#[test]
fn the_input_range_is_inclusive_at_both_ends() {
	let config = Config::parse(&config_bytes()).expect("valid");
	assert_eq!(config.input_len(), 0x1_0000_0000, "a range of 0..=0xFFFF_FFFF is four gigabytes, not one byte less");
	assert!(config.contains(0xFFFF_F000, 0x1000), "the last page is inside the range");
	assert!(!config.contains(0xFFFF_F000, 0x2000), "and one page past the end is not");
	assert_eq!(config.smallest_page(), 0x1000);
}

#[test]
fn a_map_request_is_the_bytes_the_specification_names() {
	let request = encode_map(3, 0x1000, 0x2000, 0xDEAD_0000, Direction::ToDevice).expect("encoded");
	assert_eq!(request.len(), REQ_MAP_LEN);
	assert_eq!(request[0], T_MAP);
	assert_eq!(&request[4..8], &3u32.to_le_bytes());
	assert_eq!(&request[8..16], &0x1000u64.to_le_bytes());
	// THE END IS INCLUSIVE: two pages from 0x1000 end at 0x2FFF, not 0x3000. One byte of arithmetic,
	// and getting it wrong maps a page that belongs to somebody else.
	assert_eq!(&request[16..24], &0x2FFFu64.to_le_bytes());
	assert_eq!(&request[24..32], &0xDEAD_0000u64.to_le_bytes());
	assert_eq!(&request[32..36], &MAP_F_READ.to_le_bytes(), "a to-device mapping is readable by the device and not writable");
}

#[test]
fn each_direction_sets_exactly_its_own_permission_bits() {
	let read = encode_map(1, 0x1000, 0x1000, 0, Direction::ToDevice).expect("encoded");
	let write = encode_map(1, 0x1000, 0x1000, 0, Direction::FromDevice).expect("encoded");
	let both = encode_map(1, 0x1000, 0x1000, 0, Direction::Bidirectional).expect("encoded");
	assert_eq!(u32::from_le_bytes(read[32..36].try_into().unwrap()), MAP_F_READ);
	assert_eq!(u32::from_le_bytes(write[32..36].try_into().unwrap()), MAP_F_WRITE);
	assert_eq!(u32::from_le_bytes(both[32..36].try_into().unwrap()), MAP_F_READ | MAP_F_WRITE);
}

#[test]
fn a_zero_length_or_wrapping_mapping_is_refused_before_it_reaches_the_wire() {
	assert_eq!(encode_map(1, 0x1000, 0, 0, Direction::ToDevice), Err(Fault::Malformed));
	assert_eq!(encode_map(1, u64::MAX, 2, 0, Direction::ToDevice), Err(Fault::Malformed));
	assert_eq!(encode_unmap(1, 0x1000, 0), Err(Fault::Malformed));
	assert_eq!(encode_unmap(1, u64::MAX, 2), Err(Fault::Malformed));
}

#[test]
fn an_attach_never_asks_for_bypass() {
	let request = encode_attach(3, 0x0100, 0);
	assert_eq!(request.len(), REQ_ATTACH_LEN, "twenty bytes: the four reserved ones after the flags are part of the request");
	assert_eq!(encode_detach(3, 0x0100).len(), REQ_DETACH_LEN, "and detach carries eight reserved bytes rather than four");
	assert_eq!(encode_unmap(3, 0x1000, 0x1000).expect("encoded").len(), REQ_UNMAP_LEN);
	assert_eq!(request[0], T_ATTACH);
	assert_eq!(u32::from_le_bytes(request[12..16].try_into().unwrap()), 0, "VIRTIO_IOMMU_ATTACH_F_BYPASS attaches an endpoint that is not translated, which an enforcing profile must never send");
}

#[test]
fn every_status_is_a_named_outcome_and_an_unknown_one_is_never_success() {
	assert_eq!(decode_status(&[S_OK, 0, 0, 0]), Ok(()));
	assert_eq!(decode_status(&[S_RANGE, 0, 0, 0]), Err(Fault::OutOfRange));
	assert_eq!(decode_status(&[S_NOENT, 0, 0, 0]), Err(Fault::NotMapped));
	assert_eq!(decode_status(&[S_INVAL, 0, 0, 0]), Err(Fault::Malformed));
	assert_eq!(decode_status(&[S_NOMEM, 0, 0, 0]), Err(Fault::NoSpace));
	assert_eq!(decode_status(&[S_IOERR, 0, 0, 0]), Err(Fault::Unconfirmed));
	assert_eq!(decode_status(&[S_DEVERR, 0, 0, 0]), Err(Fault::Unconfirmed));
	// A CODE FROM A LATER SPECIFICATION IS NOT A SUCCESS. The kernel does not know what state the
	// device is in, and the one thing it must not do with an unknown state is release a frame.
	assert_eq!(decode_status(&[200, 0, 0, 0]), Err(Fault::Unconfirmed));
	assert_eq!(decode_status(&[S_OK, 0]), Err(Fault::Unconfirmed), "a truncated tail is not a status");
}

#[test]
fn a_map_outside_the_devices_own_range_never_reaches_the_device() {
	let mut iommu = backend(Wire::ok());
	let (domain, _) = iommu.domain_create().expect("a domain");
	// The published input range ends at 0xFFFF_FFFF.
	assert_eq!(iommu.map(domain, DmaAddress(0xFFFF_F000), 0, 0x2000, Direction::ToDevice), Err(Fault::OutOfRange));
	assert_eq!(iommu.map(domain, DmaAddress(0x1001), 0, 0x1000, Direction::ToDevice), Err(Fault::Malformed), "an unaligned address is refused against the device's own page size");
	assert_eq!(iommu.map(domain, DmaAddress(0x1000), 0, 0x800, Direction::ToDevice), Err(Fault::Malformed), "and so is a length that is not a whole number of pages");
}

#[test]
fn a_domain_id_outside_the_published_range_is_refused() {
	let mut iommu = backend(Wire::ok());
	assert_eq!(iommu.attach(DomainId(0), EndpointId(1)), Err(Fault::OutOfRange), "the device published a domain range starting at one");
	assert_eq!(iommu.attach(DomainId(0x1_0000), EndpointId(1)), Err(Fault::OutOfRange));
	assert!(iommu.attach(DomainId(1), EndpointId(1)).is_ok());
}

#[test]
fn a_refused_status_is_a_refused_operation_and_not_a_quiet_one() {
	let mut iommu = backend(Wire::answering(S_NOMEM));
	let (domain, _) = iommu.domain_create().expect("a domain");
	assert_eq!(iommu.attach(domain, EndpointId(1)), Err(Fault::NoSpace));
	// And the backend does not believe the endpoint is attached afterwards: destroying the domain
	// sends no detach, because there is nothing attached to detach.
	assert!(iommu.domain_destroy(domain).is_ok(), "nothing to detach");
}

#[test]
fn a_transport_that_never_answers_is_unconfirmed_rather_than_assumed() {
	let mut wire = Wire::ok();
	wire.broken = true;
	let mut iommu = backend(wire);
	let (domain, _) = iommu.domain_create().expect("a domain");
	assert_eq!(iommu.attach(domain, EndpointId(1)), Err(Fault::Unconfirmed));
	assert_eq!(iommu.map(domain, DmaAddress(0x1000), 0x2000, 0x1000, Direction::ToDevice), Err(Fault::Unconfirmed));
	assert_eq!(iommu.unmap(domain, DmaAddress(0x1000), 0x1000), Err(Fault::Unconfirmed));
}

#[test]
fn a_malformed_event_is_dropped_and_the_good_ones_behind_it_are_still_read() {
	let mut wire = Wire::ok();
	// A record too short to be a fault.
	wire.events.push(alloc::vec![0u8; 8]);
	// One whose address flag is not set: the device is saying it does not know where.
	let mut no_address = alloc::vec![0u8; FAULT_LEN];
	no_address[0] = FAULT_R_MAPPING;
	no_address[4..8].copy_from_slice(&FAULT_F_WRITE.to_le_bytes());
	wire.events.push(no_address);
	// And a well-formed one.
	let mut good = alloc::vec![0u8; FAULT_LEN];
	good[0] = FAULT_R_MAPPING;
	good[4..8].copy_from_slice(&(FAULT_F_WRITE | FAULT_F_ADDRESS).to_le_bytes());
	good[8..12].copy_from_slice(&0x0100u32.to_le_bytes());
	good[16..24].copy_from_slice(&0xDEAD_0000u64.to_le_bytes());
	wire.events.push(good);

	let mut iommu = backend(wire);
	iommu.attach(DomainId(1), EndpointId(0x0100)).expect("attached");
	let mut out = [FaultEvent { endpoint: EndpointId(0), domain: DomainId(0), generation: Generation(0), address: DmaAddress(0), access: Access::Read, reason: Fault::NotMapped }; 4];
	let taken = iommu.drain_faults(&mut out);
	assert_eq!(taken, 1, "two rubbish records were dropped and the real one was still read");
	assert_eq!(out[0].endpoint, EndpointId(0x0100));
	assert_eq!(out[0].address, DmaAddress(0xDEAD_0000));
	assert_eq!(out[0].access, Access::Write);
	assert_eq!(out[0].reason, Fault::NotMapped);
}

#[test]
fn an_unknown_fault_reason_is_not_turned_into_one_this_code_recognises() {
	let mut raw = alloc::vec![0u8; FAULT_LEN];
	raw[0] = 200;
	raw[4..8].copy_from_slice(&(FAULT_F_READ | FAULT_F_ADDRESS).to_le_bytes());
	assert_eq!(decode_fault(&raw, Generation(1), DomainId(1)), Err(Fault::Malformed));
}

#[test]
fn the_whole_contract_runs_against_this_backend_exactly_as_it_does_against_the_fake() {
	// THE POINT OF THE INTERFACE, demonstrated: the manager's ordering rules were settled against
	// `fake`, and here they drive a real codec with no change at all.
	let config = Config::parse(&config_bytes()).expect("valid");
	let features = negotiate(REQUIRED | F_BYPASS_CONFIG).expect("negotiated");
	let device = VirtioIommu::new(Wire::ok(), config, features, Generation(1)).expect("a backend");
	assert!(device.enforces_directions(), "an enforcing profile needs all three directions");
	let mut iommu = Iommu::new(device, 8);
	let domain = iommu.create_domain(0x1000, 0x10_0000, Vec::new(), Generation(1)).expect("a domain");
	let endpoint = EndpointId(0x0100);
	iommu.attach(domain, endpoint).expect("attached");
	let requirements = Requirements { address_bits: 64, alignment: 0x1000, max_segments: 8, coherent: true };
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::FromDevice, &requirements).expect("mapped");
	let address = iommu.address_of(id).expect("an address");
	assert_eq!(iommu.translate(endpoint, address, Access::Write), Ok(0x8000_0000));
	assert_eq!(iommu.translate(endpoint, address, Access::Read), Err(Fault::Permission));
	iommu.begin_close(id).expect("closing");
	assert_eq!(iommu.finish_close(id), Ok(Release::FramesReusable));
	assert_eq!(iommu.translate(endpoint, address, Access::Write), Err(Fault::NotMapped));
}
