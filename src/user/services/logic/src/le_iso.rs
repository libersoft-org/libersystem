//! ISOCHRONOUS CHANNELS ON THE HCI: the commands that set connected (CIS) and broadcast (BIS) streams up, the events
//! that answer them, extended scanning and periodic advertising sync - what a broadcast is found and joined by - and
//! the ISO data packets the streams carry.
//!
//! THE SERVICE CLASSIFIES. Over USB, ISO shares the ACL bulk pair and only the connection handle tells the two apart,
//! and the handles are the service's: so a transport that carries ISO delivers every inbound data packet as ACL,
//! bounded by the larger ceiling, and `classify` reads the handle against the service's own table of stream handles
//! and applies that kind's ceiling after classifying - an ACL packet above the ACL ceiling refused. A packet for a
//! handle in neither table - data can overtake, on another pipe, the event that created its stream - is dropped and
//! counted by the caller.
//!
//! ONE SDU, ONE PACKET, OUTBOUND. This host sends each SDU whole (packet boundary "complete"), with a packet sequence
//! number and no time stamp; inbound packets are reassembled from first/continuation/last fragments, bounded.

use alloc::vec::Vec;

/// The commands.
pub mod opcode {
	pub const LE_READ_LOCAL_SUPPORTED_FEATURES: u16 = 0x2003;
	pub const LE_EXTENDED_CREATE_CONNECTION: u16 = 0x2043;
	pub const LE_READ_BUFFER_SIZE_V2: u16 = 0x2060;
	pub const LE_SET_CIG_PARAMETERS: u16 = 0x2062;
	pub const LE_CREATE_CIS: u16 = 0x2064;
	pub const LE_REMOVE_CIG: u16 = 0x2065;
	pub const LE_BIG_CREATE_SYNC: u16 = 0x206b;
	pub const LE_BIG_TERMINATE_SYNC: u16 = 0x206c;
	pub const LE_SETUP_ISO_DATA_PATH: u16 = 0x206e;
	pub const LE_REMOVE_ISO_DATA_PATH: u16 = 0x206f;
	pub const LE_SET_HOST_FEATURE: u16 = 0x2074;
	pub const LE_SET_EXTENDED_SCAN_PARAMETERS: u16 = 0x2041;
	pub const LE_SET_EXTENDED_SCAN_ENABLE: u16 = 0x2042;
	pub const LE_PERIODIC_ADVERTISING_CREATE_SYNC: u16 = 0x2044;
	pub const LE_PERIODIC_ADVERTISING_TERMINATE_SYNC: u16 = 0x2046;
}

/// The LE meta subevents.
pub mod subevent {
	pub const EXTENDED_ADVERTISING_REPORT: u8 = 0x0d;
	pub const PERIODIC_ADVERTISING_SYNC_ESTABLISHED: u8 = 0x0e;
	pub const PERIODIC_ADVERTISING_REPORT: u8 = 0x0f;
	pub const PERIODIC_ADVERTISING_SYNC_LOST: u8 = 0x10;
	pub const CIS_ESTABLISHED: u8 = 0x19;
	pub const BIG_SYNC_ESTABLISHED: u8 = 0x1d;
	pub const BIG_SYNC_LOST: u8 = 0x1e;
	pub const BIGINFO_ADVERTISING_REPORT: u8 = 0x22;
}

/// The Isochronous Channels (Host Support) feature bit `LE Set Host Feature` turns on.
pub const ISOCHRONOUS_CHANNELS_HOST_SUPPORT: u8 = 32;
/// The LE features this host reads: extended advertising, and a CIS's central role and a synchronized receiver.
pub mod feature {
	pub const EXTENDED_ADVERTISING: u64 = 1 << 12;
	pub const PERIODIC_ADVERTISING: u64 = 1 << 13;
	pub const CIS_CENTRAL: u64 = 1 << 28;
	pub const SYNCHRONIZED_RECEIVER: u64 = 1 << 31;
}

/// The LE event mask that, beside the events every LE host reads (bits 0-8), unmasks extended advertising reports,
/// periodic sync, its reports and its loss, a CIS's establishment, BIG sync and its loss, and BIGInfo reports.
pub const LE_EVENT_MASK: u64 = 0x1ff | (1 << 12) | (1 << 13) | (1 << 14) | (1 << 15) | (1 << 24) | (1 << 28) | (1 << 29) | (1 << 33);

/// The largest SDU this host reassembles: an LC3 frame of 400 bytes with room to spare.
pub const MAX_SDU: usize = 512;

// ------------------------------------------------------------------ commands

fn u24(value: u32) -> [u8; 3] {
	let bytes = value.to_le_bytes();
	[bytes[0], bytes[1], bytes[2]]
}

/// `LE Set Host Feature`: one bit, on or off.
pub fn set_host_feature(bit: u8, on: bool) -> [u8; 2] {
	[bit, u8::from(on)]
}

/// One CIS of a CIG: its id, the largest SDU each way, the 2M PHY both ways and the retransmissions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CisParameters {
	pub id: u8,
	pub max_sdu_to_peripheral: u16,
	pub max_sdu_to_central: u16,
	pub retransmissions: u8,
}

/// `LE Set CIG Parameters`: the group's SDU interval and latency both ways, sequential packing, unframed, and its CISes.
pub fn set_cig_parameters(cig: u8, sdu_interval_us: u32, max_latency_ms: u16, cises: &[CisParameters]) -> Vec<u8> {
	let mut out = alloc::vec![cig];
	out.extend_from_slice(&u24(sdu_interval_us));
	out.extend_from_slice(&u24(sdu_interval_us));
	// Worst-case sleep clock accuracy 251..500 ppm, sequential packing, unframed.
	out.extend_from_slice(&[0, 0, 0]);
	out.extend_from_slice(&max_latency_ms.to_le_bytes());
	out.extend_from_slice(&max_latency_ms.to_le_bytes());
	out.push(cises.len() as u8);
	for cis in cises {
		out.push(cis.id);
		out.extend_from_slice(&cis.max_sdu_to_peripheral.to_le_bytes());
		out.extend_from_slice(&cis.max_sdu_to_central.to_le_bytes());
		// The 2M PHY each way.
		out.extend_from_slice(&[0x02, 0x02, cis.retransmissions, cis.retransmissions]);
	}
	out
}

/// `LE Set CIG Parameters`' return: the CIS handles in the order the CISes were given.
pub fn cig_handles(params: &[u8]) -> Option<(u8, Vec<u16>)> {
	if params.len() < 3 || params[0] != 0 {
		return None;
	}
	let (cig, count) = (params[1], usize::from(params[2]));
	let handles = params.get(3..3 + count * 2)?;
	Some((cig, handles.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]]) & 0x0fff).collect()))
}

/// `LE Create CIS`: each CIS handle with the ACL link it runs beside.
pub fn create_cis(pairs: &[(u16, u16)]) -> Vec<u8> {
	let mut out = alloc::vec![pairs.len() as u8];
	for (cis, acl) in pairs {
		out.extend_from_slice(&cis.to_le_bytes());
		out.extend_from_slice(&acl.to_le_bytes());
	}
	out
}

/// `LE Remove CIG`.
pub fn remove_cig(cig: u8) -> [u8; 1] {
	[cig]
}

/// Which way an ISO data path runs, as `LE Setup ISO Data Path` names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
	/// Host to controller: what this host sends.
	Input = 0,
	/// Controller to host: what this host receives.
	Output = 1,
}

/// `LE Setup ISO Data Path` over HCI, the codec this host's own (transparent to the controller), no controller delay.
pub fn setup_iso_data_path(handle: u16, direction: Direction) -> [u8; 13] {
	let h = handle.to_le_bytes();
	[h[0], h[1], direction as u8, 0x00, 0x03, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// `LE Remove ISO Data Path`: both directions.
pub fn remove_iso_data_path(handle: u16) -> [u8; 3] {
	let h = handle.to_le_bytes();
	[h[0], h[1], 0b11]
}

/// `LE Set Extended Scan Parameters`: passive scanning on the 1M PHY, every advertisement, 100 ms interval and window.
pub fn extended_scan_parameters(own_address_type: u8) -> [u8; 8] {
	let interval = 160u16.to_le_bytes();
	[own_address_type, 0x00, 0x01, 0x00, interval[0], interval[1], interval[0], interval[1]]
}

/// THE ORDINARY SCAN'S PARAMETERS, AS EXTENDED SCANNING TAKES THEM - a controller that has extended advertising refuses the
/// legacy commands once one extended command has been sent, so a host that uses one uses them all: the legacy
/// `LE Set Scan Parameters` fields (type, interval, window, own address type, filter policy) on the 1M PHY.
pub fn extended_scan_parameters_from(legacy: &[u8; 7]) -> [u8; 8] {
	[legacy[5], legacy[6], 0x01, legacy[0], legacy[1], legacy[2], legacy[3], legacy[4]]
}

/// `LE Extended Create Connection` from the legacy `LE Create Connection` fields, on the 1M PHY.
pub fn extended_create_connection(legacy: &[u8; 25]) -> [u8; 26] {
	let mut out = [0u8; 26];
	out[0] = legacy[4];
	out[1] = legacy[12];
	out[2] = legacy[5];
	out[3..9].copy_from_slice(&legacy[6..12]);
	out[9] = 0x01;
	out[10..14].copy_from_slice(&legacy[0..4]);
	out[14..26].copy_from_slice(&legacy[13..25]);
	out
}

/// `LE Set Extended Scan Enable`: no duplicate filtering, no duration.
pub fn extended_scan_enable(on: bool) -> [u8; 6] {
	[u8::from(on), 0, 0, 0, 0, 0]
}

/// `LE Periodic Advertising Create Sync` to one advertiser's set, no skip, a 10 s supervision timeout.
pub fn periodic_create_sync(sid: u8, address_type: u8, wire_address: &[u8; 6]) -> [u8; 14] {
	let timeout = 1000u16.to_le_bytes();
	let mut out = [0u8; 14];
	out[1] = sid;
	out[2] = address_type;
	out[3..9].copy_from_slice(wire_address);
	out[11] = timeout[0];
	out[12] = timeout[1];
	out
}

/// `LE BIG Create Sync`: one BIG handle on a periodic sync, its Broadcast Code where it is encrypted, and the BISes.
pub fn big_create_sync(big: u8, sync: u16, code: Option<&[u8; 16]>, bises: &[u8]) -> Vec<u8> {
	let mut out = alloc::vec![big];
	out.extend_from_slice(&sync.to_le_bytes());
	out.push(u8::from(code.is_some()));
	out.extend_from_slice(code.unwrap_or(&[0; 16]));
	// MSE: any; a 10 s sync timeout.
	out.push(0);
	out.extend_from_slice(&1000u16.to_le_bytes());
	out.push(bises.len() as u8);
	out.extend_from_slice(bises);
	out
}

// ------------------------------------------------------------------ events

/// One extended advertising report.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExtendedReport {
	pub event_type: u16,
	pub address_type: u8,
	/// As the wire carries it, least significant byte first.
	pub wire_address: [u8; 6],
	pub sid: u8,
	pub rssi: i8,
	/// The periodic advertising interval, zero where the set has none.
	pub periodic_interval: u16,
	pub data: Vec<u8>,
}

/// A CIS up, or refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CisEstablished {
	pub status: u8,
	pub handle: u16,
	pub max_pdu_to_peripheral: u16,
	pub max_pdu_to_central: u16,
	pub iso_interval: u16,
}

/// A periodic sync up, or refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SyncEstablished {
	pub status: u8,
	pub sync: u16,
	pub sid: u8,
	pub address_type: u8,
	pub wire_address: [u8; 6],
}

/// What a BIGInfo report says of the BIG a periodic train announces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BigInfo {
	pub sync: u16,
	pub bises: u8,
	pub sdu_interval_us: u32,
	pub max_sdu: u16,
	pub encrypted: bool,
}

/// A BIG sync up - its BIS handles - or refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BigEstablished {
	pub status: u8,
	pub big: u8,
	pub handles: Vec<u16>,
}

/// The LE Audio subevents, decoded.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
	ExtendedReports(Vec<ExtendedReport>),
	SyncEstablished(SyncEstablished),
	PeriodicReport { sync: u16, data: Vec<u8>, complete: bool },
	SyncLost { sync: u16 },
	CisEstablished(CisEstablished),
	BigInfo(BigInfo),
	BigEstablished(BigEstablished),
	BigLost { big: u8, reason: u8 },
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
	Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn u24_at(bytes: &[u8], at: usize) -> Option<u32> {
	Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, 0]))
}

/// One LE meta subevent's body - the bytes after the subevent code - if it is one of these; `None` for another, or
/// for one that runs past its event.
pub fn event(subevent: u8, body: &[u8]) -> Option<Event> {
	match subevent {
		subevent::EXTENDED_ADVERTISING_REPORT => {
			let count = usize::from(*body.first()?);
			let mut at = 1;
			let mut reports = Vec::new();
			for _ in 0..count {
				let fixed = body.get(at..at + 24)?;
				let len = usize::from(fixed[23]);
				let data = body.get(at + 24..at + 24 + len)?.to_vec();
				let mut wire_address = [0u8; 6];
				wire_address.copy_from_slice(&fixed[3..9]);
				reports.push(ExtendedReport { event_type: u16::from_le_bytes([fixed[0], fixed[1]]), address_type: fixed[2], wire_address, sid: fixed[11], rssi: fixed[13] as i8, periodic_interval: u16::from_le_bytes([fixed[14], fixed[15]]), data });
				at += 24 + len;
			}
			Some(Event::ExtendedReports(reports))
		}
		subevent::PERIODIC_ADVERTISING_SYNC_ESTABLISHED => {
			let fixed = body.get(..15)?;
			let mut wire_address = [0u8; 6];
			wire_address.copy_from_slice(&fixed[5..11]);
			Some(Event::SyncEstablished(SyncEstablished { status: fixed[0], sync: u16::from_le_bytes([fixed[1], fixed[2]]) & 0x0fff, sid: fixed[3], address_type: fixed[4], wire_address }))
		}
		subevent::PERIODIC_ADVERTISING_REPORT => {
			let fixed = body.get(..7)?;
			let len = usize::from(fixed[6]);
			let data = body.get(7..7 + len)?.to_vec();
			Some(Event::PeriodicReport { sync: u16::from_le_bytes([fixed[0], fixed[1]]) & 0x0fff, data, complete: fixed[5] == 0 })
		}
		subevent::PERIODIC_ADVERTISING_SYNC_LOST => Some(Event::SyncLost { sync: u16_at(body, 0)? & 0x0fff }),
		subevent::CIS_ESTABLISHED => {
			let fixed = body.get(..28)?;
			Some(Event::CisEstablished(CisEstablished { status: fixed[0], handle: u16::from_le_bytes([fixed[1], fixed[2]]) & 0x0fff, max_pdu_to_peripheral: u16::from_le_bytes([fixed[22], fixed[23]]), max_pdu_to_central: u16::from_le_bytes([fixed[24], fixed[25]]), iso_interval: u16::from_le_bytes([fixed[26], fixed[27]]) }))
		}
		subevent::BIGINFO_ADVERTISING_REPORT => {
			let fixed = body.get(..19)?;
			Some(Event::BigInfo(BigInfo { sync: u16::from_le_bytes([fixed[0], fixed[1]]) & 0x0fff, bises: fixed[2], sdu_interval_us: u24_at(fixed, 11)?, max_sdu: u16_at(fixed, 14)?, encrypted: fixed[18] != 0 }))
		}
		subevent::BIG_SYNC_ESTABLISHED => {
			let fixed = body.get(..14)?;
			let count = usize::from(fixed[13]);
			let handles = body.get(14..14 + count * 2)?.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]]) & 0x0fff).collect();
			Some(Event::BigEstablished(BigEstablished { status: fixed[0], big: fixed[1], handles }))
		}
		subevent::BIG_SYNC_LOST => {
			let fixed = body.get(..2)?;
			Some(Event::BigLost { big: fixed[0], reason: fixed[1] })
		}
		_ => None,
	}
}

/// `LE Read Buffer Size [v2]`'s return: the LE ACL buffers and the ISO ones - their size and how many.
pub fn buffer_sizes_v2(params: &[u8]) -> Option<((u16, u8), (u16, u8))> {
	if params.len() < 7 || params[0] != 0 {
		return None;
	}
	Some(((u16_at(params, 1)?, params[3]), (u16_at(params, 4)?, params[6])))
}

// ------------------------------------------------------------------ data

/// ONE SDU, WHOLE, AS AN ISO DATA PACKET: the handle with the "complete" boundary and no time stamp, the packet
/// sequence number, the SDU's length and the SDU.
pub fn iso_packet(handle: u16, sequence: u16, sdu: &[u8]) -> Vec<u8> {
	let mut out = Vec::with_capacity(8 + sdu.len());
	out.extend_from_slice(&((handle & 0x0fff) | (0b10 << 12)).to_le_bytes());
	out.extend_from_slice(&((4 + sdu.len()) as u16 & 0x3fff).to_le_bytes());
	out.extend_from_slice(&sequence.to_le_bytes());
	out.extend_from_slice(&(sdu.len() as u16 & 0x0fff).to_le_bytes());
	out.extend_from_slice(sdu);
	out
}

/// An SDU as it arrived: its sequence number, and whether the controller said it was received well.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sdu {
	pub handle: u16,
	pub sequence: u16,
	pub valid: bool,
	pub data: Vec<u8>,
}

/// THE FRAGMENTS OF ONE INBOUND SDU, put together. A first fragment starts it, continuations add to it, a last one
/// completes it; a complete packet is an SDU on its own. Anything out of order is dropped, and so is an SDU past
/// `MAX_SDU`.
#[derive(Default)]
pub struct Reassembly {
	partial: Option<(u16, u16, bool, usize, Vec<u8>)>,
}

impl Reassembly {
	pub fn push(&mut self, packet: &[u8]) -> Option<Sdu> {
		let header = u16_at(packet, 0)?;
		let handle = header & 0x0fff;
		let boundary = (header >> 12) & 0b11;
		let stamped = (header >> 14) & 1 == 1;
		let len = usize::from(u16_at(packet, 2)? & 0x3fff);
		let body = packet.get(4..4 + len)?;
		match boundary {
			// FIRST OR COMPLETE: the load header - its time stamp where flagged, the sequence number, the SDU length and
			// its status.
			0b00 | 0b10 => {
				let skip = if stamped { 4 } else { 0 };
				let load = body.get(skip..skip + 4)?;
				let sequence = u16::from_le_bytes([load[0], load[1]]);
				let length_and_status = u16::from_le_bytes([load[2], load[3]]);
				let total = usize::from(length_and_status & 0x0fff);
				let valid = length_and_status >> 14 == 0;
				let data = &body[skip + 4..];
				if total > MAX_SDU || data.len() > total {
					self.partial = None;
					return None;
				}
				if boundary == 0b10 {
					self.partial = None;
					return (data.len() == total).then(|| Sdu { handle, sequence, valid, data: data.to_vec() });
				}
				self.partial = Some((handle, sequence, valid, total, data.to_vec()));
				None
			}
			// CONTINUATION OR LAST.
			_ => {
				let (held, sequence, valid, total, mut data) = self.partial.take()?;
				if held != handle || data.len() + body.len() > total {
					return None;
				}
				data.extend_from_slice(body);
				if boundary == 0b01 {
					self.partial = Some((held, sequence, valid, total, data));
					return None;
				}
				(data.len() == total).then_some(Sdu { handle, sequence, valid, data })
			}
		}
	}
}

// ------------------------------------------------------------------ the classifier

/// What an inbound data packet is, by its handle against the service's tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
	Acl,
	Iso,
	/// Neither table names the handle: dropped, and counted.
	Unknown,
	/// The kind's ceiling refuses it.
	TooLong,
}

/// CLASSIFY ONE DATA PACKET the transport delivered as ACL: an ISO stream's by its handle, a link's, or nobody's -
/// and then the ceiling of what it turned out to be.
pub fn classify(packet: &[u8], is_iso: impl Fn(u16) -> bool, is_link: impl Fn(u16) -> bool, acl_ceiling: u32, iso_ceiling: u32) -> Class {
	let Some(header) = u16_at(packet, 0) else { return Class::Unknown };
	let handle = header & 0x0fff;
	let len = packet.len() as u32;
	if is_iso(handle) {
		return if len > iso_ceiling { Class::TooLong } else { Class::Iso };
	}
	if is_link(handle) {
		return if len > acl_ceiling { Class::TooLong } else { Class::Acl };
	}
	Class::Unknown
}

#[cfg(test)]
mod tests;
