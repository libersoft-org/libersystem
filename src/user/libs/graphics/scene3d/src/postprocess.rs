//! HDR AND POST-PROCESSING: the bloom threshold and pyramid, the fog, and the tone map that is last.
//!
//! TONE MAPPING IS LAST, AFTER BLOOM AND FOG, and the profile says why: both are defined on LINEAR
//! RADIANCE, and a tone-mapped input would already have compressed the highlights they exist to
//! spread. That ordering is the reason `pbr::shade` does not clamp its own output - a material that
//! saturated at 1 would have thrown the highlights away before bloom ever saw them.
//!
//! THE OPERATOR IS THE 2D IMAGE-COLOUR PROFILE'S, NOT A SECOND ONE. Extended Reinhard on LUMINANCE
//! at that profile's own `WHITE`, which is read from the profile crate rather than written again
//! here. Two operators in one system is two systems, and the one thing that guarantees the 3D frame
//! and a 2D image of it look alike is that the curve is literally the same.
//!
//! ON LUMINANCE AND NOT PER CHANNEL, for the same reason the image profile gives: a per-channel
//! curve shifts hue, and it shifts it most on exactly the saturated colours the wide-gamut path
//! exists for.

use render_math::{Vec3, exp};

/// Rec. 709 luminance in linear light, which is the same triple the image-colour profile uses.
pub const LUMINANCE_REC709: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

/// The luminance the bloom knee is centred on.
pub const BLOOM_THRESHOLD: f32 = 1.0;

/// Half the width of the knee: nothing below `THRESHOLD - KNEE`, the whole excess above
/// `THRESHOLD + KNEE`.
pub const BLOOM_KNEE: f32 = 0.5;

/// How many levels the bloom pyramid has.
pub const BLOOM_LEVELS: u32 = 6;

/// How much of the pyramid is added back by default.
pub const BLOOM_WEIGHT: f32 = 0.04;

/// The luminance mapped to 1.0, taken from the image-colour profile so there is one of it.
pub fn tone_map_white() -> f32 {
	graphics_profile::image::tone_map::WHITE as f32
}

/// Linear Rec. 709 luminance.
pub fn luminance(colour: Vec3) -> f32 {
	colour.dot(LUMINANCE_REC709)
}

/// What a fragment contributes to the bloom pyramid.
///
/// A SOFT KNEE AND NOT A HARD THRESHOLD, because a hard one makes a moving highlight pop in and out:
/// a specular glint drifting across a surface crosses the threshold in one frame and the bloom
/// appears at full strength, which reads as a flash.
///
/// THE QUADRATIC IS THE ONE THAT JOINS SMOOTHLY AT BOTH ENDS. `(L - T + K)^2 / (4K)` is zero with
/// zero slope at `T - K` and equals `L - T` with unit slope at `T + K`, so the curve and its
/// derivative are continuous across the whole knee - which is what "and a quadratic between" has to
/// mean for two implementations to agree.
pub fn bloom_prefilter(colour: Vec3) -> Vec3 {
	let light = luminance(colour);
	if !(light > BLOOM_THRESHOLD - BLOOM_KNEE) {
		// WRITTEN AS A NEGATED COMPARISON because every comparison with NaN is false: a NaN
		// luminance contributes nothing rather than spreading through the whole pyramid.
		return Vec3::new(0.0, 0.0, 0.0);
	}
	let excess = if light >= BLOOM_THRESHOLD + BLOOM_KNEE {
		light - BLOOM_THRESHOLD
	} else {
		let over = light - BLOOM_THRESHOLD + BLOOM_KNEE;
		over * over / (4.0 * BLOOM_KNEE)
	};
	if light <= 0.0 {
		return Vec3::new(0.0, 0.0, 0.0);
	}
	// THE COLOUR KEEPS ITS HUE: the excess is a fraction of the luminance and scales all three
	// channels, rather than each channel being thresholded on its own.
	colour.scale(excess / light)
}

/// The 13-tap downsample, whose weights partition unity.
///
/// THIRTEEN TAPS AND NOT A BOX, because a box downsample of a bright point produces a pyramid whose
/// shape depends on where the point fell in the texel grid - the bloom of a moving highlight then
/// pulses as it crosses texel boundaries. The inner four taps at half a texel are what remove it.
pub fn downsample_13(at: (f32, f32), texel: (f32, f32), mut sample: impl FnMut(f32, f32) -> Vec3) -> Vec3 {
	let mut fetch = |x: f32, y: f32| sample(at.0 + x * texel.0, at.1 + y * texel.1);
	let outer = fetch(-2.0, -2.0).add(fetch(2.0, -2.0)).add(fetch(-2.0, 2.0)).add(fetch(2.0, 2.0));
	let edges = fetch(0.0, -2.0).add(fetch(-2.0, 0.0)).add(fetch(2.0, 0.0)).add(fetch(0.0, 2.0));
	let inner = fetch(-1.0, -1.0).add(fetch(1.0, -1.0)).add(fetch(-1.0, 1.0)).add(fetch(1.0, 1.0));
	let centre = fetch(0.0, 0.0);
	// 0.125 + 4 x 0.03125 + 4 x 0.0625 + 4 x 0.125 = 1.
	centre.scale(0.125).add(outer.scale(0.031_25)).add(edges.scale(0.0625)).add(inner.scale(0.125))
}

/// The 9-tap tent upsample, whose weights also partition unity.
pub fn upsample_tent_9(at: (f32, f32), radius: (f32, f32), mut sample: impl FnMut(f32, f32) -> Vec3) -> Vec3 {
	let mut fetch = |x: f32, y: f32| sample(at.0 + x * radius.0, at.1 + y * radius.1);
	let corners = fetch(-1.0, -1.0).add(fetch(1.0, -1.0)).add(fetch(-1.0, 1.0)).add(fetch(1.0, 1.0));
	let edges = fetch(0.0, -1.0).add(fetch(-1.0, 0.0)).add(fetch(1.0, 0.0)).add(fetch(0.0, 1.0));
	let centre = fetch(0.0, 0.0);
	// (1 + 2 + 1 + 2 + 4 + 2 + 1 + 2 + 1) / 16 = 1.
	corners.add(edges.scale(2.0)).add(centre.scale(4.0)).scale(1.0 / 16.0)
}

/// Exponential-squared fog.
///
/// `f = exp(-(density * distance)^2)`, SQUARED AND NOT LINEAR, because the squared form has no
/// visible start plane: a linear fog begins abruptly at a distance the author picked, and that edge
/// sweeps across the world as the camera moves.
///
/// `f` IS HOW MUCH OF THE SURFACE SURVIVES: 1 at the camera and 0 far away, so the mix goes toward
/// the fog colour with distance.
pub fn fog(colour: Vec3, fog_colour: Vec3, density: f32, distance: f32) -> Vec3 {
	if !density.is_finite() || !distance.is_finite() || density <= 0.0 {
		return colour;
	}
	let depth = density * distance.max(0.0);
	let surviving = exp(-(depth * depth)).clamp(0.0, 1.0);
	fog_colour.lerp(colour, surviving)
}

/// Add the pyramid back to the frame.
pub fn combine(scene: Vec3, bloom: Vec3, weight: f32) -> Vec3 {
	scene.add(bloom.scale(weight))
}

/// Extended Reinhard on luminance, at the image-colour profile's white point.
///
/// `Ld = L * (1 + L / WHITE^2) / (1 + L)`, APPLIED TO THE WHOLE RANGE. It is a global operator and
/// compressing everything is what it is for: `WHITE` is "the luminance mapped to 1.0", so a scene
/// whose diffuse white sits at 1 maps to 0.53 and something four times brighter maps to 1. That is
/// an exposure decision the scene makes by choosing its radiances, not something the curve should
/// take back by leaving part of its range alone.
///
/// AND THE CURVE IS THE IDENTITY BELOW THE PROFILE'S KNEE, which is what lets ONE operator serve
/// this path and the 2D one at once.
///
/// THE 2D PATH HAD A GUARD AT A LUMINANCE OF ONE AND THIS ONE HAD NONE, and this note used to
/// record that as a divergence nothing pinned. It was a real defect and it is fixed: extended
/// Reinhard without a knee is 0.53125 at diffuse white, so a guard that passed everything at or
/// below one through unchanged produced a 47 per cent STEP - 1.000000 at `L = 1.0` and 0.531280 at
/// `L = 1.0001`. The guard was reaching for the right thing and the curve under it could not
/// provide it, because a guard like that belongs with a curve that IS the identity at its knee.
///
/// SO THE PROFILE NAMES A KNEE AND BOTH PATHS CALL THE SAME FUNCTION. What a scene gives up is the
/// compression of its darkest range, which it was never spending - below the knee the curve was
/// already within a few per cent of the identity. What a compositor gains is that content an author
/// already put inside the range comes back as itself.
pub fn tone_map(colour: Vec3) -> Vec3 {
	let light = luminance(colour);
	// EVERY COMPARISON WITH NaN IS FALSE, and this is written so a NaN falls through unmapped rather
	// than being scaled by a NaN ratio - which would spread one bad pixel over the whole frame at
	// the next downsample. A luminance of zero falls through for the same reason: the ratio below is
	// a division by it.
	if !matches!(light.partial_cmp(&0.0), Some(core::cmp::Ordering::Greater)) {
		return colour;
	}
	let mapped = graphics_profile::image::tone_map::map_with(light as f64, tone_map_white() as f64) as f32;
	colour.scale(mapped / light)
}

/// The whole chain in the profile's order.
///
/// THE SCENE COLOUR ARRIVES ALREADY FOGGED, because fog needs the fragment's own view-space depth
/// and that is known where the fragment is shaded rather than here. The consequence is the order the
/// profile asks for: the bloom pyramid is built over the fogged frame, and tone mapping is last.
pub fn resolve(fogged_scene: Vec3, bloom: Vec3, weight: f32) -> Vec3 {
	tone_map(combine(fogged_scene, bloom, weight))
}

// -------------------------------------------------------------------------------------------------
// The resources the chain runs over, and the passes that run it.
//
// THE ARITHMETIC ABOVE IS WHAT EACH PASS COMPUTES; THIS IS WHICH PASSES THERE ARE. A layer that had
// only the operators would still have to decide how many targets a bloom needs, what each reads and
// what order they run in - and every layer would decide it differently, which is the same drift the
// frozen operators exist to prevent.
// -------------------------------------------------------------------------------------------------

/// The smallest render extent a bloom pyramid fits in.
///
/// SIX HALVINGS NEED SIXTY-FOUR TEXELS. The profile fixes the pyramid at six levels, so the
/// coarsest is a sixty-fourth of each dimension - and below that a level would have no texels at
/// all. REFUSED RATHER THAN SHORTENED: a pyramid with fewer levels has a different shape at the
/// same weight, so a small window would bloom differently from a large one rather than not at all.
pub const BLOOM_MINIMUM_EXTENT: u32 = 1 << BLOOM_LEVELS;

/// The bloom pyramid's targets, FINEST FIRST: `BLOOM_LEVELS` of them, each half the extent of the
/// one before it and the first half the render's own.
///
/// `RGBA16F` LIKE THE TARGET THEY COME FROM, because the pyramid carries linear radiance above one -
/// that is the whole of what a bloom is - and a normalised format would clip exactly the excess the
/// threshold selected.
pub fn bloom_pyramid_desc(width: u32, height: u32) -> Result<alloc::vec::Vec<render3d::resource::TextureDesc>, crate::scene::Error> {
	if width < BLOOM_MINIMUM_EXTENT || height < BLOOM_MINIMUM_EXTENT {
		return Err(crate::scene::Error::Degenerate { reason: "a render too small for a six-level bloom pyramid, whose coarsest level would have no texels" });
	}
	let mut levels = alloc::vec::Vec::with_capacity(BLOOM_LEVELS as usize);
	let (mut level_width, mut level_height) = (width, height);
	for _ in 0..BLOOM_LEVELS {
		level_width /= 2;
		level_height /= 2;
		levels.push(render3d::resource::TextureDesc {
			dimension: render3d::resource::TextureDimension::D2,
			width: level_width,
			height: level_height,
			depth: 1,
			// NO MIP CHAIN ON A PYRAMID LEVEL. The pyramid IS the chain, one target per level,
			// because each level is written by its own pass and a mip of a render target is not.
			mip_levels: 1,
			layers: 1,
			samples: 1,
			format: crate::environment::HDR_FORMAT,
			usage: render3d::resource::TextureUsage { sampled: true, colour_attachment: true, ..Default::default() },
		});
	}
	Ok(levels)
}

/// The targets one frame's chain runs over.
///
/// TWO CHAINS AND NOT ONE, which is a resource decision rather than a style: the tent upsample reads
/// the level below it AND the level it is adding into, and a pass that samples the target it writes
/// is undefined in the resource model and unorderable in the graph. So the descending pass writes
/// `pyramid` and the ascending one writes `ascent`, each target written exactly once per frame. The
/// cost is five more targets at a sixty-fourth to a quarter of the render's area; the alternative is
/// a copy per level, which is the same memory moved twice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chain<'a> {
	/// The HDR target the scene is drawn into, which `environment::hdr_target_desc` describes.
	pub scene: u32,
	/// The bloom pyramid the downsamples write, finest first, as `bloom_pyramid_desc` describes it.
	pub pyramid: &'a [u32],
	/// What the upsamples write, finest first: one per level EXCEPT the coarsest, which is where the
	/// ascent starts and so is read out of `pyramid` directly.
	pub ascent: &'a [u32],
}

/// Which pass is which, beside the graph that orders them.
///
/// THE IDENTIFIERS COME BACK NAMED rather than being left for the caller to count out of the graph:
/// a caller has to record commands into each of them, and "the fourth pass" is how a bloom comes to
/// be built in the wrong order by a caller that inserted one.
#[derive(Clone, PartialEq, Debug)]
pub struct Passes {
	pub graph: crate::graph::PassGraph,
	/// The lighting pass, which writes the HDR target.
	pub scene: u32,
	/// One per level, FINEST FIRST. The first reads the scene target and applies the threshold; each
	/// of the rest reads the level above it.
	pub downsample: alloc::vec::Vec<u32>,
	/// One per level except the coarsest, COARSEST FIRST: each reads the level below it and its own
	/// pyramid level, adds them with the 9-tap tent, and writes the ascent target.
	pub upsample: alloc::vec::Vec<u32>,
	/// Fog, the combine and the tone map, drawn to the screen. WRITES NO OFFSCREEN TARGET, which is
	/// what makes it the pass that ends the frame.
	pub resolve: u32,
}

/// Build the frame's pass graph: the lighting pass, the bloom pyramid's two halves, and the resolve.
///
/// THE ORDER IS DERIVED FROM WHAT EACH PASS READS and not declared here, which is the pass graph's
/// own rule - so a layer that inserts a pass of its own between two of these gets an order that
/// accounts for it rather than one that ignores it.
pub fn chain(chain: &Chain<'_>, first_pass: u32) -> Result<Passes, crate::scene::Error> {
	if chain.pyramid.len() != BLOOM_LEVELS as usize {
		return Err(crate::scene::Error::Degenerate { reason: "a bloom pyramid that is not the profile's six levels, which is a different bloom at the same weight" });
	}
	if chain.ascent.len() != BLOOM_LEVELS as usize - 1 {
		return Err(crate::scene::Error::Degenerate { reason: "an ascending chain that is not one target per upsample, which leaves a level with nowhere to be written" });
	}
	// ONE TARGET CANNOT BE TWO OF THEM. Two passes writing one target have no order between them,
	// and a pass reading the target it writes is undefined in the resource model - so every
	// identifier in the frame is checked against every other rather than trusted.
	let mut seen: alloc::vec::Vec<u32> = alloc::vec![chain.scene];
	for target in chain.pyramid.iter().chain(chain.ascent.iter()) {
		if seen.contains(target) {
			return Err(crate::scene::Error::Degenerate { reason: "two passes of one frame writing the same target, which has no order and no answer" });
		}
		seen.push(*target);
	}
	let mut graph = crate::graph::PassGraph::new();
	let mut next = first_pass;
	let mut identify = || {
		let id = next;
		next += 1;
		id
	};

	let scene = identify();
	graph.add(crate::graph::Pass { id: scene, writes: alloc::vec![chain.scene], reads: alloc::vec::Vec::new() });

	let mut downsample = alloc::vec::Vec::with_capacity(BLOOM_LEVELS as usize);
	for (index, target) in chain.pyramid.iter().enumerate() {
		let id = identify();
		// THE FIRST READS THE SCENE AND THRESHOLDS IT; the rest read the level above. The threshold
		// is applied ONCE, at the top, because applying it at every level would subtract the knee
		// again from a value that has already passed it.
		let source = if index == 0 { chain.scene } else { chain.pyramid[index - 1] };
		graph.add(crate::graph::Pass { id, writes: alloc::vec![*target], reads: alloc::vec![source] });
		downsample.push(id);
	}

	let mut upsample = alloc::vec::Vec::with_capacity(BLOOM_LEVELS as usize - 1);
	// COARSEST FIRST, which is the direction a tent upsample runs: each level is widened onto the
	// one above it and the sum is written to that level's ascent target. The coarsest step reads the
	// pyramid on both inputs because there is no ascent above it yet.
	for index in (0..chain.ascent.len()).rev() {
		let id = identify();
		let below = if index + 1 == chain.ascent.len() { chain.pyramid[index + 1] } else { chain.ascent[index + 1] };
		graph.add(crate::graph::Pass { id, writes: alloc::vec![chain.ascent[index]], reads: alloc::vec![below, chain.pyramid[index]] });
		upsample.push(id);
	}

	let resolve = identify();
	graph.add(crate::graph::Pass { id: resolve, writes: alloc::vec::Vec::new(), reads: alloc::vec![chain.scene, chain.ascent[0]] });

	// THE GRAPH IS ORDERED HERE RATHER THAN LEFT FOR THE CALLER TO DISCOVER IS BROKEN. Every refusal
	// this function can produce is about the frame it was asked for, so it belongs to the call that
	// asked rather than to the one that runs.
	graph.order()?;
	Ok(Passes { graph, scene, downsample, upsample, resolve })
}
