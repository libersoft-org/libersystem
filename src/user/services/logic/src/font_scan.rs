//! What a rescan of the font directory PUBLISHES, and what it withdraws.
//!
//! ONE GENERATION SWITCH, OR NONE. A client's view of the installed set must never become partial,
//! so a scan either publishes a whole set at a new generation or leaves the previous one current.
//! Truncating a list, dropping the offending face and carrying on, or tearing the catalogue down are
//! the three answers this rule exists instead of.
//!
//! AND THE RULE IS ORDERED, because "the previous generation stays served" is a promise that cannot
//! be kept for a face somebody overwrote. The catalogue holds metadata and identities, NOT face
//! bytes: once a privileged writer has replaced the only file it can read, an entry that still names
//! the old identity can never be resolved again - a resolve digests what it read and refuses it. So:
//!
//! ```text
//!   1. mark every ALREADY-PUBLISHED face this catalogue can no longer serve. Call that set W. Two
//!      ways in: a face whose bytes CHANGED and whose replacement must be rejected for any reason,
//!      and a face that is GONE - removed, or no longer named by the scan. The test is not what
//!      happened to the bytes; it is whether the identity can still be served
//!   2. if the admissible set - the previous faces, minus W, plus the new and unchanged ones the
//!      scan accepted - fits ALL THREE ceilings, publish it at a new generation and report what was
//!      rejected
//!   3. if it does not fit, publish the previous generation MINUS W at a new generation: nothing new
//!      is added, only what cannot be served is withdrawn, and the report names the ceiling. When W
//!      is empty this is the conservative old rule and THE GENERATION DOES NOT ADVANCE
//! ```
//!
//! A VALID NO-CHANGE SCAN PUBLISHES NOTHING and does not advance the generation either, which is
//! what makes a generation mean "something about the installed set differs".

use alloc::string::String;
use alloc::vec::Vec;

use crate::font_record::{FaceRecord, LIST_REPLY_FRAMING_BYTES, MAX_FACE_METADATA_BYTES, MAX_INSTALLED_FACES, MAX_LIST_REPLY_BYTES, RecordError};

/// A face as the catalogue publishes it: the name it is installed under, and its declared record.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PublishedFace {
	/// The file name in the font directory, which is what a client asks by.
	pub name: String,
	pub record: FaceRecord,
}

/// What is current.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Publication {
	pub generation: u64,
	pub faces: Vec<PublishedFace>,
}

/// What a scan found for one file: the name, and either its validated record or why it has none.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Observation {
	pub name: String,
	pub record: Result<FaceRecord, RecordError>,
}

/// Which of the three installation ceilings was exceeded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ceiling {
	InstalledFaces,
	FaceMetadataBytes,
	ListReplyBytes,
}

/// Why one face was not published.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RejectionReason {
	/// The declaration is malformed, incomplete, outside a vocabulary, or does not match its bytes.
	Declaration(RecordError),
	/// The declaration is well formed and DECLARES A DIFFERENT FACE than the one already published
	/// under this name. A replacement is a new version of the same face; a different identity is a
	/// different face and arrives under its own name. This is the capture case.
	Relabelled,
	/// The record is past its own ceiling.
	PastCeiling(Ceiling),
}

/// One rejected face, and why.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rejection {
	pub name: String,
	pub reason: RejectionReason,
}

/// What the scan did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScanOutcome {
	/// What is current AFTER the scan - the previous publication itself when nothing was published.
	pub publication: Publication,
	/// Did the generation advance?
	pub advanced: bool,
	/// The names withdrawn: published faces this catalogue can no longer serve.
	pub withdrawn: Vec<String>,
	/// Everything the scan refused, with its reason, which is what the report names.
	pub rejected: Vec<Rejection>,
	/// The ceiling the admissible set would have exceeded, when one was.
	pub breach: Option<Ceiling>,
}

/// Does a set fit all three installation ceilings?
fn breach_of(faces: &[PublishedFace]) -> Option<Ceiling> {
	if faces.len() > MAX_INSTALLED_FACES {
		return Some(Ceiling::InstalledFaces);
	}
	for face in faces {
		if face.record.encoded_len() > MAX_FACE_METADATA_BYTES {
			return Some(Ceiling::FaceMetadataBytes);
		}
	}
	if list_reply_len(faces) > MAX_LIST_REPLY_BYTES { Some(Ceiling::ListReplyBytes) } else { None }
}

/// How many bytes a LIST reply carrying these faces takes: every record, plus what a reply carries
/// in front of them.
///
/// THE FRAMING IS COUNTED RATHER THAN ASSUMED AWAY. The bound this is checked against reserves far
/// more than today's framing, which is deliberate: a bound that has to be recomputed the next time a
/// header gains a field is a bound that will be wrong again.
pub fn list_reply_len(faces: &[PublishedFace]) -> usize {
	LIST_REPLY_FRAMING_BYTES + faces.iter().map(|face| face.record.encoded_len()).sum::<usize>()
}

/// Apply the ordered rule to what a scan observed.
///
/// `observed` is every file the scan read, in whatever order the directory gave them; the result is
/// ordered by name so that two scans of one directory publish one thing.
pub fn reconcile(previous: &Publication, observed: &[Observation]) -> ScanOutcome {
	let mut accepted: Vec<PublishedFace> = Vec::new();
	let mut rejected: Vec<Rejection> = Vec::new();

	for observation in observed {
		let published = previous.faces.iter().find(|face| face.name == observation.name);
		match &observation.record {
			Ok(record) => {
				// THE RELABELLING RULE, and it applies only to a face that is ALREADY published: a
				// new name may declare whatever it likes, and a replacement may not change what the
				// face IS.
				if let Some(published) = published
					&& !published.record.declares_same_identity_as(record)
				{
					rejected.push(Rejection { name: observation.name.clone(), reason: RejectionReason::Relabelled });
					continue;
				}
				accepted.push(PublishedFace { name: observation.name.clone(), record: record.clone() });
			}
			Err(RecordError::PastMetadataCeiling) => {
				rejected.push(Rejection { name: observation.name.clone(), reason: RejectionReason::PastCeiling(Ceiling::FaceMetadataBytes) });
			}
			Err(reason) => {
				rejected.push(Rejection { name: observation.name.clone(), reason: RejectionReason::Declaration(*reason) });
			}
		}
	}
	accepted.sort_by(|left, right| left.name.cmp(&right.name));

	// STEP 1. What was published and can no longer be served: gone from the scan, or observed and
	// not accepted. A removal is not a change to the bytes, and treating only changed bytes as a
	// withdrawal is what left an identity listed whose only file no longer existed.
	let withdrawn: Vec<String> = previous.faces.iter().filter(|face| !accepted.iter().any(|kept| kept.name == face.name)).map(|face| face.name.clone()).collect();

	// STEP 2. The admissible set IS the accepted set: the previous faces that are still there and
	// still admissible are in it, the withdrawn ones are not, and the new ones are.
	if let Some(breach) = breach_of(&accepted) {
		// STEP 3. Publish the previous generation MINUS W, which adds nothing and removes only what
		// cannot be served.
		let kept: Vec<PublishedFace> = previous.faces.iter().filter(|face| !withdrawn.contains(&face.name)).cloned().collect();
		if withdrawn.is_empty() {
			return ScanOutcome { publication: previous.clone(), advanced: false, withdrawn, rejected, breach: Some(breach) };
		}
		return ScanOutcome { publication: Publication { generation: previous.generation + 1, faces: kept }, advanced: true, withdrawn, rejected, breach: Some(breach) };
	}

	// A VALID NO-CHANGE SCAN PUBLISHES NOTHING. The generation advances when a name, a digest or a
	// face's metadata differs, and not when a scan merely ran.
	let mut previous_sorted = previous.faces.clone();
	previous_sorted.sort_by(|left, right| left.name.cmp(&right.name));
	if previous_sorted == accepted {
		return ScanOutcome { publication: previous.clone(), advanced: false, withdrawn, rejected, breach: None };
	}
	ScanOutcome { publication: Publication { generation: previous.generation + 1, faces: accepted }, advanced: true, withdrawn, rejected, breach: None }
}

#[cfg(test)]
#[path = "font_scan/tests.rs"]
mod tests;
