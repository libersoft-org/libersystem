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

use crate::Error;
use crate::canonical::{Coverage, face_for};

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

	/// The faces to try for a run of this script and language, in order, the last resort last.
	///
	/// THREE BANDS, STATED RATHER THAN IMPLIED: the faces preferred for this script AND language,
	/// then those preferred for the script whatever the language, then the general faces. Within
	/// each band the stated rank decides, and the face id breaks the tie.
	pub fn order_for(&self, script: Script, language: [u8; 4]) -> Result<Vec<u16>, Error> {
		let mut out = Vec::new();
		self.preferred_into(script, language, &mut out)?;
		// THE LAST RESORT IS LAST, and it is in the list rather than a special case afterwards: a
		// caller that walks this order and finds nothing has been told everything the policy knows.
		if let Some(face) = self.last_resort
			&& !out.contains(&face)
		{
			crate::room(&mut out)?;
			out.push(face);
		}
		Ok(out)
	}

	/// The three preference bands alone, written over `out`.
	pub fn preferred_into(&self, script: Script, language: [u8; 4], out: &mut Vec<u16>) -> Result<(), Error> {
		out.clear();
		let bands: [&dyn Fn(&FaceEntry) -> bool; 3] = [&|entry| entry.script == Some(script) && entry.language == Some(language), &|entry| entry.script == Some(script) && entry.language.is_none(), &|entry| {
			entry.script.is_none()
		}];
		for band in bands {
			for entry in self.entries.iter().filter(|entry| band(entry)) {
				if !out.contains(&entry.face) {
					crate::room(out)?;
					out.push(entry.face);
				}
			}
		}
		Ok(())
	}

	/// The face nothing else need cover.
	pub fn last_resort(&self) -> Option<u16> {
		self.last_resort
	}
}

/// The face for one cluster, under the policy: THE QUESTION STAGE 4 ASKS.
///
/// WHOLE CLUSTERS, ASKED IN THE CANONICAL VIEW. A face that covers the composition or the
/// decomposition covers the cluster; a face that covers only part of it does not, which is what
/// stops a combining sequence being split across two faces.
///
/// THE CEILING BOUNDS THE PREFERENCE BANDS AND NEVER THE LAST RESORT. A cluster no face covers would
/// otherwise try every face in the catalogue, once per cluster, and the coverage question is not
/// free - so at most `FALLBACK_FACES` faces are asked. But the last resort is the face whose job is
/// exactly the cluster nothing else covers, and it is the LAST entry of the order: a ceiling cut off
/// the end of that order would refuse the very cluster it exists for. So it is always among the
/// faces asked, and the preferred faces get the rest of the ceiling.
pub fn face_for_cluster(preferred: &[u16], last_resort: Option<u16>, coverage: &impl Coverage, cluster: &[char]) -> Option<u16> {
	let ceiling = opentype_profile::limits::FALLBACK_FACES as usize;
	let asked = &preferred[..preferred.len().min(if last_resort.is_some() { ceiling - 1 } else { ceiling })];
	let last = last_resort.filter(|face| !asked.contains(face));
	face_for(coverage, asked.iter().copied().chain(last), cluster)
}
