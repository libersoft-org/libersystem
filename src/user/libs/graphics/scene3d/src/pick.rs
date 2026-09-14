//! PICKING: which drawable is at a pixel.
//!
//! THE ANSWER IS AN IDENTITY WRITTEN BY THE FRAME, not a depth unprojected. Reading the depth and
//! unprojecting it answers WHERE and not WHAT, and every application that tries it ends up matching
//! the world position against its own objects - which is this module, written badly, in the
//! application.
//!
//! THE IDENTITY IS A 32-BIT VALUE THE SCENE ASSIGNS AND THE APPLICATION MAY SET. ZERO IS RESERVED
//! FOR NOTHING, so a pick on the background is unambiguous rather than being drawable zero.
//!
//! A TRANSPARENT DRAWABLE WRITES NO IDENTITY, so a pick through glass answers what is behind it -
//! which is what a person clicking expects, and what a depth-based pick cannot express at all.
//!
//! THE RESULT ARRIVES WITH THE FRAME'S COMPLETION. A synchronous pick stalls the whole pipeline for
//! a cursor; `Pending` carries the submission it will be answered by, and `answer` turns the value
//! that was read back into a drawable.
//!
//! AND `resolve` IS THE SAME ANSWER COMPUTED WITHOUT A FRAME. A ray against the drawables that write
//! an identity gives what the attachment would have held - the nearest one that passed the depth
//! test - which is what an application picks with before a backend exists, and what a fixture holds
//! the two against each other with.

use render_math::{Mat4, Vec3, Viewport};

use crate::bounds::{Aabb, Sphere};
use crate::scene::{DrawableId, Error, Scene, VisibilityMask};

/// A pixel a pick asks about, inside an attachment of a stated size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PickRequest {
	pub x: u32,
	pub y: u32,
	pub width: u32,
	pub height: u32,
}

impl PickRequest {
	/// REFUSES A PIXEL OUTSIDE THE ATTACHMENT rather than clamping it, because a clamped pick
	/// answers about a pixel the caller did not ask about - and the caller has no way to tell.
	pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, Error> {
		if width == 0 || height == 0 || x >= width || y >= height {
			return Err(Error::OutsideAttachment { x, y, width, height });
		}
		Ok(Self { x, y, width, height })
	}
}

/// What a readback asks for.
///
/// THREE KINDS AND NOT ONE. An editor wants the identity, a CAD viewport wants the depth to place a
/// cursor in the world, and a colour picker wants the pixel; they read three different attachments
/// of the SAME pass, so one request type with a kind is what keeps them on one completion contract
/// rather than three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Readback {
	/// The per-drawable identity attachment.
	Identity,
	/// The depth attachment, which answers WHERE rather than WHAT.
	Depth,
	/// The colour attachment.
	Colour,
}

/// A pick in flight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pending {
	pub request: PickRequest,
	pub kind: Readback,
	/// The caller's own identifier for the readback buffer the value lands in.
	pub readback: u32,
	/// The submission serial this pick completes with. NOT A PROMISE OF A SYNCHRONOUS ANSWER: the
	/// value is readable when that submission completes and not before. A software backend may
	/// report that serial complete immediately and a GPU one later, WITHOUT THE API SHAPE CHANGING -
	/// which is the whole reason the ticket exists rather than a function that returns a value.
	pub serial: u64,
}

impl Pending {
	/// Whether a queue that has completed up to `completed` has completed this pick. Submission
	/// serials increase, so a later completion implies every earlier one.
	pub fn is_answered_by(&self, completed: u64) -> bool {
		completed >= self.serial
	}
}

/// Ask for a pick, to be answered when the frame completes.
pub fn request(kind: Readback, x: u32, y: u32, width: u32, height: u32, readback: u32, serial: u64) -> Result<Pending, Error> {
	Ok(Pending { request: PickRequest::new(x, y, width, height)?, kind, readback, serial })
}

/// Turn the identity that was read back into a drawable. `0` is NOTHING.
///
/// REFUSES A READBACK THAT IS NOT AN IDENTITY, because a depth value reinterpreted as an identity
/// names whichever drawable happens to hold that number - an answer that is wrong and looks right.
pub fn answer(scene: &Scene, pending: &Pending, read: DrawableId) -> Result<Option<u32>, Error> {
	if pending.kind != Readback::Identity {
		return Err(Error::Degenerate { reason: "a depth or colour readback asked for a drawable; only an identity readback names one" });
	}
	Ok(scene.drawable_of_id(read))
}

/// The SELECTION PASS: one pass that writes the identity attachment beside the depth it is resolved
/// against.
///
/// THE SAME PASS AS THE COLOUR AND NOT A SECOND ONE. A separate selection pass draws the scene
/// twice, and the second drawing is depth-tested against its own buffer - so an object that the
/// colour pass hid can win the identity pass, and the pick answers something the person cannot see.
pub fn selection_pass(id: u32, colour_target: u32, identity_target: u32, depth_target: u32) -> crate::graph::Pass {
	crate::graph::Pass { id, writes: alloc::vec![colour_target, identity_target, depth_target], reads: alloc::vec![] }
}

/// A ray in world space. `direction` need not be normalised; a distance is in units of it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Ray {
	pub origin: Vec3,
	pub direction: Vec3,
}

/// What a resolved pick found.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Hit {
	pub drawable: u32,
	pub id: DrawableId,
	/// How far along the ray, in units of `direction`.
	pub distance: f32,
}

/// Build the world-space ray through a window pixel.
///
/// TWO UNPROJECTED POINTS AND NOT AN EYE AND A DIRECTION. Under an ORTHOGRAPHIC projection every ray
/// is parallel and the camera position is on none of them, so the one-point construction is wrong
/// for exactly the projection where it is hardest to notice.
///
/// `inverse_view_projection` is the inverse of `projection * view`, which the caller already has
/// from building the frustum.
pub fn ray_from_window(window_x: f32, window_y: f32, viewport: &Viewport, inverse_view_projection: &Mat4) -> Result<Ray, Error> {
	if !(viewport.width > 0.0) || !(viewport.height > 0.0) {
		return Err(Error::Degenerate { reason: "a viewport with no area has no pixels to pick through" });
	}
	// REFUSED RATHER THAN CLAMPED, the same rule as a pick on the attachment.
	let inside = window_x >= viewport.x && window_y >= viewport.y && window_x <= viewport.x + viewport.width && window_y <= viewport.y + viewport.height;
	if !inside {
		return Err(Error::OutsideAttachment { x: window_x.max(0.0) as u32, y: window_y.max(0.0) as u32, width: viewport.width as u32, height: viewport.height as u32 });
	}
	// The inverse of `window_from_ndc`, INCLUDING ITS Y INVERSION - which is the one place the
	// convention could be got backwards, and getting it backwards picks the object mirrored about
	// the horizon.
	let ndc_x = ((window_x - viewport.x) / viewport.width) * 2.0 - 1.0;
	let ndc_y = 1.0 - ((window_y - viewport.y) / viewport.height) * 2.0;
	let near = unproject(Vec3::new(ndc_x, ndc_y, 0.0), inverse_view_projection)?;
	let far = unproject(Vec3::new(ndc_x, ndc_y, 1.0), inverse_view_projection)?;
	let direction = far.sub(near);
	if !direction.is_finite() || direction.length_squared() <= 0.0 {
		return Err(Error::Degenerate { reason: "the near and far unprojections coincide, so there is no ray" });
	}
	Ok(Ray { origin: near, direction })
}

fn unproject(ndc: Vec3, inverse: &Mat4) -> Result<Vec3, Error> {
	let clip = inverse.transform_point(ndc);
	clip.perspective_divide().map_err(|_| Error::Degenerate { reason: "an unprojected point with no perspective divide" })
}

/// Resolve a pick without a frame: the nearest drawable along the ray that WRITES AN IDENTITY.
///
/// THE SAME ANSWER THE ATTACHMENT WOULD HOLD. Transparent drawables are skipped because they write
/// no identity, and the nearest of the rest is the one that would have passed the depth test.
///
/// NEAREST AND NOT FIRST. The queues are sorted for drawing and the transparent one runs backwards,
/// so a pick that took the first hit in queue order would depend on which queue an object was in.
pub fn resolve(scene: &Scene, ray: &Ray, mask: VisibilityMask) -> Option<Hit> {
	let mut best: Option<Hit> = None;
	for (index, drawable) in scene.drawables().iter().enumerate() {
		if !scene.effectively_enabled(drawable.node) {
			continue;
		}
		if scene.effective_visibility(drawable.node) & drawable.visibility & mask == 0 {
			continue;
		}
		let Ok(material) = scene.material_of(drawable) else { continue };
		if !material.writes_id() {
			continue;
		}
		// A DRAWABLE WITH NO BOUNDS CANNOT BE PICKED BY A RAY. It is never culled, so it is drawn
		// and its identity is in the attachment; the ray has nothing to intersect, and answering
		// "nothing" is better than inventing a volume for it.
		let Some(local) = drawable.bounds else { continue };
		let Some(world) = scene.transforms().get(drawable.node as usize) else { continue };
		let transforms: &[Mat4] = if drawable.instances.is_empty() { core::slice::from_ref(world) } else { &[] };
		let mut nearest: Option<f32> = None;
		let consider = |transform: &Mat4| -> Option<f32> {
			// The sphere is the cheap reject; the box decides, because a sphere-only pick on a long
			// thin object selects it from well beside it.
			if hit_sphere(ray, &local.world_sphere(transform)).is_none() {
				return None;
			}
			hit_aabb(ray, &local.transformed(transform))
		};
		for transform in transforms {
			if let Some(distance) = consider(transform) {
				nearest = Some(nearest.map_or(distance, |held: f32| held.min(distance)));
			}
		}
		for instance in &drawable.instances {
			if let Some(distance) = consider(&instance.transform) {
				nearest = Some(nearest.map_or(distance, |held: f32| held.min(distance)));
			}
		}
		let Some(distance) = nearest else { continue };
		// A TIE GOES TO THE LOWER DRAWABLE INDEX, so two coincident objects pick the same one on
		// every run.
		if best.is_none_or(|found| distance < found.distance) {
			best = Some(Hit { drawable: index as u32, id: drawable.id(), distance });
		}
	}
	best
}

/// Where the ray enters the sphere, or `None`. An origin inside the sphere hits at `0`.
fn hit_sphere(ray: &Ray, sphere: &Sphere) -> Option<f32> {
	let to_centre = ray.origin.sub(sphere.centre);
	let a = ray.direction.dot(ray.direction);
	if a <= 0.0 {
		return None;
	}
	let b = 2.0 * to_centre.dot(ray.direction);
	let c = to_centre.dot(to_centre) - sphere.radius * sphere.radius;
	let discriminant = b * b - 4.0 * a * c;
	if discriminant < 0.0 {
		return None;
	}
	let root = sqrt(discriminant);
	let near = (-b - root) / (2.0 * a);
	let far = (-b + root) / (2.0 * a);
	if far < 0.0 {
		// Wholly behind the ray's origin.
		return None;
	}
	Some(if near < 0.0 { 0.0 } else { near })
}

/// The slab test. Returns where the ray enters the box, or `0` if it starts inside.
fn hit_aabb(ray: &Ray, box_bounds: &Aabb) -> Option<f32> {
	let mut enter = f32::NEG_INFINITY;
	let mut leave = f32::INFINITY;
	let origin = [ray.origin.x, ray.origin.y, ray.origin.z];
	let direction = [ray.direction.x, ray.direction.y, ray.direction.z];
	let minimum = [box_bounds.minimum.x, box_bounds.minimum.y, box_bounds.minimum.z];
	let maximum = [box_bounds.maximum.x, box_bounds.maximum.y, box_bounds.maximum.z];
	for axis in 0..3 {
		if direction[axis].abs() <= f32::MIN_POSITIVE {
			// PARALLEL TO THIS SLAB: it misses unless it is already between the planes. Dividing
			// here would give an infinity whose sign decides the answer, which is the classic way
			// this test goes wrong on an axis-aligned ray.
			if origin[axis] < minimum[axis] || origin[axis] > maximum[axis] {
				return None;
			}
			continue;
		}
		let inverse = 1.0 / direction[axis];
		let mut first = (minimum[axis] - origin[axis]) * inverse;
		let mut second = (maximum[axis] - origin[axis]) * inverse;
		if first > second {
			core::mem::swap(&mut first, &mut second);
		}
		if first > enter {
			enter = first;
		}
		if second < leave {
			leave = second;
		}
		if enter > leave {
			return None;
		}
	}
	if leave < 0.0 {
		return None;
	}
	Some(if enter < 0.0 { 0.0 } else { enter })
}

/// `no_std` has no `f32::sqrt`; this is the same Newton refinement `render-math` uses.
fn sqrt(value: f32) -> f32 {
	if !(value > 0.0) {
		return 0.0;
	}
	let mut estimate = value;
	let mut step = 0;
	while step < 24 {
		let next = 0.5 * (estimate + value / estimate);
		if next == estimate {
			break;
		}
		estimate = next;
		step += 1;
	}
	estimate
}
