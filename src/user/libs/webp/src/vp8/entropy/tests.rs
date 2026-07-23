use super::*;

struct BoolReader<'a> {
	bytes: &'a [u8],
	next: usize,
	value: u32,
	width: u32,
	consumed: u8,
}

impl<'a> BoolReader<'a> {
	fn new(bytes: &'a [u8]) -> Self {
		let value = (u32::from(bytes[0]) << 8) | u32::from(bytes[1]);
		Self { bytes, next: 2, value, width: 255, consumed: 0 }
	}

	fn take(&mut self, zero_probability: u8) -> bool {
		let boundary = 1 + (((self.width - 1) * u32::from(zero_probability)) >> 8);
		let scaled = boundary << 8;
		let one = self.value >= scaled;
		if one {
			self.width -= boundary;
			self.value -= scaled;
		} else {
			self.width = boundary;
		}
		while self.width < 128 {
			self.width <<= 1;
			self.value <<= 1;
			self.consumed += 1;
			if self.consumed == 8 {
				self.consumed = 0;
				self.value |= u32::from(self.bytes.get(self.next).copied().unwrap_or(0));
				self.next += 1;
			}
		}
		one
	}

	fn literal(&mut self, bits: u8) -> u32 {
		let mut value = 0u32;
		for _ in 0..bits {
			value = (value << 1) | u32::from(self.take(128));
		}
		value
	}
}

#[test]
fn probability_bits_and_literals_round_trip() {
	let mut writer = BoolWriter::new();
	let mut expected = Vec::new();
	let mut state = 0x8e37_79b9u32;
	for index in 0..8192u32 {
		state ^= state << 13;
		state ^= state >> 17;
		state ^= state << 5;
		let probability = ((state >> 24) as u8).max(1);
		let bit = state.rotate_left(index & 31) & 1 != 0;
		writer.push(bit, probability);
		expected.push((bit, probability));
	}
	writer.literal(0x5a3c, 16);
	writer.literal(0x01ab_cdef, 29);
	let encoded = writer.finish();
	assert!(encoded.len() > 2);

	let mut reader = BoolReader::new(&encoded);
	for (bit, probability) in expected {
		assert_eq!(reader.take(probability), bit);
	}
	assert_eq!(reader.literal(16), 0x5a3c);
	assert_eq!(reader.literal(29), 0x01ab_cdef);
}
