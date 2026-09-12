//! Font fallback: a POLICY with a stated order, not an accident of enumeration.
//!
//! FALLBACK THAT DEPENDS ON DIRECTORY ORDER IS A RENDERING DIFFERENCE BETWEEN TWO MACHINES. Two
//! installations with the same faces, mounted in a different order, would pick different fonts for
//! the same string - and the difference is invisible to whoever reports it, because both machines
//! are "just using the system font". So the order here is stated, total, and derived from the faces
//! themselves rather than from how they were found.
//!
//! IT OPERATES ON WHOLE CLUSTERS AND SYLLABLES. Splitting a combining sequence, an Indic syllable,
//! an Arabic joining context or an emoji ZWJ sequence across two faces is the defect this exists to
//! prevent: an `e` from one font and its acute from another do not line up, an Indic syllable split
//! at its virama loses the conjunct entirely, and a joining context split mid-word turns cursive
//! letters back into isolated ones.
//!
//! AND THE QUESTION IS ASKED IN THE CANONICAL VIEW, which is the previous item's rule and is what
//! makes the composed and decomposed spellings of one string take the same face.

use alloc::vec::Vec;
use unicode_tables::Script;

use crate::canonical::{CanonicalView, Coverage};

/// One face as the policy sees it: what it is, and what it is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FaceEntry {
	pub face: u16,
	/// The script this face is PREFERRED for, if any. A face with none is a general one.
	pub script: Option<Script>,
	/// The language tag it is preferred for, if any - which is how one Han face is chosen over
	/// another for Japanese rather than for Chinese, where the same character is drawn differently.
	pub language: Option<[u8; 4]>,
	/// The tie-break within a preference band: LOWER IS EARLIER. Stated by whoever installed the
	/// face rather than taken from the order it was found in.
	pub rank: u16,
}

/// The installed faces, in the order the policy will try them.
///
/// THE ORDER IS COMPUTED ONCE AND IS TOTAL. Two faces that tie on every stated criterion are ordered
/// by their face id, which is derived from the face's own content identity - so two machines with
/// the same faces produce the same order however they found them.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Policy {
	entries: Vec<FaceEntry>,
	/// The face used when nothing else covers a cluster. It is tried LAST and it is what makes the
	/// answer "a visible box" rather than "nothing was drawn".
	last_resort: Option<u16>,
}

impl Policy {
	pub fn new(entries: &[FaceEntry], last_resort: Option<u16>) -> Self {
		let mut entries = entries.to_vec();
		// The total order: the face id breaks every tie, and it is content-derived rather than
		// positional - which is the whole of "deterministic across machines".
		entries.sort_by_key(|entry| (entry.rank, entry.face));
		Self { entries, last_resort }
	}

	/// The faces to try for a run of this script and language, in order.
	///
	/// THREE BANDS, STATED RATHER THAN IMPLIED: the faces preferred for this script AND language,
	/// then those preferred for the script whatever the language, then the general faces. Within
	/// each band the stated rank decides, and the face id breaks the tie.
	pub fn order_for(&self, script: Script, language: [u8; 4]) -> Vec<u16> {
		let mut out: Vec<u16> = Vec::new();
		let push = |face: u16, out: &mut Vec<u16>| {
			if !out.contains(&face) {
				out.push(face);
			}
		};
		for entry in &self.entries {
			if entry.script == Some(script) && entry.language == Some(language) {
				push(entry.face, &mut out);
			}
		}
		for entry in &self.entries {
			if entry.script == Some(script) && entry.language.is_none() {
				push(entry.face, &mut out);
			}
		}
		for entry in &self.entries {
			if entry.script.is_none() {
				push(entry.face, &mut out);
			}
		}
		// THE LAST RESORT IS LAST, and it is in the list rather than a special case afterwards: a
		// caller that walks this order and finds nothing has been told everything the policy knows.
		if let Some(face) = self.last_resort {
			push(face, &mut out);
		}
		out
	}
}

/// The policy and the faces' coverage, together - which is what a cluster is actually asked of.
pub struct Faces<'a, C: Coverage> {
	pub policy: &'a Policy,
	pub coverage: &'a C,
	pub script: Script,
	pub language: [u8; 4],
}

impl<C: Coverage> Coverage for Faces<'_, C> {
	fn covers(&self, face: u16, spelling: &[char]) -> bool {
		self.coverage.covers(face, spelling)
	}

	fn faces(&self) -> &[u16] {
		// The order is the policy's; `face_for` walks it.
		self.coverage.faces()
	}
}

/// The face for one cluster, under the policy.
///
/// WHOLE CLUSTERS, ASKED IN THE CANONICAL VIEW. A face that covers the composition or the
/// decomposition covers the cluster; a face that covers only part of it does not, which is what
/// stops a combining sequence being split across two faces.
pub fn face_for_cluster<C: Coverage>(faces: &Faces<'_, C>, cluster: &[char]) -> Option<u16> {
	let view = CanonicalView::of(cluster);
	// A CLUSTER NO FACE COVERS WOULD OTHERWISE TRY EVERY FACE IN THE CATALOGUE, once per cluster, and
	// the coverage question is not free: it is asked in both canonical spellings. So the walk stops
	// at the profile's ceiling, and a cluster that needed the seventeenth face is answered the same
	// way as one nothing covers at all - which is the last resort's job.
	for face in faces.policy.order_for(faces.script, faces.language).into_iter().take(opentype_profile::limits::FALLBACK_FACES as usize) {
		if faces.coverage.covers(face, &view.composed) || faces.coverage.covers(face, &view.decomposed) {
			return Some(face);
		}
	}
	None
}

/// The faces for a run of clusters, one per cluster, with the runs they form.
///
/// A RUN ENDS WHERE THE FACE CHANGES, which is what makes the shaped runs face-homogeneous - the
/// shared contract's own requirement, and what a per-character fallback could never produce.
pub fn runs_for<C: Coverage>(faces: &Faces<'_, C>, clusters: &[Vec<char>]) -> Option<Vec<(usize, usize, u16)>> {
	let mut runs: Vec<(usize, usize, u16)> = Vec::new();
	for (index, cluster) in clusters.iter().enumerate() {
		let face = face_for_cluster(faces, cluster)?;
		match runs.last_mut() {
			Some((_, end, current)) if *current == face => *end = index + 1,
			_ => runs.push((index, index + 1, face)),
		}
	}
	Some(runs)
}
