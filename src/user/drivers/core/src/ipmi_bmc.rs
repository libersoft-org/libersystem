//! THE BMC, AS ONE `ipmi` BINDING READS AND DRIVES IT - the parts a host can test: every sequence of transactions the
//! driver runs, over `Bmc`, a channel that carries one transaction at a time. Which interface carries them - KCS, BT,
//! SSIF - is the driver's; what they are is here.
//!
//! EVERY LONG READ IS CUT INTO BOUNDED TRANSACTIONS: an SDR record in partial reads under a reservation (taken again,
//! a bounded number of times, when the BMC cancels it), FRU data in 32-byte reads, one SEL entry per request. The
//! driver interleaves a watchdog pet between any two of them.
//!
//! THE TWO ADMINISTRATIVE EXECUTORS' RULES. SEL CLEAR ERASES ONLY WHAT WAS COUNTED: preparation refuses a payload that
//! is not the target's own name, a count that is not the live one (Get SEL Info) and a BMC that reports no Reserve SEL,
//! and takes the reservation; execution sends Clear SEL under THAT reservation - which the BMC cancels when an event is
//! added in between, so nothing is erased and the outcome is `failed` - and polls the erasure to its end through Get
//! SEL Info. CHASSIS CONTROL takes one of four operations and nothing else.

use alloc::string::String;
use alloc::vec::Vec;
use ipmi::identity::{DeviceId, Identity};
use ipmi::{Failure, Request, Response, cc, fru, lan, sdr, sel};
use power_model::ipmi::Reading;

/// One transaction at a time to the BMC, with the clock its deadlines are measured against.
pub trait Bmc {
	fn ask(&mut self, request: &Request) -> Result<Response, Failure>;
	fn now_ms(&mut self) -> u64;
	fn pause(&mut self);
}

/// A transaction's outcome, reduced to what a reader of it reports.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
	Answered,
	/// A completion code other than success.
	Refused(u8),
	Unavailable,
	Malformed,
}

pub fn answer_of(outcome: &Result<Response, Failure>) -> Answer {
	match outcome {
		Ok(response) if response.ok() => Answer::Answered,
		Ok(response) if response.cc == cc::TIMEOUT || response.cc == cc::BMC_INIT => Answer::Unavailable,
		Ok(response) => Answer::Refused(response.cc),
		Err(Failure::Deadline | Failure::Bus | Failure::Interface(_)) => Answer::Unavailable,
		Err(_) => Answer::Malformed,
	}
}

/// Ask, and hand back the data of a successful answer; the reduced answer otherwise.
pub fn data<B: Bmc + ?Sized>(bmc: &mut B, request: &Request) -> Result<Vec<u8>, Answer> {
	let outcome = bmc.ask(request);
	match answer_of(&outcome) {
		Answer::Answered => Ok(outcome.map(|response| response.data).unwrap_or_default()),
		other => Err(other),
	}
}

// ------------------------------------------------------------------ identity

/// Get Device ID and Get Device GUID: the device, and the identity the BMC is named by through `binding`.
pub fn identify<B: Bmc + ?Sized>(bmc: &mut B, binding: &str) -> Result<(DeviceId, Identity), Answer> {
	let device = ipmi::identity::device_id(&data(bmc, &Request::new(ipmi::netfn::APP, ipmi::identity::GET_DEVICE_ID, &[]))?).ok_or(Answer::Malformed)?;
	// A GUID IS OPTIONAL: an error, or sixteen zeros, is no GUID and the fallback names the BMC.
	let guid = data(bmc, &Request::new(ipmi::netfn::APP, ipmi::identity::GET_DEVICE_GUID, &[])).ok().and_then(|bytes| ipmi::identity::guid(&bytes));
	Ok((device.clone(), Identity::of(&device, guid, binding)))
}

// ------------------------------------------------------------------ the SDR repository

/// How many times a cancelled reservation is taken again before the read stops.
pub const RESERVE_TRIES: u32 = 3;

/// THE WHOLE REPOSITORY, record by record in partial reads, within its bounds. A record the BMC cannot give is the end
/// of the read, and what was read stands.
pub fn read_repository<B: Bmc + ?Sized>(bmc: &mut B) -> Repository {
	let mut repository = Repository::default();
	let Ok(mut reservation) = data(bmc, &sdr::reserve_request()).map(|bytes| sdr::reservation(&bytes).unwrap_or(0)) else {
		repository.answer = Some(Answer::Unavailable);
		return repository;
	};
	let mut id = 0u16;
	let mut reserved = 0u32;
	let mut seen = Vec::new();
	'records: while id != sdr::LAST {
		// A REPOSITORY THAT LOOPS is read once.
		if seen.contains(&id) {
			break;
		}
		seen.push(id);
		let mut read = sdr::RecordRead::new(id);
		loop {
			match read.step() {
				sdr::Step::Ask { offset, count } => {
					let outcome = bmc.ask(&sdr::get_request(reservation, id, offset, count));
					match outcome {
						Ok(response) if response.ok() => {
							let Some((next, bytes)) = sdr::get_response(&response.data) else { break 'records };
							if !read.answered(next, bytes) {
								repository.records.refused += 1;
								break 'records;
							}
						}
						// THE RESERVATION WAS CANCELLED: taken again, and this record read from its start.
						Ok(response) if response.cc == cc::RESERVATION_CANCELLED && reserved < RESERVE_TRIES => {
							reserved += 1;
							match data(bmc, &sdr::reserve_request()) {
								Ok(bytes) => reservation = sdr::reservation(&bytes).unwrap_or(0),
								Err(answer) => {
									repository.answer = Some(answer);
									break 'records;
								}
							}
							read = sdr::RecordRead::new(id);
						}
						other => {
							repository.answer = Some(answer_of(&other));
							break 'records;
						}
					}
				}
				sdr::Step::Done { bytes, next } => {
					if !repository.records.add(&bytes) {
						break 'records;
					}
					id = next;
					continue 'records;
				}
			}
		}
	}
	repository
}

/// The repository, and how its read ended when it ended early.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Repository {
	pub records: sdr::Repository,
	pub answer: Option<Answer>,
}

/// Get Sensor Reading for one sensor the BMC owns.
pub fn read_sensor<B: Bmc + ?Sized>(bmc: &mut B, sensor: &sdr::Sensor) -> Result<(Reading, u16), Answer> {
	let bytes = data(bmc, &sdr::reading_request(sensor.number, sensor.lun))?;
	let reading = power_model::ipmi::reading(&bytes).ok_or(Answer::Malformed)?;
	// The threshold comparison, or the discrete state bits, as the BMC sent them.
	let bits = u16::from(bytes.get(2).copied().unwrap_or(0)) | (u16::from(bytes.get(3).copied().unwrap_or(0)) << 8);
	Ok((reading, bits))
}

// ------------------------------------------------------------------ the SEL

/// Get SEL Info.
pub fn sel_info<B: Bmc + ?Sized>(bmc: &mut B) -> Result<sel::Info, Answer> {
	sel::info(&data(bmc, &sel::info_request())?).ok_or(Answer::Malformed)
}

/// One page of the SEL: up to `sel::PAGE` entries from `first`, the ID to ask for next, and the records refused.
pub fn sel_page<B: Bmc + ?Sized>(bmc: &mut B, first: u16) -> Result<(Vec<sel::Record>, u16, Vec<sel::Refused>), Answer> {
	let mut records = Vec::new();
	let mut refused = Vec::new();
	let mut id = first;
	while id != sel::LAST && records.len() + refused.len() < sel::PAGE {
		let bytes = match data(bmc, &sel::entry_request(id)) {
			Ok(bytes) => bytes,
			// AN EMPTY LOG, or a first ID the log does not hold, is the end.
			Err(Answer::Refused(cc::NOT_PRESENT)) => return Ok((records, sel::LAST, refused)),
			Err(answer) => return Err(answer),
		};
		let Some((next, record)) = sel::entry_response(&bytes) else { return Err(Answer::Malformed) };
		match sel::record(&record) {
			Ok(found) => records.push(found),
			Err(refusal) => refused.push(refusal),
		}
		// A LOG WHOSE NEXT ID IS THE ONE JUST READ ends here rather than looping.
		if next == id {
			return Ok((records, sel::LAST, refused));
		}
		id = next;
	}
	Ok((records, id, refused))
}

// ------------------------------------------------------------------ FRU

/// One FRU device's bytes, in `fru::READ_CHUNK` reads up to its size and `fru::MAX_BYTES`.
pub fn fru_bytes<B: Bmc + ?Sized>(bmc: &mut B, device: u8) -> Result<Vec<u8>, Answer> {
	let info = fru::area_info(&data(bmc, &fru::info_request(device))?).ok_or(Answer::Malformed)?;
	let size = (info.size as usize).min(fru::MAX_BYTES);
	let mut bytes = Vec::with_capacity(size);
	while bytes.len() < size {
		let count = (size - bytes.len()).min(fru::READ_CHUNK as usize) as u8;
		let answer = data(bmc, &fru::read_request(device, bytes.len() as u16, count))?;
		let got = fru::read_response(&answer).ok_or(Answer::Malformed)?;
		if got.is_empty() || got.len() > count as usize {
			return Err(Answer::Malformed);
		}
		bytes.extend_from_slice(got);
	}
	Ok(bytes)
}

// ------------------------------------------------------------------ LAN and users

/// Every LAN channel's configuration, and its users, read-only.
pub fn lan_channels<B: Bmc + ?Sized>(bmc: &mut B) -> Result<Vec<(lan::Lan, lan::Users)>, Answer> {
	let mut channels = Vec::new();
	let mut answered = false;
	for channel in lan::CHANNELS {
		let Ok(info) = data(bmc, &lan::channel_info_request(channel)) else { continue };
		answered = true;
		if lan::medium(&info) != Some(lan::MEDIUM_LAN) {
			continue;
		}
		let mut found = lan::Lan { channel, ..lan::Lan::default() };
		let mut parameter = |selector: u8, length: usize| data(bmc, &lan::parameter_request(channel, selector)).ok().and_then(|bytes| lan::parameter(&bytes, length).map(|slice| slice.to_vec()));
		found.source = parameter(lan::parameter::IP_SOURCE, 1).map(|bytes| bytes[0] & 0x0F);
		found.address = parameter(lan::parameter::IP_ADDRESS, 4).and_then(|bytes| bytes.try_into().ok());
		found.mask = parameter(lan::parameter::SUBNET_MASK, 4).and_then(|bytes| bytes.try_into().ok());
		found.gateway = parameter(lan::parameter::GATEWAY, 4).and_then(|bytes| bytes.try_into().ok());
		found.mac = parameter(lan::parameter::MAC, 6).and_then(|bytes| bytes.try_into().ok());
		found.vlan = data(bmc, &lan::parameter_request(channel, lan::parameter::VLAN)).ok().and_then(|bytes| lan::vlan(&bytes));
		let mut users = Vec::new();
		if let Some(first) = data(bmc, &lan::user_access_request(channel, 1)).ok().and_then(|bytes| lan::access(&bytes)) {
			for id in 1..=first.max_users.min(lan::MAX_USERS) {
				let Some(access) = data(bmc, &lan::user_access_request(channel, id)).ok().and_then(|bytes| lan::access(&bytes)) else { continue };
				let name = data(bmc, &lan::user_name_request(id)).ok().and_then(|bytes| lan::user_name(&bytes)).unwrap_or_default();
				// AN UNNAMED USER WITH NO ACCESS is an empty slot.
				if name.is_empty() && access.privilege == 0x0F {
					continue;
				}
				users.push(lan::User { id, name, enabled: access.privilege != 0x0F, privilege: access.privilege });
			}
		}
		channels.push((found, users));
	}
	if !answered {
		return Err(Answer::Unavailable);
	}
	Ok(channels)
}

// ------------------------------------------------------------------ the administrative executors

/// Why an executor refused a preparation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// The payload is not the target's own name.
	Payload,
	/// The parameters are not the action's: a count of two bytes, an operation of one.
	Parameters,
	/// The count is not the live one.
	Count,
	/// The BMC reports no Reserve SEL, which a clear requires.
	NoReserve,
	/// A chassis operation this executor does not carry: power up, the diagnostic interrupt.
	Operation,
	/// The BMC did not answer as preparation needs.
	Bmc(Answer),
}

/// A SEL clear, prepared: the count confirmed and the reservation taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SelClear {
	pub count: u16,
	pub reservation: u16,
}

/// PREPARE A SEL CLEAR for the BMC named `selector`: the payload its name, the count the live one, the reservation
/// taken.
pub fn prepare_sel_clear<B: Bmc + ?Sized>(bmc: &mut B, selector: &[u8], parameters: &[u8], payload: &[u8]) -> Result<SelClear, Refusal> {
	if payload != selector {
		return Err(Refusal::Payload);
	}
	let [low, high] = parameters else { return Err(Refusal::Parameters) };
	let count = u16::from_le_bytes([*low, *high]);
	let info = sel_info(bmc).map_err(Refusal::Bmc)?;
	if !info.reserve {
		return Err(Refusal::NoReserve);
	}
	if info.entries != count {
		return Err(Refusal::Count);
	}
	let reservation = data(bmc, &sel::reserve_request()).map_err(Refusal::Bmc).and_then(|bytes| sel::reservation(&bytes).ok_or(Refusal::Bmc(Answer::Malformed)))?;
	Ok(SelClear { count, reservation })
}

/// An executor's outcome, as `liber:admin@1` names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
	Completed,
	Failed,
	Unknown,
}

/// EXECUTE A PREPARED SEL CLEAR: Clear SEL under the preparation's reservation, then the erasure polled to its end
/// through Get SEL Info within `deadline_ms`. A cancelled reservation erased nothing: `failed`.
pub fn execute_sel_clear<B: Bmc + ?Sized>(bmc: &mut B, prepared: &SelClear, deadline_ms: u64) -> Outcome {
	let outcome = bmc.ask(&sel::clear_request(prepared.reservation, true));
	match &outcome {
		Ok(response) if response.ok() => {}
		// THE BMC REFUSED - the reservation cancelled by an event added since, or anything else: nothing was erased.
		Ok(_) => return Outcome::Failed,
		// NO ANSWER: the clear may have been received and acted on.
		Err(_) => return Outcome::Unknown,
	}
	loop {
		match bmc.ask(&sel::info_request()) {
			Ok(response) if response.cc == cc::ERASE_IN_PROGRESS => {}
			Ok(response) if response.ok() => return Outcome::Completed,
			_ => return Outcome::Unknown,
		}
		if bmc.now_ms() >= deadline_ms {
			return Outcome::Unknown;
		}
		bmc.pause();
	}
}

/// PREPARE A CHASSIS CONTROL: the payload the target's name, and one of the four operations.
pub fn prepare_chassis(selector: &[u8], parameters: &[u8], payload: &[u8]) -> Result<ipmi::chassis::Control, Refusal> {
	if payload != selector {
		return Err(Refusal::Payload);
	}
	let [operation] = parameters else { return Err(Refusal::Parameters) };
	ipmi::chassis::Control::from_parameter(*operation).ok_or(Refusal::Operation)
}

/// EXECUTE IT: the BMC's refusal - QEMU's 0xD5 to a power cycle - is `failed`; silence, `unknown`.
pub fn execute_chassis<B: Bmc + ?Sized>(bmc: &mut B, operation: ipmi::chassis::Control) -> (Outcome, Option<u8>) {
	match bmc.ask(&ipmi::chassis::control(operation)) {
		Ok(response) if response.ok() => (Outcome::Completed, None),
		Ok(response) => (Outcome::Failed, Some(response.cc)),
		Err(_) => (Outcome::Unknown, None),
	}
}

/// A binding's name for the BMC's target selector, as the tool and the executors both write it.
pub fn selector(identity: &Identity) -> Option<String> {
	identity.selector().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests;
