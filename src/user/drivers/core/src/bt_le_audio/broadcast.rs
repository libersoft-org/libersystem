// THE BROADCAST SOURCE, AND THE SYNC THIS CONTROLLER KEEPS WITH IT: a Broadcast Source as BAP lays one out - its
// Broadcast Audio Announcement and name in extended advertising, its BASE in periodic advertising, two BISes of LC3 -
// and the synchronized receiver's half of the controller: the periodic sync (Core 7.8.67 ff.), the BIGInfo it reports,
// the BIG sync (7.8.106 ff.) and the streams it hands the host.
//
// SILENT UNTIL THE GATE'S WORD. `broadcast` 1 starts it in the clear, 2 encrypted with the fixture's Broadcast Code, 0
// stops it - and a receiver synchronized to it loses its BIG, then its train, as a real one would.

use super::{Advert, Audio, AudioOut, BROADCAST_ADDRESS, BROADCAST_CODE, BROADCAST_ID, BROADCAST_NAME, FIRST_BIS_HANDLE, Pace, complete, iso_packet, le24, meta, status, u16_at};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// The advertising set's SID, and the train's interval: 80 times 1.25 ms, a report every 100 ms - ten ticks.
pub const SID: u8 = 1;
const INTERVAL: u16 = 80;
const REPORT_TICKS: u64 = 10;
// THE BIG: 48 kHz, 10 ms frames of 100 octets, one channel a BIS; LE 2M, unframed, one PDU an interval sent twice.
const RATE: u32 = 48_000;
const FRAME_US: u32 = 10_000;
const OCTETS: usize = 100;
const ISO_INTERVAL: u16 = 8;
const NSE: u8 = 2;
const IRC: u8 = 2;
// The BISes - index 1 front left, index 2 front right - and the spectral line each carries: 30 and 60, which a line
// held frame after frame makes a tone at 100 * ceil(line / 2) Hz - 1500 and 3000 Hz.
const BISES: [(u8, u32, u16); 2] = [(1, 0x0000_0001, 30), (2, 0x0000_0002, 60)];
// A BIG terminated by its source, as the receiver's Sync Lost reports it.
const TERMINATED: u8 = 0x13;

// THE SOURCE: off (0), in the clear (1) or encrypted (2), and the frames its BISes carry - `None` where the codec would
// not write them.
#[derive(Default)]
pub struct Source {
	pub mode: u8,
	frames: Option<[Vec<u8>; 2]>,
}

// A periodic sync the host asked for and the controller has not found.
#[derive(Clone, Copy)]
struct Pending {
	options: u8,
	sid: u8,
	address_type: u8,
	address: [u8; 6],
}

// The train followed: its handle, whether its reports are on and filtered for duplicates, whether its data went once,
// and when the next reports are due.
struct Train {
	handle: u16,
	reporting: bool,
	duplicates: bool,
	sent: bool,
	next: u64,
}

pub struct Bis {
	index: u8,
	handle: u16,
	pub path: bool,
	pace: Pace,
}

struct Big {
	handle: u8,
	bises: Vec<Bis>,
}

#[derive(Default)]
pub struct Receiver {
	pending: Option<Pending>,
	train: Option<Train>,
	big: Option<Big>,
	// The Periodic Advertiser List: address type, address and SID.
	list: Vec<(u8, [u8; 6], u8)>,
	next_sync: u16,
}

impl Receiver {
	pub fn bis_mut(&mut self, handle: u16) -> Option<&mut Bis> {
		self.big.as_mut()?.bises.iter_mut().find(|bis| bis.handle == handle)
	}

	pub fn active(&self) -> bool {
		self.train.is_some() || self.big.as_ref().is_some_and(|big| big.bises.iter().any(|bis| bis.path))
	}
}

// ------------------------------------------------------------------ what the source sends

// THE EXTENDED ADVERTISING DATA: the Broadcast Audio Announcement with its Broadcast_ID, and the Broadcast Name.
pub fn announcement() -> Vec<u8> {
	let id = le24(BROADCAST_ID);
	let mut data = alloc::vec![0x06, 0x16, 0x52, 0x18, id[0], id[1], id[2]];
	data.push(BROADCAST_NAME.len() as u8 + 1);
	data.push(0x30);
	data.extend_from_slice(BROADCAST_NAME.as_bytes());
	data
}

// THE BASE (BAP 3.7.2.2): a presentation delay of 40 ms; one subgroup of LC3 at 48 kHz, 10 ms and 100 octets with media
// streaming; its two BISes each with its channel allocation.
pub fn base() -> Vec<u8> {
	let mut base = Vec::new();
	base.extend_from_slice(&le24(40_000));
	base.push(1);
	base.push(BISES.len() as u8);
	base.extend_from_slice(&[0x06, 0, 0, 0, 0]);
	let configuration = [0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x03, 0x04, OCTETS as u8, 0x00];
	base.push(configuration.len() as u8);
	base.extend_from_slice(&configuration);
	let metadata = [0x03, 0x02, 0x04, 0x00];
	base.push(metadata.len() as u8);
	base.extend_from_slice(&metadata);
	for (index, allocation, _) in BISES {
		let a = allocation.to_le_bytes();
		base.extend_from_slice(&[index, 6, 0x05, 0x03, a[0], a[1], a[2], a[3]]);
	}
	base
}

// THE PERIODIC ADVERTISING DATA: the Basic Audio Announcement, which is the BASE behind its service UUID.
pub fn periodic_data() -> Vec<u8> {
	let base = base();
	let mut data = alloc::vec![base.len() as u8 + 3, 0x16, 0x51, 0x18];
	data.extend_from_slice(&base);
	data
}

impl Audio {
	// THE SOURCE AS A SCAN HEARS IT while it broadcasts: non-connectable extended advertising from its static address,
	// with its SID and a periodic train.
	pub fn broadcast_advert(&self) -> Option<Advert> {
		(self.source.mode != 0).then(|| Advert { event_type: 0x0000, address_type: 0x01, address: BROADCAST_ADDRESS, secondary_phy: 1, sid: SID, rssi: -55, periodic_interval: INTERVAL, data: announcement() })
	}

	// ------------------------------------------------------------------ the gate's word

	// THE SOURCE STARTS - in the clear (1) or encrypted (2) - or stops (0). A receiver that waited for its train finds it;
	// one synchronized to it loses it.
	pub fn broadcast(&mut self, mode: u8) -> Option<Vec<AudioOut>> {
		if mode > 2 {
			return None;
		}
		let mut out = Vec::new();
		if self.source.mode != 0 && self.source.mode != mode {
			out.extend(self.lost());
			self.source.mode = 0;
			self.log.push(String::from("broadcast source stopped"));
		}
		if mode == 0 || self.source.mode == mode {
			return Some(out);
		}
		self.source.mode = mode;
		self.log.push(String::from(if mode == 1 { "broadcast source is broadcasting in the clear" } else { "broadcast source is broadcasting encrypted" }));
		if self.source.frames.is_none() {
			let mut frames = [alloc::vec![0u8; OCTETS], alloc::vec![0u8; OCTETS]];
			let written = BISES.iter().zip(frames.iter_mut()).try_for_each(|((_, _, line), frame)| crate::bt_lc3::write_tone(*line, RATE, FRAME_US, frame));
			match written {
				Ok(()) => self.source.frames = Some(frames),
				Err(why) => self.log.push(format!("broadcast source could not write its LC3: {why}")),
			}
		}
		if self.scan.on
			&& let Some(advert) = self.broadcast_advert()
		{
			out.push(meta(0x0d, &advert.extended_report()));
		}
		if let Some(pending) = self.receiver.pending
			&& self.matches(&pending)
		{
			out.extend(self.synchronised(pending));
		}
		Some(out)
	}

	// WHAT A RECEIVER LOSES when the source stops: its BIG, then its train.
	fn lost(&mut self) -> Vec<AudioOut> {
		let mut out = Vec::new();
		if let Some(big) = self.receiver.big.take() {
			out.push(meta(0x1e, &[big.handle, TERMINATED]));
		}
		if let Some(train) = self.receiver.train.take() {
			out.push(meta(0x10, &train.handle.to_le_bytes()));
		}
		out
	}

	// WHETHER A CREATE SYNC NAMES THIS SOURCE: by the Periodic Advertiser List where it asks for the list, by its SID and
	// address otherwise - and only while it broadcasts.
	fn matches(&self, pending: &Pending) -> bool {
		let ours = |address_type: u8, address: &[u8; 6], sid: u8| matches!(address_type, 0x01 | 0x03) && *address == BROADCAST_ADDRESS && sid == SID;
		self.source.mode != 0 && if pending.options & 0x01 != 0 { self.receiver.list.iter().any(|entry| ours(entry.0, &entry.1, entry.2)) } else { ours(pending.address_type, &pending.address, pending.sid) }
	}

	// THE TRAIN IS FOUND: LE Periodic Advertising Sync Established (7.7.65.14), and the first reports at once.
	fn synchronised(&mut self, pending: Pending) -> Vec<AudioOut> {
		self.receiver.pending = None;
		self.receiver.next_sync = self.receiver.next_sync.wrapping_add(1) & 0x0eff;
		let handle = self.receiver.next_sync;
		let mut body = alloc::vec![0];
		body.extend_from_slice(&handle.to_le_bytes());
		body.extend_from_slice(&[SID, 0x01]);
		let mut wire = BROADCAST_ADDRESS;
		wire.reverse();
		body.extend_from_slice(&wire);
		body.push(0x01);
		body.extend_from_slice(&INTERVAL.to_le_bytes());
		body.push(0x05);
		self.receiver.train = Some(Train { handle, reporting: pending.options & 0x02 == 0, duplicates: pending.options & 0x04 != 0, sent: false, next: self.now });
		self.log.push(String::from("broadcast source's periodic train was synchronised"));
		let mut out = alloc::vec![meta(0x0e, &body)];
		out.extend(self.reports());
		out
	}

	// THE TRAIN'S REPORTS: its data - the BASE - unless duplicates are filtered and it went already, and the BIGInfo.
	fn reports(&mut self) -> Vec<AudioOut> {
		let encrypted = self.source.mode == 2;
		let Some(train) = self.receiver.train.as_mut() else { return Vec::new() };
		train.next = self.now + REPORT_TICKS;
		if !train.reporting {
			return Vec::new();
		}
		let mut out = Vec::new();
		let handle = train.handle.to_le_bytes();
		if !train.duplicates || !train.sent {
			train.sent = true;
			let data = periodic_data();
			let mut report = alloc::vec![handle[0], handle[1], 0x7f, -55i8 as u8, 0xff, 0x00, data.len() as u8];
			report.extend_from_slice(&data);
			out.push(meta(0x0f, &report));
		}
		// LE BIGINFO ADVERTISING REPORT (7.7.65.34): the BISes, NSE, the ISO interval, BN, PTO, IRC, the PDU, the SDU
		// interval and size, the PHY, the framing and the encryption.
		let mut info = alloc::vec![handle[0], handle[1], BISES.len() as u8, NSE];
		info.extend_from_slice(&ISO_INTERVAL.to_le_bytes());
		info.extend_from_slice(&[1, 0, IRC]);
		info.extend_from_slice(&(OCTETS as u16).to_le_bytes());
		info.extend_from_slice(&le24(FRAME_US));
		info.extend_from_slice(&(OCTETS as u16).to_le_bytes());
		info.extend_from_slice(&[0x02, 0x00, u8::from(encrypted)]);
		out.push(meta(0x22, &info));
		out
	}

	// ------------------------------------------------------------------ the receiver's commands

	pub(super) fn receiver_command(&mut self, opcode: u16, params: &[u8]) -> Vec<AudioOut> {
		match opcode {
			0x2044 => self.create_sync(params),
			// CREATE SYNC CANCELLED: its completion, then the sync's end, Operation Cancelled by Host.
			0x2045 => {
				if self.receiver.pending.take().is_none() {
					return alloc::vec![complete(opcode, &[0x0c])];
				}
				let mut cancelled = alloc::vec![0x44];
				cancelled.extend_from_slice(&[0; 14]);
				alloc::vec![complete(opcode, &[0]), meta(0x0e, &cancelled)]
			}
			0x2046 => {
				let handle = u16_at(params, 0) & 0x0fff;
				if params.len() != 2 || self.receiver.train.as_ref().is_none_or(|train| train.handle != handle) {
					return alloc::vec![complete(opcode, &[0x42])];
				}
				self.receiver.train = None;
				alloc::vec![complete(opcode, &[0])]
			}
			// THE PERIODIC ADVERTISER LIST: add, remove, clear and its size.
			0x2047 | 0x2048 => {
				if params.len() != 8 {
					return alloc::vec![complete(opcode, &[0x12])];
				}
				let mut address = [0u8; 6];
				address.copy_from_slice(&params[1..7]);
				address.reverse();
				let entry = (params[0], address, params[7]);
				let held = self.receiver.list.iter().position(|held| *held == entry);
				let code = match (opcode, held) {
					(0x2047, Some(_)) => 0x12,
					(0x2047, None) => {
						self.receiver.list.push(entry);
						0
					}
					(_, Some(at)) => {
						self.receiver.list.remove(at);
						0
					}
					(_, None) => 0x42,
				};
				alloc::vec![complete(opcode, &[code])]
			}
			0x2049 => {
				self.receiver.list.clear();
				alloc::vec![complete(opcode, &[0])]
			}
			0x204a => alloc::vec![complete(opcode, &[0, 8])],
			// SET PERIODIC ADVERTISING RECEIVE ENABLE: reports on or off, duplicates filtered or not.
			0x2059 => {
				let handle = u16_at(params, 0) & 0x0fff;
				match self.receiver.train.as_mut().filter(|train| train.handle == handle && params.len() == 3) {
					Some(train) => {
						train.reporting = params[2] & 0x01 != 0;
						train.duplicates = params[2] & 0x02 != 0;
						train.sent = false;
						alloc::vec![complete(opcode, &[0])]
					}
					None => alloc::vec![complete(opcode, &[0x42])],
				}
			}
			0x206b => self.big_create_sync(params),
			// BIG TERMINATE SYNC: the BIG let go, its streams with it.
			_ => {
				let handle = params.first().copied().unwrap_or(0);
				if params.len() != 1 || self.receiver.big.as_ref().is_none_or(|big| big.handle != handle) {
					return alloc::vec![complete(opcode, &[0x42, handle])];
				}
				self.receiver.big = None;
				alloc::vec![complete(opcode, &[0, handle])]
			}
		}
	}

	// LE PERIODIC ADVERTISING CREATE SYNC: options, SID, the advertiser's address, skip, timeout and CTE type. One at a
	// time; it completes when the train is there - at once while the source broadcasts, never while it is silent, so a
	// host waiting on a silent source times out and cancels.
	fn create_sync(&mut self, params: &[u8]) -> Vec<AudioOut> {
		let opcode = 0x2044;
		if params.len() != 14 || params[0] & !0x07 != 0 || params[1] > 0x0f || params[2] > 0x03 {
			return alloc::vec![status(opcode, 0x12)];
		}
		if self.receiver.pending.is_some() {
			return alloc::vec![status(opcode, 0x0c)];
		}
		let mut address = [0u8; 6];
		address.copy_from_slice(&params[3..9]);
		address.reverse();
		let pending = Pending { options: params[0], sid: params[1], address_type: params[2], address };
		if self.receiver.train.is_some() && self.matches(&pending) {
			return alloc::vec![status(opcode, 0x0b)];
		}
		self.receiver.pending = Some(pending);
		let mut out = alloc::vec![status(opcode, 0)];
		if self.matches(&pending) {
			out.extend(self.synchronised(pending));
		}
		out
	}

	// LE BIG CREATE SYNC: the BIG handle, the train, the encryption and the Broadcast Code, MSE, a timeout and the BISes
	// wanted. LE BIG Sync Established follows - with a handle for every BIS asked for, or, for an encrypted BIG the code
	// does not open, MIC Failure and none.
	fn big_create_sync(&mut self, params: &[u8]) -> Vec<AudioOut> {
		let opcode = 0x206b;
		let count = usize::from(params.get(23).copied().unwrap_or(0));
		if params.len() < 24 || params.len() != 24 + count || params[0] > 0xef || params[3] > 1 || !(1..=0x1f).contains(&count) {
			return alloc::vec![status(opcode, 0x12)];
		}
		let (big_handle, sync) = (params[0], u16_at(params, 1) & 0x0fff);
		if self.receiver.big.is_some() {
			return alloc::vec![status(opcode, 0x0c)];
		}
		if self.receiver.train.as_ref().is_none_or(|train| train.handle != sync) {
			return alloc::vec![status(opcode, 0x02)];
		}
		let wanted = &params[24..];
		let known = wanted.iter().all(|index| BISES.iter().any(|bis| bis.0 == *index));
		let distinct = wanted.iter().enumerate().all(|(at, index)| !wanted[..at].contains(index));
		if !known || !distinct {
			return alloc::vec![status(opcode, 0x12)];
		}
		let mut out = alloc::vec![status(opcode, 0)];
		let (encryption, code) = (params[3], &params[4..20]);
		let refusal = match self.source.mode {
			2 if encryption != 1 || code != BROADCAST_CODE => Some(0x3d),
			1 if encryption != 0 => Some(0x25),
			_ => None,
		};
		if let Some(refusal) = refusal {
			let mut body = alloc::vec![refusal, big_handle];
			body.extend_from_slice(&[0; 12]);
			out.push(meta(0x1d, &body));
			if refusal == 0x3d {
				self.log.push(String::from("broadcast source's BIG refused a wrong Broadcast Code"));
			}
			return out;
		}
		// LE BIG SYNC ESTABLISHED (7.7.65.29): the transport latency, NSE, BN, PTO, IRC, the PDU, the ISO interval and a
		// handle for every BIS.
		let mut body = alloc::vec![0, big_handle];
		body.extend_from_slice(&le24(u32::from(ISO_INTERVAL) * 1250 + FRAME_US));
		body.extend_from_slice(&[NSE, 1, 0, IRC]);
		body.extend_from_slice(&(OCTETS as u16).to_le_bytes());
		body.extend_from_slice(&ISO_INTERVAL.to_le_bytes());
		body.push(count as u8);
		let mut bises = Vec::new();
		for (at, &index) in wanted.iter().enumerate() {
			let handle = FIRST_BIS_HANDLE + at as u16;
			body.extend_from_slice(&handle.to_le_bytes());
			bises.push(Bis { index, handle, path: false, pace: Pace::default() });
		}
		self.receiver.big = Some(Big { handle: big_handle, bises });
		self.log.push(format!("broadcast source's BIG was synchronised: {count} streams"));
		out.push(meta(0x1d, &body));
		out
	}

	// ------------------------------------------------------------------ on the clock

	// THE TRAIN'S REPORTS every 100 ms, and an SDU every 10 ms on each BIS whose output path is set up.
	pub(super) fn receiver_tick(&mut self, now: u64) -> Vec<AudioOut> {
		let mut out = Vec::new();
		if self.receiver.train.as_ref().is_some_and(|train| train.next <= now) {
			out.extend(self.reports());
		}
		let Some(frames) = self.source.frames.as_ref().filter(|_| self.source.mode != 0) else { return out };
		let Some(big) = self.receiver.big.as_mut() else { return out };
		for bis in big.bises.iter_mut().filter(|bis| bis.path) {
			let frame = &frames[usize::from(bis.index - 1)];
			for _ in 0..bis.pace.due(now) {
				out.push(AudioOut::Iso(iso_packet(bis.handle, bis.pace.take(), frame)));
			}
		}
		out
	}
}
