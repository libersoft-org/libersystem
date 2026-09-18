//! THE MULTISAMPLING GROUP: how many samples a pixel has, which of them a primitive covers, and what
//! the pixel comes to when they are resolved.
//!
//! EVERY SCENE HERE LOOKS AT AN EDGE, because that is the only place multisampling is visible: a
//! pixel wholly inside a primitive is the same colour at one sample and at four, and a pixel wholly
//! outside it is the clear. What changes is the pixel the edge crosses.

use crate::Outcome;
use crate::harness::{Plan, Scene, Vertex, close, full_quad, render};
use alloc::vec;
use render_math::Vec4;

/// A triangle whose long edge crosses the target at a SHALLOW angle, so many pixels are partly
/// covered.
///
/// NOT FORTY-FIVE DEGREES, and that is the whole of why the numbers here are what they are. An edge
/// running exactly corner to corner of a square target passes through pixel CORNERS: every pixel it
/// touches has all of its samples on one side or all on the other, so a four-sample run produces no
/// partial coverage at all and looks exactly like a one-sample run. The first version of this scene
/// was that edge, and it reported multisampling as broken.
fn diagonal() -> Scene {
	Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
		Vertex::at(-1.0, 0.3, 0.5).coloured(1.0, 1.0, 1.0, 1.0),
	])
}

/// How many pixels of a frame are neither the clear nor fully covered - the partly covered ones.
fn partial(frame: &crate::harness::Frame) -> u32 {
	let mut count = 0;
	for y in 0..crate::harness::HEIGHT {
		for x in 0..crate::harness::WIDTH {
			let pixel = frame.pixel(x, y);
			if pixel.x > 0.001 && pixel.x < 0.999 {
				count += 1;
			}
		}
	}
	count
}

pub fn msaa_1x() -> Outcome {
	// ONE SAMPLE IS THE ABSENCE OF MULTISAMPLING and not a special case of it: every pixel is either
	// the primitive's colour or the clear, and nothing is in between.
	let frame = render(&diagonal(), &Plan { samples: 1, ..Plan::default() })?;
	require!(partial(&frame) == 0, "at one sample a pixel is covered or not, and {} pixel(s) are in between", partial(&frame));
	require!(frame.covered_pixels() > 0, "and the triangle is drawn");
	Ok(())
}

pub fn msaa_2x() -> Outcome {
	// TWO SAMPLES MAKE ONE INTERMEDIATE VALUE POSSIBLE, which is what an edge pixel takes: half the
	// samples covered is half the colour.
	let frame = render(&diagonal(), &Plan { samples: 2, ..Plan::default() })?;
	require!(partial(&frame) > 0, "at two samples the diagonal edge produces partly covered pixels");
	for y in 0..crate::harness::HEIGHT {
		for x in 0..crate::harness::WIDTH {
			let pixel = frame.pixel(x, y);
			// The only values two samples can resolve to are none, half and all of the colour.
			require!(close(pixel.x, 0.0) || close(pixel.x, 0.5) || close(pixel.x, 1.0), "a two-sample pixel resolves to none, half or all of the colour, and ({x}, {y}) is {pixel:?}");
		}
	}
	Ok(())
}

pub fn msaa_4x() -> Outcome {
	// AND FOUR SAMPLES MAKE THREE, which is what distinguishes a four-sample implementation from a
	// two-sample one dressed up: a quarter and three quarters are values two samples cannot produce.
	let frame = render(&diagonal(), &Plan { samples: 4, ..Plan::default() })?;
	let mut quarters = false;
	for y in 0..crate::harness::HEIGHT {
		for x in 0..crate::harness::WIDTH {
			let value = frame.pixel(x, y).x;
			if close(value, 0.25) || close(value, 0.75) {
				quarters = true;
			}
			require!(close(value, 0.0) || close(value, 0.25) || close(value, 0.5) || close(value, 0.75) || close(value, 1.0), "a four-sample pixel resolves to a quarter of the colour, and ({x}, {y}) is {value}");
		}
	}
	require!(quarters, "and a diagonal edge produces pixels at a quarter or three quarters, which two samples cannot");
	Ok(())
}

pub fn sample_mask() -> Outcome {
	// THE SAMPLE MASK REMOVES SAMPLES AFTER COVERAGE AND BEFORE THE WRITE. A fully covered pixel
	// under a mask of one sample in four resolves to a quarter of the colour, which no coverage
	// rule would produce: it is the mask and nothing else.
	let frame = render(&full_quad(1.0, 1.0, 1.0), &Plan { samples: 4, sample_mask: 0b0001, count: 6, ..Plan::default() })?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 0.25), "one sample in four leaves a quarter of the colour, and the pixel is {:?}", frame.pixel(x, y));

	let frame = render(&full_quad(1.0, 1.0, 1.0), &Plan { samples: 4, sample_mask: 0b0011, count: 6, ..Plan::default() })?;
	require!(close(frame.pixel(x, y).x, 0.5), "two in four leave half, and the pixel is {:?}", frame.pixel(x, y));

	// AND IT IS THE NAMED SAMPLES THAT WERE WRITTEN, which the resolve alone cannot say: a backend
	// that wrote half the colour to ALL FOUR samples would resolve to the same half and be wrong, and
	// the difference shows the moment a second primitive covers the other two.
	require!(close(frame.sample(x, y, 0).x, 1.0) && close(frame.sample(x, y, 1).x, 1.0), "the two samples the mask names hold the whole colour: {:?} and {:?}", frame.sample(x, y, 0), frame.sample(x, y, 1));
	require!(close(frame.sample(x, y, 2).x, 0.0) && close(frame.sample(x, y, 3).x, 0.0), "and the two it does not are untouched: {:?} and {:?}", frame.sample(x, y, 2), frame.sample(x, y, 3));
	Ok(())
}

pub fn alpha_to_coverage() -> Outcome {
	// ALPHA BECOMES COVERAGE, which is what makes a cut-out leaf antialias without a sorted pass: a
	// fragment at half alpha covers half the samples of a pixel it fully covers. A backend that
	// ignored it would write the colour at every sample and resolve to the whole colour.
	let scene = Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 1.0, 1.0, 0.5),
		Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 1.0, 1.0, 0.5),
		Vertex::at(1.0, 1.0, 0.5).coloured(1.0, 1.0, 1.0, 0.5),
		Vertex::at(-1.0, 1.0, 0.5).coloured(1.0, 1.0, 1.0, 0.5),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3]);
	let frame = render(&scene, &Plan { samples: 4, alpha_to_coverage: true, count: 6, clear: Vec4::new(0.0, 0.0, 0.0, 1.0), ..Plan::default() })?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 0.5), "half an alpha covers half the samples, and the pixel resolves to {:?}", frame.pixel(x, y));

	// AND WITHOUT IT THE SAME FRAGMENT COVERS EVERY SAMPLE, which is the comparison that makes the
	// first half mean something.
	let frame = render(&scene, &Plan { samples: 4, alpha_to_coverage: false, count: 6, ..Plan::default() })?;
	require!(close(frame.pixel(x, y).x, 1.0), "and with it off the same fragment covers every sample: {:?}", frame.pixel(x, y));
	Ok(())
}

pub fn msaa_resolve() -> Outcome {
	// A RESOLVE IS THE AVERAGE OF THE SAMPLES AND NOT THE FIRST OF THEM, which is the whole
	// difference between a multisampled attachment and a bigger one. `Colour::resolve` is what a
	// pass runs at the end, and this asks it directly: an edge pixel's samples differ, and the
	// answer is between them.
	let plan = Plan { samples: 4, ..Plan::default() };
	let frame = render(&diagonal(), &plan)?;
	let resolved = frame.colour.resolve().map_err(|error| crate::Trouble::Unsupported(alloc::format!("the attachment refused to resolve: {error:?}")))?;
	require!(resolved.len() == (crate::harness::WIDTH * crate::harness::HEIGHT) as usize, "a resolve answers one value per pixel, and answered {}", resolved.len());
	let mut between = 0;
	for value in &resolved {
		if value.x > 0.001 && value.x < 0.999 {
			between += 1;
		}
	}
	require!(between > 0, "and an edge resolves to a value between the two, which none of {} pixels did", resolved.len());

	// AND AN INTEGER ATTACHMENT RESOLVES TO ONE SAMPLE AND NOT TO AN AVERAGE, because half of one
	// object id plus half of another is a third object that is not in the scene.
	let identity = frame.identity.as_ref().ok_or_else(|| crate::Trouble::Failed(alloc::string::String::from("the frame has no identity attachment")))?;
	let ids = identity.resolve().map_err(|error| crate::Trouble::Unsupported(alloc::format!("the identity attachment refused to resolve: {error:?}")))?;
	for value in &ids {
		require!(value.x == 0.0 || value.x == 1.0, "an identity resolves to one of the samples, and one answered {}", value.x);
	}
	Ok(())
}
