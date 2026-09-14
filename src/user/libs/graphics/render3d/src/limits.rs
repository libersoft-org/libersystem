//! BOUND THE DEVICE, NOT THE FRAME.
//!
//! A LIMIT ON A FRAME IS A LIMIT NOBODY CAN PLAN AGAINST. What a caller needs to know is what the
//! device will accept - the same answer every frame - so that a scene can be built against it once.
//! `Render3DLimits` is that answer, and it is handed back at construction alongside the device.
//!
//! A FAILED CHECK REFUSES OR NEGOTIATES AND NEVER SILENTLY CLAMPS. A caller given a quiet reduction
//! finds out three frames later, in a failure that names neither the number it asked for nor the one
//! it got. So:
//!
//!   * over the implementation's HARD MAXIMUM is `LimitExceeded`, carrying both numbers. It is
//!     permanent: the same request will be refused again.
//!   * over the Domain's BUDGET is `OutOfMemory`, carrying the bytes. It is temporary, and that is
//!     why it is not the same variant.
//!   * where LESS is granted than asked, construction answers the ACTUAL limits, so the caller plans
//!     against what it has rather than what it wanted.
//!
//! THE PROFILE'S MINIMUMS ARE THE FLOOR AND THIS CHECKS AGAINST THEM. A device whose limits are below
//! `RENDER3D_PROFILE_1_MIN_LIMITS` is not a conforming device, and saying so at construction is
//! better than every later refusal being a surprise.

use crate::error::Error;
use graphics_profile::render3d_spec::RENDER3D_PROFILE_1_MIN_LIMITS;

/// What a device will accept. Every field is a hard maximum, not a hint.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Render3DLimits {
	pub max_texture_extent_2d: u32,
	pub max_texture_extent_3d: u32,
	pub max_texture_layers: u32,
	/// The largest single resource, in bytes. Separate from the extents because a legal extent at a
	/// wide format can still be larger than anything the device will allocate.
	pub max_texture_bytes: u64,
	pub max_colour_attachments: u32,
	pub max_samples: u32,
	pub max_vertex_streams: u32,
	pub max_vertex_attributes: u32,
	pub max_uniform_bytes_per_stage: u32,
	pub max_samplers_per_stage: u32,
	pub max_bound_resources: u32,
	pub max_draws_per_pass: u32,
	pub max_command_bytes: u64,
	pub max_shader_instructions: u32,
	/// The deepest loop nest and the largest trip count a shader may declare. A loop bound is part
	/// of the device's limits because an unbounded one is a frame that never ends.
	pub max_shader_loop_depth: u32,
	pub max_shader_loop_trip: u32,
	pub max_anisotropy: u32,
}

impl Render3DLimits {
	/// The profile's minimums, as a device that only just conforms. The floor, not a
	/// recommendation - and what a fixture builds a device from when it wants to test the boundary
	/// rather than a particular implementation.
	pub const PROFILE_MINIMUM: Self = Self { max_texture_extent_2d: 4096, max_texture_extent_3d: 256, max_texture_layers: 256, max_texture_bytes: 256 * 1024 * 1024, max_colour_attachments: 4, max_samples: 4, max_vertex_streams: 4, max_vertex_attributes: 16, max_uniform_bytes_per_stage: 16384, max_samplers_per_stage: 16, max_bound_resources: 64, max_draws_per_pass: 65536, max_command_bytes: 16 * 1024 * 1024, max_shader_instructions: 4096, max_shader_loop_depth: 8, max_shader_loop_trip: 65536, max_anisotropy: 8 };

	/// The value of one profile minimum, by the name the frozen table uses.
	fn field(&self, name: &str) -> Option<u32> {
		Some(match name {
			"max_texture_extent_2d" => self.max_texture_extent_2d,
			"max_texture_extent_3d" => self.max_texture_extent_3d,
			"max_texture_layers" => self.max_texture_layers,
			"max_colour_attachments" => self.max_colour_attachments,
			"max_vertex_streams" => self.max_vertex_streams,
			"max_vertex_attributes" => self.max_vertex_attributes,
			"max_draws_per_pass" => self.max_draws_per_pass,
			"max_shader_instructions" => self.max_shader_instructions,
			"max_uniform_bytes_per_stage" => self.max_uniform_bytes_per_stage,
			"max_samplers_per_stage" => self.max_samplers_per_stage,
			"max_anisotropy" => self.max_anisotropy,
			_ => return None,
		})
	}

	/// Every limit the profile states a minimum for is at or above it.
	///
	/// DRIVEN BY THE FROZEN TABLE, so a minimum added to the profile without a field here is a
	/// refusal rather than a check that silently stopped covering it - which is the failure a
	/// hand-written list of comparisons has the first time the profile grows.
	pub fn conforms(&self) -> Result<(), Error> {
		for entry in RENDER3D_PROFILE_1_MIN_LIMITS {
			let Some(value) = self.field(entry.name) else {
				return Err(Error::IncompatiblePipeline { reason: "the profile states a minimum limit this implementation has no field for" });
			};
			if value < entry.minimum {
				return Err(Error::LimitExceeded { limit: entry.name, ceiling: entry.minimum as u64, asked: value as u64 });
			}
		}
		Ok(())
	}

	/// Check one request against a hard maximum. PERMANENT refusal, with both numbers.
	pub fn admit(&self, limit: &'static str, asked: u64, ceiling: u64) -> Result<(), Error> {
		if asked > ceiling { Err(Error::LimitExceeded { limit, ceiling, asked }) } else { Ok(()) }
	}

	/// A 2D texture request, checked against the extent AND the byte size.
	///
	/// BOTH, BECAUSE EITHER CAN BE THE ONE THAT FAILS. A 4096x4096 RGBA32F texture is inside the
	/// extent limit and is 256 MB, and a device that checked only the extent would accept it and
	/// fail somewhere with no number attached.
	pub fn admit_texture_2d(&self, width: u32, height: u32, bytes_per_texel: u64) -> Result<u64, Error> {
		self.admit("texture width", width as u64, self.max_texture_extent_2d as u64)?;
		self.admit("texture height", height as u64, self.max_texture_extent_2d as u64)?;
		let bytes = (width as u64).checked_mul(height as u64).and_then(|texels| texels.checked_mul(bytes_per_texel)).ok_or(Error::LimitExceeded { limit: "texture bytes", ceiling: self.max_texture_bytes, asked: u64::MAX })?;
		self.admit("texture bytes", bytes, self.max_texture_bytes)?;
		Ok(bytes)
	}

	/// A sample count the device admits AND the profile lists.
	pub fn admit_samples(&self, samples: u32) -> Result<(), Error> {
		if !crate::msaa::SAMPLE_COUNTS.contains(&samples) {
			return Err(Error::LimitExceeded { limit: "sample count", ceiling: 4, asked: samples as u64 });
		}
		self.admit("sample count", samples as u64, self.max_samples as u64)
	}
}

/// What a device construction answers: the device's ACTUAL limits, and whether they are what was
/// asked for.
///
/// THE ACTUAL LIMITS TRAVEL WITH THE DEVICE, which is the difference between negotiating and
/// clamping. A caller that asked for eight samples and got four is told so here, once, and plans
/// against four - rather than discovering it at the first pass that asked for eight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Granted {
	pub limits: Render3DLimits,
	/// True when every field is what was requested. False means read `limits` - something is lower.
	pub exactly_as_requested: bool,
}

/// Negotiate a request down to what an implementation can do.
///
/// NEVER UP. A caller asking for less than it could have gets what it asked for: a request is a
/// statement about what the caller will use, and granting more would make the limits it plans
/// against differ from the ones it was given.
pub fn negotiate(requested: &Render3DLimits, implementation: &Render3DLimits) -> Granted {
	let limits = Render3DLimits { max_texture_extent_2d: requested.max_texture_extent_2d.min(implementation.max_texture_extent_2d), max_texture_extent_3d: requested.max_texture_extent_3d.min(implementation.max_texture_extent_3d), max_texture_layers: requested.max_texture_layers.min(implementation.max_texture_layers), max_texture_bytes: requested.max_texture_bytes.min(implementation.max_texture_bytes), max_colour_attachments: requested.max_colour_attachments.min(implementation.max_colour_attachments), max_samples: requested.max_samples.min(implementation.max_samples), max_vertex_streams: requested.max_vertex_streams.min(implementation.max_vertex_streams), max_vertex_attributes: requested.max_vertex_attributes.min(implementation.max_vertex_attributes), max_uniform_bytes_per_stage: requested.max_uniform_bytes_per_stage.min(implementation.max_uniform_bytes_per_stage), max_samplers_per_stage: requested.max_samplers_per_stage.min(implementation.max_samplers_per_stage), max_bound_resources: requested.max_bound_resources.min(implementation.max_bound_resources), max_draws_per_pass: requested.max_draws_per_pass.min(implementation.max_draws_per_pass), max_command_bytes: requested.max_command_bytes.min(implementation.max_command_bytes), max_shader_instructions: requested.max_shader_instructions.min(implementation.max_shader_instructions), max_shader_loop_depth: requested.max_shader_loop_depth.min(implementation.max_shader_loop_depth), max_shader_loop_trip: requested.max_shader_loop_trip.min(implementation.max_shader_loop_trip), max_anisotropy: requested.max_anisotropy.min(implementation.max_anisotropy) };
	Granted { exactly_as_requested: limits == *requested, limits }
}

/// A budget refusal, which is the OTHER kind and carries the bytes rather than a ceiling.
pub fn admit_budget(bytes: u64, remaining: u64) -> Result<(), Error> {
	if bytes > remaining { Err(Error::OutOfMemory { bytes }) } else { Ok(()) }
}
