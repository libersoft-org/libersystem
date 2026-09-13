//! EVERY PAINT, EVALUATED PER PIXEL: solid, linear, radial, CONIC and image.
//!
//! CONIC IS IN THE PROFILE, so it is implemented here rather than typed and refused. A pie chart, a
//! colour wheel and a loading spinner are conic gradients, and an application whose renderer refuses
//! them draws them as images somebody generated somewhere else - which is the private-rasteriser
//! failure one level up.
//!
//! THE STOPS ARE INTERPOLATED IN LINEAR LIGHT. Interpolating encoded values makes the middle of a
//! black-to-white gradient too dark and the middle of a blue-to-yellow one muddy, and it is the
//! defect that makes a gradient look banded even at full precision.
//!
//! AND EVERY PAINT HAS ITS OWN TRANSFORM, which is not the shape's. A device point is mapped back
//! through the INVERSE of the shape's transform composed with the paint's, so the gradient's geometry
//! is evaluated in the space it was stated in.

use alloc::vec::Vec;

use graphics_core::ImageView;
use graphics_core::geom::{PointF, RectF};
use graphics_core::pixel::{Rgba, Working};
use graphics_core::sample::{Pyramid, Quality, Sampler, Spread};
use render2d::paint::{Color, GradientStop, Paint};
use render2d::transform::Transform;

/// A gradient's stops, resolved into the working space ONCE.
///
/// ONCE AND NOT PER PIXEL, because a colour-space conversion is a matrix multiply and a transfer
/// function is a power - doing either per pixel is how a gradient comes to cost more than the shape
/// it fills.
pub struct Ramp {
	stops: Vec<(f32, Rgba)>,
}

impl Ramp {
	pub fn new(stops: &[GradientStop], working: Working) -> Self {
		let mut resolved: Vec<(f32, Rgba)> = Vec::with_capacity(stops.len());
		for stop in stops {
			resolved.push((stop.offset.clamp(0.0, 1.0), to_working(stop.color, working)));
		}
		// STABLE BY OFFSET, so two stops at one offset keep the order they were given - which is what
		// makes a hard colour change at a single offset expressible at all.
		resolved.sort_by(|left, right| left.0.partial_cmp(&right.0).unwrap_or(core::cmp::Ordering::Equal));
		Self { stops: resolved }
	}

	/// The colour at a position along the gradient, PREMULTIPLIED and in the working space.
	pub fn at(&self, position: f32) -> Rgba {
		if self.stops.is_empty() {
			return Rgba::TRANSPARENT;
		}
		let position = position.clamp(0.0, 1.0);
		let first = self.stops[0];
		if position <= first.0 {
			return first.1;
		}
		let last = self.stops[self.stops.len() - 1];
		if position >= last.0 {
			return last.1;
		}
		for window in self.stops.windows(2) {
			let (low, high) = (window[0], window[1]);
			if position >= low.0 && position <= high.0 {
				let span = high.0 - low.0;
				if span <= 0.0 {
					return high.1;
				}
				let t = (position - low.0) / span;
				return low.1.scaled(1.0 - t).plus(high.1.scaled(t));
			}
		}
		last.1
	}
}

/// A paint, ready to be evaluated at a device point.
pub enum Shader<'a> {
	Solid(Rgba),
	Linear {
		ramp: Ramp,
		from: PointF,
		to: PointF,
		spread: Spread,
		inverse: Transform,
	},
	Radial {
		ramp: Ramp,
		from: PointF,
		from_radius: f32,
		to: PointF,
		to_radius: f32,
		spread: Spread,
		inverse: Transform,
	},
	Conic {
		ramp: Ramp,
		centre: PointF,
		start_angle: f32,
		end_angle: f32,
		spread: Spread,
		inverse: Transform,
	},
	Image {
		sampler: Sampler<'a>,
		pyramid: Option<&'a Pyramid>,
		quality: Quality,
		spread: Spread,
		inverse: Transform,
		source: graphics_core::geom::RectF,
	},
	/// A paint whose resource could not be resolved. IT DRAWS NOTHING rather than drawing black: a
	/// missing image is a missing image, and painting it opaque would hide what is under it.
	Nothing,
}

impl Shader<'_> {
	/// The paint's colour at a device point, PREMULTIPLIED in the working space.
	pub fn at(&self, x: f32, y: f32) -> Rgba {
		let point = PointF { x: x + 0.5, y: y + 0.5 };
		match self {
			Shader::Solid(colour) => *colour,
			Shader::Nothing => Rgba::TRANSPARENT,
			Shader::Linear { ramp, from, to, spread, inverse } => {
				let Some(local) = inverse.map_point(point) else { return Rgba::TRANSPARENT };
				let (dx, dy) = (to.x - from.x, to.y - from.y);
				let length_squared = dx * dx + dy * dy;
				if length_squared <= 0.0 {
					return ramp.at(1.0);
				}
				let projection = ((local.x - from.x) * dx + (local.y - from.y) * dy) / length_squared;
				ramp.at(spread_position(projection, *spread))
			}
			Shader::Radial { ramp, from, from_radius, to, to_radius, spread, inverse } => {
				let Some(local) = inverse.map_point(point) else { return Rgba::TRANSPARENT };
				match radial_position(local, *from, *from_radius, *to, *to_radius) {
					Some(position) => ramp.at(spread_position(position, *spread)),
					// OUTSIDE THE CONE OF A TWO-CIRCLE GRADIENT THERE IS NO COLOUR, which is the case
					// a one-circle implementation does not have and gets wrong by extrapolating.
					None => Rgba::TRANSPARENT,
				}
			}
			Shader::Conic { ramp, centre, start_angle, end_angle, spread, inverse } => {
				let Some(local) = inverse.map_point(point) else { return Rgba::TRANSPARENT };
				let angle = libm::atan2f(local.y - centre.y, local.x - centre.x);
				let sweep = end_angle - start_angle;
				if sweep == 0.0 {
					return ramp.at(0.0);
				}
				// THE ANGLE IS BROUGHT INTO THE SWEEP'S OWN TURN, so a gradient from 350 to 10 degrees
				// is a twenty-degree sweep across zero rather than a three-hundred-and-forty-degree
				// one backwards.
				let mut position = (angle - start_angle) / sweep;
				if position < 0.0 {
					position += (core::f32::consts::TAU / sweep).abs();
				}
				ramp.at(spread_position(position, *spread))
			}
			Shader::Image { sampler, pyramid, quality, spread, inverse, source } => {
				// THE INVERSE MAPS A DEVICE PIXEL BACK INTO THE IMAGE'S OWN TEXEL COORDINATES, which
				// is what the paint's transform is stated in - so the source rectangle is already
				// accounted for by that transform rather than added again here.
				let Some(texel) = inverse.map_point(point) else { return Rgba::TRANSPARENT };
				// THE SPREAD APPLIES TO THE SOURCE RECTANGLE AND NOT TO THE WHOLE IMAGE, which is what
				// makes a sprite from an atlas tileable: repeating the image would bring in whatever
				// its neighbours in the atlas are.
				let texel = wrap_into(*source, texel, *spread);
				match (pyramid, quality) {
					(Some(pyramid), Quality::Mipmapped) => {
						// THE FOOTPRINT IS THE DIFFERENCE BETWEEN NEIGHBOURING PIXELS' TEXELS, which
						// is what decides both the level of detail and the anisotropy - and taking it
						// from the transform's scale alone is what makes a perspective floor shimmer
						// in the distance.
						let (x_step, y_step) = footprint(inverse, point);
						pyramid.sample_anisotropic(texel.x, texel.y, x_step, y_step, *spread, 16)
					}
					_ => sampler.sample(texel.x, texel.y, *quality),
				}
			}
		}
	}

	/// Whether this shader is a single opaque colour, which a span can composite without reading the
	/// backdrop at all.
	pub fn is_opaque_solid(&self) -> bool {
		matches!(self, Shader::Solid(colour) if colour.alpha >= 1.0)
	}
}

/// Bring a texel coordinate into a source rectangle under a spread mode.
fn wrap_into(source: RectF, texel: PointF, spread: Spread) -> PointF {
	if source.width <= 0.0 || source.height <= 0.0 {
		return texel;
	}
	let axis = |value: f32, origin: f32, extent: f32| -> f32 {
		let position = (value - origin) / extent;
		origin + spread_position(position, spread) * extent
	};
	PointF { x: axis(texel.x, source.x, source.width), y: axis(texel.y, source.y, source.height) }
}

/// How far one device pixel moves in texel space, on each axis.
fn footprint(inverse: &Transform, point: PointF) -> ((f32, f32), (f32, f32)) {
	let here = inverse.map_point(point);
	let right = inverse.map_point(PointF { x: point.x + 1.0, y: point.y });
	let down = inverse.map_point(PointF { x: point.x, y: point.y + 1.0 });
	match (here, right, down) {
		(Some(here), Some(right), Some(down)) => ((right.x - here.x, right.y - here.y), (down.x - here.x, down.y - here.y)),
		_ => ((1.0, 0.0), (0.0, 1.0)),
	}
}

/// The position along a two-circle radial gradient, or `None` outside its cone.
fn radial_position(point: PointF, from: PointF, from_radius: f32, to: PointF, to_radius: f32) -> Option<f32> {
	// The standard two-circle solution: find `t` such that the point is on the circle interpolated
	// between the two, taking the LARGER root - which is the one that is in front.
	let (cdx, cdy) = (to.x - from.x, to.y - from.y);
	let dr = to_radius - from_radius;
	let (pdx, pdy) = (point.x - from.x, point.y - from.y);
	let a = cdx * cdx + cdy * cdy - dr * dr;
	let b = pdx * cdx + pdy * cdy + from_radius * dr;
	let c = pdx * pdx + pdy * pdy - from_radius * from_radius;
	if a.abs() <= f32::EPSILON {
		if b.abs() <= f32::EPSILON {
			return None;
		}
		let t = c / (2.0 * b);
		return (from_radius + t * dr >= 0.0).then_some(t);
	}
	let discriminant = b * b - a * c;
	if discriminant < 0.0 {
		return None;
	}
	let root = libm::sqrtf(discriminant);
	// THE LARGER ROOT FIRST, which is the circle in front - and a root whose interpolated radius is
	// negative is behind the cone and is not an answer.
	[(b + root) / a, (b - root) / a].into_iter().find(|t| from_radius + t * dr >= 0.0)
}

/// Apply a spread mode to a gradient position.
pub fn spread_position(position: f32, spread: Spread) -> f32 {
	if !position.is_finite() {
		return 0.0;
	}
	match spread {
		Spread::Clamp => position.clamp(0.0, 1.0),
		Spread::Repeat => position - libm::floorf(position),
		Spread::Mirror => {
			let doubled = (position * 0.5) - libm::floorf(position * 0.5);
			let folded = doubled * 2.0;
			if folded > 1.0 { 2.0 - folded } else { folded }
		}
	}
}

/// One API colour in the working space, premultiplied.
pub fn to_working(color: Color, working: Working) -> Rgba {
	let semantics = graphics_core::semantics::ImageSemantics::Color { color_space: color.space, alpha_mode: graphics_core::AlphaMode::Straight };
	match graphics_core::pixel::Decoder::new(&semantics, working) {
		Ok(decoder) => decoder.decode(Rgba::new(color.red, color.green, color.blue, color.alpha)),
		// A COLOUR IN A SPACE THAT CANNOT BE CONVERTED IS NOT DRAWN. Falling back to the numbers
		// untouched would paint a Rec. 2020 colour as though it were sRGB, which is a wrong colour
		// presented as a right one.
		Err(_) => Rgba::TRANSPARENT,
	}
}

/// Build the shader a command's paint implies.
///
/// THE INVERSE IS COMPUTED ONCE PER DRAW, not per pixel: it is a 3x3 adjugate, and a drawing that
/// inverted its transform per pixel would spend more time on the inverse than on the paint.
pub fn shader<'a>(paint: &Paint, transform: &Transform, working: Working, stops: &[Vec<GradientStop>], images: &'a dyn ImageLookup) -> Shader<'a> {
	let combined = transform.concat(&paint.transform());
	let Some(inverse) = combined.inverse() else {
		// A SINGULAR TRANSFORM COLLAPSES THE PAINT'S SPACE ONTO A LINE. There is no point to evaluate
		// the gradient at, and drawing the first stop everywhere would be a shape flooded with a
		// colour nobody chose.
		return Shader::Nothing;
	};
	match paint {
		Paint::Solid(color) => Shader::Solid(to_working(*color, working)),
		Paint::Linear { from, to, stops: handle, spread, .. } => match stops.get(handle.0 as usize) {
			Some(list) => Shader::Linear { ramp: Ramp::new(list, working), from: *from, to: *to, spread: *spread, inverse },
			None => Shader::Nothing,
		},
		Paint::Radial { from, from_radius, to, to_radius, stops: handle, spread, .. } => match stops.get(handle.0 as usize) {
			Some(list) => Shader::Radial { ramp: Ramp::new(list, working), from: *from, from_radius: *from_radius, to: *to, to_radius: *to_radius, spread: *spread, inverse },
			None => Shader::Nothing,
		},
		Paint::Conic { centre, start_angle, end_angle, stops: handle, spread, .. } => match stops.get(handle.0 as usize) {
			Some(list) => Shader::Conic { ramp: Ramp::new(list, working), centre: *centre, start_angle: *start_angle, end_angle: *end_angle, spread: *spread, inverse },
			None => Shader::Nothing,
		},
		// PLANES FIRST, because a source that has them has them INSTEAD: a video frame handed over as
		// `NV12` has no single-plane view to fall back to, and converting one here would be the
		// full-frame conversion per frame that the multi-plane model exists to remove.
		Paint::Image { image, source, quality, spread, .. } if images.planes(image.0).is_some() => match images.planes(image.0) {
			Some((view, pyramid, table)) => match Sampler::planar(view, working, *spread, table) {
				Ok(sampler) => Shader::Image { sampler, pyramid, quality: *quality, spread: *spread, inverse, source: *source },
				Err(_) => Shader::Nothing,
			},
			None => Shader::Nothing,
		},
		Paint::Image { image, source, quality, spread, .. } => match images.lookup(image.0) {
			Some((view, pyramid, table)) => match Sampler::with_table(view, working, *spread, table) {
				Ok(sampler) => Shader::Image { sampler, pyramid, quality: *quality, spread: *spread, inverse, source: *source },
				Err(_) => Shader::Nothing,
			},
			None => Shader::Nothing,
		},
	}
}

/// What a shader needs to find an image: the pixels, and the pyramid `prepare` built for them.
pub trait ImageLookup {
	fn lookup(&self, handle: u32) -> Option<(ImageView<'_>, Option<&Pyramid>, Option<&graphics_core::pixel::TransferTable>)>;

	/// The same handle as PLANES, when the source has them.
	fn planes(&self, handle: u32) -> Option<(graphics_core::planar::MultiPlaneView<'_>, Option<&Pyramid>, Option<&graphics_core::pixel::TransferTable>)> {
		let _ = handle;
		None
	}
}
