use super::*;
use alloc::string::String;

#[test]
fn extent_2d_wire_is_stable() {
	let sample = Extent2d { width: 7, height: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Extent2d::decode(&bytes).unwrap(), sample);
}
#[test]
fn offset_2d_wire_is_stable() {
	let sample = Offset2d { x: 7, y: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Offset2d::decode(&bytes).unwrap(), sample);
}
#[test]
fn rect_wire_is_stable() {
	let sample = Rect { origin: Offset2d { x: 7, y: 7 }, size: Extent2d { width: 7, height: 7 } };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(Rect::decode(&bytes).unwrap(), sample);
}
#[test]
fn pixel_format_wire_is_stable() {
	let sample = PixelFormat::A8Unorm;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(PixelFormat::decode(&bytes).unwrap(), sample);
}
#[test]
fn alpha_mode_wire_is_stable() {
	let sample = AlphaMode::Opaque;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(AlphaMode::decode(&bytes).unwrap(), sample);
}
#[test]
fn color_space_wire_is_stable() {
	let sample = ColorSpace::Srgb;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(ColorSpace::decode(&bytes).unwrap(), sample);
}
#[test]
fn row_origin_wire_is_stable() {
	let sample = RowOrigin::TopLeft;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[0];
	assert_eq!(bytes, golden);
	assert_eq!(RowOrigin::decode(&bytes).unwrap(), sample);
}
#[test]
fn image_layout_wire_is_stable() {
	let sample = ImageLayout { size: Extent2d { width: 7, height: 7 }, pitch: 7, format: PixelFormat::A8Unorm, alpha: AlphaMode::Opaque, color_space: ColorSpace::Srgb, origin: RowOrigin::TopLeft };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(ImageLayout::decode(&bytes).unwrap(), sample);
}
#[test]
fn damage_region_wire_is_stable() {
	let sample = DamageRegion { whole: true, rects: alloc::vec![Rect { origin: Offset2d { x: 7, y: 7 }, size: Extent2d { width: 7, height: 7 } }] };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(DamageRegion::decode(&bytes).unwrap(), sample);
}
