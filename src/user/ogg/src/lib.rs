#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const MAX_PACKET_SIZE: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Checksum,
	Sequence,
	TooLarge,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Packet {
	pub data: Vec<u8>,
	pub page_granule_position: Option<u64>,
	pub granule_position: Option<u64>,
	pub bos: bool,
	pub eos: bool,
}

pub struct PacketReader<'a> {
	bytes: &'a [u8],
	cursor: usize,
	serial: Option<u32>,
	next_sequence: Option<u32>,
	segments_start: usize,
	segment_count: usize,
	segment_index: usize,
	body_cursor: usize,
	page_end: usize,
	page_granule: u64,
	page_flags: u8,
	pending: Vec<u8>,
	packet_index: u64,
	finished: bool,
}

impl<'a> PacketReader<'a> {
	pub fn new(bytes: &'a [u8]) -> PacketReader<'a> {
		PacketReader { bytes, cursor: 0, serial: None, next_sequence: None, segments_start: 0, segment_count: 0, segment_index: 0, body_cursor: 0, page_end: 0, page_granule: u64::MAX, page_flags: 0, pending: Vec::new(), packet_index: 0, finished: false }
	}

	pub fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
		if self.finished {
			return Ok(None);
		}
		loop {
			if self.segment_index == self.segment_count {
				if self.page_end != 0 {
					self.cursor = self.page_end;
				}
				if self.cursor == self.bytes.len() {
					if self.pending.is_empty() {
						self.finished = true;
						return Ok(None);
					}
					return Err(Error::Truncated);
				}
				self.load_page()?;
				if self.segment_count == 0 {
					continue;
				}
			}
			let length = self.bytes[self.segments_start + self.segment_index] as usize;
			let end = self.body_cursor.checked_add(length).ok_or(Error::TooLarge)?;
			let data = self.bytes.get(self.body_cursor..end).filter(|_| end <= self.page_end).ok_or(Error::Truncated)?;
			if self.pending.len().checked_add(length).ok_or(Error::TooLarge)? > MAX_PACKET_SIZE {
				return Err(Error::TooLarge);
			}
			self.pending.try_reserve(length).map_err(|_| Error::TooLarge)?;
			self.pending.extend_from_slice(data);
			self.body_cursor = end;
			self.segment_index += 1;
			if length < 255 {
				let is_last_complete = self.bytes[self.segments_start + self.segment_index..self.segments_start + self.segment_count].iter().all(|length| *length == 255);
				let page_granule_position = (self.page_granule != u64::MAX).then_some(self.page_granule);
				let granule_position = if is_last_complete { page_granule_position } else { None };
				let packet = Packet { data: core::mem::take(&mut self.pending), page_granule_position, granule_position, bos: self.packet_index == 0 && self.page_flags & 0x02 != 0, eos: is_last_complete && self.page_flags & 0x04 != 0 };
				self.packet_index = self.packet_index.checked_add(1).ok_or(Error::TooLarge)?;
				if packet.eos {
					if self.segment_index != self.segment_count || self.page_end != self.bytes.len() {
						return Err(Error::Invalid);
					}
					self.finished = true;
				}
				return Ok(Some(packet));
			}
		}
	}

	fn load_page(&mut self) -> Result<(), Error> {
		let header = self.bytes.get(self.cursor..self.cursor + 27).ok_or(Error::Truncated)?;
		if &header[..4] != b"OggS" || header[4] != 0 || header[5] & !0x07 != 0 {
			return Err(Error::Invalid);
		}
		let continued = header[5] & 1 != 0;
		if continued != !self.pending.is_empty() {
			return Err(Error::Invalid);
		}
		let serial = u32::from_le_bytes(header[14..18].try_into().map_err(|_| Error::Truncated)?);
		if self.serial.is_some_and(|expected| expected != serial) {
			return Err(Error::Invalid);
		}
		self.serial = Some(serial);
		let sequence = u32::from_le_bytes(header[18..22].try_into().map_err(|_| Error::Truncated)?);
		if self.next_sequence.is_some_and(|expected| expected != sequence) {
			return Err(Error::Sequence);
		}
		self.next_sequence = Some(sequence.checked_add(1).ok_or(Error::Sequence)?);
		let segment_count = header[26] as usize;
		let segments_start = self.cursor.checked_add(27).ok_or(Error::TooLarge)?;
		let body_start = segments_start.checked_add(segment_count).ok_or(Error::TooLarge)?;
		let segments = self.bytes.get(segments_start..body_start).ok_or(Error::Truncated)?;
		let body_len = segments.iter().try_fold(0usize, |sum, length| sum.checked_add(*length as usize).ok_or(Error::TooLarge))?;
		let page_end = body_start.checked_add(body_len).ok_or(Error::TooLarge)?;
		let page = self.bytes.get(self.cursor..page_end).ok_or(Error::Truncated)?;
		let expected_crc = u32::from_le_bytes(header[22..26].try_into().map_err(|_| Error::Truncated)?);
		if ogg_crc(page) != expected_crc {
			return Err(Error::Checksum);
		}
		self.segments_start = segments_start;
		self.segment_count = segment_count;
		self.segment_index = 0;
		self.body_cursor = body_start;
		self.page_end = page_end;
		self.page_granule = u64::from_le_bytes(header[6..14].try_into().map_err(|_| Error::Truncated)?);
		self.page_flags = header[5];
		Ok(())
	}
}

pub fn ogg_crc(bytes: &[u8]) -> u32 {
	let mut crc = 0u32;
	for (index, &byte) in bytes.iter().enumerate() {
		let byte = if (22..26).contains(&index) { 0 } else { byte };
		crc ^= (byte as u32) << 24;
		for _ in 0..8 {
			crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
		}
	}
	crc
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	fn page(flags: u8, serial: u32, sequence: u32, granule: u64, lacing: &[u8], body: &[u8]) -> Vec<u8> {
		assert_eq!(lacing.iter().map(|value| *value as usize).sum::<usize>(), body.len());
		let mut bytes = b"OggS".to_vec();
		bytes.push(0);
		bytes.push(flags);
		bytes.extend_from_slice(&granule.to_le_bytes());
		bytes.extend_from_slice(&serial.to_le_bytes());
		bytes.extend_from_slice(&sequence.to_le_bytes());
		bytes.extend_from_slice(&0u32.to_le_bytes());
		bytes.push(lacing.len() as u8);
		bytes.extend_from_slice(lacing);
		bytes.extend_from_slice(body);
		let crc = ogg_crc(&bytes);
		bytes[22..26].copy_from_slice(&crc.to_le_bytes());
		bytes
	}

	#[test]
	fn frames_packets_across_pages_and_assigns_final_granule() {
		let mut bytes = page(0x02, 7, 0, 0, &[3, 255], &[b'h', b'd', b'r'].into_iter().chain(core::iter::repeat_n(b'a', 255)).collect::<Vec<_>>());
		bytes.extend_from_slice(&page(0x05, 7, 1, 123, &[2, 1], b"bc!"));
		let mut reader = PacketReader::new(&bytes);
		let first = reader.next_packet().unwrap().unwrap();
		assert_eq!(first.data, b"hdr");
		assert!(first.bos);
		let second = reader.next_packet().unwrap().unwrap();
		assert_eq!(second.data.len(), 257);
		assert_eq!(&second.data[255..], b"bc");
		assert_eq!(second.page_granule_position, Some(123));
		assert_eq!(second.granule_position, None);
		let third = reader.next_packet().unwrap().unwrap();
		assert_eq!(third.data, b"!");
		assert_eq!(third.granule_position, Some(123));
		assert!(third.eos);
		assert_eq!(reader.next_packet(), Ok(None));
	}

	#[test]
	fn rejects_crc_sequence_continuation_and_truncation_errors() {
		let valid = page(0x02, 9, 0, 0, &[1], b"x");
		assert_eq!(PacketReader::new(&[]).next_packet(), Ok(None));
		for len in [1, 4, 26, valid.len() - 1] {
			assert!(PacketReader::new(&valid[..len]).next_packet().is_err());
		}
		let mut corrupt = valid.clone();
		*corrupt.last_mut().unwrap() ^= 1;
		assert_eq!(PacketReader::new(&corrupt).next_packet(), Err(Error::Checksum));
		let continued = page(1, 9, 0, 0, &[1], b"x");
		assert_eq!(PacketReader::new(&continued).next_packet(), Err(Error::Invalid));
		let mut skipped = valid;
		skipped.extend_from_slice(&page(0, 9, 2, 0, &[1], b"y"));
		let mut reader = PacketReader::new(&skipped);
		assert!(reader.next_packet().unwrap().is_some());
		assert_eq!(reader.next_packet(), Err(Error::Sequence));
	}

	#[test]
	fn enforces_packet_size_cap_before_allocation() {
		let bytes = [1u8];
		let mut reader = PacketReader::new(&bytes);
		reader.pending = vec![0; MAX_PACKET_SIZE];
		reader.segments_start = 0;
		reader.segment_count = 1;
		reader.body_cursor = 0;
		reader.page_end = 1;
		assert_eq!(reader.next_packet(), Err(Error::TooLarge));
	}

	#[test]
	fn frames_staged_vorbis_stream() {
		let mut reader = PacketReader::new(include_bytes!("../../../volume/sample.ogg"));
		for signature in [b"\x01vorbis".as_slice(), b"\x03vorbis".as_slice(), b"\x05vorbis".as_slice()] {
			let packet = reader.next_packet().unwrap().unwrap();
			assert!(packet.data.starts_with(signature));
		}
		let mut audio_packets = 0usize;
		let mut final_granule = None;
		let mut saw_eos = false;
		while let Some(packet) = reader.next_packet().unwrap() {
			audio_packets += 1;
			final_granule = packet.granule_position.or(final_granule);
			saw_eos |= packet.eos;
		}
		assert!(audio_packets != 0);
		assert_eq!(final_granule, Some(512));
		assert!(saw_eos);
	}
}
