use alloc::sync::Arc;
use core::any::Any;

use super::handle::{HandleError, HandleTable};
use super::rights::Rights;
use super::{KernelObject, ObjectHeader, ObjectType};

pub(crate) struct TestObject {
	header: ObjectHeader,
	value: u64,
}

impl TestObject {
	pub(crate) fn new(value: u64) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), value })
	}

	fn value(&self) -> u64 {
		self.value
	}
}

impl KernelObject for TestObject {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Event
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

	fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
		self
	}
}

crate::tagged_test!(handle_create_lookup_close, [Handle, Object, Kernel, Smoke], id = "kernel.object.handle_create_lookup_close", covers = ["kernel"]);
fn handle_create_lookup_close() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(42);
	let handle = table.insert_object(obj, Rights::READ | Rights::WRITE, 0);
	assert_eq!(table.len(), 1);
	let looked = table.lookup(handle, Rights::READ).expect("lookup");
	assert_eq!(looked.as_any().downcast_ref::<TestObject>().unwrap().value(), 42);
	table.close(handle).expect("close");
	assert_eq!(table.len(), 0);
	assert!(matches!(table.lookup(handle, Rights::READ), Err(HandleError::BadHandle)));
}

crate::tagged_test!(handle_rights_enforced, [Handle, Object, Kernel], id = "kernel.object.handle_rights_enforced", covers = ["kernel"]);
fn handle_rights_enforced() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(7), Rights::READ, 0);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	assert!(matches!(table.lookup(handle, Rights::WRITE), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_duplicate_attenuates, [Handle, Object, Kernel], id = "kernel.object.handle_duplicate_attenuates", covers = ["kernel"]);
fn handle_duplicate_attenuates() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(1), Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0);
	let weak = table.duplicate(handle, Rights::READ).expect("duplicate");
	assert!(table.lookup(weak, Rights::READ).is_ok());
	assert!(matches!(table.lookup(weak, Rights::WRITE), Err(HandleError::AccessDenied)));
	assert!(matches!(table.duplicate(handle, Rights::EXECUTE), Err(HandleError::AccessDenied)));
	let plain = table.insert_object(TestObject::new(2), Rights::READ, 0);
	assert!(matches!(table.duplicate(plain, Rights::READ), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_revocation_invalidates, [Handle, Object, Kernel], id = "kernel.object.handle_revocation_invalidates", covers = ["kernel"]);
fn handle_revocation_invalidates() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(99);
	let handle = table.insert_object(obj.clone(), Rights::READ, 0);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	obj.header.revoke();
	assert!(matches!(table.lookup(handle, Rights::READ), Err(HandleError::Revoked)));
}

crate::tagged_test!(handle_type_sealing, [Handle, Object, Kernel], id = "kernel.object.handle_type_sealing", covers = ["kernel"]);
fn handle_type_sealing() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(5), Rights::READ, 0);
	assert!(table.lookup_typed(handle, ObjectType::Event, Rights::READ).is_ok());
	assert!(matches!(table.lookup_typed(handle, ObjectType::Channel, Rights::READ), Err(HandleError::WrongType)));
}

crate::tagged_test!(handle_refcount_lifetime, [Handle, Object, Kernel], id = "kernel.object.handle_refcount_lifetime", covers = ["kernel"]);
fn handle_refcount_lifetime() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(3);
	assert_eq!(Arc::strong_count(&obj), 1);
	let handle = table.insert_object(obj.clone(), Rights::READ, 0);
	assert_eq!(Arc::strong_count(&obj), 2);
	let looked = table.lookup(handle, Rights::READ).expect("lookup");
	assert_eq!(Arc::strong_count(&obj), 3);
	drop(looked);
	assert_eq!(Arc::strong_count(&obj), 2);
	table.close(handle).expect("close");
	assert_eq!(Arc::strong_count(&obj), 1);
}

crate::tagged_test!(a_message_carries_several_capabilities, [Object, Kernel, Syscall], id = "kernel.object.a_message_carries_several_capabilities", covers = ["kernel"]);
fn a_message_carries_several_capabilities() {
	use super::channel::{Channel, Message};
	use super::event::Event;

	// A message moved exactly one capability at every layer - the syscall built a one-element
	// vector, and the generated transport had one slot - so an interface op could not hand over
	// two however it was written. A pipeline stage needs its stdin AND its stdout, which is
	// what found this.
	let (sender, receiver) = Channel::create();
	let first = Event::create();
	let second = Event::create();
	let (first_koid, second_koid) = (first.header().koid(), second.header().koid());

	let caps = alloc::vec![super::handle::Capability::new(first as Arc<dyn KernelObject>, Rights::ALL, 0), super::handle::Capability::new(second as Arc<dyn KernelObject>, Rights::ALL, 0),];
	sender.send(Message::new(b"two".to_vec(), caps, 0)).expect("a two-capability message sends");

	let message = receiver.recv().expect("it arrives");
	assert_eq!(message.caps.len(), 2, "both capabilities survive the queue");

	// Identity, not just count: a transport that delivered the same capability twice, or
	// swapped their order, would pass a count check and wire a stage to the wrong end.
	assert_eq!(message.caps[0].object().header().koid(), first_koid, "the first capability is the first one sent");
	assert_eq!(message.caps[1].object().header().koid(), second_koid, "the second is the second, in order");
}

crate::tagged_test!(a_prepared_thread_does_not_run_until_released, [Object, Kernel, Process], id = "kernel.object.a_prepared_thread_does_not_run_until_released", covers = ["kernel"]);
fn a_prepared_thread_does_not_run_until_released() {
	use super::event::Event;
	use core::sync::atomic::{AtomicBool, Ordering};

	// The start gate a pipeline transaction needs: every stage of `a | b | c` must exist and
	// have its endpoints installed before ANY of them runs, or an early stage writes into a
	// consumer that has not been handed its reader yet.
	//
	// The gate was assumed to be missing mechanism - "the current launch starts immediately,
	// so this gate is a required mechanism" - and it is not: `SYS_THREAD_CREATE` and
	// `SYS_THREAD_START` have always been separate capability-gated steps, and every launch
	// path simply called them back to back. This asserts the property that claim rests on.
	static RAN: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(_handle: u64) {
		RAN.store(true, Ordering::SeqCst);
	}

	let thread = crate::sched::prepare_with_object(body, Event::create(), Rights::ALL, 0);

	// The scheduler is given every chance to run it. Without this the test would pass over an
	// implementation that merely deferred the start by a tick.
	crate::sched::run_until_idle();
	assert!(!RAN.load(Ordering::SeqCst), "a prepared thread does not run before it is released");

	crate::sched::start_thread(&thread);
	crate::sched::run_until_idle();
	assert!(RAN.load(Ordering::SeqCst), "releasing the thread runs it");
}

crate::tagged_test!(a_process_group_reaches_every_member, [Object, Kernel, Process], id = "kernel.object.a_process_group_reaches_every_member", covers = ["kernel"]);
fn a_process_group_reaches_every_member() {
	use super::address_space::AddressSpace;
	use super::process::Process;
	use super::process_group::{MAX_GROUP_MEMBERS, ProcessGroup};

	// A pipeline is one job: Ctrl+C interrupts `a | b | c` whole, not whichever stage holds the
	// terminal. ConsoleService holds a single Process handle today, so every stage but one
	// would keep running with nothing able to reach it - which is what M0035j deferred and
	// named.
	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain());
	let stages = alloc::vec![make(), make(), make()];
	let group = ProcessGroup::create(&stages).expect("a three-stage group");
	assert_eq!(group.size(), 3, "the group holds every stage it was created over");
	assert_eq!(group.live().len(), 3, "all three are live before anything ends");
	assert!(!group.finished(), "a group with live members is not finished");

	// Terminating one stage leaves the job unfinished, which is the property a shell needs:
	// `a | b | c` is still running while any stage is. Asserting only the all-dead case would
	// pass over an implementation that reported finished as soon as ONE member ended.
	stages[1].terminate();
	assert!(!group.finished(), "one dead stage does not finish the job");

	stages[0].terminate();
	stages[2].terminate();
	assert!(group.finished(), "the job finishes when every stage has");

	// Membership is Weak, so a group never keeps a dead process alive - a strong reference here
	// would pin every stage for the life of the job table.
	drop(stages);
	assert_eq!(group.live().len(), 0, "dropped members leave the live set");

	// Bounds are refusals, not truncation: a group missing a stage would signal an incomplete
	// job and nothing would say so.
	assert!(ProcessGroup::create(&[]).is_none(), "an empty group is refused");
	let too_many: alloc::vec::Vec<_> = (0..MAX_GROUP_MEMBERS + 1).map(|_| make()).collect();
	assert!(ProcessGroup::create(&too_many).is_none(), "a group past the cap is refused rather than truncated");
}

crate::tagged_test!(a_clean_exit_reports_its_status, [Object, Kernel, Process], id = "kernel.object.a_clean_exit_reports_its_status", covers = ["kernel"]);
fn a_clean_exit_reports_its_status() {
	use super::process::Process;

	// `SYS_USER_EXIT` took no argument and discarded a0, so a process could finish but never
	// say how it went: a waiter saw closure and nothing else, which makes a program that ran
	// and refused indistinguishable from one that worked. Everything downstream of that -
	// `pipefail`, a shell's `$?`, any success-gated step - needs to tell those apart.
	//
	// The latch is exercised directly rather than through the syscall, and that is a
	// constraint rather than a preference: `SYS_USER_EXIT` does not return, it longjmps to the
	// kernel thread that entered ring 3, so a kernel test thread calling it jumps to a stack
	// that was never parked - which double-faults, as the first version of this test did. The
	// syscall's own one line is exercised by every process in every boot instead, since all
	// 311 `exit()` calls in the tree now pass a status through it and a process that could not
	// exit would take the boot chain with it.
	let process = Process::new(super::address_space::AddressSpace::create().expect("an address space for the test process"), crate::sched::root_domain());

	// Nothing reported yet is NOT zero. That distinction is why the report carries a validity
	// flag: a process that faulted never got to say anything, and reading that as a successful
	// zero is how a broken stage would report success.
	assert_eq!(process.exit_status(), None, "a process that has not exited has no status");

	// A failing status is 42 rather than 1, so an implementation that returns "some non-zero"
	// still fails this.
	process.set_exit_status(42);
	assert_eq!(process.exit_status(), Some(42), "the reported status is the one that was set");

	// First writer wins, the same rule the recorded fault follows. A multi-threaded process
	// whose threads exit differently has one answer and it does not change under a reader.
	process.set_exit_status(7);
	assert_eq!(process.exit_status(), Some(42), "a later exit does not overwrite the first");

	// Zero is a real status and must survive the same path, or success would read as absent.
	let clean = Process::new(super::address_space::AddressSpace::create().expect("an address space for the test process"), crate::sched::root_domain());
	clean.set_exit_status(0);
	assert_eq!(clean.exit_status(), Some(0), "exiting 0 reports 0, not None");
}

crate::tagged_test!(system_power_refuses_a_caller_without_the_root_domain, [Object, Kernel, Syscall, Domain], id = "kernel.object.system_power_refuses_a_caller_without_the_root_domain", covers = ["kernel"]);
fn system_power_refuses_a_caller_without_the_root_domain() {
	use super::domain::{Domain, UNLIMITED};
	use core::sync::atomic::{AtomicI64, Ordering};

	// Stopping the machine used to need no capability at all: `SYS_SYSTEM_POWER` took an
	// action word and nothing else, so every ring-3 process in the system could halt it. It
	// now requires MANAGE on the ROOT Domain, and this asserts the refusals - which is the
	// only way to assert it, since the success path does not return and would take the suite
	// with it.
	//
	// Both wrong keys are tried, because they fail for different reasons and only one of them
	// is obvious. A caller with no handle at all is refused by the handle lookup; a caller
	// holding a perfectly good Domain that is NOT the root is refused by the identity check,
	// and that is the case that matters - killing an app Domain is not the same authority as
	// stopping the machine, so "some Domain" must not be enough.
	static NO_HANDLE: AtomicI64 = AtomicI64::new(0);
	static WRONG_DOMAIN: AtomicI64 = AtomicI64::new(0);

	extern "C" fn body(child_domain: u64) {
		unsafe {
			NO_HANDLE.store(crate::arch::syscall::invoke(crate::syscall::SYS_SYSTEM_POWER, 0, abi::POWER_OFF, 0, 0) as i64, Ordering::SeqCst);
			WRONG_DOMAIN.store(crate::arch::syscall::invoke(crate::syscall::SYS_SYSTEM_POWER, child_domain, abi::POWER_OFF, 0, 0) as i64, Ordering::SeqCst);
		}
	}

	let child = Domain::new_child(&crate::sched::root_domain(), UNLIMITED, UNLIMITED, UNLIMITED).expect("a live parent takes a child");
	crate::sched::spawn_with_object(body, child, Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(NO_HANDLE.load(Ordering::SeqCst) < 0, "a caller holding no capability is refused");
	assert_eq!(WRONG_DOMAIN.load(Ordering::SeqCst), crate::syscall::ERR_ACCESS_DENIED, "a caller holding a non-root Domain is refused by identity, not merely by type");
}

crate::tagged_test!(object_property_set_names_an_object, [Object, Kernel, Syscall], id = "kernel.object.object_property_set_names_an_object", covers = ["kernel"]);
fn object_property_set_names_an_object() {
	use super::event::Event;
	use core::sync::atomic::{AtomicBool, Ordering};
	static DONE: AtomicBool = AtomicBool::new(false);
	const NAME: &[u8] = b"irq-driver";
	extern "C" fn body(handle: u64) {
		unsafe {
			let result = crate::arch::syscall::invoke(crate::syscall::SYS_OBJECT_PROPERTY_SET, handle, crate::syscall::PROP_NAME, NAME.as_ptr() as u64, NAME.len() as u64);
			assert_eq!(result as i64, 0, "set name failed");
		}
		DONE.store(true, Ordering::SeqCst);
	}
	let event = Event::create();
	// The driver thread holds a handle to this same Event; the test keeps an Arc to
	// read the label back after the thread names it.
	crate::sched::spawn_with_object(body, event.clone(), Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	assert_eq!(event.header().name().as_deref(), Some("irq-driver"));
}

crate::tagged_test!(a_group_handle_becomes_waitable_only_once_every_stage_ends, [Object, Kernel, Process, Syscall], id = "kernel.object.a_group_handle_becomes_waitable_only_once_every_stage_ends", covers = ["kernel"]);
fn a_group_handle_becomes_waitable_only_once_every_stage_ends() {
	use super::address_space::AddressSpace;
	use super::process::Process;
	use super::process_group::ProcessGroup;
	use super::rights::Rights;
	use core::sync::atomic::{AtomicBool, Ordering};

	// `wait` knew Channel, Event, Timer, Interrupt and Process but NOT ProcessGroup, so a group
	// handle was never ready and a caller polling one waited forever. That is how a pipeline
	// job would have failed to reap: the session polls its handle to notice completion, and a
	// handle that never reports ready keeps the job in the table for the life of the session.
	static READY: AtomicBool = AtomicBool::new(false);
	extern "C" fn probe(group: u64) {
		// A finite deadline, so an unready group returns TIMED_OUT instead of parking this
		// thread forever - which is exactly what the missing arm used to do.
		let result = unsafe { crate::arch::syscall::invoke(crate::syscall::SYS_WAIT, group, 1, 0, 0) };
		READY.store(result as i64 == 0, Ordering::SeqCst);
	}

	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain());
	let stages = alloc::vec![make(), make()];
	let group = ProcessGroup::create(&stages).expect("a two-stage group");

	READY.store(true, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(!READY.load(Ordering::SeqCst), "a group with live stages is not ready");

	// The case worth pinning: SOME stages ended. An implementation reporting ready on the
	// first exit would announce a pipeline finished while a stage was still producing.
	stages[0].terminate();
	READY.store(true, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(!READY.load(Ordering::SeqCst), "a partly exited group is still not ready");

	stages[1].terminate();
	READY.store(false, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL, 0);
	crate::sched::run_until_idle();
	assert!(READY.load(Ordering::SeqCst), "the group is ready once every stage has ended");
}

crate::tagged_test!(the_console_and_display_syscalls_refuse_a_caller_without_the_capability, [Object, Kernel, Syscall], id = "kernel.object.the_console_and_display_syscalls_refuse_a_caller_without_the_capability", covers = ["kernel"]);
fn the_console_and_display_syscalls_refuse_a_caller_without_the_capability() {
	use super::privilege::{Privilege, PrivilegeKind};
	use core::sync::atomic::{AtomicI64, Ordering};

	// Three syscalls that used to need nothing but their own number: `SYS_FRAMEBUFFER_MAP`
	// took the display, `SYS_CONSOLE_ATTACH` redirected the console's input sink, and
	// `SYS_CONSOLE_FEED` typed into a privileged console. Each now needs a Privilege of the
	// matching kind.
	//
	// Three refusals are asserted per call, and the third is the one that matters: holding
	// SOME privilege is not enough. If the kind were not checked, any component granted one of
	// the three would hold all three - and the whole reason there are three is that feeding
	// the console and owning the display are different authorities held by different
	// components.
	//
	// The SUCCESS path is asserted only for `console_feed`, which is harmless: with no console
	// attached it answers WOULD_BLOCK, which is a refusal by the console rather than by the
	// gate. The other two succeed by taking the display and the input sink away from the
	// running system, which would end the suite.
	static NO_HANDLE: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
	static WRONG_KIND: [AtomicI64; 3] = [AtomicI64::new(0), AtomicI64::new(0), AtomicI64::new(0)];
	static RIGHT_KIND_FEED: AtomicI64 = AtomicI64::new(0);

	extern "C" fn body(feed_privilege: u64) {
		unsafe {
			use crate::arch::syscall::invoke;
			use crate::syscall::{SYS_CONSOLE_ATTACH, SYS_CONSOLE_FEED, SYS_FRAMEBUFFER_MAP};
			let mut fb = [0u8; 128];
			// No capability at all.
			NO_HANDLE[0].store(invoke(SYS_CONSOLE_FEED, b'x' as u64, 0, 0, 0) as i64, Ordering::SeqCst);
			NO_HANDLE[1].store(invoke(SYS_CONSOLE_ATTACH, 0, 0, 0, 0) as i64, Ordering::SeqCst);
			NO_HANDLE[2].store(invoke(SYS_FRAMEBUFFER_MAP, fb.as_mut_ptr() as u64, fb.len() as u64, 0, 0) as i64, Ordering::SeqCst);
			// A real capability of the WRONG kind: this thread holds a ConsoleInputSource, so
			// the two calls that want the other two kinds must still refuse it.
			WRONG_KIND[0].store(invoke(SYS_CONSOLE_ATTACH, 0, feed_privilege, 0, 0) as i64, Ordering::SeqCst);
			WRONG_KIND[1].store(invoke(SYS_FRAMEBUFFER_MAP, fb.as_mut_ptr() as u64, fb.len() as u64, feed_privilege, 0) as i64, Ordering::SeqCst);
			// And the matching kind gets past the gate. WOULD_BLOCK (no console attached) is
			// the console's answer, which means the capability check let it through.
			RIGHT_KIND_FEED.store(invoke(SYS_CONSOLE_FEED, b'x' as u64, 0, feed_privilege, 0) as i64, Ordering::SeqCst);
		}
	}

	let feed = Privilege::create(PrivilegeKind::ConsoleInputSource);
	crate::sched::spawn_with_object(body, feed, Rights::ALL, 0);
	crate::sched::run_until_idle();

	for (index, name) in ["console_feed", "console_attach", "framebuffer_map"].iter().enumerate() {
		assert!(crate::syscall::sys_is_err(NO_HANDLE[index].load(Ordering::SeqCst) as u64), "{name} must refuse a caller holding no capability");
	}
	for (index, name) in ["console_attach", "framebuffer_map"].iter().enumerate() {
		assert_eq!(WRONG_KIND[index].load(Ordering::SeqCst), crate::syscall::ERR_ACCESS_DENIED, "{name} must refuse a capability of the wrong kind");
	}
	let allowed = RIGHT_KIND_FEED.load(Ordering::SeqCst);
	assert!(allowed == 0 || allowed == crate::syscall::ERR_WOULD_BLOCK, "console_feed with the matching capability must get past the gate, not be refused by it (got {allowed})");
}
