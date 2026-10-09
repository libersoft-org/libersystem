use super::*;
use render3d::{Completion, ReadbackResult, ReadbackTicket, ReadbackValue, Status};

fn submitted() -> Submitted {
	Submitted { pending: Pending { request: PickRequest::new(0, 0, 1, 1).unwrap(), kind: Readback::Identity, readback: 7, serial: 19 }, source: 3 }
}
fn result(serial: u64, source: u32, destination: u32, status: Status, value: Option<ReadbackValue>) -> ReadbackResult {
	ReadbackResult { ticket: ReadbackTicket { completion: Completion { serial, source }, status, destination }, value }
}

#[test]
fn completed_pick_requires_its_actual_origin_destination_kind_and_terminal_success() {
	let request = submitted();
	let value = Some(ReadbackValue::Identity(0xffff_fffe));
	assert_eq!(completed(&request, result(19, 3, 7, Status::Complete, value)), Ok(ReadbackValue::Identity(0xffff_fffe)));
	for (serial, source, destination) in [(18, 3, 7), (20, 3, 7), (19, 4, 7), (19, 3, 8)] {
		assert!(completed(&request, result(serial, source, destination, Status::Complete, value)).is_err());
	}
	for status in [Status::Pending, Status::Cancelled, Status::BackendLost, Status::Failed(render3d::Error::InvalidRenderState { reason: "execution refused" })] {
		assert!(completed(&request, result(19, 3, 7, status, value)).is_err());
	}
	assert!(completed(&request, result(19, 3, 7, Status::Complete, None)).is_err());
	assert!(completed(&request, result(19, 3, 7, Status::Complete, Some(ReadbackValue::Depth(0.5)))).is_err());
}

#[test]
fn terminal_submission_constructor_cannot_claim_pending_without_resource_ownership() {
	assert!(render3d::Submission::terminal(1, 0, Status::Pending).is_err());
	assert!(render3d::Submission::terminal(1, 0, Status::Complete).unwrap().retained().is_empty());
}
