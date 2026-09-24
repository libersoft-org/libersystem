use super::*;
use alloc::string::String;

#[test]
fn printer_id_wire_is_stable() {
	let sample = PrinterId { slot: 7, generation: 7, binding_generation: 7, attachment: 7, incarnation: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PrinterId::decode(&bytes).unwrap(), sample);
}
#[test]
fn document_language_wire_is_stable() {
	let sample = DocumentLanguage::Postscript;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(DocumentLanguage::decode(&bytes).unwrap(), sample);
}
#[test]
fn observation_wire_is_stable() {
	let sample = Observation::Yes;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(Observation::decode(&bytes).unwrap(), sample);
}
#[test]
fn port_status_wire_is_stable() {
	let sample = PortStatus { paper_empty: Observation::Yes, selected: Observation::Yes, error: Observation::Yes, cover_open: Observation::Yes, jam: Observation::Yes };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 1, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(PortStatus::decode(&bytes).unwrap(), sample);
}
#[test]
fn printer_info_wire_is_stable() {
	let sample = PrinterInfo { id: PrinterId { slot: 7, generation: 7, binding_generation: 7, attachment: 7, incarnation: 7 }, model: String::from("x"), languages: alloc::vec![DocumentLanguage::Postscript], status: PortStatus { paper_empty: Observation::Yes, selected: Observation::Yes, error: Observation::Yes, cover_open: Observation::Yes, jam: Observation::Yes }, available: true, queued: 7, active: true, max_job_bytes: 7 };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 120, 1, 0, 1, 1, 1, 1, 1, 1, 1, 7, 1, 7, 0, 0, 0];
	assert_eq!(bytes, golden);
	assert_eq!(PrinterInfo::decode(&bytes).unwrap(), sample);
}
#[test]
fn job_state_wire_is_stable() {
	let sample = JobState::Writing;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(JobState::decode(&bytes).unwrap(), sample);
}
#[test]
fn job_cause_wire_is_stable() {
	let sample = JobCause::SizeLimit;
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1];
	assert_eq!(bytes, golden);
	assert_eq!(JobCause::decode(&bytes).unwrap(), sample);
}
#[test]
fn job_status_wire_is_stable() {
	let sample = JobStatus { state: JobState::Writing, printer: PrinterId { slot: 7, generation: 7, binding_generation: 7, attachment: 7, incarnation: 7 }, declared: 7, staged: 7, acknowledged: 7, cause: Some(JobCause::SizeLimit), delivery_uncertain: true };
	let bytes = sample.encode_vec().expect("encode");
	let golden: &[u8] = &[1, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 1, 1, 1];
	assert_eq!(bytes, golden);
	assert_eq!(JobStatus::decode(&bytes).unwrap(), sample);
}
