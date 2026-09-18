//! THE SAMPLING GROUP: where a texture coordinate lands, what is read there, and how a coordinate
//! outside the image is brought back in.
//!
//! EVERY SCENE HERE READS THE SAMPLER DIRECTLY rather than through a rendered frame, and that is
//! deliberate: what a filter or a wrap decides is one texel's value, and putting a rasteriser between
//! the decision and the check would mean a failure could be the interpolator's. The scenes that ARE
//! about the path from a coordinate to a pixel live in the pipeline group.

use crate::Outcome;
use crate::harness::close;
use alloc::vec;
use graphics_profile::image::{Semantics, Transfer};
use soft3d::texture::{Border, Filter, Kind, Level, Sampler, Texture, Wrap};

/// A two-by-two image whose four texels are four different values, so a filter that reads the wrong
/// one is visible and a filter that averages is a fifth value none of them has.
fn quad_texture() -> Texture {
	let mut level = Level::new(2, 2, 1);
	level.set(0, 0, 0, [0.0, 0.0, 0.0, 1.0]);
	level.set(1, 0, 0, [1.0, 0.0, 0.0, 1.0]);
	level.set(0, 1, 0, [0.0, 1.0, 0.0, 1.0]);
	level.set(1, 1, 0, [0.0, 0.0, 1.0, 1.0]);
	Texture { id: 1, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false }
}

/// A one-texel image of a stated colour, for the scenes about which LEVEL is read.
fn flat_level(extent: u32, value: f32) -> Level {
	let mut level = Level::new(extent, extent, 1);
	for y in 0..extent {
		for x in 0..extent {
			level.set(x, y, 0, [value, value, value, 1.0]);
		}
	}
	level
}

/// A texture whose levels are told apart by their value, so which level a read used is readable off
/// the answer.
fn levelled() -> Texture {
	Texture { id: 2, kind: Kind::Dim2, levels: vec![flat_level(4, 0.0), flat_level(2, 0.5), flat_level(1, 1.0)], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false }
}

fn plain(minify: Filter, magnify: Filter, mip: Filter) -> Sampler {
	Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, wrap_w: Wrap::ClampToEdge, minify, magnify, mip, ..Sampler::NEAREST }
}

pub fn min_nearest() -> Outcome {
	// MINIFICATION IS THE FILTER USED WHEN A TEXEL IS SMALLER THAN A PIXEL, and `magnifying` is what
	// selects between the two. Nearest picks ONE texel, which here is a value none of the others
	// share.
	let texture = quad_texture();
	let sampler = plain(Filter::Nearest, Filter::Linear, Filter::Nearest);
	let texel = soft3d::texture::sample(&texture, &sampler, [0.25, 0.25, 0.0], 0.0, false);
	require!(close(texel[0], 0.0) && close(texel[1], 0.0) && close(texel[2], 0.0), "minifying with nearest reads the texel the coordinate is in, which is the black one: {texel:?}");
	Ok(())
}

pub fn min_linear() -> Outcome {
	// LINEAR MINIFICATION AVERAGES THE FOUR TEXELS AROUND THE COORDINATE, so a point at the centre of
	// a two-by-two image is a quarter of each - a value no single texel has.
	let texture = quad_texture();
	let sampler = plain(Filter::Linear, Filter::Nearest, Filter::Nearest);
	let texel = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, false);
	require!(close(texel[0], 0.25) && close(texel[1], 0.25) && close(texel[2], 0.25), "the centre of four texels is a quarter of each: {texel:?}");
	Ok(())
}

pub fn mag_nearest() -> Outcome {
	// AND THE MAGNIFYING FILTER IS A SEPARATE FIELD, which is what a backend that used one filter for
	// both would get wrong: the same coordinate under the same sampler answers differently depending
	// on which side of the texel-to-pixel ratio the read is on.
	let texture = quad_texture();
	let sampler = plain(Filter::Linear, Filter::Nearest, Filter::Nearest);
	let texel = soft3d::texture::sample(&texture, &sampler, [0.75, 0.25, 0.0], 0.0, true);
	require!(close(texel[0], 1.0) && close(texel[1], 0.0), "magnifying with nearest reads the red texel: {texel:?}");
	Ok(())
}

pub fn mag_linear() -> Outcome {
	let texture = quad_texture();
	let sampler = plain(Filter::Nearest, Filter::Linear, Filter::Nearest);
	let texel = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true);
	require!(close(texel[0], 0.25) && close(texel[1], 0.25) && close(texel[2], 0.25), "magnifying with linear averages the four: {texel:?}");
	Ok(())
}

pub fn mip_nearest() -> Outcome {
	// NEAREST MIP FILTERING PICKS ONE LEVEL AND READS IT, so a level of 1.4 reads level one exactly
	// and a level of 1.6 reads level two exactly. A backend that blended would answer something in
	// between, which is what `MipLinear` is for.
	let texture = levelled();
	let sampler = plain(Filter::Nearest, Filter::Nearest, Filter::Nearest);
	let near = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 1.4, false);
	require!(close(near[0], 0.5), "a level of 1.4 under nearest mip filtering reads level one, whose value is 0.5: {near:?}");
	let far = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 1.6, false);
	require!(close(far[0], 1.0), "and 1.6 reads level two, whose value is 1.0: {far:?}");

	// AND AN EXPLICIT LEVEL THROUGH A DRAW READS THAT LEVEL, which is what a shader asking for one by
	// number does: the rasteriser's own derivatives are OVERRIDDEN rather than mixed with it.
	let drawn = crate::harness::drawn_texel(levelled(), sampler, crate::harness::Sampling::Level(1), [0.5, 0.5, 0.0])?;
	require!(close(drawn.x, 0.5), "a draw that names level one reads level one: {drawn:?}");
	let drawn = crate::harness::drawn_texel(levelled(), sampler, crate::harness::Sampling::Level(2), [0.5, 0.5, 0.0])?;
	require!(close(drawn.x, 1.0), "and one that names level two reads level two: {drawn:?}");
	Ok(())
}

pub fn mip_linear() -> Outcome {
	// LINEAR MIP FILTERING BLENDS THE TWO LEVELS BY THE FRACTION, which is a value neither level has.
	let texture = levelled();
	let sampler = plain(Filter::Nearest, Filter::Nearest, Filter::Linear);
	let texel = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 1.5, false);
	require!(close(texel[0], 0.75), "half way between a level of 0.5 and one of 1.0 is 0.75: {texel:?}");
	Ok(())
}

pub fn trilinear() -> Outcome {
	// TRILINEAR IS LINEAR IN BOTH DIRECTIONS AT ONCE: within each level and between the two. This is
	// the combination, and the value it produces is one neither of the other two filters can.
	let mut coarse = Level::new(2, 2, 1);
	coarse.set(0, 0, 0, [1.0, 1.0, 1.0, 1.0]);
	coarse.set(1, 0, 0, [1.0, 1.0, 1.0, 1.0]);
	coarse.set(0, 1, 0, [1.0, 1.0, 1.0, 1.0]);
	coarse.set(1, 1, 0, [1.0, 1.0, 1.0, 1.0]);
	let texture = Texture { id: 3, kind: Kind::Dim2, levels: vec![quad_texture().levels.remove(0), coarse], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false };
	let sampler = plain(Filter::Linear, Filter::Linear, Filter::Linear);
	// Level zero at the centre is a quarter of each; level one is white; half way between is the mean.
	let texel = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.5, false);
	require!(close(texel[0], 0.625), "trilinear at half a level between 0.25 and 1.0 is 0.625: {texel:?}");
	Ok(())
}

/// A four-texel row, for the wrap scenes: what is read outside `0..1` is what the rule says.
fn row_texture() -> Texture {
	let mut level = Level::new(4, 1, 1);
	for x in 0..4 {
		let value = x as f32 / 3.0;
		level.set(x, 0, 0, [value, value, value, 1.0]);
	}
	Texture { id: 4, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false }
}

fn wrapped(wrap: Wrap) -> Sampler {
	Sampler { wrap_u: wrap, wrap_v: wrap, wrap_w: wrap, minify: Filter::Nearest, magnify: Filter::Nearest, mip: Filter::Nearest, ..Sampler::NEAREST }
}

pub fn sampler_wrap_clamp() -> Outcome {
	// CLAMP READS THE EDGE TEXEL FOR EVERYTHING OUTSIDE, which is what stops a stretched image
	// wrapping round to its other side.
	let texture = row_texture();
	let sampler = wrapped(Wrap::ClampToEdge);
	let left = soft3d::texture::sample(&texture, &sampler, [-0.5, 0.5, 0.0], 0.0, true);
	require!(close(left[0], 0.0), "a coordinate left of the image reads the first texel: {left:?}");
	let right = soft3d::texture::sample(&texture, &sampler, [1.5, 0.5, 0.0], 0.0, true);
	require!(close(right[0], 1.0), "and one right of it reads the last: {right:?}");
	Ok(())
}

pub fn sampler_wrap_repeat() -> Outcome {
	// REPEAT IS THE EUCLIDEAN REMAINDER, which is the rule that makes `-0.25` and `0.75` the SAME
	// texel. The language's `%` gives `-0.25` for the first, which lands somewhere else - and the
	// difference only shows where a coordinate goes negative, which is exactly where nobody looks.
	let texture = row_texture();
	let sampler = wrapped(Wrap::Repeat);
	let inside = soft3d::texture::sample(&texture, &sampler, [0.75, 0.5, 0.0], 0.0, true);
	let outside = soft3d::texture::sample(&texture, &sampler, [-0.25, 0.5, 0.0], 0.0, true);
	require!(close(inside[0], outside[0]), "-0.25 and 0.75 are the same texel under repeat: {inside:?} against {outside:?}");
	let far = soft3d::texture::sample(&texture, &sampler, [2.75, 0.5, 0.0], 0.0, true);
	require!(close(far[0], inside[0]), "and so is 2.75: {far:?}");
	Ok(())
}

pub fn sampler_wrap_mirror() -> Outcome {
	// MIRROR REFLECTS EVERY OTHER PERIOD, which is what makes a mirrored tile seamless: the sequence
	// of texels read as the coordinate goes past one is the REVERSE of the sequence below it, so a
	// read the same distance either side of the seam reads the same texel.
	//
	// THE PROPERTY AND NOT ONE SPELLING OF IT. A mirror can be written as a fold of the COORDINATE or
	// as a fold of the texel INDEX, and the two differ by half a texel at the seam - which is a
	// difference this scene must not depend on, because the profile freezes neither. A reflection is
	// a reflection under both.
	let texture = row_texture();
	let sampler = wrapped(Wrap::MirroredRepeat);
	for distance in [0.05, 0.3, 0.45] {
		let below = soft3d::texture::sample(&texture, &sampler, [1.0 - distance, 0.5, 0.0], 0.0, true);
		let above = soft3d::texture::sample(&texture, &sampler, [1.0 + distance, 0.5, 0.0], 0.0, true);
		require!(close(below[0], above[0]), "a read {distance} either side of the seam reads one texel: {below:?} against {above:?}");
	}
	// AND IT IS NOT REPEAT, which is the other half of the claim: under repeat the same two reads land
	// on opposite ends of the image.
	let repeat = wrapped(Wrap::Repeat);
	let below = soft3d::texture::sample(&texture, &repeat, [0.7, 0.5, 0.0], 0.0, true);
	let above = soft3d::texture::sample(&texture, &repeat, [1.3, 0.5, 0.0], 0.0, true);
	require!(!close(below[0], above[0]), "under repeat the two are different texels, which is what mirroring changes");
	Ok(())
}

pub fn sampler_wrap_border() -> Outcome {
	// BORDER READS A COLOUR THAT IS NOT IN THE IMAGE AT ALL, which is what makes a clamped-to-border
	// read distinguishable from a clamped-to-edge one.
	let texture = row_texture();
	let sampler = Sampler { border: Border::OpaqueWhite, ..wrapped(Wrap::ClampToBorder) };
	let outside = soft3d::texture::sample(&texture, &sampler, [-0.5, 0.5, 0.0], 0.0, true);
	require!(close(outside[0], 1.0) && close(outside[3], 1.0), "outside a border-wrapped image is the border colour: {outside:?}");
	let inside = soft3d::texture::sample(&texture, &sampler, [0.125, 0.5, 0.0], 0.0, true);
	require!(close(inside[0], 0.0), "and inside it is the image: {inside:?}");
	Ok(())
}

pub fn lod_bias() -> Outcome {
	// A BIAS IS ADDED TO THE COMPUTED LEVEL, which is what lets an application sharpen or soften a
	// whole material without touching its coordinates.
	//
	// IT BELONGS TO THE LEVEL CALCULATION AND NOT TO THE FETCH, which is where a scene can get this
	// wrong: `sample` takes a level that has already been decided, so a bias passed to it would be a
	// bias nobody applies. `lambda` is where the derivatives, the bias and the clamps meet.
	let texture = levelled();
	let unbiased = plain(Filter::Nearest, Filter::Nearest, Filter::Nearest);
	let biased = Sampler { lod_bias: 1.0, ..unbiased };
	// A footprint of one texel across, which is level zero before any bias.
	let (ddx, ddy) = ([1.0 / 4.0, 0.0], [0.0, 1.0 / 4.0]);
	let plain_level = soft3d::texture::lambda(ddx, ddy, 4, 4, &unbiased, 3);
	let biased_level = soft3d::texture::lambda(ddx, ddy, 4, 4, &biased, 3);
	require!(close(plain_level, 0.0), "a footprint of one texel is level zero, and is {plain_level}");
	require!(close(biased_level, 1.0), "and a bias of one makes it level one, which is {biased_level}");
	let texel = soft3d::texture::sample(&texture, &biased, [0.5, 0.5, 0.0], biased_level, false);
	require!(close(texel[0], 0.5), "which reads level one, whose value is 0.5: {texel:?}");
	Ok(())
}

pub fn lod_clamp() -> Outcome {
	// AND THE CLAMPS BOUND IT AFTERWARDS, which is what stops a bias walking off the end of the
	// chain: a maximum of zero pins every read to the sharpest level however it was computed, and a
	// minimum pins it to the coarsest.
	let texture = levelled();
	let (ddx, ddy) = ([1.0 / 4.0, 0.0], [0.0, 1.0 / 4.0]);
	let pinned_sharp = Sampler { lod_bias: 5.0, max_lod: 0.0, ..plain(Filter::Nearest, Filter::Nearest, Filter::Nearest) };
	let level = soft3d::texture::lambda(ddx, ddy, 4, 4, &pinned_sharp, 3);
	require!(close(level, 0.0), "a maximum of zero pins a biased level to zero, and it is {level}");
	let texel = soft3d::texture::sample(&texture, &pinned_sharp, [0.5, 0.5, 0.0], level, false);
	require!(close(texel[0], 0.0), "which reads level zero, whose value is 0.0: {texel:?}");

	let pinned_coarse = Sampler { min_lod: 2.0, ..plain(Filter::Nearest, Filter::Nearest, Filter::Nearest) };
	let level = soft3d::texture::lambda(ddx, ddy, 4, 4, &pinned_coarse, 3);
	require!(close(level, 2.0), "and a minimum of two pins it to two, which is {level}");
	let texel = soft3d::texture::sample(&texture, &pinned_coarse, [0.5, 0.5, 0.0], level, false);
	require!(close(texel[0], 1.0), "which reads level two, whose value is 1.0: {texel:?}");
	Ok(())
}

pub fn depth_compare_sampling() -> Outcome {
	// A DEPTH-COMPARE READ ANSWERS A COVERAGE AND NOT A COLOUR: the reference is compared against the
	// stored depth and what comes back is how much of the read passed. That is what a shadow map is,
	// and a backend that answered the depth instead would produce a shadow that is a picture of a
	// depth buffer.
	let mut level = Level::new(2, 1, 1);
	level.set(0, 0, 0, [0.25, 0.0, 0.0, 1.0]);
	level.set(1, 0, 0, [0.75, 0.0, 0.0, 1.0]);
	let texture = Texture { id: 5, kind: Kind::Dim2, levels: vec![level], transfer: Transfer::Linear, semantics: Semantics::Depth, premultiplied: false };
	let sampler = Sampler { compare: Some(render3d::CompareOp::Less), ..wrapped(Wrap::ClampToEdge) };
	let nearer = soft3d::texture::sample_compare(&texture, &sampler, [0.25, 0.5], 0, 0.1).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a depth-compare read was refused: {error:?}")))?;
	require!(close(nearer, 1.0), "a reference nearer than the stored depth passes: {nearer}");
	let farther = soft3d::texture::sample_compare(&texture, &sampler, [0.25, 0.5], 0, 0.5).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a depth-compare read was refused: {error:?}")))?;
	require!(close(farther, 0.0), "and one farther than it does not: {farther}");

	// AND THE SAME READ FROM INSIDE A PASS ANSWERS THE SAME COVERAGE, which is where a shadow term is
	// computed: the third coordinate is the reference, and what the fragment gets back is how much of
	// the read passed.
	let drawn = crate::harness::drawn_texel(texture.clone(), sampler, crate::harness::Sampling::Compare(0), [0.25, 0.5, 0.1])?;
	require!(close(drawn.x, 1.0), "a nearer reference through a draw passes: {drawn:?}");
	let drawn = crate::harness::drawn_texel(texture, sampler, crate::harness::Sampling::Compare(0), [0.25, 0.5, 0.5])?;
	require!(close(drawn.x, 0.0), "and a farther one does not: {drawn:?}");
	Ok(())
}

pub fn anisotropy8() -> Outcome {
	// ANISOTROPY IS FOR THE GRAZING CASE and is defined against the isotropic answer: a footprint
	// four texels wide and one tall would take a level chosen by the LONG axis under an isotropic
	// rule, which is a blur. An anisotropic read takes several samples along the long axis at a
	// SHARPER level, so the answer keeps detail the isotropic one loses.
	let texture = levelled();
	let sampler = Sampler { anisotropy: 8, ..plain(Filter::Linear, Filter::Linear, Filter::Linear) };
	let isotropic = Sampler { anisotropy: 1, ..sampler };
	let (wide, tall) = ([4.0 / 4.0, 0.0], [0.0, 1.0 / 4.0]);
	let sharp = soft3d::texture::sample_anisotropic(&texture, &sampler, [0.5, 0.5, 0.0], wide, tall, 4, 4);
	let blurred = soft3d::texture::sample_anisotropic(&texture, &isotropic, [0.5, 0.5, 0.0], wide, tall, 4, 4);
	require!(sharp[0] < blurred[0], "an anisotropic read of a grazing footprint stays on a sharper level than an isotropic one: {sharp:?} against {blurred:?}");
	// AND THE LIMIT IS RESPECTED rather than ignored: one is the isotropic rule, which is what makes
	// the comparison above a comparison at all.
	require!(close(blurred[0], soft3d::texture::sample(&texture, &isotropic, [0.5, 0.5, 0.0], soft3d::texture::lambda(wide, tall, 4, 4, &isotropic, 3), false)[0]), "an anisotropy of one is the isotropic rule");

	// AND THE ANISOTROPIC ENTRY POINT IS REACHED FROM INSIDE A PASS, with the same grazing footprint -
	// which is the only place an application ever uses it, because the derivatives come from the
	// rasteriser.
	let drawn = crate::harness::drawn_texel(levelled(), sampler, crate::harness::Sampling::Anisotropic, [0.5, 0.5, 0.0])?;
	require!(close(drawn.x, sharp[0]), "an anisotropic read through a draw answers what the entry point does: {drawn:?} against {sharp:?}");
	Ok(())
}
