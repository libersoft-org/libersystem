//! What a backend or a conformance suite CLAIMS, checked against what the profile REQUIRES.
//!
//! ONE COMPARISON, USED BY EVERY CHECK. "Every profile feature has a backend handler", "every
//! feature has at least one conformance test" and "no test claims a feature outside the profile"
//! are the same two set differences seen three ways: what the profile has and the claims do not,
//! and what the claims have and the profile does not. Written once here, so a gate cannot implement
//! the easy direction and quietly skip the other.
//!
//! NO ALLOCATION. This is `no_std` and the crate has no dependencies; everything is an iterator over
//! borrowed slices, so the same comparison runs in a host tool and in a guest self-report.

use crate::{FeatureOwner, ProfileEntry};

/// Which features a check ranges over.
///
/// THE HANDLER CHECK IS WRONG FOR FEATURES NO BACKEND IMPLEMENTS - see `FeatureOwner`. Making the
/// range an argument is what keeps that from being a comment somebody has to remember.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
	/// Every entry: what the conformance-coverage check ranges over.
	EveryFeature,
	/// Only what a backend implements: what the handler check ranges over.
	OwnedBy(FeatureOwner),
}

impl Range {
	fn admits<F>(self, entry: &ProfileEntry<F>) -> bool {
		match self {
			Self::EveryFeature => true,
			Self::OwnedBy(owner) => entry.owner == owner,
		}
	}
}

/// A set of claimed feature names measured against a profile.
pub struct Coverage<'claims, F: 'static> {
	profile: &'static [ProfileEntry<F>],
	claims: &'claims [&'claims str],
	range: Range,
}

impl<'claims, F: 'static> Coverage<'claims, F> {
	pub fn new(profile: &'static [ProfileEntry<F>], claims: &'claims [&'claims str], range: Range) -> Self {
		Self { profile, claims, range }
	}

	/// What the profile requires in this range and nothing claims.
	pub fn missing(&self) -> impl Iterator<Item = &'static ProfileEntry<F>> + use<'_, 'claims, F> {
		self.profile.iter().filter(|entry| self.range.admits(entry)).filter(|entry| !self.claims.contains(&entry.name))
	}

	/// What is claimed and is not in the profile at all.
	///
	/// THIS IS THE CHECK NO REVIEWER MAKES. A conformance test named for a feature the profile does
	/// not have is a test measuring an extension while reporting Profile 1 coverage.
	pub fn outside_profile(&self) -> impl Iterator<Item = &'claims str> + use<'_, 'claims, F> {
		self.claims.iter().copied().filter(|name| !self.profile.iter().any(|entry| entry.name == *name))
	}

	/// Claimed, in the profile, but outside this check's range - a backend handler for a feature no
	/// backend owns, which is the stub the owner field exists to prevent.
	pub fn outside_range(&self) -> impl Iterator<Item = &'claims str> + use<'_, 'claims, F> {
		self.claims.iter().copied().filter(|name| self.profile.iter().any(|entry| entry.name == *name && !self.range.admits(entry)))
	}

	/// Nothing required is missing, nothing claimed is unknown, and nothing claimed is out of range.
	pub fn complete(&self) -> bool {
		self.missing().next().is_none() && self.outside_profile().next().is_none() && self.outside_range().next().is_none()
	}

	/// How many entries this check ranges over, which is what makes "0 of 0 missing" readable as the
	/// vacuous pass it is rather than as a completed check.
	pub fn required(&self) -> usize {
		self.profile.iter().filter(|entry| self.range.admits(entry)).count()
	}
}
