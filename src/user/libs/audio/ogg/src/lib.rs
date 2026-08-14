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
mod tests;

/// Assemble packets into Ogg pages.
///
/// The counterpart of `PacketReader`, and written against it: the only thing that makes a page
/// writer correct is that a reader gets back exactly the packets that went in, so every test here
/// is a round trip through the reader in this file rather than a comparison against bytes somebody
/// typed.
///
/// WHAT A PAGE IS, in the two rules that decide the whole layout: a packet is split into segments
/// of 255 bytes and a last segment shorter than 255, and a packet whose length is a multiple of 255
/// therefore ENDS WITH A ZERO-LENGTH SEGMENT - without it the reader would keep reading into the
/// next packet. A page carries at most 255 segments, so a packet longer than 65025 bytes spans
/// pages and the continuation flag on the next one says so.
///
/// The granule position is the CODEC's number and this writer does not invent it: it belongs to the
/// last packet that ENDS on the page, which is why `write_packet` takes it per packet and a page
/// whose last packet is unfinished carries -1 (the "no position here" value the format defines).
pub struct PageWriter {
	serial: u32,
	sequence: u32,
	out: Vec<u8>,
	// The packets held for the page being built: their bytes, and the granule of the last one that
	// ends here.
	segments: Vec<u8>,
	body: Vec<u8>,
	granule: u64,
	// Whether the page being built continues a packet that started on the previous one.
	continued: bool,
	first: bool,
}

// The most segments one page's table can name, and therefore the most bytes one page's body can
// carry (255 segments of 255 bytes).
const MAX_SEGMENTS: usize = 255;

impl PageWriter {
	pub fn new(serial: u32) -> PageWriter {
		PageWriter { serial, sequence: 0, out: Vec::new(), segments: Vec::new(), body: Vec::new(), granule: u64::MAX, continued: false, first: true }
	}

	/// Add one packet. `granule` is the position this packet ends at, or `None` for a packet whose
	/// position is not meaningful yet (the headers).
	///
	/// A packet is written across as many pages as it needs; a page is emitted as soon as its
	/// segment table is full, so the writer holds one page rather than the whole stream.
	pub fn write_packet(&mut self, packet: &[u8], granule: Option<u64>) -> Result<(), Error> {
		if packet.len() > MAX_PACKET_SIZE {
			return Err(Error::TooLarge);
		}
		let mut rest: &[u8] = packet;
		loop {
			let room: usize = MAX_SEGMENTS - self.segments.len();
			if room == 0 {
				// The page is full in the middle of this packet, so the next one continues it.
				self.flush_page(false)?;
				continue;
			}
			let take: usize = core::cmp::min(rest.len(), room.saturating_mul(255));
			let (head, tail) = rest.split_at(take);
			self.push_segments(head)?;
			rest = tail;
			// The terminating short segment - which is a ZERO when the packet's length is a
			// multiple of 255 - belongs to the packet, so it is written only when what is left is
			// nothing and there is room for it.
			if rest.is_empty() {
				if take % 255 == 0 {
					if self.segments.len() == MAX_SEGMENTS {
						self.flush_page(false)?;
					}
					self.segments.try_reserve(1).map_err(|_| Error::TooLarge)?;
					self.segments.push(0);
				}
				break;
			}
		}
		// The granule belongs to the page this packet ENDED on.
		if let Some(position) = granule {
			self.granule = position;
		}
		Ok(())
	}

	/// End the current page here, whatever is on it.
	///
	/// A PACKET BOUNDARY IS NOT ALWAYS A PAGE BOUNDARY, and sometimes a format demands one. Vorbis
	/// requires its identification header to be alone on the first page - a reader that finds
	/// anything else beside it is entitled to refuse the stream - and the writer packs as much into
	/// a page as fits, so without this the ident and the comment share one. Emitting nothing when
	/// there is nothing held, so calling it twice costs nothing and cannot produce an empty page.
	pub fn flush(&mut self) -> Result<(), Error> {
		if self.segments.is_empty() {
			return Ok(());
		}
		self.flush_page(false)
	}

	/// Emit the page being built, if it holds anything. A stream that ends mid-packet is a
	/// truncated stream, so `finish` is what says the last packet was the last one.
	pub fn finish(mut self) -> Result<Vec<u8>, Error> {
		if !self.segments.is_empty() {
			self.flush_page(true)?;
		}
		Ok(self.out)
	}

	fn push_segments(&mut self, data: &[u8]) -> Result<(), Error> {
		for chunk in data.chunks(255) {
			self.segments.try_reserve(1).map_err(|_| Error::TooLarge)?;
			self.segments.push(chunk.len() as u8);
			self.body.try_reserve(chunk.len()).map_err(|_| Error::TooLarge)?;
			self.body.extend_from_slice(chunk);
		}
		Ok(())
	}

	// Write one page out and start the next. `last` marks the end of the stream.
	fn flush_page(&mut self, last: bool) -> Result<(), Error> {
		let mut header: Vec<u8> = Vec::new();
		header.try_reserve(27 + self.segments.len()).map_err(|_| Error::TooLarge)?;
		header.extend_from_slice(b"OggS");
		header.push(0);
		// The flags, in the order the format numbers them: continued, first, last.
		let mut flags: u8 = 0;
		if self.continued {
			flags |= 0x01;
		}
		if self.first {
			flags |= 0x02;
		}
		if last {
			flags |= 0x04;
		}
		header.push(flags);
		header.extend_from_slice(&self.granule.to_le_bytes());
		header.extend_from_slice(&self.serial.to_le_bytes());
		header.extend_from_slice(&self.sequence.to_le_bytes());
		// The checksum is computed over the whole page WITH THIS FIELD ZERO, which is what
		// `ogg_crc` does by skipping bytes 22..26 - so the placeholder written here is what the
		// reader will also skip, and the two cannot disagree.
		header.extend_from_slice(&0u32.to_le_bytes());
		header.push(self.segments.len() as u8);
		header.extend_from_slice(&self.segments);
		let start: usize = self.out.len();
		self.out.try_reserve(header.len() + self.body.len()).map_err(|_| Error::TooLarge)?;
		self.out.extend_from_slice(&header);
		self.out.extend_from_slice(&self.body);
		let crc: u32 = ogg_crc(&self.out[start..]);
		self.out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
		// A page whose last packet did not end here carries no position, which the format spells
		// as all-ones rather than as a zero somebody could mistake for the start of the stream.
		self.continued = self.segments.last().is_some_and(|length| *length == 255);
		self.granule = u64::MAX;
		self.first = false;
		self.sequence = self.sequence.checked_add(1).ok_or(Error::Sequence)?;
		self.segments.clear();
		self.body.clear();
		Ok(())
	}
}
