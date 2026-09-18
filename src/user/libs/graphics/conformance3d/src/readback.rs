//! THE READBACK GROUP: what an application may read out of a finished frame, and in what form.
//!
//! A READBACK ANSWERS CONVERTED VALUES AND NOT RAW STORAGE, which is the profile's own rule and the
//! reason these are three features rather than one: a colour comes back as a colour whatever the
//! attachment's format, a depth comes back in `0..=1` whatever the format stores, and an OBJECT ID
//! comes back as the integer it was written as - because an id that went through a colour conversion
//! would identify something else.

use crate::Outcome;
use crate::harness::{Plan, Scene, Vertex, close, render};
use alloc::vec;
use render3d::depth::{DepthFormat, Stored};

/// A quad carrying a stated colour, depth and identity, which is what all three scenes read back.
fn marked() -> Scene {
	Scene::new(vec![
		Vertex::at(-1.0, -1.0, 0.375).coloured(0.25, 0.5, 0.75, 1.0).with_ident(11.0),
		Vertex::at(1.0, -1.0, 0.375).coloured(0.25, 0.5, 0.75, 1.0).with_ident(11.0),
		Vertex::at(1.0, 1.0, 0.375).coloured(0.25, 0.5, 0.75, 1.0).with_ident(11.0),
		Vertex::at(-1.0, 1.0, 0.375).coloured(0.25, 0.5, 0.75, 1.0).with_ident(11.0),
	])
	.indexed16(vec![0, 1, 2, 0, 2, 3])
}

fn drawn(format: DepthFormat) -> Result<crate::harness::Frame, crate::Trouble> {
	let plan = Plan { depth_format: format, depth_test: Some(render3d::CompareOp::Always), depth_write: true, count: 6, ..Plan::default() };
	render(&marked(), &plan)
}

pub fn readback_color() -> Outcome {
	// THE COLOUR COMES BACK AS THE COLOUR THAT WAS WRITTEN, and a resolve is what a multisampled
	// attachment answers with - so a readback of a one-sample frame is the value and a readback of a
	// four-sample one is the average, both in the same units.
	let frame = drawn(DepthFormat::Depth32F)?;
	let (x, y) = frame.centre();
	let pixel = frame.pixel(x, y);
	require!(close(pixel.x, 0.25) && close(pixel.y, 0.5) && close(pixel.z, 0.75), "a colour reads back as what was written: {pixel:?}");

	let resolved = frame.colour.resolve().map_err(|error| crate::Trouble::Unsupported(alloc::format!("the attachment refused to resolve: {error:?}")))?;
	let index = (y * crate::harness::WIDTH + x) as usize;
	require!(close(resolved[index].x, 0.25), "and the resolved form agrees with the sample: {:?}", resolved[index]);
	Ok(())
}

pub fn readback_depth() -> Outcome {
	// A DEPTH READS BACK IN `0..=1` WHATEVER THE FORMAT STORES, which is what makes a readback usable
	// by a caller that does not know the storage. The same scene in two formats answers the same
	// number - and the STORED forms are different, which is what makes that a claim.
	let float = drawn(DepthFormat::Depth32F)?;
	let normalised = drawn(DepthFormat::Depth16)?;
	let (x, y) = float.centre();
	require!(close(float.depth_at(x, y), 0.375), "a float depth reads back as the value: {}", float.depth_at(x, y));
	require!(close(normalised.depth_at(x, y), 0.375), "and a normalised one reads back as the same value: {}", normalised.depth_at(x, y));
	require!(matches!(float.depth_stored(x, y), Stored::Float(_)), "even though one stores a float");
	require!(matches!(normalised.depth_stored(x, y), Stored::Normalised(_)), "and the other stores an integer");
	Ok(())
}

pub fn readback_object_id() -> Outcome {
	// AN OBJECT ID READS BACK AS THE INTEGER IT WAS WRITTEN AS. This is what picking is, and it is a
	// separate feature from a colour readback because an id that went through a colour conversion -
	// normalised, filtered, blended or resolved by averaging - would identify something else, or
	// nothing.
	let frame = drawn(DepthFormat::Depth32F)?;
	let (x, y) = frame.centre();
	require!(frame.identity(x, y) == 11, "the identity reads back as the number the fragment wrote, and is {}", frame.identity(x, y));

	// AND WHERE NOTHING WAS DRAWN IT IS THE CLEAR, which is what makes "nothing is under this pixel"
	// a sayable answer rather than a guess.
	let smaller = Scene::new(vec![
		Vertex::at(-0.5, -0.5, 0.5).coloured(1.0, 1.0, 1.0, 1.0).with_ident(11.0),
		Vertex::at(0.0, -0.5, 0.5).coloured(1.0, 1.0, 1.0, 1.0).with_ident(11.0),
		Vertex::at(-0.5, 0.0, 0.5).coloured(1.0, 1.0, 1.0, 1.0).with_ident(11.0),
	]);
	let frame = render(&smaller, &Plan::default())?;
	require!(frame.identity(crate::harness::WIDTH - 1, 0) == 0, "a pixel nothing covered answers no object, and answered {}", frame.identity(crate::harness::WIDTH - 1, 0));
	Ok(())
}
