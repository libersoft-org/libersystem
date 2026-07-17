#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
	Truncated,
	Invalid,
	Unsupported,
	TooLarge,
}

pub fn decode(data: &[u8]) -> Result<pix::RgbaImage, Error> {
	let mut tokens = Tokens::new(data);
	let magic = tokens.next().ok_or(Error::Truncated)?;
	let ascii = match magic {
		b"P3" => true,
		b"P6" => false,
		_ => return Err(Error::Invalid),
	};
	let width = parse_u32(tokens.next().ok_or(Error::Truncated)?)?;
	let height = parse_u32(tokens.next().ok_or(Error::Truncated)?)?;
	let maximum = parse_u32(tokens.next().ok_or(Error::Truncated)?)?;
	if width == 0 || height == 0 {
		return Err(Error::Invalid);
	}
	if width > pix::MAX_DIMENSION || height > pix::MAX_DIMENSION || width as u64 * height as u64 > pix::MAX_PIXELS {
		return Err(Error::TooLarge);
	}
	if maximum == 0 || maximum > 65_535 {
		return Err(Error::Unsupported);
	}
	let samples = usize::try_from(width).ok().and_then(|width| width.checked_mul(height as usize)).and_then(|pixels| pixels.checked_mul(3)).ok_or(Error::TooLarge)?;
	let mut pixels = Vec::new();
	pixels.try_reserve_exact(samples / 3 * 4).map_err(|_| Error::TooLarge)?;
	if ascii {
		for _ in 0..samples / 3 {
			for _ in 0..3 {
				let value = parse_u32(tokens.next().ok_or(Error::Truncated)?)?;
				if value > maximum {
					return Err(Error::Invalid);
				}
				pixels.push(scale(value, maximum));
			}
			pixels.push(255);
		}
		if tokens.next().is_some() {
			return Err(Error::Invalid);
		}
	} else {
		let start = tokens.binary_start().ok_or(Error::Truncated)?;
		let bytes_per_sample = if maximum < 256 { 1 } else { 2 };
		let byte_len = samples.checked_mul(bytes_per_sample).ok_or(Error::TooLarge)?;
		let body = data.get(start..start + byte_len).ok_or(Error::Truncated)?;
		if start + byte_len != data.len() {
			return Err(Error::Invalid);
		}
		for pixel in 0..samples / 3 {
			for channel in 0..3 {
				let index = (pixel * 3 + channel) * bytes_per_sample;
				let value = if bytes_per_sample == 1 { body[index] as u32 } else { u16::from_be_bytes([body[index], body[index + 1]]) as u32 };
				if value > maximum {
					return Err(Error::Invalid);
				}
				pixels.push(scale(value, maximum));
			}
			pixels.push(255);
		}
	}
	pix::RgbaImage::new(width, height, pixels).map_err(map_pix)
}

pub fn encode(image: &pix::RgbaImage) -> Result<Vec<u8>, Error> {
	if image.pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
		return Err(Error::Unsupported);
	}
	let mut output = Vec::new();
	output.extend_from_slice(b"P6\n");
	push_u32(&mut output, image.width);
	output.push(b' ');
	push_u32(&mut output, image.height);
	output.extend_from_slice(b"\n255\n");
	let length = usize::try_from(image.pixel_count()).ok().and_then(|pixels| pixels.checked_mul(3)).ok_or(Error::TooLarge)?;
	output.try_reserve(length).map_err(|_| Error::TooLarge)?;
	for pixel in image.pixels.chunks_exact(4) {
		output.extend_from_slice(&pixel[..3]);
	}
	Ok(output)
}

struct Tokens<'a> {
	data: &'a [u8],
	position: usize,
}

impl<'a> Tokens<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, position: 0 }
	}

	fn skip(&mut self) {
		loop {
			while self.data.get(self.position).is_some_and(|byte| byte.is_ascii_whitespace()) {
				self.position += 1;
			}
			if self.data.get(self.position) != Some(&b'#') {
				break;
			}
			while self.data.get(self.position).is_some_and(|byte| *byte != b'\n') {
				self.position += 1;
			}
		}
	}

	fn next(&mut self) -> Option<&'a [u8]> {
		self.skip();
		let start = self.position;
		while self.data.get(self.position).is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'#') {
			self.position += 1;
		}
		(start != self.position).then_some(&self.data[start..self.position])
	}

	fn binary_start(&mut self) -> Option<usize> {
		let byte = *self.data.get(self.position)?;
		if !byte.is_ascii_whitespace() {
			return None;
		}
		self.position += 1;
		Some(self.position)
	}
}

fn parse_u32(token: &[u8]) -> Result<u32, Error> {
	if token.is_empty() {
		return Err(Error::Invalid);
	}
	token.iter().try_fold(0u32, |value, byte| {
		if !byte.is_ascii_digit() {
			return Err(Error::Invalid);
		}
		value.checked_mul(10).and_then(|value| value.checked_add((byte - b'0') as u32)).ok_or(Error::TooLarge)
	})
}

fn scale(value: u32, maximum: u32) -> u8 {
	((value * 255 + maximum / 2) / maximum) as u8
}

fn push_u32(output: &mut Vec<u8>, mut value: u32) {
	let mut digits = [0u8; 10];
	let mut length = 0usize;
	loop {
		digits[length] = b'0' + (value % 10) as u8;
		length += 1;
		value /= 10;
		if value == 0 {
			break;
		}
	}
	for digit in digits[..length].iter().rev() {
		output.push(*digit);
	}
}

fn map_pix(error: pix::Error) -> Error {
	match error {
		pix::Error::Invalid => Error::Invalid,
		pix::Error::TooLarge => Error::TooLarge,
	}
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests {
	use super::*;
	use alloc::vec;

	fn fnv1a(bytes: &[u8]) -> u64 {
		bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
	}

	#[test]
	fn decodes_p3_comments_and_p6_then_round_trips() {
		let p3 = b"P3\n# palette\n2 1\n15\n15 0 0 0 15 7\n";
		let decoded = decode(p3).unwrap();
		assert_eq!(decoded.pixels, vec![255, 0, 0, 255, 0, 255, 119, 255]);
		assert_eq!(decode(&encode(&decoded).unwrap()).unwrap(), decoded);
	}

	#[test]
	fn rejects_truncation_geometry_and_alpha_loss() {
		assert_eq!(decode(b"P6 1 1 255\n\x00"), Err(Error::Truncated));
		assert_eq!(decode(b"P6 20000 1 255\n"), Err(Error::TooLarge));
		assert_eq!(encode(&pix::RgbaImage::new(1, 1, vec![1, 2, 3, 4]).unwrap()), Err(Error::Unsupported));
	}

	#[test]
	fn decodes_external_netpbm_p3_comments_and_sixteen_bit_p6() {
		let p3 = include_bytes!("../tests/data/external-p3-max31.ppm");
		assert!(p3.starts_with(b"P3\n# Netpbm 11.10.2 external P3\n"));
		let decoded = decode(p3).unwrap();
		assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (13, 5, 0xbaa7_ce58_2420_6a93));

		let p6 = include_bytes!("../tests/data/external-p6-max65535.ppm");
		assert!(p6.starts_with(b"P6\n13 5\n65535\n"));
		let decoded = decode(p6).unwrap();
		assert_eq!((decoded.width, decoded.height, fnv1a(&decoded.pixels)), (13, 5, 0x571d_baab_58b1_75f0));
	}
}
