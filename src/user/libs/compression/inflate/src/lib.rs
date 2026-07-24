#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

const MAX_BITS: usize = 15;
const LENGTH_BASE: [usize; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [usize; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

struct Bits<'a> {
	data: &'a [u8],
	byte: usize,
	bit: u8,
}

impl<'a> Bits<'a> {
	fn new(data: &'a [u8]) -> Bits<'a> {
		Bits { data, byte: 0, bit: 0 }
	}

	fn take(&mut self, count: u8) -> Result<u32, Error> {
		let mut value = 0u32;
		for shift in 0..count {
			let byte = *self.data.get(self.byte).ok_or(Error::Truncated)?;
			value |= (((byte >> self.bit) & 1) as u32) << shift;
			self.bit += 1;
			if self.bit == 8 {
				self.bit = 0;
				self.byte += 1;
			}
		}
		Ok(value)
	}

	fn align(&mut self) {
		if self.bit != 0 {
			self.bit = 0;
			self.byte += 1;
		}
	}

	fn bytes(&mut self, count: usize) -> Result<&'a [u8], Error> {
		if self.bit != 0 {
			return Err(Error::Invalid);
		}
		let end = self.byte.checked_add(count).ok_or(Error::Invalid)?;
		let bytes = self.data.get(self.byte..end).ok_or(Error::Truncated)?;
		self.byte = end;
		Ok(bytes)
	}
}

struct Huffman {
	counts: [u16; MAX_BITS + 1],
	symbols: Vec<u16>,
}

impl Huffman {
	fn new(lengths: &[u8]) -> Result<Huffman, Error> {
		let mut counts = [0u16; MAX_BITS + 1];
		for &length in lengths {
			if length as usize > MAX_BITS {
				return Err(Error::Invalid);
			}
			counts[length as usize] = counts[length as usize].checked_add(1).ok_or(Error::Invalid)?;
		}
		if counts[0] as usize == lengths.len() {
			return Err(Error::Invalid);
		}
		let mut left = 1i32;
		for count in counts.iter().skip(1) {
			left = (left << 1) - *count as i32;
			if left < 0 {
				return Err(Error::Invalid);
			}
		}
		let mut offsets = [0usize; MAX_BITS + 1];
		for length in 1..MAX_BITS {
			offsets[length + 1] = offsets[length] + counts[length] as usize;
		}
		let mut symbols = vec![0; lengths.len() - counts[0] as usize];
		for (symbol, &length) in lengths.iter().enumerate() {
			if length != 0 {
				let index = offsets[length as usize];
				symbols[index] = symbol as u16;
				offsets[length as usize] += 1;
			}
		}
		Ok(Huffman { counts, symbols })
	}

	fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
		let mut code = 0u32;
		let mut first = 0u32;
		let mut index = 0usize;
		for length in 1..=MAX_BITS {
			code |= bits.take(1)?;
			let count = self.counts[length] as u32;
			if code < first + count {
				return self.symbols.get(index + (code - first) as usize).copied().ok_or(Error::Invalid);
			}
			index += count as usize;
			first = (first + count) << 1;
			code <<= 1;
		}
		Err(Error::Invalid)
	}
}

pub fn zlib(data: &[u8], expected: usize) -> Result<Vec<u8>, Error> {
	if data.len() < 6 {
		return Err(Error::Truncated);
	}
	let cmf = data[0];
	let flg = data[1];
	if cmf & 0x0f != 8 || cmf >> 4 > 7 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
		return Err(Error::Unsupported);
	}
	let trailer = data.len() - 4;
	let mut bits = Bits::new(&data[2..trailer]);
	let mut output = Vec::new();
	output.try_reserve_exact(expected).map_err(|_| Error::TooLarge)?;
	loop {
		let final_block = bits.take(1)? != 0;
		match bits.take(2)? {
			0 => stored(&mut bits, &mut output, expected)?,
			1 => {
				let (literal, distance) = fixed_trees()?;
				compressed(&mut bits, &literal, &distance, &mut output, expected)?;
			}
			2 => {
				let (literal, distance) = dynamic_trees(&mut bits)?;
				compressed(&mut bits, &literal, &distance, &mut output, expected)?;
			}
			_ => return Err(Error::Invalid),
		}
		if final_block {
			break;
		}
	}
	bits.align();
	if bits.byte != bits.data.len() || output.len() != expected {
		return Err(Error::Invalid);
	}
	let stored_adler = u32::from_be_bytes(data[trailer..].try_into().map_err(|_| Error::Truncated)?);
	if adler32(&output) != stored_adler {
		return Err(Error::Invalid);
	}
	Ok(output)
}

fn stored(bits: &mut Bits<'_>, output: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
	bits.align();
	let header = bits.bytes(4)?;
	let len = u16::from_le_bytes([header[0], header[1]]);
	let inverse = u16::from_le_bytes([header[2], header[3]]);
	if len != !inverse || output.len().checked_add(len as usize).filter(|end| *end <= limit).is_none() {
		return Err(Error::Invalid);
	}
	output.extend_from_slice(bits.bytes(len as usize)?);
	Ok(())
}

fn fixed_trees() -> Result<(Huffman, Huffman), Error> {
	let mut literal_lengths = vec![0u8; 288];
	literal_lengths[..144].fill(8);
	literal_lengths[144..256].fill(9);
	literal_lengths[256..280].fill(7);
	literal_lengths[280..].fill(8);
	Ok((Huffman::new(&literal_lengths)?, Huffman::new(&[5; 32])?))
}

fn dynamic_trees(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
	let literal_count = bits.take(5)? as usize + 257;
	let distance_count = bits.take(5)? as usize + 1;
	let code_count = bits.take(4)? as usize + 4;
	if literal_count > 286 || distance_count > 32 {
		return Err(Error::Invalid);
	}
	const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
	let mut code_lengths = [0u8; 19];
	for &index in &ORDER[..code_count] {
		code_lengths[index] = bits.take(3)? as u8;
	}
	let code_tree = Huffman::new(&code_lengths)?;
	let total = literal_count + distance_count;
	let mut lengths = Vec::with_capacity(total);
	while lengths.len() < total {
		match code_tree.decode(bits)? {
			value @ 0..=15 => lengths.push(value as u8),
			16 => {
				let previous = *lengths.last().ok_or(Error::Invalid)?;
				let repeat = bits.take(2)? as usize + 3;
				append_repeated(&mut lengths, total, previous, repeat)?;
			}
			17 => {
				let repeat = bits.take(3)? as usize + 3;
				append_repeated(&mut lengths, total, 0, repeat)?;
			}
			18 => {
				let repeat = bits.take(7)? as usize + 11;
				append_repeated(&mut lengths, total, 0, repeat)?;
			}
			_ => return Err(Error::Invalid),
		}
	}
	if lengths[256] == 0 {
		return Err(Error::Invalid);
	}
	Ok((Huffman::new(&lengths[..literal_count])?, Huffman::new(&lengths[literal_count..])?))
}

fn append_repeated(lengths: &mut Vec<u8>, total: usize, value: u8, repeat: usize) -> Result<(), Error> {
	if lengths.len().checked_add(repeat).filter(|end| *end <= total).is_none() {
		return Err(Error::Invalid);
	}
	lengths.resize(lengths.len() + repeat, value);
	Ok(())
}

fn compressed(bits: &mut Bits<'_>, literal: &Huffman, distance: &Huffman, output: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
	loop {
		match literal.decode(bits)? as usize {
			value @ 0..=255 => push(output, value as u8, limit)?,
			256 => return Ok(()),
			value @ 257..=285 => {
				let length_index = value - 257;
				let length = LENGTH_BASE[length_index] + bits.take(LENGTH_EXTRA[length_index])? as usize;
				let distance_symbol = distance.decode(bits)? as usize;
				if distance_symbol >= DIST_BASE.len() {
					return Err(Error::Invalid);
				}
				let distance = DIST_BASE[distance_symbol] + bits.take(DIST_EXTRA[distance_symbol])? as usize;
				if distance == 0 || distance > output.len() || output.len().checked_add(length).filter(|end| *end <= limit).is_none() {
					return Err(Error::Invalid);
				}
				for _ in 0..length {
					let byte = output[output.len() - distance];
					output.push(byte);
				}
			}
			_ => return Err(Error::Invalid),
		}
	}
}

fn push(output: &mut Vec<u8>, byte: u8, limit: usize) -> Result<(), Error> {
	if output.len() >= limit {
		return Err(Error::Invalid);
	}
	output.push(byte);
	Ok(())
}

fn adler32(data: &[u8]) -> u32 {
	let mut a = 1u32;
	let mut b = 0u32;
	for &byte in data {
		a = (a + byte as u32) % 65_521;
		b = (b + a) % 65_521;
	}
	b << 16 | a
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
