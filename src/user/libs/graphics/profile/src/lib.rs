//! `Render2D Core Profile 1` and `Render3D Core Profile 1`, as CODE rather than as tables in a
//! Markdown file.
//!
//! WHY THE CANONICAL FORM IS CODE. A profile that exists only as prose drifts away from the backend
//! and the conformance suite the first time somebody adds a feature in one place. Here each list is
//! an enumeration, each profile is a constant over it, and the documentation table, the capability
//! report, the backend checklist and the conformance matrix are all GENERATED from these - with a
//! hash, so a change to a profile is visible rather than discovered.
//!
//! BOTH PROFILES LIVE HERE, in one crate with one shape, because they are checked by one gate and
//! read by one reviewer. The 3D list is the profile `render3d` is measured against; it is written
//! before `render3d` exists for the same reason the 2D one is - the checks are cheap only while the
//! backends they range over are still being written.
//!
//! EVERY FEATURE NAMES ITS OWNER, because "every feature has a backend handler" is the wrong check
//! for features no backend implements. Path boolean operations, hit testing, bounds, length and
//! point-at-distance are geometry-core functions a rasteriser never sees; demanding a handler for
//! them would force a stub whose only purpose is to satisfy a gate. The handler check applies to
//! `Backend`-owned features; the conformance-coverage check applies to all of them.

#![cfg_attr(not(test), no_std)]

/// Who implements a feature, which decides which checks apply to it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FeatureOwner {
	/// The shared image, colour and conversion model.
	GraphicsCore,
	/// The backend-neutral 2D library: recording, validation, geometry and queries.
	Render2D,
	/// The backend-neutral 3D library: resource and pipeline description, passes, command lists.
	Render3D,
	/// The retained scene layer above `render3d`.
	Scene3D,
	/// A rasteriser. These are the features the "every feature has a handler" check ranges over.
	Backend,
}

/// One profile entry: the feature, the group it is documented under, and who implements it.
///
/// GENERIC OVER THE FEATURE, so the 2D and the 3D profile are the same shape rather than two shapes
/// a generator and a gate would each have to know twice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProfileEntry<F: 'static> {
	pub feature: F,
	/// The documentation group, which is what the generated table is organised by.
	pub group: &'static str,
	pub owner: FeatureOwner,
	/// The name this feature is written as everywhere - the enum variant's own spelling, so a
	/// generated table, a capability report and a conformance row cannot disagree about it.
	pub name: &'static str,
}

/// Build a profile constant out of `group, owner, variant` rows.
///
/// THE NAME IS THE VARIANT'S OWN SPELLING, taken by `stringify!` rather than written a second time
/// beside it: a list where the name and the variant are two strings is a list where they can differ.
macro_rules! profile {
	($(#[$attribute:meta])* $constant:ident : $feature:ident ; $($group:literal, $owner:ident, $variant:ident;)+) => {
		$(#[$attribute])*
		pub const $constant: &[ProfileEntry<$feature>] = &[
			$(ProfileEntry { feature: $feature::$variant, group: $group, owner: FeatureOwner::$owner, name: stringify!($variant) },)+
		];
	};
}

// DECLARED AFTER THE MACRO, because `macro_rules!` is in scope only for what follows it.
pub mod capability;
pub mod limits;
pub mod render2d;
pub mod render3d;

pub use capability::{Coverage, Range};
pub use limits::{RENDER2D_PROFILE_1_MINIMA, Render2DLimits};
pub use render2d::{RENDER2D_CORE_PROFILE_1, RENDER2D_GROUPS, Render2DFeature};
pub use render3d::{RENDER3D_CORE_PROFILE_1, RENDER3D_GROUPS, Render3DFeature};

#[cfg(test)]
mod tests;
