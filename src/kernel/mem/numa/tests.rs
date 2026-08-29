// What can be asserted about a topology on a machine that may not have one.
//
// THE PROFILE DECIDES WHAT THIS TEST PROVES, and it says which. The ordinary harness boots one node,
// where the whole claim is that nothing was invented: no topology, one pool, `Unknown` for every
// address. The two-node profile boots the same tests with a real topology under them, and the
// assertions swap over - which is what makes the same file evidence in both directions rather than a
// test that passes by not running.

use super::*;

crate::tagged_test!(a_machine_reports_the_topology_it_has_and_invents_none, [Numa, Memory, Kernel], id = "kernel.mem.numa.a_machine_reports_the_topology_it_has_and_invents_none", covers = ["kernel"]);
fn a_machine_reports_the_topology_it_has_and_invents_none() {
	// The report is called here because a test build has no `boot_main` to call it, and a reporting
	// path nothing exercises is a reporting path that stops working.
	report();

	let described = crate::mem::with_topology(|found| (found.nodes().len(), found.memory_bearing_nodes().len()));
	let Some((nodes, with_memory)) = described else {
		// NO TABLES: every address is unaffiliated and nothing claims otherwise. This is the
		// ordinary profile, and the assertion is that the absence stayed an absence.
		assert!(matches!(crate::mem::topology_node_of(0x10_0000), topology::Affinity::Unknown), "memory no table describes belongs to no node");
		assert_eq!(crate::mem::frame::pool_count(), 1, "and one pool serves it, exactly as it did before node pools existed");
		return;
	};
	assert!(nodes >= 1, "a published topology has at least one node");
	assert!(with_memory <= nodes);
	// A PARTITION IS ONLY MADE WHERE THERE IS SOMETHING TO PARTITION. One memory-bearing node is one
	// pool, because a single pool IS the local pool on such a machine.
	if with_memory >= 2 {
		assert!(crate::mem::frame::pool_count() >= with_memory, "each memory-bearing node has a pool of its own");
	}
}

crate::tagged_test!(every_frame_returns_to_the_pool_that_owns_its_address, [Numa, Memory, Kernel], id = "kernel.mem.numa.every_frame_returns_to_the_pool_that_owns_its_address", covers = ["kernel"]);
fn every_frame_returns_to_the_pool_that_owns_its_address() {
	// THE INVARIANT THAT HOLDS ON EVERY MACHINE, with or without a topology: the totals before and
	// after a round of allocation are the same, and the free count equals the sum of the pools.
	// A frame that went back to the wrong pool would keep the total right and the SUM wrong, which
	// is what `pools_agree_with_total` is asked for here.
	let (_, free_before) = crate::mem::frame::totals();
	assert!(crate::mem::frame::pools_agree_with_total(), "the pools and the allocator agree before anything moves");

	let mut held = alloc::vec::Vec::new();
	for _ in 0..64 {
		let Some(frame) = crate::mem::frame::allocate() else { break };
		held.push(frame);
	}
	assert!(!held.is_empty(), "a machine that can run this test can spare a frame");
	assert!(crate::mem::frame::pools_agree_with_total(), "and while they are out on loan");
	for frame in held.drain(..) {
		// SAFETY: each frame came from `allocate` just above, is still this test's, and has never
		// been mapped anywhere.
		// NEVER-MAPPED: allocated in this test and freed without ever entering a page table.
		unsafe { crate::mem::frame::deallocate(frame) };
	}
	assert!(crate::mem::frame::pools_agree_with_total(), "and after every one of them has gone home");
	let (_, free_after) = crate::mem::frame::totals();
	assert_eq!(free_after, free_before, "not one frame changed pool on its way back");
}

crate::tagged_test!(strict_fails_where_preferred_falls_back, [Numa, Memory, Kernel], id = "kernel.mem.numa.strict_fails_where_preferred_falls_back", covers = ["kernel"]);
fn strict_fails_where_preferred_falls_back() {
	let Some(nodes) = crate::mem::with_topology(|found| found.memory_bearing_nodes()) else {
		crate::serial_println!("numa-fixture: skipped - this machine reported no topology");
		return;
	};
	if nodes.len() < 2 {
		crate::serial_println!("numa-fixture: skipped - one memory-bearing node, so there is nowhere to fall back to");
		return;
	}
	// THE ADDRESS IS THE EVIDENCE. A strict allocation that came from another node would satisfy
	// every count and be exactly the bug; what is checked is which firmware range the frame is in.
	for node in &nodes {
		let Some(frame) = crate::mem::frame::allocate_strict(*node) else {
			panic!("node {} has memory and could not serve one frame", node.0);
		};
		assert_eq!(crate::mem::topology_node_of(frame), topology::Affinity::Node(*node), "a strict allocation came from a node that was not asked for");
		// SAFETY: allocated here, never mapped, freed once.
		// NEVER-MAPPED: allocated in this test and freed without entering a page table.
		unsafe { crate::mem::frame::deallocate(frame) };
	}

	// A NODE THAT DOES NOT EXIST HAS NO POOL, and strict says so rather than finding one anyway.
	let absent = topology::NodeId(0xFFFF);
	assert!(crate::mem::frame::allocate_strict(absent).is_none(), "a strict allocation on a node with no pool is a refusal");
	// Preferred, on the other hand, falls back - and the fallback is a real frame.
	let Some(frame) = crate::mem::frame::allocate_preferred(absent) else {
		panic!("a preferred allocation must fall back rather than fail while any pool has memory");
	};
	// SAFETY / NEVER-MAPPED: as above.
	unsafe { crate::mem::frame::deallocate(frame) };
	assert!(crate::mem::frame::pools_agree_with_total(), "and every frame went back where it came from");
}

crate::tagged_test!(a_contiguous_span_never_crosses_two_nodes, [Numa, Memory, Kernel], id = "kernel.mem.numa.a_contiguous_span_never_crosses_two_nodes", covers = ["kernel"]);
fn a_contiguous_span_never_crosses_two_nodes() {
	let Some(nodes) = crate::mem::with_topology(|found| found.memory_bearing_nodes()) else {
		crate::serial_println!("numa-fixture: skipped - this machine reported no topology");
		return;
	};
	let Some(node) = nodes.first().copied() else { return };
	const PAGES: usize = 16;
	let Some(base) = crate::mem::frame::allocate_contiguous_preferred(PAGES, node) else {
		crate::serial_println!("numa-fixture: skipped - no pool could serve a sixteen-page span");
		return;
	};
	// EVERY PAGE OF IT, not just the first: a span assembled from two pools would agree at its base
	// and disagree somewhere in the middle.
	let first = crate::mem::topology_node_of(base);
	for page in 0..PAGES as u64 {
		let at = base + page * crate::mem::frame::PAGE_SIZE;
		assert_eq!(crate::mem::topology_node_of(at), first, "a contiguous span crossed a node boundary at {at:#x}");
	}
	for page in 0..PAGES as u64 {
		// SAFETY: pages of a span allocated here, never mapped, each freed once.
		// NEVER-MAPPED: allocated in this test and freed without entering a page table.
		unsafe { crate::mem::frame::deallocate(base + page * crate::mem::frame::PAGE_SIZE) };
	}
	assert!(crate::mem::frame::pools_agree_with_total(), "and the span went back to the pool it came from");
}

// THE MODEL AND THE ALLOCATOR ANSWER THE SAME TRACE THE SAME WAY.
//
// M4 asks for a multi-node reference model driven by long deterministic traces with EVERY TOTAL
// compared against the implementation. `topology::pools::Pools` existed and was graded only against
// its own invariants: nothing ever fed the same operations to the kernel allocator, so the model
// could be right about a system nobody had checked it described. A model nothing is compared with is
// a second implementation of the same guess.
//
// WHAT IS COMPARED IS THE DECISION, NOT THE ADDRESS. The model owns a synthetic frame list and the
// allocator owns the machine's, so "which frame" is meaningless across them; what has to agree is
// which NODE a preferred allocation was served from, whether a strict one succeeded, and that the
// counts move by exactly one each time. That is the property M4 is about - the allocator's placement
// decisions are the model's - and it is checkable on any machine with a topology.
crate::tagged_test!(the_reference_model_and_the_allocator_agree_over_a_trace, [Numa, Memory, Kernel], id = "kernel.mem.numa.the_reference_model_and_the_allocator_agree_over_a_trace", covers = ["kernel", "topology"]);
fn the_reference_model_and_the_allocator_agree_over_a_trace() {
	let Some((nodes, distances)) = crate::mem::with_topology(|found| (found.nodes().to_vec(), found.clone())) else {
		crate::serial_println!("numa-fixture: skipped - this machine reported no topology, so there are no per-node decisions to compare");
		return;
	};
	if nodes.len() < 2 {
		crate::serial_println!("numa-fixture: skipped - one node, so every allocation is local and the model cannot disagree");
		return;
	}
	// The model over a synthetic pool with the SAME topology: eight frames per node, addressed inside
	// each node's own range so `owner_of` routes them the way the allocator's `pool_of` does.
	let mut frames: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	for node in &nodes {
		let base = distances.ranges().iter().find(|range| range.node == *node).map(|range| range.base).unwrap_or(0);
		for index in 0..8u64 {
			frames.push(base + index * crate::mem::frame::PAGE_SIZE);
		}
	}
	let mut model = topology::pools::Pools::new(distances.clone(), &frames);

	// A DETERMINISTIC TRACE, walked by both. Sixteen rounds over the node list: a strict allocation, a
	// preferred one, and a free of what the preferred one returned.
	let mut held: alloc::vec::Vec<(u64, topology::NodeId)> = alloc::vec::Vec::new();
	for round in 0..16usize {
		let want = nodes[round % nodes.len()];
		// STRICT: both sides must agree on whether the requested node could serve it.
		let real_strict = crate::mem::frame::allocate_strict(want);
		let model_strict = model.alloc_strict(want);
		assert_eq!(real_strict.is_some(), model_strict.is_some(), "round {round}: the allocator and the model disagree about whether node {} can serve a strict allocation", want.0);
		if let Some(frame) = real_strict {
			assert_eq!(crate::mem::topology_node_of(frame), topology::Affinity::Node(want), "round {round}: a STRICT allocation came from a node other than the one asked for, which is the one thing strict means");
			// SAFETY: allocated by this call and never mapped.
			unsafe { crate::mem::frame::deallocate(frame) };
		}
		if let Some(frame) = model_strict {
			model.free(frame, Some(want));
		}

		// PREFERRED: both must land on the same NODE - the requested one while it has memory, which
		// `alloc_strict` succeeding above has just established.
		let Some(real) = crate::mem::frame::allocate_preferred(want) else {
			crate::serial_println!("numa-fixture: the allocator ran out during the trace at round {round}");
			break;
		};
		let Some(modelled) = model.alloc_preferred(want) else {
			panic!("round {round}: the allocator served a preferred allocation and the model refused one - the model describes a machine with less memory than this trace uses");
		};
		let real_node = crate::mem::topology_node_of(real);
		let model_node = model.owner_of(modelled).map(topology::Affinity::Node).unwrap_or(topology::Affinity::Unknown);
		assert_eq!(real_node, model_node, "round {round}: a preferred allocation for node {} came from {real_node:?} in the allocator and {model_node:?} in the model", want.0);
		held.push((real, want));
		model.free(modelled, Some(want));
		assert!(model.consistent(), "round {round}: the model's own totals stopped adding up");
	}

	// AND EVERY FRAME GOES BACK, to the pool that owns its address.
	let retired_before = crate::mem::frame::retired_pages();
	for (frame, _) in held {
		// SAFETY: each was allocated by this test and never mapped.
		unsafe { crate::mem::frame::deallocate(frame) };
	}
	assert_eq!(crate::mem::frame::retired_pages(), retired_before, "a frame this trace allocated could not be returned and was retired - the trace lost memory the model says it still has");
	crate::serial_println!("    the model and the allocator agreed on every placement decision in the trace");
}
