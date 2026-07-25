use super::super::address_space::AddressSpace;
use super::super::process::Process;
use super::super::{KernelObject, ObjectType};
use super::{Thread, ThreadState};
use crate::sched;

crate::tagged_test!(thread_object_basics, [Object, Process, Smoke]);
fn thread_object_basics() {
	extern "C" fn noop(_arg: u64) {}
	let process = Process::new(AddressSpace::kernel(), sched::root_domain());
	let thread = Thread::new(noop, 0, process);
	assert_eq!(thread.object_type(), ObjectType::Thread);
	assert_eq!(thread.state(), ThreadState::Ready);
	assert!(thread.tid() >= 1);
}
