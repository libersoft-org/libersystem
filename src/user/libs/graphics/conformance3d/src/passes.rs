//! THE PASSES GROUP: what happens to an attachment at the beginning and the end of a pass, and what
//! a pass may write into.
//!
//! LOAD AND STORE ARE PER ATTACHMENT AND EACH OPTION IS ITS OWN FEATURE, because a backend that
//! honours `Clear` and ignores `Discard` has a working clear and a broken pass. What tells a discard
//! from a clear is that a clear has a VALUE and a discard has none - so the scenes below check that
//! reading a discarded attachment is refused rather than answered.

use crate::Outcome;
use crate::harness::{Plan, close, full_quad, render};
use alloc::vec;
use graphics_profile::image::{Semantics, Transfer};
use render_math::Vec4;
use render3d::resource::{LoadOp, StoreOp};
use soft3d::pass::{Colour, Operations, load_colour, store_colour};
use soft3d::texture::{Filter, Sampler, Wrap};

/// An attachment with something in it, so a load that keeps it is distinguishable from one that does
/// not.
fn written(value: f32) -> Colour {
	let mut colour = Colour::new(4, 4, 1, false);
	colour.fill(Vec4::new(value, value, value, 1.0));
	colour
}

pub fn load_clear() -> Outcome {
	// A CLEAR REPLACES WHAT IS THERE WITH A STATED VALUE, which is the only load operation that has
	// one.
	let mut attachment = written(0.5);
	load_colour(&mut attachment, &Operations { load: LoadOp::Clear, store: StoreOp::Store, clear: Vec4::new(0.25, 0.0, 0.0, 1.0) });
	require!(close(attachment.at(0, 0, 0).x, 0.25), "a clear writes its own value: {:?}", attachment.at(0, 0, 0));
	require!(close(attachment.at(3, 3, 0).x, 0.25), "everywhere, not only at the origin");
	Ok(())
}

pub fn load_load() -> Outcome {
	// A LOAD KEEPS WHAT THE PREVIOUS PASS LEFT, which is what makes a multi-pass graph possible at
	// all: a backend that cleared anyway would lose the first pass's output every time.
	let mut attachment = written(0.5);
	load_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Store, clear: Vec4::new(1.0, 1.0, 1.0, 1.0) });
	require!(close(attachment.at(2, 2, 0).x, 0.5), "a load leaves the attachment as it was: {:?}", attachment.at(2, 2, 0));
	Ok(())
}

pub fn load_discard() -> Outcome {
	// A DISCARD SAYS NOTHING ABOUT THE CONTENTS, and that is different from clearing them: a clear
	// has a value and a discard has none. `soft3d` POISONS a discarded attachment rather than leaving
	// it, so a pass that reads one reads something that is obviously not a colour - which is what
	// stops one backend's uninitialised memory becoming another's black.
	let mut attachment = written(0.5);
	load_colour(&mut attachment, &Operations { load: LoadOp::Discard, store: StoreOp::Store, clear: Vec4::new(0.0, 0.0, 0.0, 1.0) });
	let value = attachment.at(1, 1, 0);
	require!(value.x != 0.5, "a discard does not leave the previous contents: {value:?}");
	// AND IT IS NOT QUIETLY A CLEAR EITHER. The two operations differ in exactly one way - a clear has
	// a value and a discard has none - so a backend that treated `Discard` as `Clear` would be
	// conforming by accident on the machine it was written on and wrong on the next one.
	require!(value != Vec4::new(0.0, 0.0, 0.0, 1.0), "nor the clear value it was handed: {value:?}");
	// AND WHAT IT LEAVES IS THE SAME EVERYWHERE, which is what makes reading a discarded attachment
	// look obviously wrong rather than look like noise somebody might mistake for a picture.
	require!(attachment.at(0, 0, 0) == value && attachment.at(3, 3, 0) == value, "and what it leaves is one stated value over the whole attachment");
	Ok(())
}

pub fn store_store() -> Outcome {
	// A STORE KEEPS THE PASS'S OUTPUT AND SAYS SO, which is the answer a later read depends on.
	let mut attachment = written(0.75);
	let readable = store_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Store, clear: Vec4::ZERO });
	require!(readable, "a stored attachment may be read afterwards");
	require!(close(attachment.at(0, 0, 0).x, 0.75), "and holds what the pass wrote: {:?}", attachment.at(0, 0, 0));
	Ok(())
}

pub fn store_discard() -> Outcome {
	// AND A DISCARDED ONE MAY NOT, which is the whole of what makes a discard worth having: a
	// backend may throw the contents away instead of writing them back, and the contract is that
	// nobody reads them.
	let mut attachment = written(0.75);
	let readable = store_colour(&mut attachment, &Operations { load: LoadOp::Load, store: StoreOp::Discard, clear: Vec4::ZERO });
	require!(!readable, "a discarded attachment answers that it may not be read");
	require!(attachment.at(0, 0, 0).x != 0.75, "and does not quietly hold the contents anyway: {:?}", attachment.at(0, 0, 0));
	Ok(())
}

pub fn multiple_color_attachments() -> Outcome {
	// TWO ATTACHMENTS, ONE FRAGMENT, TWO WRITES. The fragment stage writes a colour to one and an
	// identity to the other, and what makes this a feature rather than an accident is that the two
	// carry DIFFERENT values from one shading.
	let scene = crate::harness::Scene::new(vec![
		crate::harness::Vertex::at(-1.0, -1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(3.0),
		crate::harness::Vertex::at(1.0, -1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(3.0),
		crate::harness::Vertex::at(1.0, 1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(3.0),
		crate::harness::Vertex::at(-1.0, 1.0, 0.5).coloured(0.25, 0.5, 0.75, 1.0).with_ident(3.0),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3]);
	let frame = render(&scene, &Plan { count: 6, ..Plan::default() })?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 0.25), "the colour attachment holds the colour: {:?}", frame.pixel(x, y));
	require!(frame.identity(x, y) == 3, "and the identity attachment holds the identity, which is {}", frame.identity(x, y));
	Ok(())
}

pub fn depth_stencil_attachment() -> Outcome {
	// A DEPTH-STENCIL ATTACHMENT IS ONE ATTACHMENT WITH TWO PLANES, and a pass writes both from one
	// fragment. What this checks is that they are independent: the depth is the fragment's and the
	// stencil is the state's.
	let scene = crate::harness::quad_at_depth(0.25, 1.0, 0.0, 0.0);
	let face = render3d::StencilFace { compare: render3d::CompareOp::Always, read_mask: 0xFF, write_mask: 0xFF, reference: 6, on_fail: render3d::StencilOp::Keep, on_depth_fail: render3d::StencilOp::Keep, on_pass: render3d::StencilOp::Replace };
	let plan = Plan { depth_format: render3d::DepthFormat::Depth24Stencil8, depth_test: Some(render3d::CompareOp::Always), depth_write: true, stencil: Some(face), count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	require!(close(frame.depth_at(x, y), 0.25), "the depth plane holds the fragment's depth: {}", frame.depth_at(x, y));
	require!(frame.stencil_at(x, y) == 6, "and the stencil plane holds the state's reference, which is {}", frame.stencil_at(x, y));
	Ok(())
}

pub fn offscreen_render_target() -> Outcome {
	// AN OFFSCREEN TARGET IS ONE NOTHING PRESENTS, and what makes it a feature is that a pass may
	// render into an extent that is not the display's. This one is a different size from every other
	// scene's, and the drawing lands in it correctly.
	let scene = full_quad(0.0, 1.0, 0.0);
	let frame = render(&scene, &Plan { width: 17, height: 9, count: 6, ..Plan::default() })?;
	require!(frame.covered_pixels() == 17 * 9, "an odd-sized offscreen target is covered entirely: {} of {}", frame.covered_pixels(), 17 * 9);
	require!(close(frame.pixel(16, 8).y, 1.0), "including its last pixel: {:?}", frame.pixel(16, 8));
	Ok(())
}

pub fn render_to_texture() -> Outcome {
	// WHAT A PASS WROTE IS WHAT THE NEXT ONE READS, which is the whole of render-to-texture: an
	// attachment becomes a texture without a copy through anything else. `from_attachment` is that
	// seam, and this checks that the texels it answers are the pixels the pass wrote.
	let scene = full_quad(0.25, 0.5, 0.75);
	let frame = render(&scene, &Plan { width: 8, height: 8, count: 6, ..Plan::default() })?;
	let texture = soft3d::texture::from_attachment(&frame.colour, 9).map_err(|error| crate::Trouble::Unsupported(alloc::format!("an attachment was refused as a texture: {error:?}")))?;
	require!(texture.id == 9, "the texture carries the identifier it was given");
	require!(texture.transfer == Transfer::Linear && texture.semantics == Semantics::Color, "and an attachment is linear colour, which is what it was composited in");
	let sampler = Sampler { wrap_u: Wrap::ClampToEdge, wrap_v: Wrap::ClampToEdge, minify: Filter::Nearest, magnify: Filter::Nearest, ..Sampler::NEAREST };
	let texel = soft3d::texture::sample(&texture, &sampler, [0.5, 0.5, 0.0], 0.0, true);
	require!(close(texel[0], 0.25) && close(texel[1], 0.5) && close(texel[2], 0.75), "and reading it answers what the pass drew: {texel:?}");
	Ok(())
}
