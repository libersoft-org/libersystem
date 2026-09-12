use super::*;

use crate::font_record::{FaceFormat, FaceSlant, FaceWidth, MAX_FACE_METADATA_BYTES};

fn digest(seed: u8) -> [u8; 32] {
	[seed; 32]
}

fn record(family: &str, seed: u8) -> FaceRecord {
	FaceRecord { family: String::from(family), style: String::from("Book"), format: FaceFormat::TruetypeGlyf, face_index: 0, weight: 400, width: FaceWidth::Normal, slant: FaceSlant::Upright, axes: Vec::new(), digest: digest(seed) }
}

fn face(name: &str, family: &str, seed: u8) -> PublishedFace {
	PublishedFace { name: String::from(name), record: record(family, seed) }
}

fn seen(face: &PublishedFace) -> Observation {
	Observation { name: face.name.clone(), record: Ok(face.record.clone()) }
}

fn published(faces: &[PublishedFace], generation: u64) -> Publication {
	Publication { generation, faces: faces.to_vec() }
}

#[test]
// THE ORDINARY SCAN: a face appears, and it is published at a new generation.
fn a_new_face_is_published_at_a_new_generation() {
	let existing = face("sans.ttf", "Liber Sans", 1);
	let arrival = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[existing.clone()], 7);
	let outcome = reconcile(&previous, &[seen(&existing), seen(&arrival)]);
	assert!(outcome.advanced);
	assert_eq!(outcome.publication.generation, 8);
	assert_eq!(outcome.publication.faces, alloc::vec![existing, arrival]);
	assert!(outcome.withdrawn.is_empty());
	assert!(outcome.rejected.is_empty());
	assert_eq!(outcome.breach, None);
}

#[test]
// A VALID NO-CHANGE SCAN PUBLISHES NOTHING AND SPENDS NO GENERATION. This is the case the first
// version of the bound missed: an unchanged scan is neither a publication nor a failure, so a
// recovery caller could ask for a full directory read in a loop and nothing would count it.
fn a_scan_that_finds_nothing_different_does_not_advance_the_generation() {
	let one = face("sans.ttf", "Liber Sans", 1);
	let two = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[one.clone(), two.clone()], 4);
	// The directory order is not the published order, and that must not count as a change.
	let outcome = reconcile(&previous, &[seen(&two), seen(&one)]);
	assert!(!outcome.advanced);
	assert_eq!(outcome.publication, previous);
	assert!(outcome.withdrawn.is_empty());
	assert_eq!(outcome.breach, None);
}

#[test]
// A REPLACEMENT IS A NEW VERSION OF THE SAME FACE: the bytes change, the declaration does not, and
// the generation moves so that everything derived from the old bytes is invalidated.
fn replacing_a_face_with_new_bytes_advances_the_generation() {
	let original = face("sans.ttf", "Liber Sans", 1);
	let previous = published(&[original.clone()], 2);
	let replaced = PublishedFace { name: original.name.clone(), record: record("Liber Sans", 9) };
	let outcome = reconcile(&previous, &[seen(&replaced)]);
	assert!(outcome.advanced);
	assert_eq!(outcome.publication.generation, 3);
	assert_eq!(outcome.publication.faces, alloc::vec![replaced]);
	assert!(outcome.withdrawn.is_empty(), "the face is still served, under new bytes");
}

#[test]
// THE CAPTURE CASE. A privileged writer replaces an installed face with one whose declaration says
// it is a DIFFERENT face - relabelling it to capture somebody else's fallback. The replacement is
// refused, and the publication is WITHDRAWN at a new generation rather than left listing an identity
// whose only file no longer holds those bytes.
fn a_relabelled_replacement_is_refused_and_the_face_is_withdrawn() {
	let original = face("sans.ttf", "Liber Sans", 1);
	let other = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[original.clone(), other.clone()], 5);
	let relabelled = PublishedFace { name: original.name.clone(), record: record("Someone Else", 9) };
	let outcome = reconcile(&previous, &[seen(&relabelled), seen(&other)]);
	assert!(outcome.advanced, "a withdrawal is a new generation");
	assert_eq!(outcome.publication.generation, 6);
	assert_eq!(outcome.publication.faces, alloc::vec![other], "the relabelled face is gone from LIST");
	assert_eq!(outcome.withdrawn, alloc::vec![String::from("sans.ttf")]);
	assert_eq!(outcome.rejected, alloc::vec![Rejection { name: String::from("sans.ttf"), reason: RejectionReason::Relabelled }]);
	assert_eq!(outcome.breach, None);
	// AND THE NEW BYTES ARE NOT PUBLISHED EITHER, under any name: capture is what must not happen.
	assert!(!outcome.publication.faces.iter().any(|face| face.record.family == "Someone Else"));
}

#[test]
// A FACE THAT IS GONE IS WITHDRAWN, and a removal is not a change to any bytes - which is the door
// the first version of this rule left open: an identity stayed listed whose only file no longer
// existed.
fn a_removed_face_is_withdrawn_at_a_new_generation() {
	let one = face("sans.ttf", "Liber Sans", 1);
	let two = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[one.clone(), two.clone()], 3);
	let outcome = reconcile(&previous, &[seen(&two)]);
	assert!(outcome.advanced);
	assert_eq!(outcome.publication.faces, alloc::vec![two]);
	assert_eq!(outcome.withdrawn, alloc::vec![String::from("sans.ttf")]);
}

#[test]
// SIXTY-FIVE NEW FACES: the admissible set does not fit, nothing new is published, the previous
// generation stays current and the failure names the ceiling. A bad drop into the directory cannot
// take fonts away from a running system.
fn a_scan_that_would_install_a_sixty_fifth_face_publishes_nothing() {
	let existing = face("sans.ttf", "Liber Sans", 1);
	let previous = published(&[existing.clone()], 11);
	let mut observed = alloc::vec![seen(&existing)];
	for index in 0..MAX_INSTALLED_FACES {
		let arrival = face(&alloc::format!("new{index:02}.ttf"), "Arrival", index as u8);
		observed.push(seen(&arrival));
	}
	let outcome = reconcile(&previous, &observed);
	assert_eq!(outcome.breach, Some(Ceiling::InstalledFaces));
	assert!(!outcome.advanced, "the previous generation stays current");
	assert_eq!(outcome.publication, previous);
	assert!(outcome.withdrawn.is_empty());
	// AND EXACTLY SIXTY-FOUR FITS, which is the bound and not one less.
	let mut at_bound = alloc::vec![seen(&existing)];
	for index in 0..MAX_INSTALLED_FACES - 1 {
		let arrival = face(&alloc::format!("new{index:02}.ttf"), "Arrival", index as u8);
		at_bound.push(seen(&arrival));
	}
	let fits = reconcile(&previous, &at_bound);
	assert_eq!(fits.breach, None);
	assert!(fits.advanced);
	assert_eq!(fits.publication.faces.len(), MAX_INSTALLED_FACES);
}

#[test]
// A NEW FACE WHOSE RECORD IS PAST THE METADATA CEILING: the same conservative answer - previous
// generation unchanged, nothing withdrawn, the failure reported against the ceiling it exceeded.
fn a_new_face_past_the_metadata_ceiling_leaves_the_publication_alone() {
	let existing = face("sans.ttf", "Liber Sans", 1);
	let previous = published(&[existing.clone()], 2);
	let oversized = Observation { name: String::from("huge.ttf"), record: Err(RecordError::PastMetadataCeiling) };
	let outcome = reconcile(&previous, &[seen(&existing), oversized]);
	assert!(!outcome.advanced);
	assert_eq!(outcome.publication, previous);
	assert!(outcome.withdrawn.is_empty());
	assert_eq!(outcome.rejected, alloc::vec![Rejection { name: String::from("huge.ttf"), reason: RejectionReason::PastCeiling(Ceiling::FaceMetadataBytes) }]);
}

#[test]
// A REPLACEMENT OF A PUBLISHED FACE PAST THE CEILING IS THE CASE THE SINGLE OLD RULE GOT WRONG, and
// it is the one an attacker reaches: the face is WITHDRAWN at a new generation, every other face
// carries over unchanged, and the failure is reported. "The previous generation stays served" was a
// promise this design cannot keep for a file somebody overwrote.
fn a_replacement_past_the_metadata_ceiling_withdraws_that_face_and_keeps_the_rest() {
	let one = face("sans.ttf", "Liber Sans", 1);
	let two = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[one.clone(), two.clone()], 20);
	let oversized = Observation { name: one.name.clone(), record: Err(RecordError::PastMetadataCeiling) };
	let outcome = reconcile(&previous, &[oversized, seen(&two)]);
	assert!(outcome.advanced);
	assert_eq!(outcome.publication.generation, 21);
	assert_eq!(outcome.publication.faces, alloc::vec![two]);
	assert_eq!(outcome.withdrawn, alloc::vec![String::from("sans.ttf")]);
	assert_eq!(outcome.rejected[0].reason, RejectionReason::PastCeiling(Ceiling::FaceMetadataBytes));
}

#[test]
// WHERE THE TWO RULES MEET: a published face is removed while enough new ones arrive to breach a
// ceiling. The removed face is withdrawn at a new generation, nothing new is published, and the
// failure names the ceiling - because a removal is not something the ceiling fallback may postpone.
fn a_removal_is_withdrawn_even_when_the_scan_breaches_a_ceiling() {
	let gone = face("gone.ttf", "Liber Gone", 1);
	let kept = face("kept.ttf", "Liber Kept", 2);
	let previous = published(&[gone.clone(), kept.clone()], 30);
	let mut observed = alloc::vec![seen(&kept)];
	for index in 0..MAX_INSTALLED_FACES {
		let arrival = face(&alloc::format!("new{index:02}.ttf"), "Arrival", index as u8);
		observed.push(seen(&arrival));
	}
	let outcome = reconcile(&previous, &observed);
	assert_eq!(outcome.breach, Some(Ceiling::InstalledFaces));
	assert!(outcome.advanced, "the removal is published even though nothing new is");
	assert_eq!(outcome.publication.generation, 31);
	assert_eq!(outcome.publication.faces, alloc::vec![kept], "nothing new is added: only what cannot be served is removed");
	assert_eq!(outcome.withdrawn, alloc::vec![String::from("gone.ttf")]);
}

#[test]
// A MALFORMED OR MISMATCHED DECLARATION IS REPORTED WITH ITS REASON, and for an already-published
// face it withdraws the same way a relabelling does: one outcome for every rejection, because the
// reason differs and the state it leaves does not - the identity names bytes that cannot be served.
fn every_rejected_replacement_leaves_the_same_state() {
	let one = face("sans.ttf", "Liber Sans", 1);
	let two = face("serif.ttf", "Liber Serif", 2);
	let previous = published(&[one.clone(), two.clone()], 40);
	for reason in [RecordError::DigestMismatch, RecordError::UnknownValue, RecordError::Missing] {
		let outcome = reconcile(&previous, &[Observation { name: one.name.clone(), record: Err(reason) }, seen(&two)]);
		assert!(outcome.advanced, "{reason:?} withdraws at a new generation");
		assert_eq!(outcome.publication.faces, alloc::vec![two.clone()]);
		assert_eq!(outcome.withdrawn, alloc::vec![String::from("sans.ttf")]);
		assert_eq!(outcome.rejected, alloc::vec![Rejection { name: String::from("sans.ttf"), reason: RejectionReason::Declaration(reason) }]);
	}
}

#[test]
// THE WORST-CASE LIST REPLY, which is the fixture the framing assertion needs: sixty-four records
// each encoding to EXACTLY the per-record maximum. The records alone fill 16384, so anything above
// it is framing - which is what makes "strictly larger than 16384" mean what it says. An ordinary
// catalogue of shorter faces encodes below that and would have failed the assertion this replaces.
fn the_worst_case_reply_is_larger_than_its_records_and_inside_the_bound() {
	let mut faces = Vec::new();
	for index in 0..MAX_INSTALLED_FACES {
		let mut record = record("F", index as u8);
		// Pad the family until the record encodes to exactly the per-record maximum.
		while record.encoded_len() < MAX_FACE_METADATA_BYTES {
			record.family.push('F');
		}
		assert_eq!(record.encoded_len(), MAX_FACE_METADATA_BYTES);
		faces.push(PublishedFace { name: alloc::format!("face{index:02}.ttf"), record });
	}
	let reply = list_reply_len(&faces);
	assert!(reply > MAX_INSTALLED_FACES * MAX_FACE_METADATA_BYTES, "a reply is never only its records: {reply}");
	assert!(reply <= MAX_LIST_REPLY_BYTES, "the worst case fits the bound: {reply}");
	assert_eq!(breach_of(&faces), None, "sixty-four maximum records are an admissible catalogue");
	// WATCHED TO FAIL: one byte more per record and the per-record ceiling refuses it.
	let mut raised = faces.clone();
	raised[0].record.family.push('F');
	assert_eq!(breach_of(&raised), Some(Ceiling::FaceMetadataBytes));
}
