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
