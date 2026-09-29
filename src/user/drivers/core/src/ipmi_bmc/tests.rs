use super::*;
use alloc::vec;
use ipmi::netfn;

// A BMC MADE OF TABLES: a device ID and a GUID, an SDR repository that cancels its first reservation once when asked
// to, sensor readings, a SEL whose reservation every added record cancels, FRU bytes, a chassis, an erase that
// takes a poll to finish - and a record of every command it received, in order.
#[derive(Default)]
struct Fake {
	guid: Option<[u8; 16]>,
	records: Vec<Vec<u8>>,
	cancel_once: bool,
	readings: Vec<(u8, [u8; 3])>,
	sel: Vec<[u8; 16]>,
	reserve: bool,
	reservation: u16,
	erase_polls: u32,
	fru: Vec<u8>,
	chassis_refuses: Option<u8>,
	received: Vec<(u8, u8)>,
	clock: u64,
}

impl Fake {
	fn answer(&mut self, request: &Request) -> Response {
		let ok = |data: Vec<u8>| Response { cc: 0, data };
		let refuse = |cc: u8| Response { cc, data: Vec::new() };
		self.received.push((request.netfn, request.cmd));
		match (request.netfn, request.cmd) {
			(netfn::APP, ipmi::identity::GET_DEVICE_ID) => ok(vec![0x20, 0x81, 0x02, 0x05, 0x02, 0xBF, 0x57, 0x01, 0x00, 0x34, 0x12]),
			(netfn::APP, ipmi::identity::GET_DEVICE_GUID) => self.guid.map_or(refuse(cc::INVALID_COMMAND), |guid| ok(guid.to_vec())),
			(netfn::STORAGE, sdr::RESERVE_SDR_REPOSITORY) => {
				self.reservation = self.reservation.wrapping_add(1);
				ok(self.reservation.to_le_bytes().to_vec())
			}
			(netfn::STORAGE, sdr::GET_SDR) => {
				let d = &request.data;
				let (reservation, id, offset, count) = (u16::from_le_bytes([d[0], d[1]]), u16::from_le_bytes([d[2], d[3]]) as usize, d[4] as usize, d[5] as usize);
				if offset != 0 && reservation != self.reservation {
					return refuse(cc::RESERVATION_CANCELLED);
				}
				if self.cancel_once && offset != 0 {
					self.cancel_once = false;
					self.reservation = self.reservation.wrapping_add(1);
					return refuse(cc::RESERVATION_CANCELLED);
				}
				let Some(record) = self.records.get(id) else { return refuse(cc::NOT_PRESENT) };
				let next = if id + 1 < self.records.len() { (id + 1) as u16 } else { sdr::LAST };
				let mut out = next.to_le_bytes().to_vec();
				out.extend_from_slice(&record[offset..(offset + count).min(record.len())]);
				ok(out)
			}
			(netfn::SENSOR_EVENT, sdr::GET_SENSOR_READING) => match self.readings.iter().find(|(number, _)| *number == request.data[0]) {
				Some((_, reading)) => ok(reading.to_vec()),
				None => refuse(cc::NOT_PRESENT),
			},
			(netfn::STORAGE, sel::GET_SEL_INFO) => {
				if self.erase_polls > 0 {
					self.erase_polls -= 1;
					return refuse(cc::ERASE_IN_PROGRESS);
				}
				let mut out = vec![0x51];
				out.extend_from_slice(&(self.sel.len() as u16).to_le_bytes());
				out.extend_from_slice(&[0; 10]);
				out.push(if self.reserve { 0x02 } else { 0x00 });
				ok(out)
			}
			(netfn::STORAGE, sel::RESERVE_SEL) => {
				self.reservation = self.reservation.wrapping_add(1);
				ok(self.reservation.to_le_bytes().to_vec())
			}
			(netfn::STORAGE, sel::GET_SEL_ENTRY) => {
				let id = u16::from_le_bytes([request.data[2], request.data[3]]) as usize;
				let Some(record) = self.sel.get(id) else { return refuse(cc::NOT_PRESENT) };
				let next = if id + 1 < self.sel.len() { (id + 1) as u16 } else { sel::LAST };
				let mut out = next.to_le_bytes().to_vec();
				out.extend_from_slice(record);
				ok(out)
			}
			(netfn::STORAGE, sel::CLEAR_SEL) => {
				if u16::from_le_bytes([request.data[0], request.data[1]]) != self.reservation {
					return refuse(cc::RESERVATION_CANCELLED);
				}
				self.sel.clear();
				self.erase_polls = 2;
				ok(vec![0])
			}
			(netfn::STORAGE, fru::GET_FRU_INVENTORY_AREA_INFO) => ok(vec![self.fru.len() as u8, (self.fru.len() >> 8) as u8, 0]),
			(netfn::STORAGE, fru::READ_FRU_DATA) => {
				let offset = u16::from_le_bytes([request.data[1], request.data[2]]) as usize;
				let count = (request.data[3] as usize).min(self.fru.len() - offset);
				let mut out = vec![count as u8];
				out.extend_from_slice(&self.fru[offset..offset + count]);
				ok(out)
			}
			(netfn::CHASSIS, ipmi::chassis::CHASSIS_CONTROL) => self.chassis_refuses.map_or(ok(Vec::new()), refuse),
			_ => refuse(cc::INVALID_COMMAND),
		}
	}

	// A record added to the SEL cancels its reservation, as the specification requires.
	fn add_event(&mut self) {
		let mut record = [0u8; 16];
		record[0..2].copy_from_slice(&(self.sel.len() as u16).to_le_bytes());
		record[2] = 0x02;
		self.sel.push(record);
		self.reservation = self.reservation.wrapping_add(1);
	}
}

impl Bmc for Fake {
	fn ask(&mut self, request: &Request) -> Result<Response, Failure> {
		Ok(self.answer(request))
	}

	fn now_ms(&mut self) -> u64 {
		self.clock
	}

	fn pause(&mut self) {
		self.clock += 10;
	}
}

fn temperature_record(id: u16, number: u8) -> Vec<u8> {
	let mut body = vec![0u8; 43];
	body[0] = sdr::BMC_OWNER;
	body[2] = number;
	body[7] = sdr::TEMPERATURE;
	body[8] = sdr::THRESHOLD;
	body[13] = 0b0011_1000;
	body[16] = 1;
	body[19] = 1;
	body[31] = 95;
	body[32] = 85;
	body[33] = 70;
	body[42] = 0xC4;
	body.extend_from_slice(b"Temp");
	let mut out = id.to_le_bytes().to_vec();
	out.extend_from_slice(&[0x51, sdr::FULL, body.len() as u8]);
	out.extend_from_slice(&body);
	out
}

#[test]
fn the_bmc_is_identified_by_its_guid_or_its_fallback() {
	let mut bmc = Fake { guid: Some([7; 16]), ..Fake::default() };
	let (device, identity) = identify(&mut bmc, "pci:0000:00:05.0").expect("answered");
	assert_eq!((device.product, identity.name()), (0x1234, alloc::format!("bmc:{}", "07".repeat(16))));
	let mut plain = Fake::default();
	let (_, identity) = identify(&mut plain, "acpi:\\_SB_.PCI0.SF8_.MI00").expect("answered without a GUID");
	assert_eq!(identity.name(), "bmc:00157-1234-20@acpi:\\_SB_.PCI0.SF8_.MI00");
}

#[test]
fn the_repository_is_read_in_partial_reads_and_a_cancelled_reservation_taken_again() {
	let mut bmc = Fake { records: vec![temperature_record(0, 0x30), temperature_record(1, 0x31)], cancel_once: true, ..Fake::default() };
	let read = read_repository(&mut bmc);
	assert_eq!(read.answer, None);
	assert_eq!(read.records.sensors().map(|sensor| sensor.number).collect::<Vec<_>>(), [0x30, 0x31]);
	let reservations = bmc.received.iter().filter(|command| **command == (netfn::STORAGE, sdr::RESERVE_SDR_REPOSITORY)).count();
	assert_eq!(reservations, 2, "the cancelled reservation was taken again");
	assert!(bmc.received.iter().filter(|command| **command == (netfn::STORAGE, sdr::GET_SDR)).count() >= 6, "each record in several partial reads");
	// AND A SENSOR READ THROUGH IT.
	bmc.readings.push((0x30, [42, 0xC0, 0xC0]));
	let sensor = read.records.sensors().next().expect("a sensor").clone();
	let (reading, bits) = read_sensor(&mut bmc, &sensor).expect("read");
	assert_eq!((reading.raw, bits, sensor.value(&reading)), (42, 0xC0, power_model::convert::Tagged::Known(42_000)));
}

#[test]
fn a_sel_page_reads_one_entry_per_request_and_ends_at_the_last() {
	let mut bmc = Fake::default();
	for _ in 0..3 {
		bmc.add_event();
	}
	let (records, next, refused) = sel_page(&mut bmc, sel::FIRST).expect("a page");
	assert_eq!((records.len(), next, refused.len()), (3, sel::LAST, 0));
	let (records, next, _) = sel_page(&mut Fake::default(), sel::FIRST).expect("an empty log is a page");
	assert_eq!((records.len(), next), (0, sel::LAST));
}

fn selector() -> Vec<u8> {
	b"bmc:0707070707070707070707070707070707".to_vec()
}

// THE SEL CLEAR'S RULES: its payload the target's name, its count the live one, a BMC with Reserve SEL - and its
// clear under the preparation's reservation.
#[test]
fn a_sel_clear_erases_only_what_was_counted() {
	let mut bmc = Fake { reserve: true, ..Fake::default() };
	for _ in 0..3 {
		bmc.add_event();
	}
	assert_eq!(prepare_sel_clear(&mut bmc, &selector(), &[3, 0], b"other"), Err(Refusal::Payload));
	assert_eq!(prepare_sel_clear(&mut bmc, &selector(), &[3], &selector()), Err(Refusal::Parameters));
	assert_eq!(prepare_sel_clear(&mut bmc, &selector(), &[2, 0], &selector()), Err(Refusal::Count), "a count that is not the live one");
	assert_eq!(prepare_sel_clear(&mut Fake::default(), &selector(), &[0, 0], &selector()), Err(Refusal::NoReserve));
	let prepared = prepare_sel_clear(&mut bmc, &selector(), &[3, 0], &selector()).expect("prepared");
	assert_eq!(execute_sel_clear(&mut bmc, &prepared, 1_000), Outcome::Completed, "cleared and the erasure polled to its end");
	assert!(bmc.sel.is_empty());
	// AN EVENT BETWEEN PREPARATION AND EXECUTION cancels the reservation: nothing is erased.
	let mut bmc = Fake { reserve: true, ..Fake::default() };
	bmc.add_event();
	let prepared = prepare_sel_clear(&mut bmc, &selector(), &[1, 0], &selector()).expect("prepared");
	bmc.add_event();
	assert_eq!(execute_sel_clear(&mut bmc, &prepared, 1_000), Outcome::Failed);
	assert_eq!(bmc.sel.len(), 2, "every record, the new one included, is still in the log");
}

#[test]
fn chassis_control_takes_four_operations_and_reports_the_bmc_s_refusal() {
	assert_eq!(prepare_chassis(&selector(), &[3], &selector()), Ok(ipmi::chassis::Control::HardReset));
	assert_eq!(prepare_chassis(&selector(), &[1], &selector()), Err(Refusal::Operation), "power up");
	assert_eq!(prepare_chassis(&selector(), &[3], b"x"), Err(Refusal::Payload));
	assert_eq!(prepare_chassis(&selector(), &[3, 0], &selector()), Err(Refusal::Parameters));
	let mut refusing = Fake { chassis_refuses: Some(cc::NOT_SUPPORTED_IN_STATE), ..Fake::default() };
	assert_eq!(execute_chassis(&mut refusing, ipmi::chassis::Control::PowerCycle), (Outcome::Failed, Some(0xD5)), "QEMU's refusal of a power cycle");
	assert_eq!(execute_chassis(&mut Fake::default(), ipmi::chassis::Control::SoftShutdown), (Outcome::Completed, None));
}

#[test]
fn fru_data_is_read_in_bounded_chunks() {
	let mut bmc = Fake { fru: (0..100u8).collect(), ..Fake::default() };
	let bytes = fru_bytes(&mut bmc, 0).expect("read");
	assert_eq!(bytes, (0..100u8).collect::<Vec<_>>());
	assert_eq!(bmc.received.iter().filter(|command| **command == (netfn::STORAGE, fru::READ_FRU_DATA)).count(), 4, "32 bytes at a time");
}

#[test]
fn an_answer_is_reduced_to_what_a_reader_reports() {
	assert_eq!(answer_of(&Ok(Response { cc: cc::TIMEOUT, data: Vec::new() })), Answer::Unavailable);
	assert_eq!(answer_of(&Ok(Response { cc: 0xCB, data: Vec::new() })), Answer::Refused(0xCB));
	assert_eq!(answer_of(&Err(Failure::TooLong)), Answer::Malformed);
	assert_eq!(answer_of(&Err(Failure::Deadline)), Answer::Unavailable);
}
