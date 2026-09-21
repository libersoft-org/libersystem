//! `Scene3D Extended Profile 1`, WALKED ENTRY BY ENTRY, on the same terms as the two core profiles.
//!
//! IT IS OPTIONAL AS A WHOLE AND NOT FEATURE BY FEATURE. An implementation conforms to
//! `Scene3D Core Profile 1` while supporting none of this; one that CLAIMS Extended claims every
//! entry. So this half of the suite is walked only when the layer claims it, and when it is walked
//! every entry has to answer.
//!
//! THE SCENES ARE PROPERTY CHECKS AND NOT PICTURES, for the reason the core scene half gives: a
//! picture agrees with itself whatever order it was composed in, and these fail one at a time and
//! name the feature. The equations are exact and their expected values are computed from the
//! profile, so a second implementation that gets the same numbers is a second implementation that
//! read the same document.

pub mod animation;
pub mod detail;
pub mod environment;
pub mod limits;
pub mod material;
pub mod postprocess;
pub mod shadows;

use scene3d::Limits;

/// The limits a scene claiming Extended is allowed to be at: the profile's own floor.
///
/// A SUITE THAT BUILT ITS SCENES AT SOME LARGER SIZE WOULD NEVER REACH A LIMIT AT ALL, which is the
/// same reason the core half uses `PROFILE_MINIMUM`.
pub fn claimed() -> Limits {
	Limits::EXTENDED_MINIMUM
}

/// Whether two numbers agree to within what the profile admits for a direct term: four parts in 255.
pub fn close(left: f32, right: f32) -> bool {
	(left - right).abs() <= 4.0 / 255.0
}

/// The tighter one, for values the profile states exactly rather than as a tolerance.
pub fn exact(left: f32, right: f32) -> bool {
	(left - right).abs() <= 1e-4
}
