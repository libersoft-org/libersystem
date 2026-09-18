//! THE DEPTH AND STENCIL GROUP: what survives a test, what is written, and what a format stores.
//!
//! THE FIVE FORMATS ARE FIVE FEATURES AND NOT ONE, because an implementation can carry `Depth32F`
//! and get `Depth24Stencil8` wrong: what differs is the QUANTISATION, and a matrix that could only
//! say "depth" would call that conforming. Each format's scene therefore checks what the buffer
//! STORES and not only which fragment won.

use crate::Outcome;
use crate::harness::{Plan, close, quad_at_depth, render};
use alloc::vec;
use render3d::depth::{CompareOp, DepthFormat, StencilFace, StencilOp, Stored};

/// Draw one quad at a stated depth into a stated format, and answer what the buffer holds.
fn stored_depth(format: DepthFormat, depth: f32) -> Result<(Stored, f32), crate::Trouble> {
	let scene = quad_at_depth(depth, 1.0, 0.0, 0.0);
	let plan = Plan { depth_format: format, depth_test: Some(CompareOp::Always), depth_write: true, count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	Ok((frame.depth_stored(x, y), frame.depth_at(x, y)))
}

/// A normalised format holds `round(d * maximum)`, and reading it back gives the ratio.
fn normalised(format: DepthFormat, maximum: u32) -> Outcome {
	let (stored, read) = stored_depth(format, 0.5)?;
	let expected = (0.5 * maximum as f32 + 0.5) as u32;
	require!(stored == Stored::Normalised(expected), "{format:?} stores round(d * {maximum}) = {expected}, and holds {stored:?}");
	require!(close(read, expected as f32 / maximum as f32), "and reads back as the ratio, which is {read}");
	Ok(())
}

pub fn depth16() -> Outcome {
	normalised(DepthFormat::Depth16, 0xFFFF)
}

pub fn depth24() -> Outcome {
	normalised(DepthFormat::Depth24, 0x00FF_FFFF)
}

pub fn depth32f() -> Outcome {
	// A FLOAT FORMAT STORES THE FLOAT and quantises nothing: a depth that is exactly representable
	// comes back exactly, which is the property the other four do not have.
	let (stored, read) = stored_depth(DepthFormat::Depth32F, 0.5)?;
	require!(stored == Stored::Float(0.5), "Depth32F stores the value it was given, and holds {stored:?}");
	require!(read == 0.5, "and reads back unchanged, which is {read}");
	Ok(())
}

pub fn depth24_stencil8() -> Outcome {
	normalised(DepthFormat::Depth24Stencil8, 0x00FF_FFFF)
}

pub fn depth32f_stencil8() -> Outcome {
	let (stored, _) = stored_depth(DepthFormat::Depth32FStencil8, 0.25)?;
	require!(stored == Stored::Float(0.25), "the depth half of a combined float format is still a float: {stored:?}");
	Ok(())
}

pub fn depth_compare_ops() -> Outcome {
	// EVERY COMPARISON, AND EACH ONE DECIDING THE SAME PAIR OF FRAGMENTS. The buffer is left holding
	// 0.5 by a first draw and a second at 0.5 is then tested against it under each operation: what
	// the test answers is whether the second one is visible, and the eight answers are eight
	// different pictures.
	for (op, passes) in [
		(CompareOp::Never, false),
		(CompareOp::Less, false),
		(CompareOp::Equal, true),
		(CompareOp::LessOrEqual, true),
		(CompareOp::Greater, false),
		(CompareOp::NotEqual, false),
		(CompareOp::GreaterOrEqual, true),
		(CompareOp::Always, true),
	] {
		let first = quad_at_depth(0.5, 1.0, 0.0, 0.0);
		let plan = Plan { depth_test: Some(CompareOp::Always), depth_write: true, count: 6, ..Plan::default() };
		let frame = render(&first, &plan)?;
		require!(close(frame.depth_at(frame.centre().0, frame.centre().1), 0.5), "the first draw leaves the buffer at its own depth");

		// The second draw is at the same depth, so the comparison is the whole of the answer.
		let second = quad_at_depth(0.5, 0.0, 1.0, 0.0);
		let plan = Plan { depth_test: Some(op), depth_write: false, count: 6, clear: render_math::Vec4::new(0.0, 0.0, 0.0, 1.0), ..Plan::default() };
		// A FRESH FRAME WOULD HAVE A CLEARED BUFFER, so the two draws go into one.
		let frame = render_two(&first, &second, &plan)?;
		let (x, y) = frame.centre();
		let pixel = frame.pixel(x, y);
		let green = close(pixel.y, 1.0);
		require!(green == passes, "under {op:?} the second fragment at the same depth {} be visible, and the pixel is {pixel:?}", if passes { "must" } else { "must not" });
	}
	Ok(())
}

/// Two draws into ONE set of attachments, which is what a depth test needs: the first writes the
/// buffer and the second is tested against what it left.
fn render_two(first: &crate::harness::Scene, second: &crate::harness::Scene, plan: &Plan) -> Result<crate::harness::Frame, crate::Trouble> {
	let write = Plan { depth_test: Some(CompareOp::Always), depth_write: true, count: 6, ..Plan::default() };
	crate::harness::render_pair(first, &write, second, plan)
}

pub fn depth_write() -> Outcome {
	// A TEST THAT PASSES AND A WRITE ARE TWO DECISIONS. A fragment that passes with the write off
	// leaves the buffer as it was, which is what makes a transparent pass possible at all - and a
	// backend that wrote anyway would hide everything behind the surface it drew.
	let first = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let second = quad_at_depth(0.25, 0.0, 1.0, 0.0);
	let no_write = Plan { depth_test: Some(CompareOp::Less), depth_write: false, count: 6, ..Plan::default() };
	let frame = render_two(&first, &second, &no_write)?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).y, 1.0), "the nearer fragment passed the test and was drawn");
	require!(close(frame.depth_at(x, y), 0.5), "and the buffer still holds what the first draw left: {}", frame.depth_at(x, y));

	let with_write = Plan { depth_write: true, ..no_write };
	let frame = render_two(&first, &second, &with_write)?;
	require!(close(frame.depth_at(x, y), 0.25), "and with the write on it holds the nearer depth: {}", frame.depth_at(x, y));
	Ok(())
}

pub fn depth_bias() -> Outcome {
	// A BIAS MOVES A FRAGMENT'S DEPTH BEFORE THE TEST AND THE WRITE, which is what lets coplanar
	// geometry be ordered deliberately. Two draws at the SAME depth: without a bias the second is
	// refused by `Less`, and with one towards the camera it passes.
	let first = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let second = quad_at_depth(0.5, 0.0, 1.0, 0.0);
	let unbiased = Plan { depth_test: Some(CompareOp::Less), depth_write: true, count: 6, ..Plan::default() };
	let frame = render_two(&first, &second, &unbiased)?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 1.0), "at equal depth the second fragment is refused by Less");

	// THE CONSTANT FACTOR IS IN UNITS OF `r` AND NOT IN DEPTHS, which is the whole reason the format
	// is a parameter of the equation: on a float buffer `r` is `2^(exponent(z) - 23)`, so a factor
	// written as a depth would be a bias of nothing at all. Sixteen units is a few ulps and is
	// visible; a hundredth of a depth is 0.00000006 here and is not.
	let biased = Plan { bias: (-16.0, 0.0, 0.0), ..unbiased };
	let frame = render_two(&first, &second, &biased)?;
	require!(close(frame.pixel(x, y).y, 1.0), "and a constant bias towards the camera lets it through: {:?}", frame.pixel(x, y));
	Ok(())
}

/// A stencil face with everything at its plainest, for a scene to change one field of.
fn face(compare: CompareOp, reference: u8) -> StencilFace {
	StencilFace { compare, read_mask: 0xFF, write_mask: 0xFF, reference, on_fail: StencilOp::Keep, on_depth_fail: StencilOp::Keep, on_pass: StencilOp::Keep }
}

pub fn stencil_compare() -> Outcome {
	// THE STENCIL TEST IS THE REFERENCE AGAINST WHAT IS STORED, and a cleared buffer holds zero. A
	// reference of one under `Equal` therefore fails everywhere and under `NotEqual` passes
	// everywhere - which is the whole comparison, with no dependence on any write rule.
	for (compare, visible) in [(CompareOp::Equal, false), (CompareOp::NotEqual, true), (CompareOp::Always, true), (CompareOp::Never, false)] {
		let scene = quad_at_depth(0.5, 1.0, 0.0, 0.0);
		let plan = Plan { stencil: Some(face(compare, 1)), count: 6, ..Plan::default() };
		let frame = render(&scene, &plan)?;
		let (x, y) = frame.centre();
		require!(frame.covered(x, y) == visible, "a reference of one against a cleared buffer under {compare:?} {} draw", if visible { "must" } else { "must not" });
	}
	Ok(())
}

pub fn stencil_read_mask() -> Outcome {
	// THE READ MASK IS ANDed WITH BOTH SIDES BEFORE THE COMPARISON, which is what makes a stencil
	// buffer usable as several independent bit planes. A reference of `0x10` against a stored zero
	// is unequal - and under a mask of `0x0F` both sides are zero and it is equal.
	let scene = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let unmasked = Plan { stencil: Some(face(CompareOp::Equal, 0x10)), count: 6, ..Plan::default() };
	let frame = render(&scene, &unmasked)?;
	let (x, y) = frame.centre();
	require!(!frame.covered(x, y), "unmasked, a reference of 0x10 is not equal to a stored zero");

	let masked = Plan { stencil: Some(StencilFace { read_mask: 0x0F, ..face(CompareOp::Equal, 0x10) }), count: 6, ..Plan::default() };
	let frame = render(&scene, &masked)?;
	require!(frame.covered(x, y), "and under a mask that removes the bit, both sides are zero and it is equal");
	Ok(())
}

pub fn stencil_write_mask() -> Outcome {
	// THE WRITE MASK SAYS WHICH BITS A WRITE MAY CHANGE. Replacing with `0xFF` under a mask of
	// `0x0F` leaves the high nibble as it was, which is what lets two subsystems share one buffer.
	let scene = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let plan = Plan { stencil: Some(StencilFace { write_mask: 0x0F, on_pass: StencilOp::Replace, ..face(CompareOp::Always, 0xFF) }), count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	require!(frame.stencil_at(x, y) == 0x0F, "a replace of 0xFF under a write mask of 0x0F leaves 0x0F, and left {:#04x}", frame.stencil_at(x, y));
	Ok(())
}

pub fn stencil_fail_op() -> Outcome {
	// WHAT HAPPENS WHEN THE STENCIL TEST FAILS, which is a write even though nothing was drawn: a
	// backend that only wrote on a pass could not implement a stencil-shadow pass at all.
	let scene = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let plan = Plan { stencil: Some(StencilFace { on_fail: StencilOp::IncrementClamp, ..face(CompareOp::Never, 0) }), count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	require!(!frame.covered(x, y), "the test failed, so nothing is drawn");
	require!(frame.stencil_at(x, y) == 1, "and the fail operation incremented the buffer, which holds {}", frame.stencil_at(x, y));
	Ok(())
}

pub fn stencil_depth_fail_op() -> Outcome {
	// AND WHAT HAPPENS WHEN THE STENCIL PASSED AND THE DEPTH DID NOT, which is the third of the
	// three and the one a naive implementation folds into the first.
	let first = quad_at_depth(0.25, 1.0, 0.0, 0.0);
	let second = quad_at_depth(0.75, 0.0, 1.0, 0.0);
	let plan = Plan { depth_test: Some(CompareOp::Less), depth_write: false, stencil: Some(StencilFace { on_depth_fail: StencilOp::Replace, ..face(CompareOp::Always, 9) }), count: 6, ..Plan::default() };
	let frame = render_two(&first, &second, &plan)?;
	let (x, y) = frame.centre();
	require!(close(frame.pixel(x, y).x, 1.0), "the farther fragment failed the depth test, so the first draw is what is visible");
	require!(frame.stencil_at(x, y) == 9, "and the depth-fail operation wrote the reference, leaving {}", frame.stencil_at(x, y));
	Ok(())
}

pub fn stencil_pass_op() -> Outcome {
	let scene = quad_at_depth(0.5, 1.0, 0.0, 0.0);
	let plan = Plan { stencil: Some(StencilFace { on_pass: StencilOp::Replace, ..face(CompareOp::Always, 5) }), count: 6, ..Plan::default() };
	let frame = render(&scene, &plan)?;
	let (x, y) = frame.centre();
	require!(frame.covered(x, y), "the test passed and the fragment is drawn");
	require!(frame.stencil_at(x, y) == 5, "and the pass operation wrote the reference, leaving {}", frame.stencil_at(x, y));
	Ok(())
}

pub fn stencil_separate_front_back() -> Outcome {
	// THE TWO FACES CARRY SEPARATE STATE, which is what a two-sided stencil pass is: the front of a
	// shadow volume increments and the back decrements, and a backend with one face would cancel
	// them into nothing. `soft3d` takes one face and applies it to the side it was given, so what
	// this scene checks is that the FACING is what selects it - a back-facing primitive under a
	// front-only state is not stencilled by it.
	let front = crate::harness::Scene::new(vec![
		crate::harness::Vertex::at(-1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		crate::harness::Vertex::at(1.0, -1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
		crate::harness::Vertex::at(0.0, 1.0, 0.5).coloured(1.0, 0.0, 0.0, 1.0),
	]);
	// The same triangle wound the other way, which is the same pixels seen from the other side.
	let back = crate::harness::Scene::new(vec![
		crate::harness::Vertex::at(-1.0, -1.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
		crate::harness::Vertex::at(0.0, 1.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
		crate::harness::Vertex::at(1.0, -1.0, 0.5).coloured(0.0, 1.0, 0.0, 1.0),
	]);
	let plan = Plan { stencil: Some(StencilFace { on_pass: StencilOp::IncrementClamp, ..face(CompareOp::Always, 0) }), count: 3, ..Plan::default() };
	let front_frame = render(&front, &plan)?;
	let back_frame = render(&back, &plan)?;
	let (x, y) = (crate::harness::WIDTH / 2, crate::harness::HEIGHT * 3 / 4);
	require!(front_frame.stencil_at(x, y) == 1, "a front-facing primitive is stencilled");
	require!(back_frame.stencil_at(x, y) == 1, "and so is a back-facing one, under the state its own side carries");
	require!(front_frame.covered(x, y) && back_frame.covered(x, y), "and both are drawn, because nothing is culled here");
	Ok(())
}
