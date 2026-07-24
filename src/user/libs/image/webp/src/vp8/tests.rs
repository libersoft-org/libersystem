use super::*;
use alloc::{vec, vec::Vec};

fn webp_from_frame(frame: &[u8]) -> Vec<u8> {
	let mut output = Vec::new();
	output.extend_from_slice(b"RIFF");
	let padded = frame.len() + (frame.len() & 1);
	output.extend_from_slice(&u32::try_from(12 + padded).unwrap().to_le_bytes());
	output.extend_from_slice(b"WEBPVP8 ");
	output.extend_from_slice(&u32::try_from(frame.len()).unwrap().to_le_bytes());
	output.extend_from_slice(frame);
	if frame.len() & 1 != 0 {
		output.push(0);
	}
	output
}

#[test]
fn keyframes_decode_at_odd_dimensions_and_quality_endpoints() {
	let mut pixels = Vec::new();
	for y in 0..17u8 {
		for x in 0..19u8 {
			pixels.extend_from_slice(&[x.wrapping_mul(11), y.wrapping_mul(13), x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7)), 255]);
		}
	}
	let image = pix::RgbaImage::new(19, 17, pixels).unwrap();
	let low = webp_from_frame(&encode_keyframe(&image, 0, 0).unwrap());
	let high = webp_from_frame(&encode_keyframe(&image, 100, 100).unwrap());
	assert_ne!(low, high);
	for encoded in [low, high] {
		let decoded = crate::decode(&encoded).unwrap();
		assert_eq!((decoded.width, decoded.height), (19, 17));
		assert!(decoded.pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
		assert!(decoded.pixels.iter().skip(3).step_by(4).all(|alpha| *alpha == 255));
	}
	assert_eq!(encode_keyframe(&pix::RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap(), 101, 100), Err(crate::Error::Unsupported));
}
