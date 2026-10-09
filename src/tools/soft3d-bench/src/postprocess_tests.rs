use super::*;
use render_math::{Vec3, Vec4};
fn half(value: f32) -> f32 {
	graphics_core::pixel::half_to_f32(graphics_core::pixel::f32_to_half(value))
}
fn rounded(value: Vec3) -> Vec3 {
	Vec3::new(half(value.x), half(value.y), half(value.z))
}

#[test]
fn hdr_chain_executes_all_passes_and_matches_the_frozen_constant_field() {
	assert!(Postprocess::new(64, 48, false).is_none());
	let mut chain = Postprocess::new(64, 64, true).expect("the minimum six-level chain");
	assert!(chain.bytes() > 64 * 64 * 8);
	let before_execution = chain.bytes();
	assert_eq!(chain.passes.len(), 12);
	let radiance = Vec3::new(4.0, 2.0, 1.0);
	let mut scene = Colour::try_new(64, 64, 1, false).unwrap();
	scene.fill(Vec4::new(radiance.x, radiance.y, radiance.z, 1.0));
	assert_eq!(chain.execute(&mut scene, &soft3d::frame::Serial).unwrap(), (12, 0));
	assert!(chain.bytes() > before_execution, "the report includes scratch reserved by the first execution");
	let down = rounded(postprocess::bloom_prefilter(radiance));
	let mut up = down;
	for _ in 0..5 {
		up = rounded(down.add(up));
	}
	let expected = postprocess::resolve(radiance, up, postprocess::BLOOM_WEIGHT);
	for y in 0..64 {
		for x in 0..64 {
			let actual = scene.at(x, y, 0);
			assert!((actual.x - expected.x).abs() < 0.0001 && (actual.y - expected.y).abs() < 0.0001 && (actual.z - expected.z).abs() < 0.0001, "({x},{y}): {actual:?} vs {expected:?}");
		}
	}
	scene.fill(Vec4::new(radiance.x, radiance.y, radiance.z, 1.0));
	assert_eq!(
		crate::allocation_test::count(|| {
			chain.execute(&mut scene, &soft3d::frame::Serial).unwrap();
		}),
		0,
		"steady-state HDR execution allocates nothing"
	);
}
