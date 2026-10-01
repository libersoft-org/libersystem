use super::*;
use alloc::vec::Vec;

// A SCRIPTED TRANSACTION: every step answers as `script` says - `Some(true)` done, `Some(false)` failed and completed
// (a partial step still owes its undo), `None` failed without completing (the drivers' step that unwound itself) - and
// what ran is the order.
fn run(script: &[(Action, Option<bool>)], end_after: Option<Action>) -> Vec<Action> {
	let mut plan = Plan::new();
	let mut ran = Vec::new();
	while let Some(action) = plan.next() {
		ran.push(action);
		match script.iter().find(|(scripted, _)| *scripted == action).map(|(_, answer)| *answer).unwrap_or(Some(true)) {
			Some(true) => plan.completed(action),
			Some(false) => {
				plan.failed(action);
				plan.completed(action);
			}
			None => plan.failed(action),
		}
		if end_after == Some(action) {
			plan.end();
		}
		assert!(ran.len() < 32, "the plan ends");
	}
	ran
}

use Action::*;

#[test]
fn a_sleep_that_slept_resumes_in_reverse_with_the_thaw_last() {
	assert_eq!(run(&[], None), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, Wake, DriversResume, Release, Resumed, Thaw]);
}

#[test]
fn a_refusal_at_the_check_undoes_nothing_because_nothing_was_taken() {
	assert_eq!(run(&[(Check, Some(false))], None), [Check]);
}

#[test]
fn a_driver_that_refuses_unwinds_the_steps_taken_and_owes_no_resume_of_its_own() {
	assert_eq!(run(&[(Drivers, None)], None), [Check, Announce, Freeze, Flush, Drivers, Release, Resumed, Thaw]);
}

#[test]
fn a_freeze_that_failed_part_way_is_still_thawed_last() {
	assert_eq!(run(&[(Freeze, Some(false))], None), [Check, Announce, Freeze, Resumed, Thaw]);
}

#[test]
fn an_entry_the_kernel_refused_unwinds_everything_before_it() {
	assert_eq!(run(&[(Enter, Some(false))], None), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, Wake, DriversResume, Release, Resumed, Thaw]);
}

#[test]
fn a_failed_undo_step_does_not_stop_the_resume() {
	assert_eq!(run(&[(Release, Some(false)), (DriversResume, Some(false))], None), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, Wake, DriversResume, Release, Resumed, Thaw]);
}

#[test]
fn the_orderly_sequence_ends_the_transaction_at_its_next_step() {
	assert_eq!(run(&[], Some(Drivers)), [Check, Announce, Freeze, Flush, Drivers, DriversResume, Release, Resumed, Thaw], "the step in flight finishes and is undone");
	assert_eq!(run(&[], Some(Announce)), [Check, Announce, Resumed]);
}

// A SCRIPTED HIBERNATION, the same way; `restored_after` says the entry answered in the restored machine.
fn run_hibernation(hybrid: bool, script: &[(Action, Option<bool>)], restored: bool) -> Vec<Action> {
	let mut plan = Plan::hibernation(hybrid);
	let mut ran = Vec::new();
	while let Some(action) = plan.next() {
		ran.push(action);
		match script.iter().find(|(scripted, _)| *scripted == action).map(|(_, answer)| *answer).unwrap_or(Some(true)) {
			Some(true) => plan.completed(action),
			Some(false) => {
				plan.failed(action);
				plan.completed(action);
			}
			None => plan.failed(action),
		}
		if action == Enter && restored {
			plan.restored();
		}
		assert!(ran.len() < 32, "the plan ends");
	}
	ran
}

#[test]
fn a_hibernation_writes_its_image_and_goes_off_and_owes_the_whole_undo_if_it_does_not() {
	// THE MACHINE THAT RAN ON: the image written and the machine off. `DiskOff` returns only when it did not happen, and
	// the undo then brings the machine back running - the image discarded before a write reaches the volume.
	assert_eq!(run_hibernation(false, &[(DiskOff, Some(false))], false), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, ImageDrivers, ImageWrite, DiskOff, Wake, DriversResume, Discard, Release, Resumed, Thaw]);
}

#[test]
fn an_image_that_could_not_be_written_unwinds_to_a_running_machine() {
	// A FAILED WRITE STILL COMPLETED ITS STEP: whatever it left on the disk is discarded.
	assert_eq!(run_hibernation(false, &[(ImageWrite, Some(false))], false), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, ImageDrivers, ImageWrite, Wake, DriversResume, Discard, Release, Resumed, Thaw]);
	// And one that never got as far as writing owes nothing to discard.
	assert_eq!(run_hibernation(false, &[(ImageDrivers, None)], false), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, ImageDrivers, Wake, DriversResume, Release, Resumed, Thaw]);
}

#[test]
fn the_restored_machine_writes_nothing_and_resumes_as_after_any_sleep() {
	// THE SECOND ANSWER OF THE ENTRY: no image step runs in the machine the image restored, and nothing is discarded -
	// the restore invalidated the image before it replaced memory.
	assert_eq!(run_hibernation(false, &[], true), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, Wake, DriversResume, Release, Resumed, Thaw]);
	assert_eq!(run_hibernation(true, &[], true), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, Wake, DriversResume, Release, Resumed, Thaw]);
}

#[test]
fn a_hybrid_sleep_keeps_its_image_through_s3_and_discards_it_before_any_write() {
	assert_eq!(run_hibernation(true, &[], false), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, ImageDrivers, ImageWrite, ImageSuspend, RamEnter, Wake, DriversResume, Discard, Release, Resumed, Thaw]);
	// AN S3 THE KERNEL REFUSED leaves a machine running too: its image goes the same way.
	assert_eq!(run_hibernation(true, &[(RamEnter, Some(false))], false), [Check, Announce, Freeze, Flush, Drivers, Platform, Enter, ImageDrivers, ImageWrite, ImageSuspend, RamEnter, Wake, DriversResume, Discard, Release, Resumed, Thaw]);
}
