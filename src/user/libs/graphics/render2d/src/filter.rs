//! FILTERS ARE A BOUNDED GRAPH RATHER THAN THREE NAMED EFFECTS.
//!
//! A FILTER CHAIN IS WHAT AN APPLICATION ACTUALLY WANTS: a shadow is a blur of an alpha channel,
//! offset, tinted and composited UNDER the thing that cast it, and an API with a `drop_shadow` call
//! has one shadow and no way to make another. A graph has all of them and a bound.
//!
//! THE BOUNDS MAP IS WHAT MAKES IT AFFORDABLE. Each node says which input rectangle it needs for a
//! given output rectangle. Without that the whole graph has to be computed over the whole surface,
//! which is why a blur over a small dirty region costs a full-screen blur in implementations that
//! skipped it.

use alloc::vec::Vec;

use graphics_core::geom::RectF;

use crate::Error;
use crate::paint::Color;
use crate::resource::ImageHandle;

/// One node of a filter graph.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FilterNode {
	/// The thing being filtered.
	Source,
	/// WHAT IS UNDER THE LAYER, which is what a backdrop filter reads.
	///
	/// A FROSTED PANEL IS A BLUR OF ITS BACKDROP AND NOT OF ITSELF, and without this node the whole
	/// class of effects - the translucent toolbar, the modal's dimmed background, the glass card - has
	/// to be built by drawing the scene twice. It is also what makes the dependency direction real:
	/// a layer whose graph reads this cannot be composited until what is beneath it is finished.
	Backdrop,
	/// An image from the list's table, for a filter that composites one in.
	Image(ImageHandle),
	/// A Gaussian blur, with its standard deviation in DEVICE pixels on each axis.
	Blur { input: u16, x: f32, y: f32 },
	/// A translation, which is what makes a shadow a shadow.
	Offset { input: u16, dx: f32, dy: f32 },
	/// A colour matrix, five columns by four rows, applied to unpremultiplied linear colour.
	ColorMatrix { input: u16, matrix: [[f32; 5]; 4] },
	/// Flood a colour over the whole output rectangle.
	Flood { color: Color },
	/// Two inputs under a compositing operator.
	Composite { source: u16, backdrop: u16, operator: crate::blend::Operator },
	/// Two inputs under a blend mode.
	Blend { source: u16, backdrop: u16, mode: crate::blend::BlendMode },
	/// Keep only where the second input has alpha, which is what every clip-shaped effect is built on.
	In { input: u16, mask: u16 },
	/// A THREE BY THREE convolution over premultiplied colour: sharpen, emboss, edge detect.
	///
	/// THREE BY THREE AND NOT A GENERAL SIZE. A larger kernel is either a blur - which has its own
	/// node and a separable implementation - or a graph of these, and a variable-size kernel makes a
	/// node an allocation and its per-pixel cost unbounded, which is the pair of properties the whole
	/// graph exists to avoid.
	Convolution {
		input: u16,
		/// Row-major, with the centre weight at `[1][1]`.
		weights: [[f32; 3]; 3],
		/// What the weighted sum is divided by. A divisor of zero uses the sum of the weights, and a
		/// sum of zero uses one - so an edge-detect kernel does not have to state the obvious.
		divisor: f32,
		/// Added after the division, in the same units.
		bias: f32,
	},
	/// The per-channel MAXIMUM over a rectangular structuring element: what thickens a glyph or an
	/// outline.
	MorphologyDilate { input: u16, x: f32, y: f32 },
	/// The per-channel MINIMUM over the same element, which thins one.
	MorphologyErode { input: u16, x: f32, y: f32 },
	/// Sample the input at an offset read from another input's colour.
	///
	/// `scale * (channel - 0.5)` IN EACH AXIS, so a map of flat half-grey is the identity - which is
	/// what makes a displacement map composable with a gradient, a noise field or a rendered shape
	/// without the caller biasing it first.
	DisplacementMap { input: u16, map: u16, scale: f32, x_channel: Channel, y_channel: Channel },
	/// The input inside a rectangle and transparent black outside it.
	Crop { input: u16, rect: RectF },
	/// The input's contents inside a rectangle, repeated over the whole output.
	Tile { input: u16, rect: RectF },
}

/// Which channel of a displacement map an axis is read from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
	Red,
	Green,
	Blue,
	Alpha,
}

impl FilterNode {
	/// The nodes this one reads, by index.
	pub fn inputs(&self) -> impl Iterator<Item = u16> + '_ {
		let pair = match self {
			FilterNode::Blur { input, .. } | FilterNode::Offset { input, .. } | FilterNode::ColorMatrix { input, .. } | FilterNode::Convolution { input, .. } | FilterNode::MorphologyDilate { input, .. } | FilterNode::MorphologyErode { input, .. } | FilterNode::Crop { input, .. } | FilterNode::Tile { input, .. } => (Some(*input), None),
			FilterNode::Composite { source, backdrop, .. } | FilterNode::Blend { source, backdrop, .. } => (Some(*source), Some(*backdrop)),
			FilterNode::In { input, mask } => (Some(*input), Some(*mask)),
			FilterNode::DisplacementMap { input, map, .. } => (Some(*input), Some(*map)),
			FilterNode::Source | FilterNode::Backdrop | FilterNode::Image(_) | FilterNode::Flood { .. } => (None, None),
		};
		[pair.0, pair.1].into_iter().flatten()
	}

	/// The input rectangle this node needs to produce an output rectangle.
	///
	/// A BLUR NEEDS MORE THAN IT PRODUCES, by three standard deviations on each side - which is where
	/// a Gaussian has effectively fallen to nothing, and is the number that decides whether a shadow's
	/// edge is complete or clipped.
	pub fn required_input(&self, output: RectF) -> RectF {
		match self {
			FilterNode::Blur { x, y, .. } => {
				let (grow_x, grow_y) = (x.abs() * 3.0, y.abs() * 3.0);
				RectF::new(output.x - grow_x, output.y - grow_y, output.width + grow_x * 2.0, output.height + grow_y * 2.0)
			}
			FilterNode::Offset { dx, dy, .. } => RectF::new(output.x - dx, output.y - dy, output.width, output.height),
			// ONE PIXEL ON EVERY SIDE, which is the kernel's reach and is why the size is frozen: a
			// bounds map for a kernel whose size is a parameter has to be computed rather than stated.
			FilterNode::Convolution { .. } => RectF::new(output.x - 1.0, output.y - 1.0, output.width + 2.0, output.height + 2.0),
			FilterNode::MorphologyDilate { x, y, .. } | FilterNode::MorphologyErode { x, y, .. } => {
				let (grow_x, grow_y) = (x.abs(), y.abs());
				RectF::new(output.x - grow_x, output.y - grow_y, output.width + grow_x * 2.0, output.height + grow_y * 2.0)
			}
			// THE WHOLE DISPLACEMENT IN EVERY DIRECTION. The map's own values decide where each pixel
			// is read from, and nothing here knows them - so the bound is the largest offset the scale
			// can produce, which is half the scale, taken conservatively as the whole of it.
			FilterNode::DisplacementMap { scale, .. } => {
				let reach = scale.abs();
				RectF::new(output.x - reach, output.y - reach, output.width + reach * 2.0, output.height + reach * 2.0)
			}
			// A CROP NEEDS ONLY WHAT SURVIVES IT, which is what makes a crop the cheap way to bound an
			// effect rather than a mask applied after the cost was already paid.
			FilterNode::Crop { rect, .. } => output.intersection(rect),
			// AND A TILE NEEDS ITS RECTANGLE WHATEVER IS ASKED FOR: every output pixel comes from
			// inside it, and no output pixel comes from anywhere else.
			FilterNode::Tile { rect, .. } => *rect,
			_ => output,
		}
	}
}

/// A bounded graph of filter nodes, with its output the LAST node.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct FilterGraph {
	nodes: Vec<FilterNode>,
}

impl FilterGraph {
	pub fn nodes(&self) -> &[FilterNode] {
		&self.nodes
	}

	/// Whether this graph reads what is UNDER the layer it is applied to.
	///
	/// THE QUESTION A COMPOSITOR HAS TO ASK BEFORE IT REORDERS ANYTHING. A layer with a backdrop
	/// dependency cannot be drawn before what is beneath it, cannot be cached independently of it,
	/// and damages everything its own bounds cover when the backdrop changes.
	pub fn reads_backdrop(&self) -> bool {
		self.nodes.iter().any(|node| matches!(node, FilterNode::Backdrop))
	}

	/// Append a node, refusing a cycle and refusing to exceed the profile's ceilings.
	///
	/// A NODE MAY ONLY READ NODES BEFORE IT, which makes the graph acyclic BY CONSTRUCTION rather than
	/// by a check that a later edit can defeat. A cycle is a filter that never finishes, and finding
	/// one during preparation is later than finding it here.
	pub fn push(&mut self, node: FilterNode) -> Result<u16, Error> {
		let limits = graphics_profile::RENDER2D_PROFILE_1_MIN_LIMITS;
		if self.nodes.len() as u64 + 1 > limits.max_filter_nodes as u64 {
			return Err(Error::LimitExceeded { limit: "filter nodes", ceiling: limits.max_filter_nodes as u64 });
		}
		let next = self.nodes.len() as u16;
		for input in node.inputs() {
			if input >= next {
				return Err(Error::FilterCycle);
			}
		}
		let radius = match node {
			FilterNode::Blur { x, y, .. } => x.abs().max(y.abs()) * 3.0,
			FilterNode::MorphologyDilate { x, y, .. } | FilterNode::MorphologyErode { x, y, .. } => x.abs().max(y.abs()),
			// A DISPLACEMENT REACHES AS FAR AS ITS SCALE, so it is bounded by the same ceiling: a map
			// with a scale of ten thousand is a filter that reads the whole surface for every pixel.
			FilterNode::DisplacementMap { scale, .. } => scale.abs(),
			_ => 0.0,
		};
		if radius > limits.max_filter_radius as f32 {
			return Err(Error::LimitExceeded { limit: "filter radius", ceiling: limits.max_filter_radius as u64 });
		}
		// A TILE OF NOTHING NEVER ADVANCES, which is a loop that does not finish rather than a picture
		// that is wrong - so it is refused where it is built.
		if let FilterNode::Tile { rect, .. } = node
			&& rect.is_empty()
		{
			return Err(Error::DegenerateShape { what: "a filter tile with no area" });
		}
		self.nodes.push(node);
		Ok(next)
	}

	/// The input rectangle the WHOLE graph needs to produce an output rectangle.
	///
	/// WALKED BACKWARDS FROM THE OUTPUT, because that is the direction the question runs: a caller
	/// knows what it wants drawn and needs to know what to read. Walking forwards gives the other
	/// answer, which is what a node produces from what it has - and using it for this is how a shadow
	/// comes out with a straight edge where it left the dirty region.
	pub fn required_input(&self, output: RectF) -> RectF {
		let mut needed: Vec<Option<RectF>> = alloc::vec![None; self.nodes.len()];
		if let Some(last) = needed.last_mut() {
			*last = Some(output);
		}
		let mut union: Option<RectF> = None;
		for index in (0..self.nodes.len()).rev() {
			let Some(want) = needed[index] else { continue };
			let node = self.nodes[index];
			let input = node.required_input(want);
			if matches!(node, FilterNode::Source | FilterNode::Backdrop) {
				union = Some(match union {
					Some(existing) => union_of(existing, input),
					None => input,
				});
			}
			for source in node.inputs() {
				let slot = &mut needed[source as usize];
				*slot = Some(match *slot {
					Some(existing) => union_of(existing, input),
					None => input,
				});
			}
		}
		union.unwrap_or(output)
	}
}

fn union_of(left: RectF, right: RectF) -> RectF {
	let x = left.x.min(right.x);
	let y = left.y.min(right.y);
	let right_edge = left.right().max(right.right());
	let bottom = left.bottom().max(right.bottom());
	RectF::new(x, y, right_edge - x, bottom - y)
}
