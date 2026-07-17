use alloc::vec::Vec;

pub(super) struct BoolWriter {
	bytes: Vec<u8>,
	low: u32,
	width: u32,
	shifts_until_byte: u8,
}

impl BoolWriter {
	pub(super) const fn new() -> Self {
		Self { bytes: Vec::new(), low: 0, width: 255, shifts_until_byte: 24 }
	}

	pub(super) fn push(&mut self, one: bool, zero_probability: u8) {
		debug_assert!(zero_probability != 0);
		let boundary = 1 + (((self.width - 1) * u32::from(zero_probability)) >> 8);
		if one {
			self.low += boundary;
			self.width -= boundary;
		} else {
			self.width = boundary;
		}

		while self.width < 128 {
			self.width <<= 1;
			if self.low & 0x8000_0000 != 0 {
				self.propagate_carry();
			}
			self.low <<= 1;
			self.shifts_until_byte -= 1;
			if self.shifts_until_byte == 0 {
				self.bytes.push((self.low >> 24) as u8);
				self.low &= 0x00ff_ffff;
				self.shifts_until_byte = 8;
			}
		}
	}

	pub(super) fn literal(&mut self, value: u32, bits: u8) {
		debug_assert!(bits <= 32);
		for shift in (0..bits).rev() {
			self.push(value & (1u32 << shift) != 0, 128);
		}
	}

	pub(super) fn symbol(&mut self, tree: &[i8], probabilities: &[u8], target: i8) {
		self.symbol_from(tree, probabilities, target, 0);
	}

	pub(super) fn symbol_from(&mut self, tree: &[i8], probabilities: &[u8], target: i8, first_node: usize) {
		let mut decisions = [false; 16];
		let mut probability_indices = [0usize; 16];
		let count = find_symbol(tree, first_node, target, &mut decisions, &mut probability_indices, 0).expect("VP8 symbol is present in its coding tree");
		for index in 0..count {
			self.push(decisions[index], probabilities[probability_indices[index]]);
		}
	}

	pub(super) fn finish(mut self) -> Vec<u8> {
		let mut remaining = self.shifts_until_byte;
		let mut low = self.low;
		if low & (1u32 << (32 - remaining)) != 0 {
			self.propagate_carry();
		}
		low <<= remaining & 7;
		remaining >>= 3;
		while remaining > 1 {
			low <<= 8;
			remaining -= 1;
		}
		for _ in 0..4 {
			self.bytes.push((low >> 24) as u8);
			low <<= 8;
		}
		self.bytes
	}

	fn propagate_carry(&mut self) {
		let mut cursor = self.bytes.len();
		loop {
			assert!(cursor != 0, "VP8 boolean carry escaped the output prefix");
			cursor -= 1;
			if self.bytes[cursor] == 0xff {
				self.bytes[cursor] = 0;
			} else {
				self.bytes[cursor] += 1;
				return;
			}
		}
	}
}

fn find_symbol(tree: &[i8], node: usize, target: i8, decisions: &mut [bool; 16], probability_indices: &mut [usize; 16], depth: usize) -> Option<usize> {
	if depth == decisions.len() || node + 1 >= tree.len() {
		return None;
	}
	for branch in 0..2usize {
		let next = tree[node + branch];
		decisions[depth] = branch != 0;
		probability_indices[depth] = node / 2;
		if next <= 0 {
			if -next == target {
				return Some(depth + 1);
			}
		} else if let Some(length) = find_symbol(tree, next as usize, target, decisions, probability_indices, depth + 1) {
			return Some(length);
		}
	}
	None
}

#[cfg(test)]
#[path = "entropy/tests.rs"]
mod tests;
