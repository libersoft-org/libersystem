use super::*;

const EPOCH: Epoch = Epoch { attachment: 1, session: 1, content: 1 };

fn handles_payload(count: u32) -> Vec<u8> {
	let mut bytes = count.to_le_bytes().to_vec();
	for handle in 1..=count {
		bytes.extend_from_slice(&handle.to_le_bytes());
	}
	bytes
}

// The device-facing path end to end: a GetObjectHandles stream, in pulls of 4096 bytes, through the stream
// parser and into the snapshot - never a preallocated vector.
fn snapshot_of(count: u32, budget: &mut Budget) -> Result<Vec<u32>, SnapshotEnd> {
	let payload = handles_payload(count);
	let mut stream = ((ptp::HEADER + payload.len()) as u32).to_le_bytes().to_vec();
	stream.extend_from_slice(&ptp::DATA.to_le_bytes());
	stream.extend_from_slice(&ptp::GET_OBJECT_HANDLES.to_le_bytes());
	stream.extend_from_slice(&5u32.to_le_bytes());
	stream.extend_from_slice(&payload);
	stream.extend_from_slice(&12u32.to_le_bytes());
	stream.extend_from_slice(&ptp::RESPONSE.to_le_bytes());
	stream.extend_from_slice(&ptp::OK.to_le_bytes());
	stream.extend_from_slice(&5u32.to_le_bytes());
	let mut inbound = ptp::Inbound::new(ptp::GET_OBJECT_HANDLES, 5, true, HANDLES_LIMIT);
	let mut snapshot: Option<Snapshot> = None;
	let mut outcome: Result<(), SnapshotEnd> = Ok(());
	let mut answered = false;
	for pull in stream.chunks(CHUNK) {
		let fed = inbound.feed(pull, &mut |piece| {
			if outcome.is_err() {
				return;
			}
			match piece {
				ptp::Piece::Data(declared) => match Snapshot::new(declared) {
					Ok(started) => snapshot = Some(started),
					Err(end) => outcome = Err(end),
				},
				ptp::Piece::Payload(bytes) => {
					if let Some(snapshot) = snapshot.as_mut() {
						outcome = snapshot.feed(bytes, budget);
					}
				}
				ptp::Piece::Response(response) => answered = response.code == ptp::OK,
			}
		});
		match fed {
			Err(ptp::Fault::TooLarge(payload)) => return Err(SnapshotEnd::OverLimit(declared_count(payload))),
			Err(_) => return Err(SnapshotEnd::Corrupt),
			Ok(()) => {}
		}
		outcome?;
	}
	assert!(answered, "the final response closed the snapshot");
	snapshot.ok_or(SnapshotEnd::Corrupt)?.finish().map(|(ids, _)| ids)
}

#[test]
fn forty_thousand_handles_the_exact_limit_and_one_over() {
	let mut budget = Budget::default();
	let ids = snapshot_of(40_000, &mut budget).unwrap();
	assert_eq!((ids.len(), ids[0], ids[39_999]), (40_000, 1, 40_000));
	assert_eq!(budget.snapshots(), 160_000, "the IDs were charged to the snapshot budget");
	budget.release_snapshot(160_000);
	assert_eq!(snapshot_of(MAX_HANDLES, &mut budget).map(|ids| ids.len()), Ok(65_536), "exactly the limit");
	budget.release_snapshot(u64::from(MAX_HANDLES) * 4);
	// One over is refused from the container's header, with its count, and nothing is charged.
	assert_eq!(snapshot_of(MAX_HANDLES + 1, &mut budget), Err(SnapshotEnd::OverLimit(MAX_HANDLES + 1)));
	assert_eq!(budget.used(), 0);
}

#[test]
fn a_count_that_disagrees_with_the_length_is_corrupt() {
	let mut budget = Budget::default();
	let mut snapshot = Snapshot::new(4 + 4 * 3).unwrap();
	assert_eq!(snapshot.feed(&4u32.to_le_bytes(), &mut budget), Err(SnapshotEnd::Corrupt), "four IDs claimed in three IDs' length");
	assert_eq!(budget.used(), 0, "nothing was reserved for the claim");
	assert_eq!(Snapshot::new(5).err(), Some(SnapshotEnd::Corrupt), "a length that is not whole IDs");
	assert_eq!(Snapshot::new(2).err(), Some(SnapshotEnd::Corrupt), "a length without a count");
	// A snapshot the data phase did not fill.
	let mut short = Snapshot::new(4 + 8).unwrap();
	short.feed(&2u32.to_le_bytes(), &mut budget).unwrap();
	short.feed(&[1, 0, 0, 0, 2], &mut budget).unwrap();
	assert_eq!(short.finish().err(), Some(SnapshotEnd::Corrupt));
}

#[test]
fn the_budget_and_the_snapshot_budget_are_charged_before_allocation() {
	let mut budget = Budget::default();
	// Eight cursors' worth of full snapshots is the snapshot budget, exactly.
	for _ in 0..8 {
		budget.charge_snapshot(u64::from(MAX_HANDLES) * 4).unwrap();
	}
	assert_eq!(budget.charge_snapshot(4), Err(Refusal::Exhausted));
	assert!(budget.charge(DATA_BUDGET - SNAPSHOT_BUDGET).is_ok(), "the rest of the budget is still there for pages and chunks");
	assert_eq!(budget.charge(1), Err(Refusal::Exhausted));
	budget.release_snapshot(u64::from(MAX_HANDLES) * 4);
	assert!(budget.charge_snapshot(4).is_ok());
	// A snapshot that cannot be charged is refused before its IDs are reserved.
	let mut full = Budget::default();
	full.charge(DATA_BUDGET - 8).unwrap();
	let mut snapshot = Snapshot::new(4 + 4 * 3).unwrap();
	assert_eq!(snapshot.feed(&3u32.to_le_bytes(), &mut full), Err(SnapshotEnd::Exhausted));
}

#[test]
fn one_cursor_and_one_transfer_per_client_and_per_device() {
	let held = [Holder { client: 1, device: 1 }];
	assert_eq!(admit(&held, Holder { client: 1, device: 2 }, MAX_CURSORS), Err(Refusal::Exhausted), "a second for one client");
	assert_eq!(admit(&held, Holder { client: 2, device: 1 }, MAX_CURSORS), Err(Refusal::Exhausted), "a second on one device");
	assert!(admit(&held, Holder { client: 2, device: 2 }, MAX_CURSORS).is_ok());
	let eight: Vec<Holder> = (0..8).map(|at| Holder { client: at, device: at + 100 }).collect();
	assert_eq!(admit(&eight, Holder { client: 50, device: 50 }, MAX_TRANSFERS), Err(Refusal::Exhausted), "past eight in all");
}

#[test]
fn identities_are_stale_once_any_epoch_moves() {
	assert!(scoped(EPOCH, EPOCH).is_ok());
	for moved in [Epoch { attachment: 2, ..EPOCH }, Epoch { session: 2, ..EPOCH }, Epoch { content: 2, ..EPOCH }] {
		assert_eq!(scoped(moved, EPOCH), Err(Refusal::Stale), "{moved:?}");
	}
	let event = |code| ptp::Event { code, transaction: 0, params: [0; 3], count: 0 };
	assert_eq!(change(&event(ptp::OBJECT_ADDED)), Change::Content);
	assert_eq!(change(&event(ptp::STORE_REMOVED)), Change::Content);
	assert_eq!(change(&event(ptp::DEVICE_RESET)), Change::Session);
	assert_eq!(change(&event(ptp::CANCEL_TRANSACTION)), Change::Cancelled);
	assert_eq!(change(&event(0x4006)), Change::None, "a property change touches no identity");
	let mut session = Session::new(9);
	assert_eq!((session.transaction(), session.transaction()), (1, 2));
	session.next = u32::MAX - 1;
	assert_eq!((session.transaction(), session.transaction()), (u32::MAX - 1, 1), "never all-ones, never zero");
}

#[test]
fn a_cursor_pages_two_at_a_time_and_expires_when_abandoned() {
	let mut cursor = Cursor::new(Holder { client: 1, device: 1 }, EPOCH, 7, alloc::vec![10, 11, 12], 12, 100);
	assert_eq!((cursor.count(), cursor.upcoming()), (3, &[10, 11][..]));
	cursor.advance(1, 150);
	assert_eq!((cursor.remaining(), cursor.upcoming()), (2, &[11, 12][..]), "a page that fitted one entry moves by one");
	cursor.advance(2, 200);
	assert_eq!((cursor.remaining(), cursor.upcoming()), (0, &[][..]));
	assert!(!cursor.expired(200 + CURSOR_IDLE_TICKS - 1));
	assert!(cursor.expired(200 + CURSOR_IDLE_TICKS), "sixty seconds untouched");
	// Two entries within 8192 bytes with the envelope, one when they are not; never none.
	assert_eq!(fits(16, &[4000, 4000]), 2);
	assert_eq!(fits(16, &[4096, 4096]), 1);
	assert_eq!(fits(16, &[9000]), 1);
	assert_eq!(fits(16, &[100, 100, 100]), 2, "two records at most");
}

fn transfer(size: Option<u32>) -> Transfer {
	Transfer::new(Holder { client: 1, device: 1 }, EPOCH, 7, 99, size, 0).unwrap()
}

#[test]
fn complete_needs_every_byte_the_framing_and_the_final_response() {
	let mut ok = transfer(Some(12_288));
	ok.demanded(1);
	ok.framed(12_288, 1);
	assert_eq!(ok.state, State::Reading);
	let mut offsets = Vec::new();
	for chunk in [4084usize, 4096, 4096, 12] {
		offsets.push(ok.delivered(chunk, 2).unwrap());
	}
	assert_eq!(offsets, [0, 4084, 8180, 12_276], "each chunk at its exact offset");
	assert!(!ok.settle(), "no response yet: not an ending");
	ok.answered(true);
	assert!(ok.settle());
	assert_eq!(ok.status(), Status { state: State::Complete, expected: 12_288, delivered: 12_288, cause: None });
	// The response without every byte, a failed response, and bytes past the length.
	let mut short = transfer(Some(10));
	short.framed(10, 1);
	short.delivered(4, 1).unwrap();
	short.answered(true);
	short.settle();
	assert_eq!((short.state, short.delivered, short.cause), (State::Partial, 4, Some(Cause::Corrupt)));
	let mut refused = transfer(Some(10));
	refused.answered(false);
	refused.settle();
	assert_eq!((refused.state, refused.delivered, refused.cause), (State::Partial, 0, Some(Cause::DeviceError)), "a failure before any byte is partial with none");
	let mut over = transfer(Some(10));
	over.framed(10, 1);
	assert_eq!(over.delivered(11, 1), None);
	assert_eq!(over.cause, Some(Cause::Corrupt));
	// Framed at another length.
	let mut mismatched = transfer(Some(10));
	mismatched.framed(11, 1);
	assert_eq!(mismatched.cause, Some(Cause::Corrupt));
}

#[test]
fn a_zero_byte_object_still_needs_its_final_response() {
	let mut empty = transfer(Some(0));
	assert!(!empty.settle());
	assert_eq!(empty.state, State::Opening);
	empty.answered(true);
	empty.settle();
	assert_eq!((empty.state, empty.delivered), (State::Complete, 0), "framed or not, the matching success completes it");
}

#[test]
fn unrepresentable_sizes_are_refused_before_a_transfer_exists() {
	assert_eq!(Transfer::new(Holder { client: 1, device: 1 }, EPOCH, 1, 1, None, 0).err(), Some(Refusal::Unsupported), "the four-gigabyte sentinel");
	assert!(Transfer::new(Holder { client: 1, device: 1 }, EPOCH, 1, 1, Some(u32::MAX - 12), 0).is_ok(), "the largest one container frames");
	assert_eq!(Transfer::new(Holder { client: 1, device: 1 }, EPOCH, 1, 1, Some(u32::MAX - 11), 0).err(), Some(Refusal::Unsupported));
}

#[test]
fn every_terminal_cause_is_kept_with_its_count() {
	for cause in [Cause::Removed, Cause::TimedOut, Cause::Cancelled, Cause::Corrupt, Cause::Stale, Cause::DeviceError] {
		let mut ended = transfer(Some(100));
		ended.framed(100, 0);
		ended.delivered(40, 0).unwrap();
		ended.fail(cause);
		ended.fail(Cause::Cancelled);
		assert_eq!(ended.status(), Status { state: State::Partial, expected: 100, delivered: 40, cause: Some(cause) }, "the first ending stands");
		ended.answered(true);
		ended.settle();
		assert_eq!(ended.state, State::Partial, "a late success does not complete it");
	}
}

#[test]
fn thirty_seconds_without_progress_or_demand_times_a_transfer_out() {
	let idle = transfer(Some(100));
	assert!(!idle.timed_out(TRANSFER_IDLE_TICKS - 1));
	assert!(idle.timed_out(TRANSFER_IDLE_TICKS), "a client that never asks");
	let mut waiting = transfer(Some(100));
	waiting.demanded(1000);
	waiting.framed(100, 1000);
	waiting.delivered(10, 2000).unwrap();
	assert_eq!(waiting.deadline(), Some(2000 + TRANSFER_IDLE_TICKS), "progress moves the deadline");
	assert!(waiting.timed_out(2000 + TRANSFER_IDLE_TICKS), "a device that stops delivering");
}

#[test]
fn identities_are_resolved_before_anything_reaches_a_device() {
	let current = Named { slot: 3, generation: 1, binding: 1, epoch: EPOCH, incarnation: 5, context: 0 };
	let mine = Named { context: 7, ..current };
	assert!(resolve(&current, &mine, 7).is_ok());
	assert_eq!(resolve(&current, &Named { slot: 4, ..mine }, 7), Err(Refusal::NotFound), "another device");
	assert_eq!(resolve(&current, &Named { generation: 2, ..mine }, 7), Err(Refusal::NotFound), "a replacement is another device");
	assert_eq!(resolve(&current, &mine, 8), Err(Refusal::Denied), "another client's identity");
	assert_eq!(resolve(&current, &Named { incarnation: 4, ..mine }, 7), Err(Refusal::Stale), "a previous incarnation");
	assert_eq!(resolve(&current, &Named { epoch: Epoch { content: 9, ..EPOCH }, ..mine }, 7), Err(Refusal::Stale));
}

#[test]
fn a_link_serializes_transactions_and_recovers_within_its_deadline() {
	let mut link = Link::new();
	assert_eq!(link.begin(), Err(Refusal::Again), "nothing before attach");
	let first = link.attached(10).unwrap();
	assert!(link.opened(true));
	link.survey_transaction();
	link.surveyed();
	let epoch = link.epoch;
	let transaction = link.begin().unwrap();
	assert_eq!(link.begin(), Err(Refusal::Again), "one transaction at a time");
	link.end();
	assert_eq!(link.begin().unwrap(), transaction + 1, "transaction IDs move on");
	// A cancel that works keeps the session and the epoch.
	link.cancel(1000);
	assert_eq!(link.phase, Phase::Cancelling);
	link.cancelled(true, 1010);
	assert_eq!((link.phase, link.epoch), (Phase::Ready, epoch));
	// A cancel that does not is a reset, and a reset stales everything named before.
	link.begin().unwrap();
	link.cancel(2000);
	link.cancelled(false, 2050);
	assert_eq!(link.phase, Phase::Resetting);
	assert!(!link.expired(2000 + RECOVERY_TICKS - 1));
	assert!(link.expired(2000 + RECOVERY_TICKS), "two seconds from the cancel, not from the reset");
	let second = link.attached(11).unwrap();
	assert_ne!(first, second, "a new session ID");
	assert!(scoped(link.epoch, epoch).is_err());
	link.opened(true);
	link.surveyed();
	assert!(!link.expired(10_000), "recovered: no deadline left");
	// A reset whose attachment did not move, or a failed recovery, leaves it unavailable.
	link.reset(20_000);
	assert_eq!(link.attached(11), Err(Refusal::Invalid));
	assert_eq!(link.begin(), Err(Refusal::Unsupported));
	let mut failed = Link::new();
	failed.attached(1).unwrap();
	assert!(!failed.opened(false));
	failed.fail();
	assert_eq!(failed.phase, Phase::Unavailable);
	// A reported change moves only the content epoch.
	let mut changed = Link::new();
	changed.attached(1).unwrap();
	let before = changed.epoch;
	changed.changed();
	assert_eq!((changed.epoch.attachment, changed.epoch.session), (before.attachment, before.session));
	assert_ne!(changed.epoch, before);
}
