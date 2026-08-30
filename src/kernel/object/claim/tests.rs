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
	// A REAL MAPPING, THROUGH THE REAL SEAM, WITH REAL PAGE TABLE ENTRIES.
	//
	// This used to hand the object a fabricated address and never install a PTE, so it asserted that
	// a NUMBER had been cleared - which a revocation that left the page tables untouched passes just
	// as well as one that does not. The address space is the kernel's because the runner's context
	// has no current thread; which space it is does not matter, only that the revocation reaches the
	// one that was RECORDED.
	//
	// The sequence is the map syscall's own: claim the reservation, install the entries, commit.
	let space = crate::sched::kernel_as();
	let base = space.alloc_vrange(crate::mem::frame::PAGE_SIZE);
	assert_ne!(base, 0, "a virtual range for the mapping");
	assert!(memory.claim_mapping(), "the reservation is free");
	let flags = crate::arch::paging::PRESENT | crate::arch::paging::WRITABLE | crate::arch::paging::NO_CACHE | crate::arch::paging::NO_EXECUTE;
	space.map(base, memory.aligned_phys_base(), flags);
	assert!(crate::arch::paging::translate(base).is_some(), "the entry is installed before the release, or the assertion below proves nothing");
	assert!(memory.commit_mapping(base, space.clone()), "the claim is live, so the commit stands");
	device::release_claim(key).expect("released");
	assert_ne!(memory.header().generation(), before, "every capability to it is invalid now");
	assert_ne!(memory.mapped_at_for_test(), base, "and the mapping it had is gone, not merely unreachable");
	// THE PAGE TABLE, ASKED DIRECTLY. The record being cleared is the bookkeeping; this is the
	// property - a driver holding the raw address finds nothing there.
	assert!(crate::arch::paging::translate(base).is_none(), "the revocation reached the address space, not only the record");
	// AND THE REGISTRY GAVE THE ROW BACK. It is swept on every release, so a claim that ends takes
	// its own rows out - otherwise a boot that binds and unbinds would grow this table forever.
	assert_eq!(device::derived_rows(), rows, "a released claim leaves nothing behind in the registry");
}

// A CLAIM'S SNAPSHOT SAYS WHAT IT STILL HOLDS, so a manager that did not make the binding can
// reconstruct the charge instead of starting it at zero.
//
// `DeviceClaimSnapshot` carried the state, the generation and the release deadline and nothing else,
// while `granted_resources` - DeviceManager's own count - counts the RESOURCE frames IT sent during
// the CURRENT bind. A reconstructed node has sent none, so it reported a binding charged with nothing
// while the kernel held its MMIO window. M0165's M5 asks for the device-specific holdings to be
// reconstructable from this snapshot, and this is the fixture for it.
crate::tagged_test!(a_claims_snapshot_names_what_it_still_holds, [Object, Kernel, Pci], id = "kernel.object.claim.a_claims_snapshot_names_what_it_still_holds", covers = ["kernel"]);
fn a_claims_snapshot_names_what_it_still_holds() {
	use crate::object::KernelObject;
	use crate::object::device_memory::DeviceMemory;
	let index = device::add_synthetic_device();

	// NOTHING HELD BEFORE ANYBODY CLAIMS IT, which is the baseline a restart is compared against.
	let free = device::snapshot(index).expect("a slot exists for a device the table has");
	assert_eq!(free.mmio_windows, 0, "an unclaimed device holds no window");
	assert_eq!(free.irq_vectors, 0, "an unclaimed device holds no vector");
	assert_eq!(free.iommu_grants, 0, "an unclaimed device holds no grant");

	let key = device::claim(index).expect("claimed");
	let memory = DeviceMemory::for_claim(key, 0xfeed_2000, 0x1000).expect("a device memory");
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(memory.clone() as alloc::sync::Arc<dyn KernelObject>))), "recorded as derived");

	// AND NOW IT NAMES THE WINDOW. Counted from the kernel's own record of what was minted under this
	// key, rather than from a number somebody remembered to keep in step.
	let held = device::snapshot(index).expect("the slot is still there");
	assert_eq!(held.mmio_windows, 1, "the claim derived one MMIO window and its snapshot has to say so - this is what a new manager reconstructs from");
	assert_eq!(held.generation, key.generation, "and the snapshot is about THIS binding");

	device::release_claim(key).expect("released");
	let after = device::snapshot(index).expect("the slot outlives the claim");
	assert_eq!(after.mmio_windows, 0, "a released claim holds no window - the post-restart baseline is zero, which is the whole point of being able to read it");
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

crate::tagged_test!(a_forced_release_takes_a_live_interrupt_away, [Object, Kernel, Pci, Interrupt], id = "kernel.object.claim.a_forced_release_takes_a_live_interrupt_away", covers = ["kernel"]);
fn a_forced_release_takes_a_live_interrupt_away() {
	// THE VECTOR, NOT ONLY THE CAPABILITY.
	//
	// Revoking an interrupt's header makes every HANDLE to it fail lookup and does nothing to the
	// hardware binding: the unbind lived in `Interrupt::drop`, which a forced release cannot reach,
	// because the holder is still running by definition and a wait in progress keeps the object
	// alive as long as it likes. So a released device kept a bound, deliverable vector aimed at the
	// old driver - and the next claimant could not be given that slot either, because it was still
	// owned.
	//
	// This holds the `Arc` across the release, which is the hostile case: nothing here lets the
	// object drop, so anything the release does it has to do itself.
	use crate::object::KernelObject;
	use crate::object::interrupt::Interrupt;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let interrupt = Interrupt::new(0x71).expect("an interrupt object");
	interrupt.mark_bound();
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(interrupt.clone() as alloc::sync::Arc<dyn KernelObject>))), "recorded as derived");
	interrupt.signal();
	assert!(interrupt.is_pending(), "a live interrupt that fired is pending");

	assert_eq!(device::release_claim(key), Ok(ClaimState::Free), "the teardown confirmed");

	assert!(interrupt.is_revoked(), "the release revoked the interrupt itself, not only every handle to it");
	assert!(!interrupt.is_pending(), "and what it had already delivered does not survive the release");
	// AND IT CANNOT BE SIGNALLED AGAIN. The dispatch table holds this weakly and can still upgrade
	// it for a message that was already in flight; what must not happen is that message reaching the
	// old holder.
	interrupt.signal();
	assert!(!interrupt.is_pending(), "a revoked interrupt is not signalled");
}

crate::tagged_test!(closing_the_last_claim_handle_releases_while_another_reference_is_alive, [Object, Kernel, Pci, Handle], id = "kernel.object.claim.closing_the_last_claim_handle_releases_while_another_reference_is_alive", covers = ["kernel"]);
fn closing_the_last_claim_handle_releases_while_another_reference_is_alive() {
	// THE LAST HANDLE, NOT THE LAST `Arc` - and the difference is a whole class of stuck device.
	//
	// The release used to be `Claim::drop`. That is the last strong reference, which equals the last
	// handle only in a single-threaded process: a thread parked in `SYS_WAIT` on the claim holds an
	// `Arc` until the claim settles, so a second thread closing the process's only claim handle
	// dropped no object, started no release, and left the first thread waiting for a settlement
	// nothing would ever produce. An in-flight syscall holding the object does the same for a
	// shorter time.
	//
	// The retained `Arc` here is that waiter. Nothing drops it, and the close must release anyway.
	use crate::object::handle::{Capability, HandleTable};
	use crate::object::rights::Rights;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let claim = Claim::create(key).expect("a claim object");
	let mut table = HandleTable::new();
	let handle = table.insert(Capability::new(claim.clone() as alloc::sync::Arc<dyn crate::object::KernelObject>, Rights::WAIT | Rights::MANAGE));
	assert_eq!(device::claim_state(index), Some(ClaimState::Claimed), "held while the handle is open");
	assert!(!claim.is_settled(), "and the claim has not settled");

	table.close(handle).expect("closed");

	assert!(claim.is_settled(), "closing the last handle settled the claim, with this reference still alive");
	assert_eq!(claim.outcome(), Some(abi::CLAIM_STATE_FREE), "and it settled FREE, which is what a confirmed teardown reports");
	assert_eq!(device::claim_state(index), Some(ClaimState::Free), "the device is claimable again");
	// The reference outlived the release, which is the point: dropping it now must not tear
	// anything down a second time.
	drop(claim);
	assert_eq!(device::claim_state(index), Some(ClaimState::Free));
}

crate::tagged_test!(a_release_in_progress_refuses_a_late_derivation, [Object, Kernel, Pci], id = "kernel.object.claim.a_release_in_progress_refuses_a_late_derivation", covers = ["kernel"]);
fn a_release_in_progress_refuses_a_late_derivation() {
	// THE ONE-TIME SWEEP IS NOT A BARRIER BY ITSELF.
	//
	// `begin_release` moves the slot to `Releasing` under the claims lock and the sweep then drains
	// the derived table under a different one. A syscall that had already passed capability lookup
	// could therefore register its object AFTER the sweep had run, and hand out a capability nothing
	// would ever revoke - against a device already being given away, or against the next binding's
	// domain. Registration now asks whether the key is still the live claim, under the release's own
	// lock.
	use crate::object::KernelObject;
	use crate::object::device_memory::DeviceMemory;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let memory = DeviceMemory::for_claim(key, 0xfeed_1000, 0x1000).expect("a device memory");
	let weak = alloc::sync::Arc::downgrade(&(memory.clone() as alloc::sync::Arc<dyn KernelObject>));
	assert!(device::register_derived(key, weak.clone()), "a live claim derives capabilities");

	device::begin_release_for_test(key).expect("the release starts");
	assert!(!device::register_derived(key, weak.clone()), "a claim that is being released derives nothing further");
	device::finish_release_for_test(index, true);

	// AND A STALE KEY IS REFUSED AFTER THE RELEASE HAS FINISHED, which is the same rule one step
	// later: the generation has moved on and the row would belong to nobody.
	assert!(!device::register_derived(key, weak), "a key whose claim has ended derives nothing at all");
	let next = device::claim(index).expect("claimable again");
	device::release_claim(next).expect("released");
}

crate::tagged_test!(a_release_that_lands_mid_map_leaves_no_mapping_behind, [Object, Kernel, Pci, Paging], id = "kernel.object.claim.a_release_that_lands_mid_map_leaves_no_mapping_behind", covers = ["kernel"]);
fn a_release_that_lands_mid_map_leaves_no_mapping_behind() {
	// THE WINDOW BETWEEN RESERVING A MAPPING AND PUBLISHING IT, which is where a claim release could
	// pass through and find nothing to do.
	//
	// `SYS_DEVICE_MEMORY_MAP` reserves the object, allocates a range, installs the page table
	// entries and only then records where it mapped. A release landing inside that sequence swept a
	// reservation sentinel, read it as "not mapped yet", and unmapped nothing - and the syscall then
	// published a live mapping of device registers AFTER the only sweep the claim will ever run. The
	// holder kept raw BAR access with the claim already `Free`.
	//
	// The window is reproduced exactly rather than raced for: reserve, release, then attempt the
	// commit the syscall would attempt.
	use crate::object::KernelObject;
	use crate::object::device_memory::DeviceMemory;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");
	let memory = DeviceMemory::for_claim(key, 0xfeed_1000, 0x1000).expect("a device memory");
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(memory.clone() as alloc::sync::Arc<dyn KernelObject>))), "recorded as derived");

	let space = crate::sched::kernel_as();
	let base = space.alloc_vrange(crate::mem::frame::PAGE_SIZE);
	assert_ne!(base, 0, "a virtual range for the mapping");
	// The syscall's first step.
	assert!(memory.claim_mapping(), "the reservation is taken, and the object now looks unmapped to a sweep");
	let flags = crate::arch::paging::PRESENT | crate::arch::paging::WRITABLE | crate::arch::paging::NO_CACHE | crate::arch::paging::NO_EXECUTE;
	space.map(base, memory.aligned_phys_base(), flags);
	assert!(crate::arch::paging::translate(base).is_some(), "the entries are installed, which is the state the release has to survive");

	// THE RELEASE ARRIVES HERE, one instruction before the commit.
	device::release_claim(key).expect("released");

	// AND THE COMMIT IS REFUSED, which is the fix. Publishing here would leave a mapping no sweep
	// will ever visit.
	assert!(!memory.commit_mapping(base, space.clone()), "a claim that ended while the mapping was being built does not get to publish it");
	// The builder takes its own work down, because nothing else can: the sweep had nothing to find.
	space.unmap(base);
	space.free_vrange(base, crate::mem::frame::PAGE_SIZE);
	assert!(crate::arch::paging::translate(base).is_none(), "and no mapping of device registers is left behind");
	// AND THE OBJECT IS TERMINAL: a second attempt cannot reserve it again, so a holder cannot simply
	// call map once more after the claim is gone.
	assert!(!memory.claim_mapping(), "a revoked device memory is never mappable again");
}

crate::tagged_test!(a_rolled_back_msi_acquire_does_not_unbind_the_slots_next_owner, [Object, Kernel, Interrupt], id = "kernel.object.claim.a_rolled_back_msi_acquire_does_not_unbind_the_slots_next_owner", covers = ["kernel"]);
fn a_rolled_back_msi_acquire_does_not_unbind_the_slots_next_owner() {
	// A ROLLBACK THAT RUNS AFTER THE SLOT HAS BEEN GIVEN AWAY, which is what the acquire path could
	// do.
	//
	// `sys_device_msix_acquire` binds the vector and can still fail afterwards - the derived
	// registry may refuse the registration - and its rollback frees the registry slot. The bound
	// `Interrupt` was left still believing it owned the binding, so its `Drop` called the
	// architectural `unbind` after another core could already have taken the freed slot: mask,
	// unmap and RETIRE, against a replacement's binding.
	//
	// Reproduced without racing: bind, disown as the rollback now does, then bind a REPLACEMENT to
	// the same vector and drop the first. The replacement must still be bound.
	use crate::arch::interrupts::{acquire_msi, bind_msi, is_bound, release_msi_for_device, release_unused_msi, unbind};
	use crate::mem::frame;
	use crate::object::interrupt::Interrupt;
	// A frame standing in for a device's MSI-X table, the same fixture the ports' own interrupt
	// suites use: `acquire_msi` programs entry 0 into it.
	let table = frame::allocate().expect("a frame for the fake MSI-X table");
	let owner: u32 = 41;
	let Some(vector) = acquire_msi(table, 0, owner) else {
		crate::serial_println!("    no MSI vector free on this machine - the rollback case is not exercised");
		// SAFETY: allocated by this call and never mapped.
		unsafe { frame::deallocate(table) };
		return;
	};
	let first = Interrupt::new(vector).expect("an interrupt object");
	assert!(bind_msi(vector, &first), "the first binder takes the slot");
	assert!(is_bound(vector), "and the slot says so");

	// THE ROLLBACK, IN THE ORDER THE SYSCALL NOW PERFORMS IT: disown, then free. Reversing these two
	// lines is the defect - the slot becomes reusable while this object still claims it.
	first.disown();
	release_unused_msi(vector);
	assert!(!is_bound(vector), "the rollback gave the binding back");

	// THE NEXT OWNER, which the stale drop below must not touch. The slot was FREED rather than
	// retired, so it is handed straight out again.
	let again = acquire_msi(table, 0, owner).expect("the freed slot is handed out again");
	assert_eq!(again, vector, "the same slot, which is what makes the collision below possible at all");
	let replacement = Interrupt::new(vector).expect("a second interrupt object");
	assert!(bind_msi(vector, &replacement), "the replacement takes the slot the rollback gave back");
	drop(first);
	assert!(is_bound(vector), "the rolled-back acquire's drop did not tear down the slot's new owner");

	// And the replacement gives it back the ordinary way, so the vector is not leaked to the rest of
	// the suite.
	unbind(vector);
	let _ = release_msi_for_device(owner);
	// SAFETY: allocated by this call and never mapped.
	unsafe { frame::deallocate(table) };
}

crate::tagged_test!(ending_a_claim_takes_the_dma_buffers_it_authorised, [Object, Kernel, Pci, Dma], id = "kernel.object.claim.ending_a_claim_takes_the_dma_buffers_it_authorised", covers = ["kernel"]);
fn ending_a_claim_takes_the_dma_buffers_it_authorised() {
	// THE THIRD KIND OF DERIVED CAPABILITY, and the one M9 names that had no test of its own.
	//
	// A `DmaBuffer` created against a device capability is stamped with the CLAIM that capability
	// carries, and registered in the derived table for exactly the reason the MMIO mapping and the
	// interrupt are: a buffer the revocation cannot reach outlives the binding that justified it,
	// and its frames are physical addresses a device may still have in a live descriptor. The MMIO
	// and interrupt halves were each proved and this one was assumed.
	use crate::object::KernelObject;
	use crate::object::device_memory::DeviceMemory;
	use crate::object::dma_buffer::DmaBuffer;
	let index = device::add_synthetic_device();
	let key = device::claim(index).expect("claimed");

	// The buffer is created the way the syscall creates it - against the claim, in a Domain - and
	// registered as derived the way the syscall registers it.
	let memory = DeviceMemory::for_claim(key, 0xfeed_2000, 0x1000).expect("a device memory");
	assert_eq!(memory.claim(), Some(key), "the device capability carries the claim the buffer is stamped from");
	let buffer = DmaBuffer::create_for(&crate::sched::root_domain(), 0x1000, memory.device_index()).expect("a dma buffer");
	let before = buffer.header().generation();
	let rows = device::derived_rows();
	assert!(device::register_derived(key, alloc::sync::Arc::downgrade(&(buffer.clone() as alloc::sync::Arc<dyn KernelObject>))), "recorded as derived");
	assert_eq!(device::derived_rows(), rows + 1, "the registry grew by the buffer this binding authorised");
	// A buffer with frames, so "the revocation reached it" is a claim about something real.
	assert!(!buffer.frames().is_empty(), "the buffer holds physical frames a device could name");

	device::release_claim(key).expect("released");

	// EVERY CAPABILITY TO IT IS INVALID, which is what a generation bump means: a handle naming this
	// object no longer resolves, so a driver that kept one cannot hand its frames to the device.
	assert_ne!(buffer.header().generation(), before, "the buffer's capabilities are revoked with the claim that authorised them");
	assert_eq!(device::derived_rows(), rows, "and the released claim leaves no row behind");
}

crate::tagged_test!(two_threads_attenuating_one_handle_move_it_exactly_once, [Object, Kernel, Channel, Handle], id = "kernel.object.claim.two_threads_attenuating_one_handle_move_it_exactly_once", covers = ["kernel"]);
fn two_threads_attenuating_one_handle_move_it_exactly_once() {
	// TWO THREADS OF ONE PROCESS RACING ONE HANDLE, which is the shape M9 names and the channel
	// tests do not have: they drive a sender and a receiver, each with its own handle, so nothing
	// there contends for a single table entry.
	//
	// An attenuating send MOVES the handle - the sender's copy is spent - so two threads sending the
	// SAME handle must produce exactly one delivery and one refusal. The failure this rules out is a
	// send that reads the entry, builds the attenuated capability, and only then removes the source:
	// both threads would pass the read and the receiver would get the capability twice, which is a
	// capability duplicated by a race rather than by `SYS_HANDLE_DUPLICATE`.
	use crate::object::channel::Channel;
	use core::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
	static RESULTS: [AtomicI64; 2] = [AtomicI64::new(i64::MIN), AtomicI64::new(i64::MIN)];
	static DONE: AtomicUsize = AtomicUsize::new(0);
	static SUBJECT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

	// Both threads send the same handle over their own endpoint argument.
	extern "C" fn racer(slot_and_channel: u64) {
		unsafe {
			let slot = (slot_and_channel >> 32) as usize;
			let channel = slot_and_channel & 0xffff_ffff;
			let subject = SUBJECT.load(Ordering::SeqCst);
			let payload = *b"r";
			let transfer = abi::CapTransfer { handle: subject, rights: abi::RIGHT_READ, _pad: 0 };
			let answer = crate::arch::syscall::invoke(crate::syscall::SYS_CHANNEL_SEND_ATTENUATED, channel, payload.as_ptr() as u64, payload.len() as u64, &transfer as *const abi::CapTransfer as u64) as i64;
			RESULTS[slot].store(answer, Ordering::SeqCst);
			DONE.fetch_add(1, Ordering::SeqCst);
		}
	}

	RESULTS[0].store(i64::MIN, Ordering::SeqCst);
	RESULTS[1].store(i64::MIN, Ordering::SeqCst);
	DONE.store(0, Ordering::SeqCst);

	// One process, one handle table. The subject is a MemoryObject because it is the cheapest thing
	// to mint with real rights; what is under test is the table entry, not the object.
	let (a, b) = Channel::create();
	let (process, first) = crate::sched::prepare_shared_process(a.clone(), Rights::ALL);
	let second = process.install(b.clone(), Rights::ALL).expect("a second endpoint in the same table");
	let subject_object = crate::object::memory_object::MemoryObject::create_in(process.domain(), 4096).expect("a subject");
	let subject = process.install(subject_object, Rights::ALL).expect("the handle both threads race for");
	SUBJECT.store(subject, Ordering::SeqCst);

	let one = crate::sched::prepare_in_process(racer, first, &process);
	let two = crate::sched::prepare_in_process(racer, (1u64 << 32) | second, &process);
	crate::sched::start_thread(&one);
	crate::sched::start_thread(&two);
	crate::sched::run_until_idle();

	assert_eq!(DONE.load(Ordering::SeqCst), 2, "both threads ran");
	let (first_answer, second_answer) = (RESULTS[0].load(Ordering::SeqCst), RESULTS[1].load(Ordering::SeqCst));
	// EXACTLY ONE MOVED IT. The other finds the entry gone and is refused - it does not deliver a
	// second copy, and it does not succeed silently.
	let delivered = (first_answer == 0) as usize + (second_answer == 0) as usize;
	assert_eq!(delivered, 1, "one send moved the handle and the other did not: {first_answer} and {second_answer}");
	let refused = (first_answer == crate::syscall::ERR_BAD_HANDLE) as usize + (second_answer == crate::syscall::ERR_BAD_HANDLE) as usize;
	assert_eq!(refused, 1, "and the loser was refused by name rather than failing some other way: {first_answer} and {second_answer}");
	// AND THE SOURCE ENTRY IS SPENT ONCE. A table that still holds it would mean the move duplicated
	// rather than transferred.
	let spent = process.handles().lock().lookup(crate::object::handle::Handle::from_raw(subject), Rights::empty()).is_err();
	assert!(spent, "the raced handle is gone from the table it was sent from - a move that duplicated would leave it there");
}
