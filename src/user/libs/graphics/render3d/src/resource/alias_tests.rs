use super::*;
use crate::{CommandList, Render3DLimits};

fn target(texture: u32, samples: u32, layer: u32) -> RenderTargetView {
	RenderTargetView { texture, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: layer, layer_count: 1 }, format: "RGBA8", samples, width: 1, height: 1, load: LoadOp::Clear, store: StoreOp::Store }
}

fn validate(colour: &[RenderTargetView], resolve: &[Option<RenderTargetView>]) -> Result<(), Error> {
	RenderTargetSet { colour, depth_stencil: None, resolve }.validate(&Render3DLimits::PROFILE_MINIMUM)
}

#[test]
fn overlapping_writable_views_are_refused_but_disjoint_subresources_are_legal() {
	let first = target(7, 1, 0);
	assert!(matches!(validate(&[first, first], &[]), Err(Error::TargetMismatch { .. })));
	let range = RenderTargetView { view: TextureViewDesc { layer_count: 2, ..first.view }, ..first };
	assert!(validate(&[range, target(7, 1, 1)], &[]).is_err());
	assert!(validate(&[target(7, 1, 1), range], &[]).is_err());
	assert_eq!(validate(&[first, target(7, 1, 1)], &[]), Ok(()));
	assert_eq!(validate(&[first, RenderTargetView { view: TextureViewDesc { base_mip: 1, ..first.view }, ..first }], &[]), Ok(()));
	assert_eq!(validate(&[first, target(8, 1, 0)], &[]), Ok(()));
}

#[test]
fn resolve_destinations_must_not_alias_any_source_or_each_other() {
	let colours = [target(1, 4, 0), target(2, 4, 0)];
	assert!(validate(&colours, &[Some(target(1, 1, 0)), None]).is_err());
	assert!(validate(&colours, &[Some(target(2, 1, 0)), None]).is_err());
	assert!(validate(&colours, &[Some(target(3, 1, 0)), Some(target(3, 1, 0))]).is_err());
	assert_eq!(validate(&colours, &[Some(target(3, 1, 0)), Some(target(3, 1, 1))]), Ok(()));
	assert_eq!(validate(&colours, &[Some(target(3, 1, 0)), Some(target(4, 1, 0))]), Ok(()));
}

#[test]
fn sampling_a_resolve_destination_is_the_same_per_pass_write_hazard() {
	let colour = [target(1, 4, 0)];
	let resolve = [Some(target(2, 1, 0))];
	let targets = RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &resolve };
	let view = target(2, 1, 0).view;
	assert!(matches!(refuse_sampled_attachment(&[(2, view)], &targets), Err(Error::TargetMismatch { .. })));
	assert_eq!(refuse_sampled_attachment(&[(2, TextureViewDesc { base_mip: 1, ..view })], &targets), Ok(()));
}

#[test]
fn plane_overlap_is_symmetric_and_range_arithmetic_does_not_wrap() {
	let view = target(1, 1, 0).view;
	for (left, right, overlaps) in [
		(Aspect::Colour, Aspect::DepthAndStencil, false),
		(Aspect::Depth, Aspect::Stencil, false),
		(Aspect::Depth, Aspect::DepthAndStencil, true),
		(Aspect::Stencil, Aspect::DepthAndStencil, true),
	] {
		let left = TextureViewDesc { aspect: left, ..view };
		let right = TextureViewDesc { aspect: right, ..view };
		assert_eq!(left.overlaps(&right), overlaps);
		assert_eq!(right.overlaps(&left), overlaps);
	}
	let high = TextureViewDesc { base_mip: u32::MAX, mip_count: 2, base_layer: u32::MAX, layer_count: 2, ..view };
	assert!(high.overlaps(&high));
	assert!(!view.overlaps(&high));
}

#[test]
fn rejected_alias_does_not_partially_publish_a_recorded_pass() {
	let mut commands = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	let duplicated = [target(1, 1, 0), target(1, 1, 0)];
	assert!(commands.begin_render_pass(1, &RenderTargetSet { colour: &duplicated, depth_stencil: None, resolve: &[] }).is_err());
	assert!(commands.commands().is_empty());
	assert!(commands.passes().is_empty());
	let valid = [target(1, 1, 0), target(1, 1, 1)];
	commands.begin_render_pass(1, &RenderTargetSet { colour: &valid, depth_stencil: None, resolve: &[] }).unwrap();
	commands.end_render_pass().unwrap();
	commands.finish().unwrap();
	assert_eq!(commands.passes().len(), 1);
}
