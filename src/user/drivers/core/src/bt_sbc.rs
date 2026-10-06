// THE FIXTURE'S SBC: what the emulated headset reads from a stream and what the emulated phone writes into one.
//
// A TEST FIXTURE, DEVELOPMENT-ONLY, AND WRITTEN APART FROM THE HOST'S CODEC: its own frame reader, CRC, bit allocation
// and dequantization, from the A2DP specification's Appendix B - so a host frame this reads correctly is one two
// implementations agree on. It needs no filter bank: the headset judges what it hears in the subband domain - which
// subband carries the energy - and the phone writes a tone straight into one subband, which the host's synthesis turns
// into audio at that band's frequencies.

const OFFSETS4: [[i32; 4]; 4] = [[-1, 0, 0, 0], [-2, 0, 0, 1], [-2, 0, 0, 1], [-2, 0, 0, 1]];
const OFFSETS8: [[i32; 8]; 4] = [[-2, 0, 0, 0, 0, 0, 0, 1], [-3, 0, 0, 0, 0, 0, 1, 2], [-4, 0, 0, 0, 0, 0, 1, 2], [-4, 0, 0, 0, 0, 0, 1, 2]];

// A frame's header, as the fixture reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header {
	pub frequency: u32,
	pub blocks: usize,
	// 0 mono, 1 dual, 2 stereo, 3 joint stereo.
	pub mode: u8,
	pub snr: bool,
	pub subbands: usize,
	pub bitpool: u32,
}

impl Header {
	pub fn channels(&self) -> usize {
		if self.mode == 0 { 1 } else { 2 }
	}

	pub fn length(&self) -> usize {
		let fixed = 4 + (4 * self.subbands * self.channels()) / 8;
		let bitpool = self.bitpool as usize;
		fixed
			+ match self.mode {
				0 | 1 => (self.blocks * self.channels() * bitpool).div_ceil(8),
				2 => (self.blocks * bitpool).div_ceil(8),
				_ => (self.subbands + self.blocks * bitpool).div_ceil(8),
			}
	}

	fn frequency_index(&self) -> usize {
		match self.frequency {
			16_000 => 0,
			32_000 => 1,
			44_100 => 2,
			_ => 3,
		}
	}

	pub fn mode_name(&self) -> &'static str {
		["mono", "dual channel", "stereo", "joint stereo"][usize::from(self.mode & 3)]
	}
}

// mSBC's one configuration, behind its own sync word: 16 kHz mono, fifteen blocks, eight subbands, loudness, bitpool 26.
pub const MSBC: Header = Header { frequency: 16_000, blocks: 15, mode: 0, snr: false, subbands: 8, bitpool: 26 };

pub fn header(frame: &[u8]) -> Option<Header> {
	if frame.len() >= 4 && frame[0] == 0xad {
		return Some(MSBC);
	}
	if frame.len() < 4 || frame[0] != 0x9c {
		return None;
	}
	Some(Header { frequency: [16_000, 32_000, 44_100, 48_000][usize::from(frame[1] >> 6)], blocks: [4, 8, 12, 16][usize::from((frame[1] >> 4) & 3)], mode: (frame[1] >> 2) & 3, snr: frame[1] & 2 != 0, subbands: if frame[1] & 1 != 0 { 8 } else { 4 }, bitpool: u32::from(frame[2]) })
}

// CRC-8, x^8 + x^4 + x^3 + x^2 + 1 from 0x0F, a bit at a time over `bits` bits.
fn crc(bytes: &[u8], bits: usize) -> u8 {
	let mut register: u8 = 0x0f;
	for at in 0..bits {
		let incoming = (bytes[at / 8] >> (7 - (at % 8))) & 1;
		let feedback = (register >> 7) ^ incoming;
		register <<= 1;
		if feedback == 1 {
			register ^= 0x1d;
		}
	}
	register
}

// The bit allocation, from the scale factors and the bitpool: the specification's slices, over one channel or both.
fn allocation(header: &Header, factors: &[[i32; 8]; 2]) -> [[u32; 8]; 2] {
	let subbands = header.subbands;
	let offsets = |sb: usize| if subbands == 4 { OFFSETS4[header.frequency_index()][sb] } else { OFFSETS8[header.frequency_index()][sb] };
	let mut need = [[0i32; 8]; 2];
	for ch in 0..header.channels() {
		for sb in 0..subbands {
			need[ch][sb] = if header.snr {
				factors[ch][sb]
			} else if factors[ch][sb] == 0 {
				-5
			} else {
				let loudness = factors[ch][sb] - offsets(sb);
				if loudness > 0 { loudness / 2 } else { loudness }
			};
		}
	}
	let mut bits = [[0u32; 8]; 2];
	let groups: &[&[usize]] = if header.mode >= 2 {
		&[&[0, 1]]
	} else if header.channels() == 2 {
		&[&[0], &[1]]
	} else {
		&[&[0]]
	};
	for group in groups {
		let pool = header.bitpool as i32;
		let biggest = group.iter().flat_map(|&ch| need[ch][..subbands].iter().copied()).max().unwrap_or(0);
		let (mut spent, mut slice_cost, mut slice) = (0i32, 0i32, biggest + 1);
		loop {
			slice -= 1;
			spent += slice_cost;
			slice_cost = 0;
			for &ch in group.iter() {
				for &n in &need[ch][..subbands] {
					if n > slice + 1 && n < slice + 16 {
						slice_cost += 1;
					} else if n == slice + 1 {
						slice_cost += 2;
					}
				}
			}
			if spent + slice_cost >= pool {
				break;
			}
		}
		if spent + slice_cost == pool {
			spent += slice_cost;
			slice -= 1;
		}
		for &ch in group.iter() {
			for sb in 0..subbands {
				bits[ch][sb] = if need[ch][sb] < slice + 2 { 0 } else { (need[ch][sb] - slice).min(16) as u32 };
			}
		}
		// Round one: the allocated ones and those one short; round two: one bit each. Channel by channel inside a subband.
		for round in 0..2 {
			'spend: for sb in 0..subbands {
				for &ch in group.iter() {
					if spent >= pool {
						break 'spend;
					}
					if round == 0 {
						if bits[ch][sb] >= 2 && bits[ch][sb] < 16 {
							bits[ch][sb] += 1;
							spent += 1;
						} else if need[ch][sb] == slice + 1 && pool > spent + 1 {
							bits[ch][sb] = 2;
							spent += 2;
						}
					} else if bits[ch][sb] < 16 {
						bits[ch][sb] += 1;
						spent += 1;
					}
				}
			}
		}
	}
	bits
}

struct Bits<'a> {
	bytes: &'a [u8],
	at: usize,
}

impl Bits<'_> {
	fn take(&mut self, count: u32) -> Option<u32> {
		let mut value = 0;
		for _ in 0..count {
			let byte = *self.bytes.get(self.at / 8)?;
			value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
			self.at += 1;
		}
		Some(value)
	}
}

// WHAT A FRAME CARRIED, as far as the headset judges it: its header, whether its CRC held, and the energy in each
// subband of the first channel - sum of squared dequantized samples, in sample units squared.
pub struct Heard {
	pub header: Header,
	pub crc_good: bool,
	pub energy: [u64; 8],
	pub length: usize,
}

pub fn hear(frame: &[u8]) -> Option<Heard> {
	let header = header(frame)?;
	let length = header.length();
	if frame.len() < length {
		return None;
	}
	let (subbands, channels) = (header.subbands, header.channels());
	let mut reader = Bits { bytes: frame, at: 32 };
	let mut joined = [false; 8];
	if header.mode == 3 {
		for flag in joined.iter_mut().take(subbands) {
			*flag = reader.take(1)? == 1;
		}
	}
	let mut factors = [[0i32; 8]; 2];
	for row in factors.iter_mut().take(channels) {
		for factor in row.iter_mut().take(subbands) {
			*factor = reader.take(4)? as i32;
		}
	}
	// The check covers header bytes 1 and 2 and everything from byte 4 to the end of the scale factors.
	let covered = reader.at - 32;
	let mut checked = alloc::vec![frame[1], frame[2]];
	checked.extend_from_slice(&frame[4..4 + covered.div_ceil(8)]);
	let crc_good = crc(&checked, 16 + covered) == frame[3];
	let bits = allocation(&header, &factors);
	let mut energy = [0u64; 8];
	for _ in 0..header.blocks {
		let mut values = [[0i64; 8]; 2];
		for ch in 0..channels {
			for sb in 0..subbands {
				let width = bits[ch][sb];
				if width == 0 {
					continue;
				}
				let q = i64::from(reader.take(width)?);
				let levels = (1i64 << width) - 1;
				let scale = 1i64 << (factors[ch][sb] + 1);
				values[ch][sb] = ((2 * q + 1) * scale) / levels - scale;
			}
		}
		for sb in 0..subbands {
			let left = if joined[sb] { values[0][sb] + values[1][sb] } else { values[0][sb] };
			energy[sb] += (left * left) as u64;
		}
	}
	Some(Heard { header, crc_good, energy, length })
}

// THE PHONE'S FRAME: a tone written straight into one subband of both channels - full scale factor there, silence
// elsewhere - at 44.1 kHz joint stereo, sixteen blocks, eight subbands, loudness, bitpool 35. `phase` alternates the
// tone's sign block by block, which is a tone at that subband's centre after synthesis.
pub fn tone_frame(subband: usize, amplitude_factor: u32, phase: &mut u32, out: &mut [u8]) -> usize {
	let header = Header { frequency: 44_100, blocks: 16, mode: 3, snr: false, subbands: 8, bitpool: 35 };
	tone(&header, subband, amplitude_factor, phase, out)
}

// THE HEADSET'S VOICE FRAME: the same tone in mSBC, its microphone heard by the host.
pub fn msbc_tone_frame(subband: usize, amplitude_factor: u32, phase: &mut u32, out: &mut [u8]) -> usize {
	tone(&MSBC, subband, amplitude_factor, phase, out)
}

fn tone(header: &Header, subband: usize, amplitude_factor: u32, phase: &mut u32, out: &mut [u8]) -> usize {
	let length = header.length();
	out[..length].fill(0);
	let channels = header.channels();
	let mut factors = [[0i32; 8]; 2];
	for row in factors.iter_mut().take(channels) {
		row[subband] = amplitude_factor as i32;
	}
	let bits = allocation(header, &factors);
	let mut writer = Writer { bytes: out, at: 0 };
	if *header == MSBC {
		writer.put(0xad, 8);
		writer.put(0, 16);
	} else {
		writer.put(0x9c, 8);
		// 44.1 kHz, sixteen blocks, joint stereo, loudness, eight subbands.
		writer.put((2 << 6) | (3 << 4) | (3 << 2) | 1, 8);
		writer.put(header.bitpool, 8);
	}
	writer.put(0, 8);
	if header.mode == 3 {
		for _ in 0..8 {
			writer.put(0, 1);
		}
	}
	for row in factors.iter().take(channels) {
		for &factor in row.iter() {
			writer.put(factor as u32, 4);
		}
	}
	let covered = writer.at - 32;
	let mut checked = alloc::vec![writer.bytes[1], writer.bytes[2]];
	checked.extend_from_slice(&writer.bytes[4..4 + covered.div_ceil(8)]);
	writer.bytes[3] = crc(&checked, 16 + covered);
	for _ in 0..header.blocks {
		*phase ^= 1;
		for ch in 0..channels {
			for sb in 0..8 {
				let width = bits[ch][sb];
				if width == 0 {
					continue;
				}
				let top = (1u32 << width) - 1;
				// The middle level is silence; near the top or the bottom, three quarters of the scale either way.
				let value = if sb == subband { if *phase == 0 { top - top / 8 } else { top / 8 } } else { top / 2 };
				writer.put(value, width);
			}
		}
	}
	length
}

struct Writer<'a> {
	bytes: &'a mut [u8],
	at: usize,
}

impl Writer<'_> {
	fn put(&mut self, value: u32, count: u32) {
		for shift in (0..count).rev() {
			if (value >> shift) & 1 == 1 {
				self.bytes[self.at / 8] |= 0x80 >> (self.at % 8);
			}
			self.at += 1;
		}
	}
}

#[cfg(test)]
mod tests {
	// THE FIXTURE'S OWN TWO HALVES AGREE: a frame the phone writes is one the headset reads - its length, its CRC and
	// the subband its tone is in.
	#[test]
	fn what_the_phone_writes_the_headset_reads() {
		let mut frame = [0u8; 128];
		let mut phase = 0;
		for _ in 0..4 {
			let length = super::tone_frame(2, 12, &mut phase, &mut frame);
			let heard = super::hear(&frame[..length]).expect("a frame");
			assert_eq!(heard.length, length);
			assert_eq!(length, 83, "44.1 kHz joint stereo, sixteen blocks, eight subbands, bitpool 35");
			assert!(heard.crc_good);
			let loudest = (0..8).max_by_key(|&sb| heard.energy[sb]).unwrap();
			assert_eq!(loudest, 2);
			let length = super::msbc_tone_frame(5, 11, &mut phase, &mut frame);
			assert_eq!(length, 57, "mSBC's fixed frame");
			let heard = super::hear(&frame[..length]).expect("an mSBC frame");
			assert!(heard.crc_good);
			assert_eq!((0..8).max_by_key(|&sb| heard.energy[sb]).unwrap(), 5);
		}
		frame[5] ^= 1;
		assert!(!super::hear(&frame[..83]).unwrap().crc_good, "a scale factor changed fails the CRC");
	}
}
