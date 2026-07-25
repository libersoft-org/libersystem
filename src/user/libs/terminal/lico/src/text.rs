//! Incremental bounded UTF-8 decoding.

/// Unicode replacement character emitted for malformed UTF-8.
pub const REPLACEMENT_CHARACTER: u32 = 0xfffd;

/// Result of decoding a bounded byte chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedText {
	pub consumed: usize,
	pub produced: usize,
}

enum Step {
	None,
	Scalar(u32),
	Retry(u32),
}

/// Streaming UTF-8 decoder with no allocation and no unbounded state.
pub struct TextDecoder {
	value: u32,
	minimum: u32,
	remaining: u8,
}

impl Default for TextDecoder {
	fn default() -> Self {
		Self::new()
	}
}

impl TextDecoder {
	pub const fn new() -> TextDecoder {
		TextDecoder { value: 0, minimum: 0, remaining: 0 }
	}

	/// Decode as much of `input` as fits in `output`.
	///
	/// Invalid continuations emit one replacement character and retry the current byte as
	/// a fresh starter, preserving valid bytes that immediately follow malformed input.
	pub fn decode(&mut self, input: &[u8], output: &mut [u32]) -> DecodedText {
		let mut consumed = 0;
		let mut produced = 0;
		while consumed < input.len() && produced < output.len() {
			match self.step(input[consumed]) {
				Step::None => consumed += 1,
				Step::Scalar(value) => {
					output[produced] = value;
					produced += 1;
					consumed += 1;
				}
				Step::Retry(value) => {
					output[produced] = value;
					produced += 1;
				}
			}
		}
		DecodedText { consumed, produced }
	}

	/// Finish a stream. A truncated scalar becomes one replacement character.
	pub fn finish(&mut self) -> Option<u32> {
		if self.remaining == 0 {
			return None;
		}
		self.reset();
		Some(REPLACEMENT_CHARACTER)
	}

	pub const fn is_idle(&self) -> bool {
		self.remaining == 0
	}

	fn step(&mut self, byte: u8) -> Step {
		if self.remaining == 0 {
			return self.start(byte);
		}
		if byte & 0xc0 != 0x80 {
			self.reset();
			return Step::Retry(REPLACEMENT_CHARACTER);
		}
		self.value = (self.value << 6) | (byte & 0x3f) as u32;
		self.remaining -= 1;
		if self.remaining != 0 {
			return Step::None;
		}
		let value = self.value;
		let valid = value >= self.minimum && value <= 0x10ffff && !(0xd800..=0xdfff).contains(&value);
		self.reset();
		if valid { Step::Scalar(value) } else { Step::Scalar(REPLACEMENT_CHARACTER) }
	}

	fn start(&mut self, byte: u8) -> Step {
		match byte {
			0x00..=0x7f => Step::Scalar(byte as u32),
			0xc2..=0xdf => {
				self.value = (byte & 0x1f) as u32;
				self.minimum = 0x80;
				self.remaining = 1;
				Step::None
			}
			0xe0..=0xef => {
				self.value = (byte & 0x0f) as u32;
				self.minimum = 0x800;
				self.remaining = 2;
				Step::None
			}
			0xf0..=0xf4 => {
				self.value = (byte & 0x07) as u32;
				self.minimum = 0x10000;
				self.remaining = 3;
				Step::None
			}
			_ => Step::Scalar(REPLACEMENT_CHARACTER),
		}
	}

	fn reset(&mut self) {
		self.value = 0;
		self.minimum = 0;
		self.remaining = 0;
	}
}
