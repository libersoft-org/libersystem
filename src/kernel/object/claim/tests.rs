// WHAT A HOSTILE OR DEAD HOLDER CANNOT DO.
//
// Every one of these names the device it uses. The nearest thing in the tree before this milestone -
// the hardware suite's bus-master check - looked for a device nobody was driving and returned
// quietly when every device on the machine was claimed, which on a healthy boot is most of them. A
// gate whose subject can vanish is a gate that passes when there was nothing to test, so the subject
// here is a SYNTHETIC device-table entry: it cannot vanish, nothing else can claim it, and there is
// no branch in which one of these tests has nothing to say.

use super::*;
use crate::device::{self, ClaimError, ClaimState};

crate::tagged_test!(a_second_claim_of_one_device_is_refused_by_name, [Object, Kernel, Pci], id = "kernel.object.claim.a_second_claim_of_one_device_is_refused_by_name", covers = ["kernel"]);
fn a_second_claim_of_one_device_is_refused_by_name() {
	// THE COUNT IS GONE AND THIS IS WHY. `acquire_bus_master` incremented an owner count and turned
	// bus mastering on at the 0 -> 1 transition, and nothing refused a second acquisition - so two
	// drivers naming one index both got a `DeviceMemory` for its BAR and both drove it. Exclusivity
	// held only because DeviceManager happens to launch one driver per device.
	//
	// And the refusal has a name of its own rather than the ERR_INVALID everything used to collapse
	// into: a caller that cannot tell "somebody else has it" from "you passed nonsense" cannot retry
	// correctly, because the first is worth waiting on and the second never will be.
	let index = device::add_synthetic_device();
	assert_eq!(device::claim_state(index), Some(ClaimState::Free), "a fresh slot is free");
	let key = device::claim(index).expect("the first claim succeeds");
	assert_eq!(device::claim_state(index), Some(ClaimState::Claimed));
	assert_eq!(device::claim(index), Err(ClaimError::AlreadyClaimed), "and the second one is refused");
	assert_eq!(device::claim_state(index), Some(ClaimState::Claimed), "a refused claim changes nothing");
	assert_eq!(device::release_claim(key), Ok(ClaimState::Free));
	assert_eq!(device::claim_state(index), Some(ClaimState::Free), "and after the release it is claimable again");
}

crate::tagged_test!(a_new_claim_is_a_new_binding_and_the_old_key_is_refused, [Object, Kernel, Pci], id = "kernel.object.claim.a_new_claim_is_a_new_binding_and_the_old_key_is_refused", covers = ["kernel"]);
fn a_new_claim_is_a_new_binding_and_the_old_key_is_refused() {
	// A NEW CLAIM OF THE SAME DEVICE IS NEVER CONTINUITY WITH THE LAST ONE. The index is the same
	// every time a device is claimed, released and claimed again, so the index alone cannot say
	// which binding anything belongs to. The generation can, and this is the arithmetic the whole
	// milestone rests on: a release naming a stale generation is REFUSED rather than applied to
	// whoever holds the device now.
	let index = device::add_synthetic_device();
	let first = device::claim(index).expect("claimed");
	device::release_claim(first).expect("released");
	let second = device::claim(index).expect("claimed again");
	assert_ne!(first.generation, second.generation, "a new claim of one device is a new binding");
	assert!(second.generation > first.generation, "and generations only ever advance");
	assert_eq!(device::release_claim(first), Err(ClaimError::Stale), "the previous binding's key does not reach this one");
	assert_eq!(device::claim_state(index), Some(ClaimState::Claimed), "and the refusal left the live claim alone");
	device::release_claim(second).expect("the current key still works");
}

crate::tagged_test!(a_slot_that_runs_out_of_generations_is_retired_rather_than_wrapped, [Object, Kernel, Pci], id = "kernel.object.claim.a_slot_that_runs_out_of_generations_is_retired_rather_than_wrapped", covers = ["kernel"]);
fn a_slot_that_runs_out_of_generations_is_retired_rather_than_wrapped() {
	// `checked_add`, not `wrapping_add`. A wrapped generation makes a stale key valid again, which
	// is the one thing the key exists to prevent - and it is the rule handle slots already follow.
	// At one claim per microsecond a `u64` lasts about six hundred thousand years, so this branch is
	// unreachable in practice; that is exactly why it is worth executing here rather than reasoning
	// about.
	let index = device::add_synthetic_device();
	device::exhaust_generations_of(index);
	assert_eq!(device::claim(index), Err(ClaimError::Retired), "the slot has no generation left to mint");
	assert_eq!(device::claim(index), Err(ClaimError::Retired), "and it stays retired for the life of the boot");
}

crate::tagged_test!(a_claim_handle_settles_once_and_reports_which_binding_it_was, [Object, Kernel, Pci], id = "kernel.object.claim.a_claim_handle_settles_once_and_reports_which_binding_it_was", covers = ["kernel"]);
fn a_claim_handle_settles_once_and_reports_which_binding_it_was() {
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let claim = Claim::create(key).expect("a claim object");
	assert!(!claim.is_settled(), "a live claim has not settled");
	assert_eq!(claim.outcome(), None);
	assert_eq!(claim.key().device_index as usize, index);
	assert_eq!(claim.release(), abi::CLAIM_STATE_FREE, "the teardown confirmed, so the device is free");
	assert!(claim.is_settled());
	assert_eq!(claim.release(), abi::CLAIM_STATE_FREE, "a second release answers with the first one's result");
	assert_eq!(device::claim_state(index), Some(ClaimState::Free));
}

crate::tagged_test!(the_last_close_of_a_claim_handle_is_a_forced_release, [Object, Kernel, Pci, Handle], id = "kernel.object.claim.the_last_close_of_a_claim_handle_is_a_forced_release", covers = ["kernel"]);
fn the_last_close_of_a_claim_handle_is_a_forced_release() {
	// NOT A LEAK AND NOT A SILENT ORPHAN. A DeviceManager that died is the case a cold
	// reconstruction has to survive, and it survives it by finding devices that are `Free`,
	// `Releasing` or `Quarantined` rather than claimed by a process that no longer exists.
	//
	// The last close and the last `Arc` coincide here BY CONSTRUCTION rather than by luck: a claim
	// handle carries neither TRANSFER nor DUPLICATE, so it can never be in a message, in a second
	// table, or in two slots of one.
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	assert_eq!(device::claim_state(index), Some(ClaimState::Claimed));
	{
		let _claim = Claim::create(key).expect("a claim object");
		assert_eq!(device::claim_state(index), Some(ClaimState::Claimed), "still held while the object lives");
	}
	assert_eq!(device::claim_state(index), Some(ClaimState::Free), "and released the moment the last reference went");
}

crate::tagged_test!(ending_a_claim_takes_the_mapping_and_not_just_the_handle, [Object, Kernel, Pci, Paging], id = "kernel.object.claim.ending_a_claim_takes_the_mapping_and_not_just_the_handle", covers = ["kernel"]);
fn ending_a_claim_takes_the_mapping_and_not_just_the_handle() {
	// A HANDLE THAT REFUSES IS NOT EVIDENCE THAT A MAPPING IS GONE. The driver has a raw virtual
	// address it has been using for as long as it has been running, and revoking the capability does
	// not touch it - so the revocation has to reach the address space as well.
	//
	// Both halves are checked: the object's revocation generation moves, which is what makes every
	// capability to it fail lookup, and the recorded mapping is taken down.
	use crate::object::KernelObject;
	use crate::object::device_memory::DeviceMemory;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let memory = DeviceMemory::for_claim(key, 0xfeed_0000, 0x1000).expect("a device memory");
	let before = memory.header().generation();
	let rows = device::derived_rows();
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(memory.clone() as alloc::sync::Arc<dyn KernelObject>))), "recorded as derived");
	assert_eq!(device::derived_rows(), rows + 1, "the registry grew by the one capability this binding derived");
	// A mapping the revocation has to find. The address space is this thread's, which is the one a
	// driver's mapping would be in.
	// THE KERNEL'S OWN ADDRESS SPACE, because this test does not run on a thread: the runner's
	// context has no current thread, and reaching for one here died on the `expect` rather than on
	// the property being checked. Which space it is does not matter - what matters is that the
	// revocation reaches the one that was RECORDED, rather than whichever happens to be active.
	memory.set_mapped_in(0x4444_0000, crate::sched::kernel_as());
	device::release_claim(key).expect("released");
	assert_ne!(memory.header().generation(), before, "every capability to it is invalid now");
	assert_eq!(memory.mapped_at_for_test(), 0, "and the mapping it had is gone, not merely unreachable");
	// AND THE REGISTRY GAVE THE ROW BACK. It is swept on every release, so a claim that ends takes
	// its own rows out - otherwise a boot that binds and unbinds would grow this table forever.
	assert_eq!(device::derived_rows(), rows, "a released claim leaves nothing behind in the registry");
}

crate::tagged_test!(a_capability_from_a_previous_binding_cannot_speak_for_this_one, [Object, Kernel, Pci, Dma], id = "kernel.object.claim.a_capability_from_a_previous_binding_cannot_speak_for_this_one", covers = ["kernel"]);
fn a_capability_from_a_previous_binding_cannot_speak_for_this_one() {
	// `SYS_DEVICE_QUIESCED`'s whole authority is "the holder of this capability has just reset the
	// hardware". A `DeviceMemory` that outlived its claim - sitting in a message queue, or in a
	// process being torn down - is held by somebody who did no such thing to the device as it is
	// NOW, and without the generation it would release the frames and vectors the current driver is
	// still using. Nothing forged it; it simply became a statement about a different machine.
	use crate::object::device_memory::DeviceMemory;
	let index = device::add_synthetic_device();
	let first = device::claim(index).expect("claimed");
	let stale = DeviceMemory::for_claim(first, 0xfeed_1000, 0x1000).expect("a device memory");
	device::release_claim(first).expect("released");
	let second = device::claim(index).expect("claimed again");
	assert!(!device::claim_is_current(stale.claim().expect("it names a binding")), "the old binding is not the current one");
	assert!(device::claim_is_current(second), "and the current one is");
	device::release_claim(second).expect("released");
	assert!(!device::claim_is_current(second), "a released key is current for nothing");
}

crate::tagged_test!(an_acquisition_that_cannot_answer_leaves_no_device_taken, [Object, Kernel, Pci, Handle, Syscall], id = "kernel.object.claim.an_acquisition_that_cannot_answer_leaves_no_device_taken", covers = ["kernel"]);
fn an_acquisition_that_cannot_answer_leaves_no_device_taken() {
	// THAT ACQUISITION IS ONE OPERATION OR NONE OF IT.
	//
	// It installs two handles and copies a struct out, and a partial success leaves a claim alive
	// that its would-be owner never learned the name of - a device nothing can release and nothing
	// can rebind, for the life of the boot.
	//
	// The copy-out is the last thing that can fail and the only one that can fail AFTER the handles
	// exist, which is what makes it the case worth forcing: everything earlier fails before the
	// device is taken, because both handle slots are BOOKED before either object is minted and an
	// install against a booking cannot fail. So this is the whole rollback - both handles closed,
	// the device given back - reached at the one point where there is something to roll back.
	use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};
	static REFUSAL: AtomicI64 = AtomicI64::new(0);
	static DONE: AtomicBool = AtomicBool::new(false);
	let index = device::add_synthetic_device();
	static INDEX: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
	INDEX.store(index as u64, Ordering::SeqCst);
	extern "C" fn body(_arg: u64) {
		unsafe {
			// A destination the copy cannot reach. The exception table turns the fault into a short
			// write rather than a dead kernel, which is what makes this forceable at all.
			let refusal = crate::arch::syscall::invoke(crate::syscall::SYS_DEVICE_CLAIM, INDEX.load(Ordering::SeqCst), crate::tests::device_privilege(), 0, 0) as i64;
			REFUSAL.store(refusal, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	crate::sched::spawn_with_object(body, crate::object::event::Event::create().expect("a test event"), crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the probe thread ran to completion");
	assert!(REFUSAL.load(Ordering::SeqCst) < 0, "an acquisition that cannot answer is a refusal");
	assert_eq!(device::claim_state(index), Some(ClaimState::Free), "and the device was given back, not left claimed by nobody");
	assert_eq!(device::claim(index).map(|key| key.device_index), Ok(index as u32), "so the next claimant can have it");
}

crate::tagged_test!(a_claim_handle_cannot_leave_the_process_that_took_it, [Object, Kernel, Pci, Handle, Syscall], id = "kernel.object.claim.a_claim_handle_cannot_leave_the_process_that_took_it", covers = ["kernel"]);
fn a_claim_handle_cannot_leave_the_process_that_took_it() {
	// STAYING IS A PROPERTY IT HAS TO BE GIVEN, not one it has by being called that.
	//
	// A claim handle that can be moved leaves the manager's Domain, survives its killing, and holds
	// the forced release off exactly when the machine most needs it. So it carries neither
	// RIGHT_TRANSFER nor RIGHT_DUPLICATE, and both refusals are checked: one right without the other
	// would leave the same hole by the other route.
	use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
	static MOVED: AtomicI64 = AtomicI64::new(0);
	static COPIED: AtomicI64 = AtomicI64::new(0);
	static RELEASED: AtomicI64 = AtomicI64::new(0);
	static DONE: AtomicBool = AtomicBool::new(false);
	static INDEX: AtomicU64 = AtomicU64::new(0);
	let index = device::add_synthetic_device();
	INDEX.store(index as u64, Ordering::SeqCst);
	extern "C" fn body(_arg: u64) {
		unsafe {
			let grant = crate::tests::claim_device(INDEX.load(Ordering::SeqCst)).expect("a synthetic device is claimable");
			let (mut near, mut far): (u64, u64) = (0, 0);
			assert_eq!(crate::arch::syscall::invoke(crate::syscall::SYS_CHANNEL_CREATE, &mut near as *mut u64 as u64, &mut far as *mut u64 as u64, 0, 0) as i64, 0, "a channel to try to move it over");
			let _ = far;
			let payload = *b"c";
			MOVED.store(crate::arch::syscall::invoke(crate::syscall::SYS_CHANNEL_SEND, near, payload.as_ptr() as u64, payload.len() as u64, grant.claim) as i64, Ordering::SeqCst);
			COPIED.store(crate::arch::syscall::invoke(crate::syscall::SYS_HANDLE_DUPLICATE, grant.claim, abi::RIGHTS_ALL as u64, 0, 0) as i64, Ordering::SeqCst);
			// And it still works for the one thing it is for.
			RELEASED.store(crate::arch::syscall::invoke(crate::syscall::SYS_DEVICE_RELEASE, grant.claim, 0, 0, 0) as i64, Ordering::SeqCst);
			DONE.store(true, Ordering::SeqCst);
		}
	}
	crate::sched::spawn_with_object(body, crate::object::event::Event::create().expect("a test event"), crate::object::rights::Rights::ALL);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst), "the probe thread ran to completion");
	assert_eq!(MOVED.load(Ordering::SeqCst), crate::syscall::ERR_ACCESS_DENIED, "a claim handle may not be moved to another process");
	assert_eq!(COPIED.load(Ordering::SeqCst), crate::syscall::ERR_ACCESS_DENIED, "nor copied, which would be the same hole by the other route");
	assert_eq!(RELEASED.load(Ordering::SeqCst), abi::CLAIM_STATE_FREE as i64, "and the handle it refused to move is the one that ends the binding");
	assert_eq!(device::claim_state(index), Some(ClaimState::Free));
}

crate::tagged_test!(a_teardown_that_runs_out_of_time_is_latched_terminal_and_a_late_finish_releases_nothing, [Object, Kernel, Pci], id = "kernel.object.claim.a_teardown_that_runs_out_of_time_is_latched_terminal_and_a_late_finish_releases_nothing", covers = ["kernel"]);
fn a_teardown_that_runs_out_of_time_is_latched_terminal_and_a_late_finish_releases_nothing() {
	// THE RACE M6 IS BUILT AROUND, both outcomes, on one synthetic device.
	//
	// When DeviceManager dies, the last close of its claim handle starts a forced teardown before
	// any new manager exists to bound it - so the deadline is the CLAIM'S OWN, minted at the release
	// from a constant of the kernel's. A new manager reads the state to find out what it may do.
	//
	// The half that has to be latched IN THE KERNEL: a binding marked terminal in userspace while
	// the kernel's claim stays `Releasing` is two authorities over one device, and a late teardown
	// reaching `Free` would put the frames and vectors back into circulation against a state a
	// manager has already been told is final.
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("a fresh synthetic slot claims");

	// BEFORE THE DEADLINE: the snapshot reports what is happening and changes nothing.
	let snapshot = device::snapshot(index).expect("a synthetic slot answers");
	assert_eq!(snapshot.state, abi::CLAIM_STATE_CLAIMED, "a live claim reads as claimed");
	assert_eq!(snapshot.generation, key.generation, "and the snapshot names the binding, not the row");
	assert_eq!(snapshot.release_deadline, 0, "nothing is being torn down, so there is no deadline to answer by");

	// The ordinary path: a release that confirms reaches `Free` and the deadline is spent with it.
	assert_eq!(device::release_claim(key), Ok(device::ClaimState::Free));
	let after = device::snapshot(index).expect("still a slot");
	assert_eq!(after.state, abi::CLAIM_STATE_FREE);
	assert_eq!(after.release_deadline, 0, "a settled claim carries no deadline");

	// AND THE OTHER SIDE. A second binding, torn down past its deadline: the snapshot LATCHES it.
	let second = device::claim(index).expect("free again, so claimable again");
	assert_ne!(second.generation, key.generation, "a new claim is a new binding");
	device::begin_release_for_test(second).expect("the teardown starts");
	let releasing = device::snapshot(index).expect("still a slot");
	assert_eq!(releasing.state, abi::CLAIM_STATE_RELEASING, "a teardown under way reads as releasing");
	assert!(releasing.release_deadline > 0, "and it carries the deadline it must confirm by");

	// Wind the deadline into the past - which is what a teardown that does not complete looks like
	// from outside - and read again. The read is what latches, atomically, under the claim lock.
	device::expire_release_for_test(index);
	let latched = device::snapshot(index).expect("still a slot");
	assert_eq!(latched.state, abi::CLAIM_STATE_QUARANTINED, "the deadline passed and nothing observed the device go quiet");

	// A LATE COMPLETION RELEASES NOTHING. This is the half that would silently undo the latch:
	// the teardown finishes, reports confirmed, and the claim must stay terminal anyway.
	assert_eq!(device::finish_release_for_test(index, true), device::ClaimState::Quarantined, "a completion after the latch releases nothing");
	assert_eq!(device::claim_state(index), Some(device::ClaimState::Quarantined));
	assert_eq!(device::claim(index), Err(ClaimError::Quarantined), "and it is not claimed again this boot");
}
