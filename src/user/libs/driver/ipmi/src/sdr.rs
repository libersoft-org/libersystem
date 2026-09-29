//! THE SENSOR DATA REPOSITORY (IPMI 2.0, 33 and 43): what the BMC's sensors are, read record by record in partial
//! reads under a reservation, and each record decoded at its specification's offsets.
//!
//! WHAT IS READ: full sensor records (type 0x01), with their conversion factors and thresholds; compact records (0x02),
//! which carry no conversion; event-only records (0x03), listed without a reading; FRU device locators (0x11), which
//! name the FRU devices the inventory reads; management controller locators (0x12). Any other record is kept as its
//! type alone. AT MOST `MAX_RECORDS` RECORDS AND `MAX_SENSORS` SENSORS: a larger repository is reported as truncated
//! and never read past. A record whose length is not what arrived, or whose ID string runs past it, is refused while
//! the rest of the repository is still read.

use crate::Request;
use crate::fru::{self, Field};
use alloc::string::String;
use alloc::vec::Vec;
use power_model::ipmi::{Format, Linear, Reading, TemperatureSensor};

pub const GET_SDR_REPOSITORY_INFO: u8 = 0x20;
pub const RESERVE_SDR_REPOSITORY: u8 = 0x22;
pub const GET_SDR: u8 = 0x23;
/// Get Sensor Reading, on the sensor/event network function.
pub const GET_SENSOR_READING: u8 = 0x2D;

pub const MAX_RECORDS: usize = 512;
pub const MAX_SENSORS: usize = 256;
/// One partial read asks for at most this many bytes.
pub const CHUNK: u8 = 16;
/// The record header: ID, version, type, length.
pub const HEADER: usize = 5;
/// The record ID that ends the repository.
pub const LAST: u16 = 0xFFFF;
/// The BMC's own slave address, the owner of the sensors this layer reads.
pub const BMC_OWNER: u8 = 0x20;

pub const FULL: u8 = 0x01;
pub const COMPACT: u8 = 0x02;
pub const EVENT_ONLY: u8 = 0x03;
pub const FRU_LOCATOR: u8 = 0x11;
pub const MC_LOCATOR: u8 = 0x12;

/// The sensor type of a temperature.
pub const TEMPERATURE: u8 = 0x01;
/// The event/reading type code of a threshold sensor.
pub const THRESHOLD: u8 = 0x01;

/// Get SDR Repository Info's answer, the parts this layer uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RepositoryInfo {
	pub version: u8,
	pub records: u16,
}

pub fn repository_info(data: &[u8]) -> Option<RepositoryInfo> {
	Some(RepositoryInfo { version: *data.first()?, records: crate::le16(data, 1)? })
}

pub fn reserve_request() -> Request {
	Request::new(crate::netfn::STORAGE, RESERVE_SDR_REPOSITORY, &[])
}

pub fn reservation(data: &[u8]) -> Option<u16> {
	crate::le16(data, 0)
}

/// Get SDR for `count` bytes of record `id` at `offset`, under `reservation`.
pub fn get_request(reservation: u16, id: u16, offset: u8, count: u8) -> Request {
	let [r0, r1] = reservation.to_le_bytes();
	let [i0, i1] = id.to_le_bytes();
	Request::new(crate::netfn::STORAGE, GET_SDR, &[r0, r1, i0, i1, offset, count])
}

/// Get SDR's answer: the next record's ID and the bytes returned.
pub fn get_response(data: &[u8]) -> Option<(u16, &[u8])> {
	Some((crate::le16(data, 0)?, data.get(2..)?))
}

pub fn reading_request(number: u8, lun: u8) -> Request {
	Request { netfn: crate::netfn::SENSOR_EVENT, lun, cmd: GET_SENSOR_READING, data: alloc::vec![number] }
}

/// Why a record was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Shorter than its header, or than its type's fixed fields.
	Short,
	/// Its header's length is not the bytes that arrived.
	Length,
	/// Its ID string runs past the record.
	Name,
}

/// Which kind of record a sensor came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
	Full,
	Compact,
	EventOnly,
}

/// The six thresholds a full record may carry, as raw readings, and which of them are readable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Thresholds {
	/// Bit 5 upper non-recoverable, 4 upper critical, 3 upper non-critical, 2 lower non-recoverable, 1 lower critical,
	/// 0 lower non-critical.
	pub readable: u8,
	pub upper_non_recoverable: u8,
	pub upper_critical: u8,
	pub upper_non_critical: u8,
	pub lower_non_recoverable: u8,
	pub lower_critical: u8,
	pub lower_non_critical: u8,
}

impl Thresholds {
	fn readable(&self, bit: u8, raw: u8) -> Option<u8> {
		(self.readable & (1 << bit) != 0).then_some(raw)
	}

	pub fn unr(&self) -> Option<u8> {
		self.readable(5, self.upper_non_recoverable)
	}

	pub fn uc(&self) -> Option<u8> {
		self.readable(4, self.upper_critical)
	}

	pub fn unc(&self) -> Option<u8> {
		self.readable(3, self.upper_non_critical)
	}

	pub fn lnr(&self) -> Option<u8> {
		self.readable(2, self.lower_non_recoverable)
	}

	pub fn lc(&self) -> Option<u8> {
		self.readable(1, self.lower_critical)
	}

	pub fn lnc(&self) -> Option<u8> {
		self.readable(0, self.lower_non_critical)
	}
}

/// A sensor, from a full, compact or event-only record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sensor {
	pub record_id: u16,
	pub kind: Kind,
	pub owner: u8,
	pub lun: u8,
	pub number: u8,
	pub entity: (u8, u8),
	pub sensor_type: u8,
	pub event_type: u8,
	/// Sensor Units 1 (the analog data format, rate and modifier), base and modifier units.
	pub units: (u8, u8, u8),
	/// A full record's conversion factors.
	pub linear: Option<Linear>,
	pub thresholds: Thresholds,
	pub name: String,
}

impl Sensor {
	/// Whether a reading of it can be asked for here: the BMC owns it and it is not event-only.
	pub fn readable(&self) -> bool {
		self.kind != Kind::EventOnly && self.owner == BMC_OWNER
	}

	pub fn threshold(&self) -> bool {
		self.event_type == THRESHOLD
	}

	/// Whether it is a temperature THIS LAYER CAN PUBLISH: a threshold sensor of type temperature in a full record.
	pub fn temperature(&self) -> bool {
		self.sensor_type == TEMPERATURE && self.threshold() && self.kind == Kind::Full && self.readable()
	}

	/// The thermal zone it is, with its last reading.
	pub fn temperature_sensor(&self, reading: Option<Reading>) -> Option<TemperatureSensor> {
		let linear = self.linear?;
		Some(TemperatureSensor { linear, base_unit: self.units.1, upper_non_critical: self.thresholds.unc(), upper_critical: self.thresholds.uc(), upper_non_recoverable: self.thresholds.unr(), reading })
	}

	/// A reading in milli-units of its base unit, through the one conversion: `Unsupported` without factors.
	pub fn value(&self, reading: &Reading) -> power_model::convert::Tagged<i64> {
		match self.linear {
			Some(linear) => reading.value(|raw| linear.value(raw, 3)),
			None => power_model::convert::Tagged::Unsupported,
		}
	}
}

/// A FRU device locator: a logical FRU device reached with Read FRU Data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FruLocator {
	pub record_id: u16,
	/// The FRU device ID, when the device is logical.
	pub device: u8,
	pub logical: bool,
	pub entity: (u8, u8),
	pub name: String,
}

/// A management controller locator.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct McLocator {
	pub record_id: u16,
	pub address: u8,
	pub channel: u8,
	pub name: String,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Record {
	Sensor(Sensor),
	Fru(FruLocator),
	Mc(McLocator),
	/// A record of a type this layer does not decode.
	Other {
		record_id: u16,
		record_type: u8,
	},
}

// The ID string at `at`: its type/length byte and the bytes it names, which must be inside the record.
fn name(record: &[u8], at: usize) -> Result<String, Refusal> {
	let Some(&type_length) = record.get(at) else { return Ok(String::new()) };
	let length = (type_length & 0x1F) as usize;
	let bytes = record.get(at + 1..at + 1 + length).ok_or(Refusal::Name)?;
	Ok(match fru::decode(fru::kind(type_length), bytes) {
		// A string whose type is "unicode" or unspecified is shown as its bytes.
		Field::Binary(raw) => Field::Binary(raw).text(),
		Field::Text(text) => text,
	})
}

/// Decode one whole record.
pub fn record(bytes: &[u8]) -> Result<Record, Refusal> {
	if bytes.len() < HEADER {
		return Err(Refusal::Short);
	}
	if HEADER + bytes[4] as usize != bytes.len() {
		return Err(Refusal::Length);
	}
	let record_id = u16::from_le_bytes([bytes[0], bytes[1]]);
	let record_type = bytes[3];
	let needs = |fixed: usize| if bytes.len() < fixed { Err(Refusal::Short) } else { Ok(()) };
	match record_type {
		FULL => {
			needs(48)?;
			let units = (bytes[20], bytes[21], bytes[22]);
			let factors: [u8; 7] = bytes[23..30].try_into().map_err(|_| Refusal::Short)?;
			let linear = Linear::from_record(units.0, &factors);
			let thresholds = if bytes[13] == THRESHOLD { Thresholds { readable: bytes[18] & 0x3F, upper_non_recoverable: bytes[36], upper_critical: bytes[37], upper_non_critical: bytes[38], lower_non_recoverable: bytes[39], lower_critical: bytes[40], lower_non_critical: bytes[41] } } else { Thresholds::default() };
			Ok(Record::Sensor(Sensor { record_id, kind: Kind::Full, owner: bytes[5], lun: bytes[6] & 3, number: bytes[7], entity: (bytes[8], bytes[9]), sensor_type: bytes[12], event_type: bytes[13], units, linear: (linear.format != Format::None).then_some(linear), thresholds, name: name(bytes, 47)? }))
		}
		COMPACT => {
			needs(32)?;
			Ok(Record::Sensor(Sensor { record_id, kind: Kind::Compact, owner: bytes[5], lun: bytes[6] & 3, number: bytes[7], entity: (bytes[8], bytes[9]), sensor_type: bytes[12], event_type: bytes[13], units: (bytes[20], bytes[21], bytes[22]), linear: None, thresholds: Thresholds::default(), name: name(bytes, 31)? }))
		}
		EVENT_ONLY => {
			needs(17)?;
			Ok(Record::Sensor(Sensor { record_id, kind: Kind::EventOnly, owner: bytes[5], lun: bytes[6] & 3, number: bytes[7], entity: (bytes[8], bytes[9]), sensor_type: bytes[10], event_type: bytes[11], units: (0, 0, 0), linear: None, thresholds: Thresholds::default(), name: name(bytes, 16)? }))
		}
		FRU_LOCATOR => {
			needs(16)?;
			Ok(Record::Fru(FruLocator { record_id, device: bytes[6], logical: bytes[7] & 0x80 != 0, entity: (bytes[12], bytes[13]), name: name(bytes, 15)? }))
		}
		MC_LOCATOR => {
			needs(16)?;
			Ok(Record::Mc(McLocator { record_id, address: bytes[5], channel: bytes[6] & 0x0F, name: name(bytes, 15)? }))
		}
		_ => Ok(Record::Other { record_id, record_type }),
	}
}

/// THE REPOSITORY AS READ: every record decoded, the ones refused counted, and whether a bound stopped the read.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Repository {
	pub records: Vec<Record>,
	pub refused: u32,
	pub truncated: bool,
}

impl Repository {
	/// Add one record's bytes; false once a bound is reached, when the read stops and the repository is truncated.
	pub fn add(&mut self, bytes: &[u8]) -> bool {
		if self.records.len() + self.refused as usize >= MAX_RECORDS {
			self.truncated = true;
			return false;
		}
		match record(bytes) {
			Ok(Record::Sensor(sensor)) => {
				if self.sensors().count() >= MAX_SENSORS {
					self.truncated = true;
					return false;
				}
				self.records.push(Record::Sensor(sensor));
			}
			Ok(other) => self.records.push(other),
			Err(_) => self.refused += 1,
		}
		true
	}

	pub fn sensors(&self) -> impl Iterator<Item = &Sensor> {
		self.records.iter().filter_map(|record| match record {
			Record::Sensor(sensor) => Some(sensor),
			_ => None,
		})
	}

	pub fn fru_devices(&self) -> impl Iterator<Item = &FruLocator> {
		self.records.iter().filter_map(|record| match record {
			Record::Fru(locator) if locator.logical => Some(locator),
			_ => None,
		})
	}
}

/// THE PARTIAL READ OF ONE RECORD: the header first, then the body in `CHUNK`s. The caller sends `next()`'s request
/// and hands back the bytes; `Done` carries the record and the ID of the next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecordRead {
	pub id: u16,
	bytes: Vec<u8>,
	next: u16,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
	/// Ask for `count` bytes at `offset`.
	Ask { offset: u8, count: u8 },
	/// The whole record, and the next record's ID.
	Done { bytes: Vec<u8>, next: u16 },
}

impl RecordRead {
	pub fn new(id: u16) -> RecordRead {
		RecordRead { id, bytes: Vec::new(), next: LAST }
	}

	/// What to ask for next, or the record.
	pub fn step(&self) -> Step {
		if self.bytes.len() < HEADER {
			return Step::Ask { offset: self.bytes.len() as u8, count: (HEADER - self.bytes.len()) as u8 };
		}
		let total = HEADER + self.bytes[4] as usize;
		if self.bytes.len() >= total {
			return Step::Done { bytes: self.bytes[..total].to_vec(), next: self.next };
		}
		Step::Ask { offset: self.bytes.len() as u8, count: ((total - self.bytes.len()) as u8).min(CHUNK) }
	}

	/// The answer to the last `Ask`: the next record's ID and the bytes. False when they are not what was asked for.
	pub fn answered(&mut self, next: u16, bytes: &[u8]) -> bool {
		let Step::Ask { count, .. } = self.step() else { return false };
		if bytes.is_empty() || bytes.len() > count as usize {
			return false;
		}
		self.next = next;
		self.bytes.extend_from_slice(bytes);
		true
	}
}
