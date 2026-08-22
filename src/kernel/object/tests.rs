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
	let handle = table.insert_object(obj, Rights::READ | Rights::WRITE);
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
	let handle = table.insert_object(TestObject::new(7), Rights::READ);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	assert!(matches!(table.lookup(handle, Rights::WRITE), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_duplicate_attenuates, [Handle, Object, Kernel], id = "kernel.object.handle_duplicate_attenuates", covers = ["kernel"]);
fn handle_duplicate_attenuates() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(1), Rights::READ | Rights::WRITE | Rights::DUPLICATE);
	let weak = table.duplicate(handle, Rights::READ).expect("duplicate");
	assert!(table.lookup(weak, Rights::READ).is_ok());
	assert!(matches!(table.lookup(weak, Rights::WRITE), Err(HandleError::AccessDenied)));
	assert!(matches!(table.duplicate(handle, Rights::EXECUTE), Err(HandleError::AccessDenied)));
	let plain = table.insert_object(TestObject::new(2), Rights::READ);
	assert!(matches!(table.duplicate(plain, Rights::READ), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_revocation_invalidates, [Handle, Object, Kernel], id = "kernel.object.handle_revocation_invalidates", covers = ["kernel"]);
fn handle_revocation_invalidates() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(99);
	let handle = table.insert_object(obj.clone(), Rights::READ);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	obj.header.revoke();
	assert!(matches!(table.lookup(handle, Rights::READ), Err(HandleError::Revoked)));
}

crate::tagged_test!(handle_type_sealing, [Handle, Object, Kernel], id = "kernel.object.handle_type_sealing", covers = ["kernel"]);
fn handle_type_sealing() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(5), Rights::READ);
	assert!(table.lookup_typed(handle, ObjectType::Event, Rights::READ).is_ok());
	assert!(matches!(table.lookup_typed(handle, ObjectType::Channel, Rights::READ), Err(HandleError::WrongType)));
}

crate::tagged_test!(handle_refcount_lifetime, [Handle, Object, Kernel], id = "kernel.object.handle_refcount_lifetime", covers = ["kernel"]);
fn handle_refcount_lifetime() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(3);
	assert_eq!(Arc::strong_count(&obj), 1);
	let handle = table.insert_object(obj.clone(), Rights::READ);
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
	let first = Event::create().expect("a test event");
	let second = Event::create().expect("a test event");
	let (first_koid, second_koid) = (first.header().koid(), second.header().koid());

	let caps = alloc::vec![super::handle::Capability::new(first as Arc<dyn KernelObject>, Rights::ALL), super::handle::Capability::new(second as Arc<dyn KernelObject>, Rights::ALL),];
	sender.send(Message::new(b"two".to_vec(), caps)).expect("a two-capability message sends");

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

	let thread = crate::sched::prepare_with_object(body, Event::create().expect("a test event"), Rights::ALL);

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
	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain()).expect("a test process");
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
	let process = Process::new(super::address_space::AddressSpace::create().expect("an address space for the test process"), crate::sched::root_domain()).expect("a test process");

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
	let clean = Process::new(super::address_space::AddressSpace::create().expect("an address space for the test process"), crate::sched::root_domain()).expect("a test process");
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
	crate::sched::spawn_with_object(body, child, Rights::ALL);
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
	let event = Event::create().expect("a test event");
	// The driver thread holds a handle to this same Event; the test keeps an Arc to
	// read the label back after the thread names it.
	crate::sched::spawn_with_object(body, event.clone(), Rights::ALL);
	crate::sched::run_until_idle();
	assert!(DONE.load(Ordering::SeqCst));
	event.header().with_name(|name| assert_eq!(name, Some("irq-driver")));
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

	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain()).expect("a test process");
	let stages = alloc::vec![make(), make()];
	let group = ProcessGroup::create(&stages).expect("a two-stage group");

	READY.store(true, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL);
	crate::sched::run_until_idle();
	assert!(!READY.load(Ordering::SeqCst), "a group with live stages is not ready");

	// The case worth pinning: SOME stages ended. An implementation reporting ready on the
	// first exit would announce a pipeline finished while a stage was still producing.
	stages[0].terminate();
	READY.store(true, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL);
	crate::sched::run_until_idle();
	assert!(!READY.load(Ordering::SeqCst), "a partly exited group is still not ready");

	stages[1].terminate();
	READY.store(false, Ordering::SeqCst);
	crate::sched::spawn_with_object(probe, group.clone(), Rights::ALL);
	crate::sched::run_until_idle();
	assert!(READY.load(Ordering::SeqCst), "the group is ready once every stage has ended");
}

crate::tagged_test!(signalling_a_job_reaches_a_group_the_same_way_it_reaches_a_process, [Object, Kernel, Process, Syscall], id = "kernel.object.signalling_a_job_reaches_a_group_the_same_way_it_reaches_a_process", covers = ["kernel"]);
fn signalling_a_job_reaches_a_group_the_same_way_it_reaches_a_process() {
	use super::address_space::AddressSpace;
	use super::process::Process;
	use super::process_group::ProcessGroup;
	use super::rights::Rights;
	use core::sync::atomic::{AtomicI64, Ordering};

	// EVERYTHING THAT SIGNALS A JOB WAS WRITTEN AGAINST A PROCESS, and a pipeline's job-control
	// handle is a ProcessGroup. The tty turns Ctrl+C into `signal(fg, SIG_INT)`, `fg` resumes a
	// stopped job with `signal(job, SIG_CONT)`, and the session's table holds whatever it was
	// handed. Each of those refused a group with `bad handle` - so a foreground pipeline could not
	// be interrupted and a stopped one could not be resumed - and refused it SILENTLY, because
	// nothing checks the return value of a signal.
	//
	// So `SYS_PROCESS_SIGNAL` takes either. This asserts the group case reaches every member,
	// which is what makes one Ctrl+C stop a whole line rather than its last stage.
	static RESULT: AtomicI64 = AtomicI64::new(0);
	extern "C" fn stop_the_group(group: u64) {
		RESULT.store(unsafe { crate::arch::syscall::invoke(crate::syscall::SYS_PROCESS_SIGNAL, group, crate::syscall::SIG_STOP, 0, 0) } as i64, Ordering::SeqCst);
	}

	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain()).expect("a test process");
	let stages = alloc::vec![make(), make(), make()];
	let group = ProcessGroup::create(&stages).expect("a three-stage group");

	// THE REFUSAL FIRST, while nothing is stopped yet - so "no stage was stopped" is a statement
	// about this signal rather than about the order the assertions happen to be in.
	//
	// A stage of a pipeline is given its stdio endpoints and its manifest's capabilities; it is
	// never given the group, precisely so it cannot signal its siblings. This pins the RIGHT rather
	// than the absence: the fallback above must not have widened what a group handle can be used
	// for, and the way to be sure is to hold one without the authority and be refused. A handle
	// with READ can be asked what the job did and cannot end it.
	crate::sched::spawn_with_object(stop_the_group, group.clone(), Rights::READ | Rights::WAIT);
	crate::sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), crate::syscall::ERR_ACCESS_DENIED, "a group handle without MANAGE cannot signal the job");
	for (index, stage) in stages.iter().enumerate() {
		assert!(!stage.is_stopped(), "stage {index} was not stopped by the refused signal");
	}

	crate::sched::spawn_with_object(stop_the_group, group.clone(), Rights::ALL);
	crate::sched::run_until_idle();
	assert_eq!(RESULT.load(Ordering::SeqCst), 0, "the process-signal syscall accepted a group handle");
	// EVERY member, not the first one. A fan-out that stopped at the first process would leave a
	// pipeline half-suspended, which is worse than not suspending it: the stages still running
	// fill their pipes and block against stages that will never drain them.
	for (index, stage) in stages.iter().enumerate() {
		assert!(stage.is_stopped(), "stage {index} was stopped by one signal to the group");
	}
}

crate::tagged_test!(a_group_remembers_what_each_stage_finished_as_after_the_stage_is_gone, [Object, Kernel, Process, Syscall], id = "kernel.object.a_group_remembers_what_each_stage_finished_as_after_the_stage_is_gone", covers = ["kernel"]);
fn a_group_remembers_what_each_stage_finished_as_after_the_stage_is_gone() {
	use super::address_space::AddressSpace;
	use super::process::Process;
	use super::process_group::ProcessGroup;

	// A PIPELINE'S EXIT IS THE LAST STAGE'S, which is a sensible default and a terrible way to
	// notice that the third of five stages died: `a | b | c` where `b` faults still ends with `c`
	// reporting success, because `c` read an empty input and had nothing to complain about. So the
	// group has to remember what each member finished as - and the hard part is WHEN.
	//
	// A group holds its members WEAKLY, deliberately, so it never keeps a dead process alive; a
	// Process releases its user frames on drop rather than on exit, so holding one strongly would
	// pin an address space for the length of the job's history. That means by the time anybody asks
	// about a finished pipeline the processes may be gone, and a stats call that read them would
	// answer differently depending on how quickly it was asked.
	//
	// The record is therefore taken at the moment each member reaches a terminal state, through a
	// weak back-link from the process to its groups. This test is the reason that is not
	// over-engineering: it DROPS every member before reading, which is exactly the state a shell
	// asking about a finished job is in.
	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain()).expect("a test process");
	let stages = alloc::vec![make(), make(), make()];
	let group = ProcessGroup::create(&stages).expect("a three-stage group");

	// Stage 0 exits cleanly with a status, stage 1 is killed, stage 2 exits cleanly with zero -
	// the three answers a pipeline can give, in one group.
	stages[0].set_exit_status(3);
	stages[0].mark_exited();
	stages[1].terminate();
	stages[2].set_exit_status(0);
	stages[2].mark_exited();

	// EVERY STRONG REFERENCE GONE. What is left is the group and its weak members, which is the
	// state that used to make this question unanswerable.
	drop(stages);

	let mut records: [Option<super::process_group::StageRecord>; super::process_group::MAX_GROUP_MEMBERS] = [const { None }; _];
	let written = group.records_into(&mut records);
	assert_eq!(written, 3, "one slot per stage, in creation order");
	let first = records[0].expect("stage 0 finished and was recorded");
	assert_eq!(first.state, crate::syscall::PROC_STATE_STOPPED, "stage 0 exited cleanly");
	assert_eq!(first.completion_valid, 1, "and it got to say what with");
	assert_eq!(first.completion, 3, "which was 3");
	let second = records[1].expect("stage 1 finished and was recorded");
	assert_eq!(second.state, crate::syscall::PROC_STATE_FAILED, "stage 1 was killed");
	// A KILLED STAGE HAS NO EXIT CODE, and `completion_valid` is what says so. Zero is both the
	// commonest success value and the natural "nothing here", so a caller must never have to guess
	// which it is looking at - a pipeline whose middle stage was killed would otherwise read as one
	// whose middle stage succeeded.
	assert_eq!(second.completion_valid, 0, "a killed stage never reported anything");
	let third = records[2].expect("stage 2 finished and was recorded");
	assert_eq!(third.state, crate::syscall::PROC_STATE_STOPPED);
	assert_eq!(third.completion_valid, 1, "an exit status of zero is a status and not an absence");
	assert_eq!(third.completion, 0);
}

crate::tagged_test!(a_stages_slot_survives_an_earlier_stage_being_dropped, [Object, Kernel, Process], id = "kernel.object.a_stages_slot_survives_an_earlier_stage_being_dropped", covers = ["kernel"]);
fn a_stages_slot_survives_an_earlier_stage_being_dropped() {
	use super::address_space::AddressSpace;
	use super::process::Process;
	use super::process_group::{MAX_GROUP_MEMBERS, ProcessGroup};

	// THE POSITION OF A MEMBER IS THE ABI. `SYS_PROCESS_GROUP_STATS` promises per-member stats "in
	// the order the processes were created into the group", and a shell reads slot `i` as stage `i`
	// of the line it typed. So anything that renumbers the list breaks the contract - and the
	// existing records test cannot see it, because it ends all three stages BEFORE dropping any
	// reference, which is the one ordering in which the renumbering is invisible.
	//
	// The order here is the interleaved one a real pipeline produces: a stage finishes, its last
	// reference goes away as the job table reaps it, something reads the live set, and THEN the next
	// stage finishes.
	let make = || Process::new(AddressSpace::create().expect("address space"), crate::sched::root_domain()).expect("a test process");
	let a = make();
	let b = make();
	let c = make();
	let group = ProcessGroup::create(&[a.clone(), b.clone(), c.clone()]).expect("a three-stage group");

	a.set_exit_status(7);
	a.mark_exited();
	drop(a);

	// `finished()` is the condition a group wait completes on, so an ordinary pipeline waiting on
	// its own job reaches this on every poll - which is what makes this a defect on the pipeline
	// path rather than a wrong answer in a diagnostic.
	assert!(!group.finished(), "B and C are still running");

	b.set_exit_status(9);
	b.mark_exited();

	let mut records: [Option<super::process_group::StageRecord>; MAX_GROUP_MEMBERS] = [const { None }; _];
	let written = group.records_into(&mut records);
	assert_eq!(written, 3, "one slot per stage: a dropped member leaves its slot behind, not a gap closed up");
	let first = records[0].expect("stage A finished and was recorded");
	assert_eq!(first.completion, 7, "slot 0 is stage A's and stays stage A's after A is gone");
	let second = records[1].expect("stage B finished and was recorded");
	assert_eq!(second.completion, 9, "slot 1 is stage B's - not whichever position B slid into");
	assert!(records[2].is_none(), "stage C has not finished");

	// The live set skips the dead slot without renumbering the living ones, which is the same
	// property from the other side: C is still the third stage.
	let mut live: [Option<alloc::sync::Arc<Process>>; MAX_GROUP_MEMBERS] = [const { None }; _];
	let count = group.live_into(&mut live);
	assert_eq!(count, 2, "A is gone; B and C are still held here");
	assert_eq!(group.size(), 3, "the group is still three stages wide");

	// AND A GROUP'S STAGES MUST BE DISTINCT. Two capabilities to one process is ordinary, so a
	// caller can hand the same process twice - and a repeated member cannot mean what a pipeline
	// needs it to mean: one slot would take both records, the other would read as permanently
	// running, and one group signal would reach that process twice.
	assert!(ProcessGroup::create(&[c.clone(), c.clone()]).is_none(), "a repeated member is refused rather than half-recorded");

	// AND THE STAGE THAT NEVER FINISHED IS FINISHED HERE. A test that leaves a process in a running
	// state hands the rest of the suite a process that will never exit - which is a resource every
	// later test pays for, and this suite runs two hundred and sixty tests in one boot.
	c.terminate();
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

	let feed = Privilege::create(PrivilegeKind::ConsoleInputSource).expect("a test privilege");
	crate::sched::spawn_with_object(body, feed, Rights::ALL);
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

crate::tagged_test!(a_transfer_that_is_never_committed_is_a_leak_the_table_can_see, [Handle, Object, Kernel], id = "kernel.object.handle.a_transfer_that_is_never_committed_is_a_leak_the_table_can_see", covers = ["kernel"]);
fn a_transfer_that_is_never_committed_is_a_leak_the_table_can_see() {
	// `take_for_transfer` empties a slot and RESERVES it, and exactly one of `commit_taken` or
	// `restore_taken` has to follow. `sys_thread_create`'s success path followed neither, so every
	// spawn that passed a bootstrap capability left a slot that no longer named anything and could
	// never be reused - and, through `uncharge_handles`, a unit of the domain's quota with it.
	//
	// Both endings are exercised here, because the defect was that one of them was missing.
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(7), Rights::ALL);
	assert_eq!(table.len(), 1);

	// Taken: the slot holds nothing, and the handle names nothing.
	let cap = table.take_for_transfer(handle, Rights::TRANSFER).expect("the capability may be taken for transfer");
	assert_eq!(table.len(), 0, "a taken capability is not in the table");
	assert!(table.lookup(handle, Rights::READ).is_err(), "the handle names nothing while the transfer is in flight");

	// Committed: the slot goes back into circulation under a new generation, so the OLD handle
	// value stays dead and the index is available again.
	table.commit_taken(handle);
	let reused = table.insert_object(TestObject::new(9), Rights::ALL);
	assert!(table.lookup(reused, Rights::READ).is_ok(), "the slot is usable again after a committed transfer");
	assert!(table.lookup(handle, Rights::READ).is_err(), "the old handle value must not come back to life");

	// Restored: the capability returns to the very handle it was named by, which is what makes a
	// refused send cost the caller nothing.
	let cap2 = table.take_for_transfer(reused, Rights::TRANSFER).expect("taken again");
	table.restore_taken(reused, cap2);
	assert!(table.lookup(reused, Rights::READ).is_ok(), "a restored capability answers to its original handle");
	drop(cap);
}

crate::tagged_test!(a_table_torn_down_mid_transfer_does_not_hand_the_slot_out_twice, [Handle, Object, Kernel], id = "kernel.object.handle.a_table_torn_down_mid_transfer_does_not_hand_the_slot_out_twice", covers = ["kernel"]);
fn a_table_torn_down_mid_transfer_does_not_hand_the_slot_out_twice() {
	// `close_all` runs when a process terminates, and a transfer can be in flight when it does.
	// It used to clear the free list and push EVERY index - including the one reserved for the
	// transfer - so the `restore_taken` that followed wrote a live capability into a slot that was
	// simultaneously free, and the next insert could hand the same index to somebody else.
	let mut table = HandleTable::new();
	let keep = table.insert_object(TestObject::new(1), Rights::ALL);
	let moving = table.insert_object(TestObject::new(2), Rights::ALL);
	let cap = table.take_for_transfer(moving, Rights::TRANSFER).expect("taken for transfer");

	table.close_all();

	// The reserved index is not in circulation, so a fresh insert cannot collide with it.
	let fresh = table.insert_object(TestObject::new(3), Rights::ALL);
	assert_ne!(fresh.raw(), moving.raw(), "close_all must not hand out a slot with a transfer in flight");
	assert!(table.lookup(fresh, Rights::READ).is_ok(), "the fresh handle is live");

	// And the transfer ends, WITHOUT reviving a handle in a table that has been torn down.
	//
	// This used to assert the opposite - that the capability came back reachable at its own handle -
	// and that was `close_all` failing to be the terminal barrier its name implies: the process had
	// finished tearing down, and an object was then put back into its table and held alive there
	// until the last reference to the `Process` went. A delayed release rather than a leak, and
	// still not what "closed" should mean.
	table.restore_taken(moving, cap);
	assert!(table.lookup(moving, Rights::READ).is_err(), "a closed table takes nothing back");
	assert!(table.lookup(keep, Rights::READ).is_err(), "everything else was closed");

	// The reservation is resolved either way, so the slot is not left in the state that has no exit:
	// reserved, empty, and skipped by everything.
	let after = table.insert_object(TestObject::new(4), Rights::ALL);
	assert!(table.lookup(after, Rights::READ).is_ok(), "the table still works after the transfer ended");
}

crate::tagged_test!(a_transfer_that_can_no_longer_be_resolved_gives_the_slot_back, [Handle, Object, Kernel], id = "kernel.object.handle.a_transfer_that_can_no_longer_be_resolved_gives_the_slot_back", covers = ["kernel"]);
fn a_transfer_that_can_no_longer_be_resolved_gives_the_slot_back() {
	// `take_for_transfer` promises that exactly one of `commit_taken`/`restore_taken` follows, and
	// `sys_thread_create` had a path that could reach NEITHER: the capability was destroyed inside
	// the child by a racing `terminate`, so there was nothing to restore, and the slot stayed
	// `reserved: true` with `cap: None` - not on the free list, never committed, skipped by
	// `close_all` forever, and one unit of handle quota short. `abandon_taken` is the third outcome.
	let domain = crate::object::domain::Domain::root();
	let mut table = HandleTable::new();
	table.set_domain(domain.clone());
	let before = domain.account().handles().used();
	let moving = table.insert_object(TestObject::new(1), Rights::ALL);
	assert_eq!(domain.account().handles().used(), before + 1, "the handle is charged");
	let cap = table.take_for_transfer(moving, Rights::TRANSFER).expect("taken for transfer");
	drop(cap); // the capability is gone: neither party has it, which is the case with no outcome

	table.abandon_taken(moving);
	assert_eq!(domain.account().handles().used(), before, "the quota comes back");
	assert!(table.lookup(moving, Rights::READ).is_err(), "and the handle value is dead");
	// The slot is back in circulation rather than stranded - the whole cost of the missing outcome.
	let reused = table.insert_object(TestObject::new(2), Rights::ALL);
	assert_eq!(reused.raw() as u32, moving.raw() as u32, "the slot is reusable again");
	assert_ne!(reused.raw(), moving.raw(), "under a new generation, so the old value stays dead");
}

crate::tagged_test!(one_device_holds_one_msi_slot, [Object, Kernel], id = "kernel.object.one_device_holds_one_msi_slot", covers = ["kernel"]);
fn one_device_holds_one_msi_slot() {
	use crate::arch::common::msi::MsiRegistry;
	// The registry answered "is this SLOT free" and never "does this owner already hold one". Every
	// backend programs the device's MSI-X table ENTRY 0 for whatever slot it was given, so two live
	// acquisitions for one device produce two vectors and two registry slots pointing at one hardware
	// entry - which then carries the second vector, leaving the first `Interrupt` bound to a vector
	// the device will never raise and its later `unbind` masking the entry the second is live on.
	//
	// `has_live` is the question; `sys_device_msix_acquire` is where it is asked, because the
	// registry is the mechanism and the kernel's own bring-up test uses it directly on a device the
	// booted system has already claimed.
	let registry: MsiRegistry<4> = MsiRegistry::new();
	assert!(!registry.has_live(7), "a fresh registry holds nothing for anybody");
	let first = registry.acquire(7, 4).expect("a free slot for device 7");
	assert!(registry.has_live(7), "and the device holds one now");
	assert!(!registry.has_live(8), "which says nothing about another device");

	// A RETIRED slot does not count. Its `Interrupt` is gone; what the pending state protects is that
	// no NEW owner is given that VECTOR while a message for it may still be in flight - a fact about
	// the slot, not about the device. A driver restarting on the same device reprograms its own
	// entry 0, which is what should happen, and refusing that broke the xHCI bring-up path when this
	// rule was first written into the registry.
	registry.retire(first);
	assert!(!registry.has_live(7), "a pending slot is not a live claim");
	let restarted = registry.acquire(7, 4).expect("a restarting driver takes a fresh slot");
	assert_ne!(restarted, first, "a different slot: the retired one is still waiting for its quiesce");
	assert!(registry.has_live(7), "and that one is live");

	// And the quiesce is what puts the retired slot back.
	assert_eq!(registry.release_for_device(7), 1, "the device's quiesce releases its pending vector");
	registry.free(restarted);
	assert!(!registry.has_live(7), "with nothing left, the device holds nothing");
}

crate::tagged_test!(a_returned_message_is_still_charged_to_the_sender, [Channel, Object, Kernel], id = "kernel.object.channel.a_returned_message_is_still_charged_to_the_sender", covers = ["kernel"]);
fn a_returned_message_is_still_charged_to_the_sender() {
	use crate::object::channel::{Channel, Message};
	use crate::object::domain::Domain;
	// The queued-bytes charge used to be refunded when a message left the queue, which made a
	// receive that then failed its copy put an UNACCOUNTED message back through `return_to_head` -
	// and past the limit. The charge now travels with the message and is released only when
	// delivery commits, so a message that goes back is a message that was never uncounted.
	let domain = Domain::root();
	let (a, b) = Channel::create();
	let payload = alloc::vec![7u8; 64];
	let bytes = payload.len() as u64;
	let before = domain.account().ipc_queue().used();
	a.send_charged(Message::new(payload, alloc::vec::Vec::new()), &domain).expect("the send is accepted and charged");
	let charged = domain.account().ipc_queue().used();
	assert_eq!(charged, before + bytes, "the sender is charged for what is queued");

	// Take it off the queue the way the TRANSACTIONAL receive does - the one the syscall uses, and
	// the only one `return_to_head` is ever paired with - and put it back without committing.
	//
	// It used to be written with `Channel::recv`, which reads the same and is not the same call:
	// that one has no rollback and no put-back, so it commits as it dequeues. Testing the rule
	// through the wrong receive is what let the other one keep a charge nobody released.
	let (id, _, _) = b.peek_identified().expect("the message is there to peek at");
	let Ok(message) = b.recv_identified(id, usize::MAX, abi::MAX_MESSAGE_CAPS) else {
		panic!("the message is there to take");
	};
	assert_eq!(domain.account().ipc_queue().used(), charged, "taking a message off the queue does not refund it - delivery has not committed");
	b.return_to_head(message);
	assert_eq!(domain.account().ipc_queue().used(), charged, "a returned message is still accounted for");

	// And committing releases it exactly once.
	let (id, _, _) = b.peek_identified().expect("still there to peek at");
	let Ok(mut message) = b.recv_identified(id, usize::MAX, abi::MAX_MESSAGE_CAPS) else {
		panic!("still there");
	};
	b.commit_delivery(&mut message);
	assert_eq!(domain.account().ipc_queue().used(), before, "a committed delivery releases the charge");
	drop(message);
	assert_eq!(domain.account().ipc_queue().used(), before, "and dropping it afterwards does not refund it twice");
}

crate::tagged_test!(the_committed_receive_refunds_what_it_takes, [Channel, Object, Kernel], id = "kernel.object.channel.the_committed_receive_refunds_what_it_takes", covers = ["kernel"]);
fn the_committed_receive_refunds_what_it_takes() {
	use crate::object::channel::{Channel, Message};
	use crate::object::domain::Domain;
	// `Channel::recv` has no rollback and no put-back: the message leaves with the caller, so the
	// dequeue IS the point of no return. When the charge moved to the delivery commit, this path
	// was left with no commit at all - so every message the kernel read this way left its sender's
	// Domain charged for bytes queued nowhere. The kernel reads the boot-chain reports and the
	// crash channel exactly like this, in a loop.
	let domain = Domain::root();
	let (a, b) = Channel::create();
	let before = domain.account().ipc_queue().used();
	for _ in 0..16 {
		a.send_charged(Message::new(alloc::vec![7u8; 64], alloc::vec::Vec::new()), &domain).expect("the send is accepted and charged");
	}
	assert_eq!(domain.account().ipc_queue().used(), before + 16 * 64, "the sender is charged for what is queued");
	while let Ok(message) = b.recv() {
		drop(message);
	}
	assert_eq!(domain.account().ipc_queue().used(), before, "a kernel-side drain returns the quota it consumed");

	// And the structural half: a message dropped without any explicit release refunds itself, so no
	// future path can lose a charge by returning early.
	a.send_charged(Message::new(alloc::vec![7u8; 64], alloc::vec::Vec::new()), &domain).expect("charged again");
	let (id, _, _) = b.peek_identified().expect("queued");
	let Ok(taken) = b.recv_identified(id, usize::MAX, abi::MAX_MESSAGE_CAPS) else {
		panic!("taken transactionally, and never committed");
	};
	assert_eq!(domain.account().ipc_queue().used(), before + 64, "still charged while it is in flight");
	drop(taken);
	assert_eq!(domain.account().ipc_queue().used(), before, "and refunded when it dies uncommitted");
}

crate::tagged_test!(an_unmap_cannot_steal_a_mapping_that_is_still_being_made, [Memory, Object, Kernel], id = "kernel.object.memory.an_unmap_cannot_steal_a_mapping_that_is_still_being_made", covers = ["kernel"]);
fn an_unmap_cannot_steal_a_mapping_that_is_still_being_made() {
	use crate::object::memory_object::MemoryObject;

	// A mapper records `(cr3, 0)` - "being mapped, by someone" - drops the object lock, builds the
	// page-table entries, and only then writes the real base. `remove_mapping` matched on `cr3`
	// ALONE, so a sibling thread calling unmap inside that window took the reservation, unmapped
	// `0 + page * PAGE_SIZE` (the first pages of the address space, belonging to whatever else is
	// mapped low), and left the original mapper with nothing to commit into. Its real PTEs then
	// existed in the page tables and in no registry at all: teardown could not find them, and the
	// frames behind them were retired while live translations still pointed there.
	//
	// The window is a lock drop, not a schedule, so the property is checkable without racing
	// anything: reserve, then ask what an unmap sees.
	let object = MemoryObject::create(4096).expect("a one-page object");
	const CR3: u64 = 0x1234_0000;

	assert!(object.reserve_mapping(CR3), "the reservation is taken");
	assert_eq!(object.mapping_for_test(CR3), Some(0), "a reservation is recorded as base 0");

	// THE STEAL, attempted at exactly the moment it used to succeed. `take_committed_mapping` is
	// the selection `remove_mapping` performs; running it directly exercises the rule without
	// needing an address space to unmap pages from.
	assert!(object.take_committed_mapping(CR3).is_none(), "an unmap must not claim a mapping that is not finished");
	assert_eq!(object.mapping_for_test(CR3), Some(0), "and must leave the reservation standing");
	assert_eq!(object.mapping_count_for_test(), 1, "nothing was removed");

	// So the mapper still has its slot, and committing fills it in.
	assert!(object.commit_mapping(CR3, 0x4000), "the reservation survived for its owner to commit");
	assert_eq!(object.mapping_for_test(CR3), Some(0x4000), "the real base replaced the sentinel");

	// A second commit finds no reservation - it must not overwrite a committed base.
	assert!(!object.commit_mapping(CR3, 0x9000), "a commit with no reservation of its own reports failure");
	assert_eq!(object.mapping_for_test(CR3), Some(0x4000), "and changes nothing");

	// And now that it IS committed, an unmap finds it.
	assert_eq!(object.take_committed_mapping(CR3), Some(0x4000), "a committed mapping is removable");
}
