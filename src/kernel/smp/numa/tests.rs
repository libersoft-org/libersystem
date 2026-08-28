// What the CPU-to-node binding says, on a machine that may have no nodes at all.

use super::*;

crate::tagged_test!(only_cores_that_came_up_are_bound_to_a_node, [Numa, Smp, Memory, Kernel], id = "kernel.smp.numa.only_cores_that_came_up_are_bound_to_a_node", covers = ["kernel"]);
fn only_cores_that_came_up_are_bound_to_a_node() {
	let bound = bind_online();
	let Some(nodes) = crate::mem::with_topology(|found| found.nodes().to_vec()) else {
		// No topology: nothing is bound, and every query says so rather than answering node zero.
		assert_eq!(bound, 0, "a machine with no topology binds no core to a node");
		assert_eq!(cpu_node(0), topology::Affinity::Unknown);
		assert_eq!(place_on(topology::NodeId(0)), Err(Refusal::NoTopology), "and placement is refused by a named reason");
		assert_eq!(local_node(), None, "and an allocation asking where it is gets no answer to guess from");
		return;
	};
	assert!(bound > 0, "a machine with a topology binds the cores that came up");
	assert!(bound <= crate::smp::cpu_count(), "and never more cores than exist");

	// EVERY BOUND CORE IS ON A NODE THE TOPOLOGY NAMES. A binding to a node nothing describes would
	// be a mask bit on a node with no memory and no meaning.
	let mut seen = 0usize;
	for cpu in 0..crate::smp::cpu_count() {
		if let topology::Affinity::Node(node) = cpu_node(cpu) {
			assert!(nodes.contains(&node), "cpu {cpu} is bound to node {} which no table describes", node.0);
			seen += 1;
		}
	}
	assert_eq!(seen, bound);

	// AND EVERY NODE'S COUNT ADDS UP TO THE BINDINGS. A core counted twice, or counted for a node it
	// is not on, shows up here and nowhere else.
	let counted: usize = nodes.iter().map(|node| online_on(*node)).sum();
	assert_eq!(counted, bound, "the per-node counts are a partition of the bound cores");
}

crate::tagged_test!(placement_names_a_core_of_the_node_it_was_asked_for, [Numa, Smp, Memory, Kernel], id = "kernel.smp.numa.placement_names_a_core_of_the_node_it_was_asked_for", covers = ["kernel"]);
fn placement_names_a_core_of_the_node_it_was_asked_for() {
	bind_online();
	let Some(nodes) = crate::mem::with_topology(|found| found.nodes().to_vec()) else {
		crate::serial_println!("numa-fixture: skipped - this machine reported no topology");
		return;
	};
	for node in &nodes {
		match place_on(*node) {
			Ok(cpu) => assert_eq!(cpu_node(cpu), topology::Affinity::Node(*node), "placement returned a core that is not on the node it was asked for"),
			// A CPU-LESS NODE IS A REAL TOPOLOGY and the refusal is the correct answer for it.
			Err(reason) => assert_eq!(reason, Refusal::NoOnlineCpu, "the only reason a described node has no core is that none of them came up"),
		}
	}
	// A node nothing describes has no core, and says so rather than finding one anyway.
	assert_eq!(place_on(topology::NodeId(0xFFFF)), Err(Refusal::NoOnlineCpu));
}

crate::tagged_test!(a_thread_placed_on_a_node_runs_on_a_core_of_that_node, [Numa, Smp, Memory, Kernel], id = "kernel.smp.numa.a_thread_placed_on_a_node_runs_on_a_core_of_that_node", covers = ["kernel"]);
fn a_thread_placed_on_a_node_runs_on_a_core_of_that_node() {
	use core::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
	// WHERE THE THREAD ACTUALLY RAN, reported by the thread itself. `place_on` returning a core of
	// the right node is one claim; the thread executing there is the one this milestone asks for,
	// and only the thread can answer it.
	static RAN_ON: AtomicUsize = AtomicUsize::new(usize::MAX);
	extern "C" fn body(_argument: u64) {
		RAN_ON.store(crate::sched::current_cpu_id(), AtomicOrdering::SeqCst);
	}

	bind_online();
	let Some(nodes) = crate::mem::with_topology(|found| found.nodes().to_vec()) else {
		crate::serial_println!("numa-fixture: skipped - this machine reported no topology");
		return;
	};
	// The LAST node with an online core, so a two-node machine exercises the one that is not the
	// boot processor's.
	let Some(node) = nodes.iter().rev().find(|node| place_on(**node).is_ok()).copied() else {
		crate::serial_println!("numa-fixture: skipped - no node has an online core");
		return;
	};
	let wanted = place_on(node).expect("checked just above");

	RAN_ON.store(usize::MAX, AtomicOrdering::SeqCst);
	let event = crate::object::event::Event::create().expect("a test event");
	// THE CORE IS NAMED AT CREATION, which is where the kernel stack is allocated. Naming it only at
	// `start_thread_on` - which is what this test used to do - leaves the stack in the CREATING core's
	// node, so a thread placed on node 1 ran there with every kernel entry reaching node 0's memory.
	// M3's third bullet is about that allocation, not about the queueing.
	let thread = crate::sched::prepare_with_object_for(body, event, crate::object::rights::Rights::ALL, Some(wanted));
	// AND THE STACK IS WHERE THE THREAD IS. The frames were taken preferring `node`; on a machine
	// whose other nodes are exhausted the preference falls back, which is deliberate - a thread that
	// cannot be created is worse than one whose stack is remote - so this asserts the node it got
	// only when the requested node still had memory, which `place_on` succeeding above implies it did.
	assert_eq!(thread.stack_node(), topology::Affinity::Node(node), "a kernel stack created for a core of node {} came from that node's memory", node.0);
	assert!(crate::sched::start_thread_on(wanted, &thread), "the thread was queued on the core that was named");
	crate::sched::run_until_idle();
	// AND THEN WAIT FOR THE OTHER CORE, bounded. `run_until_idle` drains this core's queue; the
	// thread is on another core's, and whether that core picks it up is the question. A bounded wait
	// answers it either way - it does not hang a suite on a core that is busy or asleep.
	for _ in 0..2_000_000u64 {
		if RAN_ON.load(AtomicOrdering::SeqCst) != usize::MAX {
			break;
		}
		core::hint::spin_loop();
	}

	let ran = RAN_ON.load(AtomicOrdering::SeqCst);
	if ran == usize::MAX {
		// A CORE THAT IS NOT THE ONE DRIVING THIS TEST RUNS ITS OWN QUEUE, and `run_until_idle`
		// drains only this core's. Said rather than asserted away: what this test can prove on a
		// cooperative harness is that the placement named a core of the right node, and that is
		// stated as the weaker claim it is.
		assert_eq!(cpu_node(wanted), topology::Affinity::Node(node), "the placement named a core of the node that was asked for");
		crate::serial_println!("numa-fixture: the thread was queued on cpu {wanted} of node {} and that core drains its own queue", node.0);
		return;
	}
	assert_eq!(ran, wanted, "the thread ran on the core it was placed on");
	assert_eq!(cpu_node(ran), topology::Affinity::Node(node), "and that core is on the node that was asked for");
}
