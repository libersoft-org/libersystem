//! What `render2d` is BOUNDED by.
//!
//! WHY A RECORDING API NEEDS LIMITS AT ALL. Without them a display list is an unbounded allocation
//! driven by application data: a document decides how many path points, how deep a clip stack and
//! how many filter nodes this process allocates for. That is the shape this tree bounds everywhere
//! else, and a 2D API is not the place to stop.
//!
//! THE GUARANTEED MINIMA ARE PART OF THE PROFILE, which is what keeps "supports Profile 1" from
//! meaning "accepts ten path points". A backend may raise any of them; none may be lowered.

/// The bounds a `render2d` implementation declares.
///
/// EVERY FIELD IS A COUNT OR A BYTE SIZE THE RECORDER CAN CHECK BEFORE ALLOCATING. A limit that
/// could only be discovered by exceeding it is a limit that has already allocated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Render2DLimits {
	/// Commands in one display list.
	pub max_commands: u32,
	/// Distinct resources - images, gradients, paths, glyph runs - one list may reference.
	pub max_resources: u32,
	/// Verbs in one path.
	pub max_path_verbs: u32,
	/// Points in one path.
	pub max_path_points: u32,
	/// Subpaths in one path.
	pub max_subpaths: u32,
	/// Nested clips.
	pub max_clip_depth: u32,
	/// Nested layers.
	pub max_layer_depth: u32,
	/// Nodes in one filter graph.
	pub max_filter_nodes: u32,
	/// The largest kernel radius any filter node may ask for, in device pixels.
	pub max_filter_radius: u32,
	/// Glyphs in one run.
	pub max_glyphs_per_run: u32,
	/// The largest image extent, in pixels, on either axis.
	pub max_image_extent: u32,
	/// Pixels one layer may cover. A layer is an offscreen: its AREA is what it costs, and a bound
	/// on each axis alone would admit a one-pixel-tall layer as wide as memory.
	pub max_layer_pixels: u64,
	/// Scratch bytes `prepare` may hold for one list.
	pub max_prepared_scratch_bytes: u64,
	/// Bytes a backend may keep in its own caches for one target.
	pub max_cache_bytes: u64,
	/// Bytes one recorded display list may occupy.
	pub max_display_list_bytes: u64,
}

/// THE GUARANTEED MINIMA OF PROFILE 1. A conforming implementation accepts at least these; it may
/// declare more. They are the numbers "supports Profile 1" actually promises.
///
/// THEY ARE SIZED BY WHAT A REAL DOCUMENT NEEDS, not by what is convenient to implement: a page of
/// text is tens of thousands of path points once its glyphs are outlines, a UI is hundreds of
/// commands deep before any content, and a filter chain behind a panel is a handful of nodes with a
/// radius in the tens of pixels.
pub const RENDER2D_PROFILE_1_MINIMA: Render2DLimits = Render2DLimits { max_commands: 65_536, max_resources: 4_096, max_path_verbs: 65_536, max_path_points: 262_144, max_subpaths: 8_192, max_clip_depth: 32, max_layer_depth: 16, max_filter_nodes: 64, max_filter_radius: 256, max_glyphs_per_run: 4_096, max_image_extent: 16_384, max_layer_pixels: 64 * 1024 * 1024, max_prepared_scratch_bytes: 64 * 1024 * 1024, max_cache_bytes: 256 * 1024 * 1024, max_display_list_bytes: 32 * 1024 * 1024 };

impl Render2DLimits {
	/// Does this declaration meet Profile 1's guaranteed minima?
	///
	/// FIELD BY FIELD, AND THE FIRST SHORTFALL IS NAMED. "The limits are too small" sends a reader
	/// to compare fifteen pairs of numbers by hand, which is how the wrong one gets raised.
	pub fn meets_profile_1(&self) -> Result<(), &'static str> {
		let minima = RENDER2D_PROFILE_1_MINIMA;
		let checks: [(&'static str, u64, u64); 15] = [
			("max_commands", u64::from(self.max_commands), u64::from(minima.max_commands)),
			("max_resources", u64::from(self.max_resources), u64::from(minima.max_resources)),
			("max_path_verbs", u64::from(self.max_path_verbs), u64::from(minima.max_path_verbs)),
			("max_path_points", u64::from(self.max_path_points), u64::from(minima.max_path_points)),
			("max_subpaths", u64::from(self.max_subpaths), u64::from(minima.max_subpaths)),
			("max_clip_depth", u64::from(self.max_clip_depth), u64::from(minima.max_clip_depth)),
			("max_layer_depth", u64::from(self.max_layer_depth), u64::from(minima.max_layer_depth)),
			("max_filter_nodes", u64::from(self.max_filter_nodes), u64::from(minima.max_filter_nodes)),
			("max_filter_radius", u64::from(self.max_filter_radius), u64::from(minima.max_filter_radius)),
			("max_glyphs_per_run", u64::from(self.max_glyphs_per_run), u64::from(minima.max_glyphs_per_run)),
			("max_image_extent", u64::from(self.max_image_extent), u64::from(minima.max_image_extent)),
			("max_layer_pixels", self.max_layer_pixels, minima.max_layer_pixels),
			("max_prepared_scratch_bytes", self.max_prepared_scratch_bytes, minima.max_prepared_scratch_bytes),
			("max_cache_bytes", self.max_cache_bytes, minima.max_cache_bytes),
			("max_display_list_bytes", self.max_display_list_bytes, minima.max_display_list_bytes),
		];
		for (name, declared, required) in checks {
			if declared < required {
				return Err(name);
			}
		}
		Ok(())
	}
}
