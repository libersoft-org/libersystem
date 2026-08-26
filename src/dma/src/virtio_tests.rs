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
	// What the device writes into the properties area of a probe answer.
	properties: Vec<u8>,
	broken: bool,
}

impl Wire {
	fn ok() -> Self {
		Self { answers: Vec::new(), sent: Vec::new(), events: Vec::new(), properties: Vec::new(), broken: false }
	}

	fn answering(status: u8) -> Self {
		Self { answers: alloc::vec![status], sent: Vec::new(), events: Vec::new(), properties: Vec::new(), broken: false }
	}
}

impl Transport for Wire {
	fn request(&mut self, request: &[u8], answer: &mut [u8], status_at: usize) -> Result<(), Fault> {
		self.sent.push(request.to_vec());
		if self.broken {
			return Err(Fault::Unconfirmed);
		}
		// Whatever a test queued for the device-writable part, then the status where the caller says
		// it goes - which is the layout the real device writes.
		if !self.properties.is_empty() {
			let n = self.properties.len().min(answer.len());
			answer[..n].copy_from_slice(&self.properties[..n]);
		}
		let status = if self.answers.is_empty() { S_OK } else { self.answers.remove(0) };
		if status_at < answer.len() {
			answer[status_at] = status;
		}
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
fn a_domain_built_from_a_probe_never_allocates_inside_a_region_the_endpoint_reserved() {
	// THE ANSWER IS APPLIED, NOT PRINTED. `install_probed_regions` used to log a reserved hole under
	// a comment saying "the space is told rather than trusted to avoid it by accident" - and the
	// space was not told, because the domain had already been created whole by then. Probing before
	// the attach is what lets the reservations be part of the domain, and this is that end to end:
	// a device reports a hole, the domain is built with it, and the allocator steps over it.
	let mut properties = Vec::new();
	// A reserved region at 0x2000, one page.
	properties.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	properties.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	properties.push(RESV_MEM_T_RESERVED);
	properties.extend_from_slice(&[0u8; 3]);
	properties.extend_from_slice(&0x2000u64.to_le_bytes());
	properties.extend_from_slice(&0x2fffu64.to_le_bytes());
	let regions = decode_probe(&properties).expect("a complete answer");
	assert_eq!(regions, alloc::vec![ProbedRegion { kind: RegionKind::Reserved, base: 0x2000, len: 0x1000 }]);

	let reserved: Vec<Reserved> = regions.iter().filter(|r| r.kind == RegionKind::Reserved).map(|r| Reserved { base: r.base, len: r.len }).collect();
	let config = Config::parse(&config_bytes()).expect("valid");
	let features = negotiate(REQUIRED | F_BYPASS_CONFIG).expect("negotiated");
	let device = VirtioIommu::new(Wire::ok(), config, features, Generation(1)).expect("a backend");
	let mut iommu = Iommu::new(device, 8);
	let domain = iommu.create_domain(0x1000, 0x4000, reserved, Generation(1)).expect("a domain");
	let endpoint = EndpointId(0x0100);
	iommu.attach(domain, endpoint).expect("attached");
	let requirements = Requirements::new(64, 0x1000, 8, true).expect("a device this shape exists");

	let first = iommu.map(domain, 0x10_0000, 0x1000, Direction::ToDevice, &requirements).expect("room below the hole");
	assert_eq!(iommu.address_of(first), Some(DmaAddress(0x1000)));
	let second = iommu.map(domain, 0x11_0000, 0x1000, Direction::ToDevice, &requirements).expect("room above it");
	assert_eq!(iommu.address_of(second), Some(DmaAddress(0x3000)), "the page the endpoint reserved is stepped over, not handed to it");
}

#[test]
fn a_record_too_short_to_be_a_fault_is_dropped_and_a_less_detailed_one_is_not() {
	// THE TWO ARE NOT THE SAME THING, and they used to be. A record shorter than the structure is a
	// device that did not send a fault report; a record that IS one and does not carry an address is
	// a controller reporting a real fault it did not record the address of. Refusing the second
	// discarded exactly the message the fault queue exists to deliver.
	let mut wire = Wire::ok();
	// A record too short to be a fault. Still dropped: there is nothing here to read.
	wire.events.push(alloc::vec![0u8; 8]);
	// One whose address flag is not set: the controller is saying it does not know where.
	let mut no_address = alloc::vec![0u8; FAULT_LEN];
	no_address[0] = FAULT_R_MAPPING;
	no_address[4..8].copy_from_slice(&FAULT_F_WRITE.to_le_bytes());
	no_address[8..12].copy_from_slice(&0x0100u32.to_le_bytes());
	// A NON-ZERO ADDRESS FIELD BEHIND A CLEAR FLAG. The field is not meaningful, so it must not be
	// read - and reading it would be this side inventing a number the flag said to ignore.
	no_address[16..24].copy_from_slice(&0xFFFF_FFFFu64.to_le_bytes());
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
	let mut out = [FaultEvent { endpoint: EndpointId(0), domain: DomainId(0), generation: Generation(0), address: None, access: Access::Read, reason: Fault::NotMapped }; 4];
	let taken = iommu.drain_faults(&mut out);
	assert_eq!(taken, 2, "the short record was dropped, and BOTH real faults were read");
	assert_eq!(out[0].endpoint, EndpointId(0x0100));
	assert_eq!(out[0].address, None, "no address, rather than the field the flag said to ignore");
	assert_eq!(out[0].access, Access::Write);
	assert_eq!(out[0].reason, Fault::NotMapped);
	assert_eq!(out[1].address, Some(DmaAddress(0xDEAD_0000)));
	assert_eq!(out[1].access, Access::Write);
}

#[test]
fn an_unknown_fault_reason_is_not_turned_into_one_this_code_recognises() {
	// AND IT IS NOT DROPPED EITHER. It became `Malformed` and `drain_faults` threw it away, so a
	// controller reporting a reason added after this was written reported nothing at all. The rule
	// `decode_status` has always used for status codes is the right one here too: a meaning this
	// code does not know is a state it does not know, which is `Unconfirmed`.
	let mut raw = alloc::vec![0u8; FAULT_LEN];
	raw[0] = 200;
	raw[4..8].copy_from_slice(&(FAULT_F_READ | FAULT_F_ADDRESS).to_le_bytes());
	raw[16..24].copy_from_slice(&0x4000u64.to_le_bytes());
	let event = decode_fault(&raw, Generation(1), DomainId(1)).expect("a fault this code cannot name is still a fault");
	assert_eq!(event.reason, Fault::Unconfirmed, "not NotMapped, not UnknownEndpoint - unconfirmed");
	assert_eq!(event.address, Some(DmaAddress(0x4000)), "and everything it DID say is still read");
	// A direction this port does not recognise is read conservatively rather than refused: an
	// execute fault names no read and no write bit, and it is still a fault.
	let mut exec_only = alloc::vec![0u8; FAULT_LEN];
	exec_only[0] = FAULT_R_MAPPING;
	exec_only[4..8].copy_from_slice(&FAULT_F_ADDRESS.to_le_bytes());
	assert_eq!(decode_fault(&exec_only, Generation(1), DomainId(1)).expect("still a fault").access, Access::Read);
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
	let requirements = Requirements::new(64, 0x1000, 8, true).expect("a device this shape exists");
	let id = iommu.map(domain, 0x8000_0000, 0x1000, Direction::FromDevice, &requirements).expect("mapped");
	let address = iommu.address_of(id).expect("an address");
	assert_eq!(iommu.translate(endpoint, address, Access::Write), Ok(0x8000_0000));
	assert_eq!(iommu.translate(endpoint, address, Access::Read), Err(Fault::Permission));
	iommu.begin_close(id).expect("closing");
	assert_eq!(iommu.finish_close(id), Ok(Release::FramesReusable));
	assert_eq!(iommu.translate(endpoint, address, Access::Write), Err(Fault::NotMapped));
}

fn fault_event(endpoint: u32, address: u64) -> Vec<u8> {
	let mut bytes = alloc::vec![0u8; FAULT_LEN];
	bytes[0] = FAULT_R_MAPPING;
	bytes[4..8].copy_from_slice(&(FAULT_F_WRITE | FAULT_F_ADDRESS).to_le_bytes());
	bytes[8..12].copy_from_slice(&endpoint.to_le_bytes());
	bytes[16..24].copy_from_slice(&address.to_le_bytes());
	bytes
}

#[test]
fn a_fault_names_the_domain_of_the_endpoint_that_raised_it() {
	// TWO ENDPOINTS IN TWO DOMAINS - which is every machine this backend was written for, because
	// each bus-mastering device is given a domain of its own. The attribution used to take the FIRST
	// entry of the attachment list for every event, so every fault after the first device named the
	// wrong domain, and the quarantine and stale-generation decisions taken on it were about a
	// binding that had not faulted.
	let mut wire = Wire::ok();
	wire.events.push(fault_event(0x0200, 0xDEAD_0000));
	wire.events.push(fault_event(0x0099, 0xBEEF_0000));
	let mut iommu = backend(wire);
	iommu.attach(DomainId(1), EndpointId(0x0100)).expect("attached");
	iommu.attach(DomainId(2), EndpointId(0x0200)).expect("attached");

	let mut out = [FaultEvent { endpoint: EndpointId(0), domain: DomainId(0), generation: Generation(0), address: None, access: Access::Read, reason: Fault::NotMapped }; 4];
	assert_eq!(iommu.drain_faults(&mut out), 2);
	assert_eq!(out[0].endpoint, EndpointId(0x0200), "the device names the requester");
	assert_eq!(out[0].domain, DomainId(2), "and this side resolves which domain that requester is in");
	// An endpoint this backend holds no binding for is not attributed to somebody else's domain.
	assert_eq!(out[1].endpoint, EndpointId(0x0099));
	assert_eq!(out[1].domain, DomainId(0), "no domain, rather than the wrong one");
}

#[test]
fn a_binding_is_stamped_with_the_generation_it_was_made_under() {
	// A rebind moves the generation, and the bindings made before it do not move with it: a fault
	// from the older binding must still name the generation that binding was made under, which is
	// what lets a stale completion be told from a current one.
	let mut wire = Wire::ok();
	wire.events.push(fault_event(0x0100, 0x1000));
	let mut iommu = backend(wire);
	iommu.attach(DomainId(1), EndpointId(0x0100)).expect("attached");
	iommu.set_generation(Generation(7));
	let mut out = [FaultEvent { endpoint: EndpointId(0), domain: DomainId(0), generation: Generation(0), address: None, access: Access::Read, reason: Fault::NotMapped }; 2];
	assert_eq!(iommu.drain_faults(&mut out), 1);
	assert_eq!(out[0].generation, Generation(1), "the generation the binding was made under, not the one current now");
}

// A device that publishes a probe buffer, which the ordinary fixture does not: the two are different
// machines and the probe path only exists on the second.
fn probing_backend(wire: Wire, probe_size: u32) -> VirtioIommu<Wire> {
	let mut bytes = config_bytes();
	bytes[32..36].copy_from_slice(&probe_size.to_le_bytes());
	let config = Config::parse(&bytes).expect("a valid config");
	let features = negotiate(REQUIRED | F_BYPASS_CONFIG | F_PROBE).expect("features");
	VirtioIommu::new(wire, config, features, Generation(1)).expect("a backend")
}

#[test]
fn a_probe_reads_the_doorbell_the_device_reports_and_skips_what_it_does_not_know() {
	// THE DEVICE IS ASKED RATHER THAN THE ADDRESS COMPILED IN. `negotiate` has acknowledged `F_PROBE`
	// since this backend was written and nothing ever sent a probe, so the reserved regions were an
	// empty list by omission - and the one that matters is the doorbell every endpoint writes its
	// interrupts to.
	let mut wire = Wire::ok();
	let mut properties = Vec::new();
	// A property type this driver does not know, skipped by its own declared length.
	properties.extend_from_slice(&0x0abcu16.to_le_bytes());
	properties.extend_from_slice(&8u16.to_le_bytes());
	properties.extend_from_slice(&[0xffu8; 8]);
	// The MSI doorbell, inclusive on the wire.
	properties.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	properties.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	properties.push(RESV_MEM_T_MSI);
	properties.extend_from_slice(&[0u8; 3]);
	properties.extend_from_slice(&0xfee0_0000u64.to_le_bytes());
	properties.extend_from_slice(&0xfeef_ffffu64.to_le_bytes());
	// A hole nothing may be mapped in.
	properties.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	properties.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	properties.push(RESV_MEM_T_RESERVED);
	properties.extend_from_slice(&[0u8; 3]);
	properties.extend_from_slice(&0x1000u64.to_le_bytes());
	properties.extend_from_slice(&0x1fffu64.to_le_bytes());
	wire.properties = properties;

	let mut iommu = probing_backend(wire, 256);
	let regions = iommu.probe(EndpointId(0x0100)).expect("probed");
	assert_eq!(regions.len(), 2, "the unknown property was skipped by its length, not stumbled over");
	assert_eq!(regions[0], ProbedRegion { kind: RegionKind::MsiDoorbell, base: 0xfee0_0000, len: 0x10_0000 }, "inclusive on the wire, a length here");
	assert_eq!(regions[1], ProbedRegion { kind: RegionKind::Reserved, base: 0x1000, len: 0x1000 });
	// The request is the one the specification defines: type, endpoint, and the reserved tail.
	let sent = iommu.transport().sent.last().expect("a request was sent");
	assert_eq!(sent.len(), REQ_PROBE_LEN);
	assert_eq!(sent[0], T_PROBE);
	assert_eq!(u32::from_le_bytes([sent[4], sent[5], sent[6], sent[7]]), 0x0100);
}

#[test]
fn a_probe_property_that_runs_past_the_buffer_refuses_the_whole_answer() {
	// A device describing memory it did not send. Everything behind it is unlocatable - so this used
	// to stop the walk and hand back what had been read so far, which the caller could not tell from
	// a complete list. What may be behind the cut is the doorbell this endpoint's interrupts go
	// through or a hole its allocator must avoid, and a domain built from half an answer is a domain
	// built on the assumption that the missing half said nothing.
	let mut properties = Vec::new();
	properties.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	properties.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	properties.push(RESV_MEM_T_MSI);
	properties.extend_from_slice(&[0u8; 3]);
	properties.extend_from_slice(&0xfee0_0000u64.to_le_bytes());
	properties.extend_from_slice(&0xfeef_ffffu64.to_le_bytes());
	properties.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	properties.extend_from_slice(&4096u16.to_le_bytes());
	assert_eq!(decode_probe(&properties), Err(Fault::Malformed), "a good property before the cut does not make a cut answer usable");
	// A region whose end is below its start is a device contradicting itself. THAT one is dropped
	// rather than refused: the device sent everything it said it would, and one self-contradicting
	// range is not a truncated list.
	let mut backwards = Vec::new();
	backwards.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	backwards.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	backwards.push(RESV_MEM_T_MSI);
	backwards.extend_from_slice(&[0u8; 3]);
	backwards.extend_from_slice(&0x2000u64.to_le_bytes());
	backwards.extend_from_slice(&0x1000u64.to_le_bytes());
	assert_eq!(decode_probe(&backwards), Ok(Vec::new()));
	// A region ending at the top of the address space is a length this arithmetic cannot express,
	// and it used to wrap to zero rather than say so.
	let mut to_the_top = Vec::new();
	to_the_top.extend_from_slice(&PROBE_T_RESV_MEM.to_le_bytes());
	to_the_top.extend_from_slice(&(PROBE_RESV_MEM_LEN as u16).to_le_bytes());
	to_the_top.push(RESV_MEM_T_MSI);
	to_the_top.extend_from_slice(&[0u8; 3]);
	to_the_top.extend_from_slice(&0u64.to_le_bytes());
	to_the_top.extend_from_slice(&u64::MAX.to_le_bytes());
	assert_eq!(decode_probe(&to_the_top), Err(Fault::Malformed));
	// And a zero type terminates the list, which is a COMPLETE answer with nothing in it.
	assert_eq!(decode_probe(&[0, 0, 0, 0]), Ok(Vec::new()));
}
