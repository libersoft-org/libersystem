//! THE RESOURCES GROUP: what a frame may be made of, and what a description that is not one is
//! refused for.
//!
//! A RESOURCE FEATURE IS A DESCRIPTION AND A READ, and both halves are here. The description is
//! `render3d`'s - it is what a GPU backend would create an object from - and the read is `soft3d`'s,
//! because a dimension nothing can sample is a dimension that exists only in a struct. A scene that
//! checked only the first would pass on a backend that accepted every description and implemented
//! none.

use crate::Outcome;
use crate::harness::close;
use alloc::vec;
use graphics_profile::image::{Semantics, Transfer};
use render3d::resource::{BufferDesc, BufferUsage, HostVisibility, TextureDesc, TextureDimension, TextureUsage};
use render3d::{Render3DLimits, resource};
use soft3d::texture::{Filter, Kind, Level, Sampler, Texture, Wrap};

fn limits() -> Render3DLimits {
	Render3DLimits::PROFILE_MINIMUM
}

/// A sampler with nothing surprising in it, for the scenes that are about the texture rather than
/// about the read.
fn plain_sampler() -> Sampler {
	Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, wrap_w: Wrap::ClampToEdge, minify: Filter::Nearest, magnify: Filter::Nearest, mip: Filter::Nearest, ..Sampler::NEAREST }
}

/// One level of a stated extent and depth, with every texel carrying its own position so a read that
/// lands in the wrong place is visible in the value.
fn positional(width: u32, height: u32, depth: u32) -> Level {
	let mut level = Level::new(width, height, depth);
	for z in 0..depth {
		for y in 0..height {
			for x in 0..width {
				level.set(x, y, z, [x as f32 / 8.0, y as f32 / 8.0, z as f32 / 8.0, 1.0]);
			}
		}
	}
	level
}

fn texture_of(kind: Kind, levels: alloc::vec::Vec<Level>) -> Texture {
	Texture { id: 1, kind, levels, transfer: Transfer::Linear, semantics: Semantics::Color, premultiplied: false }
}

pub fn buffer() -> Outcome {
	// A BUFFER IS A SIZE, A USE AND A VISIBILITY, and the description is where a mistake is refused:
	// a zero-sized buffer is an out-of-range access at every later use, and creating it successfully
	// moves the refusal somewhere with less context.
	let good = BufferDesc { size: 1024, usage: BufferUsage { vertex: true, ..BufferUsage::default() }, host_visibility: HostVisibility::Upload };
	good.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an ordinary vertex buffer was refused: {error:?}")))?;

	let empty = BufferDesc { size: 0, ..good };
	require!(empty.validate(&limits()).is_err(), "a zero-sized buffer is refused rather than created");

	// AND A BUFFER WITH NO USE AT ALL is a buffer nothing may do anything with.
	let useless = BufferDesc { usage: BufferUsage::default(), ..good };
	require!(useless.validate(&limits()).is_err(), "a buffer declared for no use is refused");
	Ok(())
}

pub fn texture_2d() -> Outcome {
	let desc = TextureDesc { dimension: TextureDimension::D2, width: 4, height: 4, depth: 1, mip_levels: 1, layers: 1, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } };
	desc.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a plain 2D texture was refused: {error:?}")))?;
	require!(TextureDesc { width: 0, ..desc }.validate(&limits()).is_err(), "and one with a zero extent has no texels");

	// AND IT IS READ AS ONE. The texel at a stated coordinate is the one the position says.
	let texture = texture_of(Kind::Dim2, vec![positional(4, 4, 1)]);
	let texel = soft3d::texture::sample(&texture, &plain_sampler(), [0.625, 0.125, 0.0], 0.0, true);
	require!(close(texel[0], 2.0 / 8.0) && close(texel[1], 0.0), "the read lands on the texel the coordinate names: {texel:?}");
	Ok(())
}

pub fn texture_cube() -> Outcome {
	// A CUBE IS SIX SQUARE FACES AND THE COUNT IS CHECKED. A description with five is not a cube map
	// with one missing; it is a description of nothing, and a backend that accepted it would index
	// past its own storage on the sixth face.
	let desc = TextureDesc { dimension: TextureDimension::Cube, width: 4, height: 4, depth: 1, mip_levels: 1, layers: 6, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } };
	desc.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a cube map was refused: {error:?}")))?;
	require!(TextureDesc { layers: 5, ..desc }.validate(&limits()).is_err(), "five layers is not a cube map");
	require!(TextureDesc { height: 8, ..desc }.validate(&limits()).is_err(), "and a cube map's faces are square");

	// AND THE FACE IS SELECTED BY THE MAJOR AXIS OF A DIRECTION, which is what makes a cube a cube
	// rather than an array somebody indexes by hand: `+x` and `-x` are two different faces of one
	// texture, reached by two directions and no index.
	let (positive, _) = soft3d::texture::cube_face([1.0, 0.0, 0.0]);
	let (negative, _) = soft3d::texture::cube_face([-1.0, 0.0, 0.0]);
	require!(positive != negative, "opposite directions reach different faces, and both answered {positive}");
	let (vertical, _) = soft3d::texture::cube_face([0.0, 1.0, 0.0]);
	require!(vertical != positive && vertical != negative, "and a third axis is a third face");

	// AND A DIRECTION THROUGH A DRAW REACHES THAT FACE, which is what a cube map is used for: a
	// reflection hands the sampler a direction and gets a colour, with no index anywhere.
	let cube = texture_of(Kind::Cube, vec![positional(4, 4, 6)]);
	let along_x = crate::harness::drawn_texel(cube, plain_sampler(), crate::harness::Sampling::Cube, [1.0, 0.0, 0.0])?;
	let cube = texture_of(Kind::Cube, vec![positional(4, 4, 6)]);
	let along_y = crate::harness::drawn_texel(cube, plain_sampler(), crate::harness::Sampling::Cube, [0.0, 1.0, 0.0])?;
	require!(!close(along_x.z, along_y.z), "two directions through a draw read two faces: {along_x:?} against {along_y:?}");
	Ok(())
}

pub fn texture_2d_array() -> Outcome {
	// AN ARRAY IS ADDRESSED BY AN EXPLICIT INDEX AND NOT BY A FILTERED COORDINATE, which is the
	// difference between a 2D array and a 3D texture: there is no blending between layers.
	let desc = TextureDesc { dimension: TextureDimension::D2Array, width: 4, height: 4, depth: 1, mip_levels: 1, layers: 3, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } };
	desc.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a 2D array was refused: {error:?}")))?;

	let texture = texture_of(Kind::Array, vec![positional(4, 4, 3)]);
	let sampler = plain_sampler();
	let first = soft3d::texture::sample_array(&texture, &sampler, [0.5, 0.5], 0, 0.0).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a layer read was refused: {error:?}")))?;
	let third = soft3d::texture::sample_array(&texture, &sampler, [0.5, 0.5], 2, 0.0).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a layer read was refused: {error:?}")))?;
	require!(close(first[2], 0.0), "layer zero carries its own index: {first:?}");
	require!(close(third[2], 2.0 / 8.0), "and layer two carries its own: {third:?}");
	require!(soft3d::texture::sample_array(&texture, &sampler, [0.5, 0.5], 3, 0.0).is_err(), "and a layer past the end is refused rather than wrapped");

	// AND AN INDEXED LAYER READ THROUGH A DRAW ANSWERS THAT LAYER, which is what a texture atlas and a
	// material array are: one binding, one index, no blending between them.
	let drawn = crate::harness::drawn_texel(texture_of(Kind::Array, vec![positional(4, 4, 3)]), plain_sampler(), crate::harness::Sampling::Array(2), [0.5, 0.5, 0.0])?;
	require!(close(drawn.z, 2.0 / 8.0), "a draw that names layer two reads layer two: {drawn:?}");
	Ok(())
}

pub fn texture_3d() -> Outcome {
	// A 3D TEXTURE'S THIRD DIMENSION IS DEPTH AND NOT AN ARRAY, which the description says in the
	// only way that matters: a 3D texture with layers is refused.
	let desc = TextureDesc { dimension: TextureDimension::D3, width: 4, height: 4, depth: 4, mip_levels: 1, layers: 1, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } };
	desc.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a 3D texture was refused: {error:?}")))?;
	require!(TextureDesc { layers: 2, ..desc }.validate(&limits()).is_err(), "a 3D texture is not an array");
	require!(TextureDesc { depth: 0, ..desc }.validate(&limits()).is_err(), "and one with no depth has no texels");

	// AND THE THIRD COORDINATE IS FILTERED LIKE THE OTHER TWO, which is what an array's is not.
	let texture = texture_of(Kind::Dim3, vec![positional(4, 4, 4)]);
	let near = soft3d::texture::sample(&texture, &plain_sampler(), [0.5, 0.5, 0.125], 0.0, true);
	let far = soft3d::texture::sample(&texture, &plain_sampler(), [0.5, 0.5, 0.875], 0.0, true);
	require!(!close(near[2], far[2]), "two depths read two different slices: {near:?} against {far:?}");
	Ok(())
}

pub fn sampler() -> Outcome {
	// A SAMPLER IS SEPARATE FROM THE TEXTURE, which is what lets one image be read two ways in one
	// frame. The same texture and two samplers answer differently, and that is the whole feature.
	let texture = texture_of(Kind::Dim2, vec![positional(4, 4, 1)]);
	let nearest = plain_sampler();
	let linear = Sampler { minify: Filter::Linear, magnify: Filter::Linear, ..plain_sampler() };
	let sharp = soft3d::texture::sample(&texture, &nearest, [0.5, 0.5, 0.0], 0.0, true);
	let smooth = soft3d::texture::sample(&texture, &linear, [0.5, 0.5, 0.0], 0.0, true);
	require!(!close(sharp[0], smooth[0]), "one texture read through two samplers answers two values: {sharp:?} against {smooth:?}");

	// AND ITS WRAP IS PER AXIS, which is what a sampler with one wrap field could not express.
	let mixed = Sampler { wrap_u: Wrap::Repeat, wrap_v: Wrap::ClampToEdge, ..plain_sampler() };
	let wrapped_u = soft3d::texture::sample(&texture, &mixed, [1.125, 0.125, 0.0], 0.0, true);
	let clamped_v = soft3d::texture::sample(&texture, &mixed, [0.125, 1.5, 0.0], 0.0, true);
	require!(close(wrapped_u[0], 0.0), "u repeats, so 1.125 is 0.125: {wrapped_u:?}");
	require!(close(clamped_v[1], 3.0 / 8.0), "and v clamps, so 1.5 is the last row: {clamped_v:?}");

	// AND THE SAMPLER IS REACHED FROM INSIDE A PASS, which is where an application reads it. Every
	// other scene in the sampling group reads the entry point DIRECTLY, on purpose - a rasteriser
	// between the decision and the check would let a failure be the interpolator's - so this is the
	// one that says the draw path arrives at the sampler at all.
	let drawn = crate::harness::drawn_texel(texture_of(Kind::Dim2, vec![positional(4, 4, 1)]), nearest, crate::harness::Sampling::Plain, [0.625, 0.125, 0.0])?;
	require!(crate::harness::colour_close(drawn, 2.0 / 8.0, 0.0 / 8.0, 0.0), "a fragment stage that samples a bound texture reads the texel the coordinate names: {drawn:?}");
	Ok(())
}

pub fn mipmaps() -> Outcome {
	// A MIP CHAIN IS A LENGTH THE EXTENT DECIDES, and a description claiming more levels than the
	// extent has is refused rather than truncated.
	let desc = TextureDesc { dimension: TextureDimension::D2, width: 8, height: 8, depth: 1, mip_levels: 4, layers: 1, samples: 1, format: "RGBA8", usage: TextureUsage { sampled: true, ..TextureUsage::default() } };
	require!(desc.full_mip_chain() == 4, "an eight-by-eight texture has four levels, and the description says {}", desc.full_mip_chain());
	desc.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a full mip chain was refused: {error:?}")))?;
	require!(TextureDesc { mip_levels: 5, ..desc }.validate(&limits()).is_err(), "and a fifth level is one the extent does not have");

	// AND GENERATING THE CHAIN HALVES THE EXTENT EACH TIME, ending at one texel.
	let mut texture = texture_of(Kind::Dim2, vec![positional(8, 8, 1)]);
	soft3d::texture::generate_mips(&mut texture);
	require!(texture.levels() == 4, "generating the chain of an eight-by-eight texture makes four levels, and made {}", texture.levels());
	let smallest = texture.level(3).ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("the last level is missing")))?;
	require!(smallest.width == 1 && smallest.height == 1, "and the last one is a single texel, which is {}x{}", smallest.width, smallest.height);
	Ok(())
}

pub fn render_target() -> Outcome {
	// A RENDER TARGET IS A VIEW WITH A LOAD AND A STORE, and what makes it a resource feature rather
	// than a pass one is that the TEXTURE has to say it may be one: a description without the usage
	// is a texture nothing may render into, and finding that out at the draw is finding it out too
	// late.
	let renderable = TextureDesc { dimension: TextureDimension::D2, width: 8, height: 8, depth: 1, mip_levels: 1, layers: 1, samples: 1, format: "RGBA8", usage: TextureUsage { colour_attachment: true, sampled: true, ..TextureUsage::default() } };
	renderable.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a renderable texture was refused: {error:?}")))?;

	let view = resource::RenderTargetView { texture: 1, view: resource::TextureViewDesc { dimension: TextureDimension::D2, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1, aspect: resource::Aspect::Colour }, format: "RGBA8", samples: 1, width: 8, height: 8, load: resource::LoadOp::Clear, store: resource::StoreOp::Store };
	let attachments = [view];
	let set = resource::RenderTargetSet { colour: &attachments, depth_stencil: None, resolve: &[] };
	set.validate(&limits()).map_err(|error| crate::Trouble::Unsupported(alloc::format!("a single-attachment target set was refused: {error:?}")))?;

	// AND EVERY ATTACHMENT OF ONE SET IS THE SAME SIZE, because a pass writes one fragment to all of
	// them: a set whose attachments disagree is a set where half the writes are out of range.
	let mismatched = [view, resource::RenderTargetView { width: 4, height: 4, ..view }];
	let set = resource::RenderTargetSet { colour: &mismatched, depth_stencil: None, resolve: &[] };
	require!(set.validate(&limits()).is_err(), "a set whose attachments are different sizes is refused");
	Ok(())
}
