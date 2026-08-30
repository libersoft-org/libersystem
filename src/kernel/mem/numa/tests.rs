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
	//
	// AND THE CORES ARE BOUND FIRST, in the order a boot performs it. `report_machine` calls
	// `bind_online` and THEN reports, because firmware describes processors it believes exist and
	// binding is what establishes which of them answered - so a report taken before it describes a
	// machine no boot ever produces, with every node showing zero online cores. Exercising the
	// reporting path against a state that cannot occur is not exercising it.
	crate::smp::numa::bind_online();
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

	// AND THE GRAPH EXACTLY, not three counts that a table half-read would also satisfy.
	//
	// Counting nodes says nothing about which memory and which processors are on them, and the
	// defect this milestone actually found - a proximity domain read from the wrong offset - kept
	// every count right. What follows asserts the normalized graph itself: each range routes to its
	// own node at BOTH ends, each described processor routes to its node, and the distance matrix is
	// symmetric with every local entry strictly below every remote one.
	crate::mem::with_topology(|found| {
		let named = found.nodes();
		assert!(!found.ranges().is_empty(), "a topology with nodes describes memory somewhere");
		for range in found.ranges() {
			assert!(named.contains(&range.node), "a memory range is on node {} which the node list does not name", range.node.0);
			assert!(range.end() > range.base, "a range with no bytes in it is a table read wrong, not a node with no memory");
			// `found.node_of_address`, NOT `crate::mem::topology_node_of`: this closure runs under
			// the topology lock and that helper takes it again, which on a spinlock is the machine
			// stopping. The routing being asserted is the same function either way.
			assert_eq!(found.node_of_address(range.base), topology::Affinity::Node(range.node), "the first byte of a range routes to another node");
			assert_eq!(found.node_of_address(range.end() - 1), topology::Affinity::Node(range.node), "the last byte of a range routes to another node - the range is being read as shorter or longer than it is");
		}
		for &(hardware_id, node) in found.cpus() {
			assert!(named.contains(&node), "processor {hardware_id} is on node {} which the node list does not name", node.0);
			assert_eq!(found.node_of_cpu(hardware_id), topology::Affinity::Node(node), "a described processor does not route back to its own node");
		}
		for &from in named {
			let local = found.distance(from, from);
			for &to in named {
				assert_eq!(found.distance(from, to), found.distance(to, from), "the distance matrix is not symmetric between nodes {} and {}", from.0, to.0);
				if to != from {
					assert!(found.distance(from, to) > local, "node {} is described as no further from node {} than from itself", to.0, from.0);
				}
			}
			// AND THE FALLBACK ORDER IS THE DISTANCES, STARTING AT HOME. This is the order every
			// preferred allocation walks, so an order that disagreed with the matrix would place
			// memory correctly by accident on a two-node machine and wrongly on any larger one.
			let order = found.fallback_order(from);
			assert_eq!(order.first().copied(), Some(from), "the fallback order for node {} does not start with itself", from.0);
			for pair in order.windows(2) {
				assert!(found.distance(from, pair[0]) <= found.distance(from, pair[1]), "the fallback order for node {} is not sorted by distance", from.0);
			}
		}
	});
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
	// preferred one, and a free of each - on BOTH sides, in the same round.
	//
	// THE TOTALS ARE COMPARED AFTER EVERY OPERATION, not once at the end. A trace that ends with the
	// right numbers can have been wrong in the middle, and the middle is where a placement decision
	// lives.
	//
	// AND THE MODEL IS OUTSIDE EVERY BRACKET, which is the part that has to be got right. The model
	// is an ordinary data structure on the KERNEL HEAP, and a heap that grows takes a frame from the
	// same allocator this is measuring - so a bracket with a model call inside it charges the
	// allocator for the model's own `Vec`. Measured, on riscv64: `round 4: the allocator's totals did
	// not come back`, on a round where nothing was wrong with either side. Each bracket now holds one
	// allocation and its free and nothing else at all.
	//
	// The scratch row is allocated ONCE, above the loop, for the same reason: building it per round
	// would allocate inside the measurement.
	//
	// AND THE COUNTS ARE A FLOOR, NOT AN EQUALITY, because this allocator's reclamation is DELAYED.
	// A free can hand back more than it was given: `deallocate` is where the quarantine gets drained,
	// so a frame some earlier test retired comes home inside this bracket and the count RISES.
	// Measured, on riscv64: `left: (128415, 127809) right: (128415, 127804)` - five frames MORE free
	// after an allocation and its free, from a round where nothing was wrong. An equality here
	// asserts that this allocator does not do the one thing M2 requires it to do.
	//
	// What must hold exactly is the other two: the machine does not change size, and NOTHING IS
	// RETIRED - a frame this trace could not return would be, and that is the failure the count was
	// reaching for. A count that FALLS is that same failure seen from the other side.
	let mut counts_before: alloc::vec::Vec<Option<usize>> = nodes.iter().map(|_| None).collect();
	let retired_start = crate::mem::frame::retired_pages();
	for round in 0..16usize {
		let want = nodes[round % nodes.len()];

		// STRICT, ON THE ALLOCATOR, MEASURED ALONE. Both sides must agree on whether the requested
		// node could serve it - but the model is asked afterwards, outside the bracket.
		let totals_before = crate::mem::frame::totals();
		for (at, node) in nodes.iter().enumerate() {
			counts_before[at] = crate::mem::frame::free_in_node(Some(*node));
		}
		let real_strict = crate::mem::frame::allocate_strict(want);
		let real_strict_node = real_strict.map(crate::mem::topology_node_of);
		if let Some(frame) = real_strict {
			// SAFETY: allocated by this call and never mapped.
			unsafe { crate::mem::frame::deallocate(frame) };
		}
		let (total_after, free_after) = crate::mem::frame::totals();
		assert_eq!(total_after, totals_before.0, "round {round}: the machine changed size across a strict allocation and its free");
		assert!(free_after >= totals_before.1, "round {round}: {} frame(s) went missing across a strict allocation and its free", totals_before.1 - free_after);
		assert_eq!(crate::mem::frame::retired_pages(), retired_start, "round {round}: a frame could not be returned after a strict allocation and was retired - the trace lost memory the model says it still has");
		for (at, node) in nodes.iter().enumerate() {
			let now = crate::mem::frame::free_in_node(Some(*node));
			assert!(now >= counts_before[at], "round {round}: node {}'s own free count fell across a strict allocation and its free, from {:?} to {now:?} - the frame did not go back to the pool that owns its address", node.0, counts_before[at]);
		}
		if real_strict.is_some() {
			assert_eq!(real_strict_node, Some(topology::Affinity::Node(want)), "round {round}: a STRICT allocation came from a node other than the one asked for, which is the one thing strict means");
		}
		let model_strict = model.alloc_strict(want);
		assert_eq!(real_strict.is_some(), model_strict.is_some(), "round {round}: the allocator and the model disagree about whether node {} can serve a strict allocation", want.0);
		if let Some(frame) = model_strict {
			model.free(frame, Some(want));
		}

		// PREFERRED, THE SAME SHAPE. Both must land on the same NODE - the requested one while it has
		// memory, which `alloc_strict` succeeding above has just established.
		let totals_before = crate::mem::frame::totals();
		for (at, node) in nodes.iter().enumerate() {
			counts_before[at] = crate::mem::frame::free_in_node(Some(*node));
		}
		let real = crate::mem::frame::allocate_preferred(want);
		let real_node = real.map(crate::mem::topology_node_of);
		if let Some(frame) = real {
			// THE SAME OWNERSHIP ON BOTH SIDES, WHICH IS WHAT MAKES IT ONE TRACE.
			//
			// This HELD the real frame and freed the model's - and a comment claiming both were
			// freed sat directly above the push that kept one. The two states diverged on the first
			// round and stayed diverged: the model was full for the whole trace while the allocator
			// was being drained, so every later comparison was between two different machines and
			// the model could never have reached a fallback the allocator reached. Both are freed
			// now, in the same round, and the pressure the second phase needs is INJECTED into both
			// rather than accumulated in one.
			// SAFETY: allocated by this call and never mapped.
			unsafe { crate::mem::frame::deallocate(frame) };
		}
		let (total_after, free_after) = crate::mem::frame::totals();
		assert_eq!(total_after, totals_before.0, "round {round}: the machine changed size across a preferred allocation and its free");
		assert!(free_after >= totals_before.1, "round {round}: {} frame(s) went missing across a preferred allocation and its free", totals_before.1 - free_after);
		assert_eq!(crate::mem::frame::retired_pages(), retired_start, "round {round}: a frame could not be returned after a preferred allocation and was retired - the trace lost memory the model says it still has");
		for (at, node) in nodes.iter().enumerate() {
			let now = crate::mem::frame::free_in_node(Some(*node));
			assert!(now >= counts_before[at], "round {round}: node {}'s own free count fell across a preferred allocation and its free, from {:?} to {now:?} - the frame did not go back to the pool that owns its address", node.0, counts_before[at]);
		}
		if real.is_none() {
			crate::serial_println!("numa-fixture: the allocator ran out during the trace at round {round}");
			break;
		}
		let Some(modelled) = model.alloc_preferred(want) else {
			panic!("round {round}: the allocator served a preferred allocation and the model refused one - the model describes a machine with less memory than this trace uses");
		};
		let model_node = model.owner_of(modelled).map(topology::Affinity::Node).unwrap_or(topology::Affinity::Unknown);
		assert_eq!(real_node, Some(model_node), "round {round}: a preferred allocation for node {} came from {real_node:?} in the allocator and {model_node:?} in the model", want.0);
		model.free(modelled, Some(want));
		assert!(model.consistent(), "round {round}: the model's own totals stopped adding up");
	}

	// AND NOTHING WAS RETIRED OVER THE WHOLE TRACE. Every frame was freed in its own round; a frame
	// that could not be returned would be retired, which is memory lost rather than restored.
	//
	// THE SNAPSHOT IS THE ONE FROM BEFORE THE LOOP. This read the counter and compared it with
	// itself on the next line, so it was an assertion that could not fail - a retirement in the
	// middle of the trace passed it exactly as an empty trace did.
	assert_eq!(crate::mem::frame::retired_pages(), retired_start, "a frame this trace allocated could not be returned and was retired - the trace lost memory the model says it still has");

	// PHASE TWO: THE SAME TRACE WITH NODE 0 EMPTY ON BOTH SIDES.
	//
	// Above, neither side is under pressure, so the trace compares the easy half of the contract -
	// where a preferred allocation lands when the node it asked for can serve it. What M5 asks about
	// is the other half, and neither the model nor the allocator reached it. The exhaustion is made
	// the same way on both: the allocator is told to pretend the node is empty, and the model's
	// frames for that node are taken out strictly and held, which for a pool model IS empty.
	let first = nodes[0];
	let mut drained: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
	while let Some(frame) = model.alloc_strict(first) {
		drained.push(frame);
	}
	assert!(!drained.is_empty(), "the model had nothing on node {} to begin with", first.0);
	assert_eq!(model.free_in(Some(first)), 0, "the model's node {} is empty", first.0);
	crate::mem::frame::pretend_node_is_empty(Some(first));

	for round in 0..8usize {
		// STRICT ON THE EMPTY NODE: both refuse, and neither answers with somebody else's memory.
		assert!(model.alloc_strict(first).is_none(), "round {round}: the model served a strict allocation from an empty node");
		assert!(crate::mem::frame::allocate_strict(first).is_none(), "round {round}: the allocator served a strict allocation from an empty node while the model refused");

		// PREFERRED ON THE EMPTY NODE: both fall back, and to the SAME node.
		let Some(modelled) = model.alloc_preferred(first) else {
			panic!("round {round}: the model has no memory left anywhere, so this trace cannot compare a fallback");
		};
		let Some(real) = crate::mem::frame::allocate_preferred(first) else {
			panic!("round {round}: the allocator refused a preferred allocation the model served");
		};
		let model_node = model.owner_of(modelled).map(topology::Affinity::Node).unwrap_or(topology::Affinity::Unknown);
		assert_eq!(crate::mem::topology_node_of(real), model_node, "round {round}: the fallback from an empty node {} went to different nodes in the model and the allocator", first.0);
		assert_ne!(model_node, topology::Affinity::Node(first), "round {round}: the fallback stayed on the node that has nothing");
		// SAFETY: allocated by this call and never mapped.
		unsafe { crate::mem::frame::deallocate(real) };
		model.free(modelled, None);
		assert!(model.consistent(), "round {round}: the model's totals stopped adding up under pressure");
	}

	crate::mem::frame::pretend_node_is_empty(None);
	for frame in drained {
		model.free(frame, Some(first));
	}
	assert!(model.consistent(), "the model is back where it started");
	crate::serial_println!("    the model and the allocator agreed on every placement decision in the trace, with node {} full and empty", first.0);
}

crate::tagged_test!(the_placement_matrix_runs_through_the_real_allocator, [Numa, Memory, Kernel], id = "kernel.mem.numa.the_placement_matrix_runs_through_the_real_allocator", covers = ["kernel"]);
fn the_placement_matrix_runs_through_the_real_allocator() {
	// M5'S FAILURE MATRIX, DRIVEN THROUGH THE ALLOCATOR THIS KERNEL USES.
	//
	// What was here asked for a node that does not exist (`0xFFFF`) and accepted a preferred success
	// without checking WHICH node served it - so the fallback was proved to happen and never proved
	// to go anywhere in particular, and the exhaustion case was a node with no pool rather than a
	// pool with no memory. Those are different states: one exercises "there is nothing to ask", the
	// other "the node is real and has nothing left", and only the second is the one a machine
	// reaches.
	//
	// EXHAUSTION IS INJECTED, NOT PERFORMED. Emptying a real node means taking every frame it has -
	// millions, on any machine worth testing - and the matrix needs the STATE rather than the work.
	// `pretend_node_is_empty` leaves the pool exactly as it is and makes the two node-aware paths
	// skip it, so what is exercised is the preference ORDER rather than a shortcut around it.
	let Some(nodes) = crate::mem::with_topology(|found| found.memory_bearing_nodes()) else {
		crate::serial_println!("numa-matrix: skipped - this machine reported no topology");
		return;
	};
	if nodes.len() < 2 {
		crate::serial_println!("numa-matrix: skipped - one memory-bearing node, so there is nowhere to fall back to");
		return;
	}
	let (first, second) = (nodes[0], nodes[1]);
	let (total_before, free_before) = crate::mem::frame::totals();
	// THE EXACT PER-NODE FIGURES, which is what M5 asks to be restored. The global count returning to
	// where it started is also what a matrix that moved a frame from one pool to the other and back
	// would produce.
	let (in_first, in_second) = (crate::mem::frame::free_in_node(Some(first)), crate::mem::frame::free_in_node(Some(second)));
	assert!(in_first.is_some() && in_second.is_some(), "both memory-bearing nodes have a pool of their own");

	crate::mem::frame::pretend_node_is_empty(Some(first));

	// 1. STRICT REFUSES. The node is real, has a pool, and can serve nothing.
	assert!(crate::mem::frame::allocate_strict(first).is_none(), "a strict allocation on an exhausted node is a refusal, not a frame from somewhere else");

	// 2. PREFERRED FALLS BACK, AND TO A NAMED NODE. The address is the evidence: a fallback that
	//    satisfied the count and came from anywhere would be exactly the defect.
	let Some(fallback) = crate::mem::frame::allocate_preferred(first) else {
		panic!("a preferred allocation must fall back while another pool has memory");
	};
	let served = crate::mem::topology_node_of(fallback);
	assert_ne!(served, topology::Affinity::Node(first), "the fallback did not come from the exhausted node");
	assert_eq!(served, topology::Affinity::Node(second), "and it came from the next node in the preference order, not from whichever pool answered first");

	// 3. A CONTIGUOUS SPAN GOES TO THE NODE THAT WAS ASKED FOR, and every page of it. Asking for the
	//    second node while the first is exhausted also proves the span did not silently move.
	let pages = 4usize;
	let Some(span) = crate::mem::frame::allocate_contiguous_preferred(pages, second) else {
		panic!("a four-page span could not be served from a node with memory");
	};
	for page in 0..pages as u64 {
		assert_eq!(crate::mem::topology_node_of(span + page * 4096), topology::Affinity::Node(second), "page {page} of a contiguous span left the node it was placed on");
	}

	// 4. EVERYTHING GOES BACK, ROUTED BY ADDRESS.
	// SAFETY / NEVER-MAPPED: allocated here, never entered a page table, freed once.
	unsafe {
		crate::mem::frame::deallocate(fallback);
		for page in 0..pages as u64 {
			crate::mem::frame::deallocate(span + page * 4096);
		}
	}

	crate::mem::frame::pretend_node_is_empty(None);

	// 5. AND THE TOTALS ARE EXACTLY WHAT THEY WERE - per pool and overall. A matrix that ended with
	//    the right answers and the wrong accounting has moved memory between pools.
	assert!(crate::mem::frame::pools_agree_with_total(), "every frame went back to the pool it came from");
	let (total_after, free_after) = crate::mem::frame::totals();
	assert_eq!(total_after, total_before, "no frame appeared or vanished");
	assert_eq!(free_after, free_before, "and the free count is where it started");
	assert_eq!(crate::mem::frame::free_in_node(Some(first)), in_first, "node {}'s own free count is not what it was before the matrix ran", first.0);
	assert_eq!(crate::mem::frame::free_in_node(Some(second)), in_second, "node {}'s own free count is not what it was before the matrix ran", second.0);

	// 6. THE EXHAUSTED NODE SERVES AGAIN once it is not pretending. Without this the injection could
	//    leak into whatever runs next and every later allocation would be testing a lie.
	let Some(back) = crate::mem::frame::allocate_strict(first) else {
		panic!("the node refuses strict allocations after the injection was lifted");
	};
	assert_eq!(crate::mem::topology_node_of(back), topology::Affinity::Node(first));
	// SAFETY / NEVER-MAPPED: as above.
	unsafe { crate::mem::frame::deallocate(back) };

	// 6b. AND A RETIREMENT IS ROUTED BY THE FRAME'S OWNER TOO.
	//
	//     M2 asks that freeing, retirement and delayed reclamation all route by the frame's PHYSICAL
	//     owner, and the suite proved the first with a per-node count and the other two only through
	//     a generic counter that says nothing about which pool paid. A frame taken from node
	//     `second` and RETIRED must be charged to node `second`: it is out of circulation, so that
	//     node's free count is one lower than it was and stays there.
	let Some(condemned) = crate::mem::frame::allocate_strict(second) else {
		crate::serial_println!("numa-matrix: incomplete - node {} could not serve the frame for the retirement", second.0);
		return;
	};
	let owner_free = crate::mem::frame::free_in_node(Some(second));
	let quarantined_before = crate::mem::frame::quarantined();
	// SAFETY / NEVER-MAPPED: allocated by the call above, never entered a page table, and retired
	// rather than freed - which holds it until every core has been told to forget its mappings.
	unsafe { crate::mem::frame::retire(&[condemned]) };
	// RETIREMENT IS DELAYED RECLAMATION, NOT AN IMMEDIATE FREE. The frame goes to the quarantine and
	// waits there for the shootdown; what M2 asks is that neither the wait nor the return leaks it
	// into the wrong pool.
	assert_eq!(crate::mem::frame::quarantined(), quarantined_before + 1, "a retired frame waits in the quarantine");
	assert_eq!(crate::mem::frame::free_in_node(Some(second)), owner_free, "and while it waits it is not in any node's free count - least of all another node's");
	assert!(crate::mem::frame::pools_agree_with_total(), "and the pools still add up with a frame held out of one of them");

	//     AND THE DELAYED RECLAMATION GOES HOME. The drain is what returns a quarantined frame, and
	//     it must return it to the pool that owns its ADDRESS - which is the same rule the free path
	//     follows, applied to the path that runs much later and on whichever core drains.
	if crate::mem::frame::drain_quarantine_fully(64) {
		assert_eq!(crate::mem::frame::quarantined(), quarantined_before, "the quarantine drained");
		assert_eq!(crate::mem::frame::free_in_node(Some(second)), owner_free.map(|count| count + 1), "a frame reclaimed out of the quarantine went back to the node that owns its address");
		assert!(crate::mem::frame::pools_agree_with_total(), "and the pools add up again afterwards");
	} else {
		crate::serial_println!("numa-matrix: incomplete - the quarantine did not drain, so the delayed reclamation could not be followed home");
		return;
	}

	// 7. AND A FRAME OF ONE NODE IS GIVEN BACK BY A CORE OF ANOTHER.
	//
	//    `deallocate` finds the pool from the frame rather than from whoever is freeing it, which is
	//    the property that makes a cross-node free correct; freeing on the core that allocated
	//    proves nothing about it, because that core's pool is the right one either way. So a frame of
	//    node `second` is handed to a core of node `first` - a `deallocate` that consulted the
	//    CALLER's node would put it in the wrong pool, and node `second`'s own count would not come
	//    back.
	//
	//    ITS OWN SECTION, AFTER THE TOTALS ABOVE, because a kernel thread has a kernel STACK: those
	//    frames are taken from the node the thread was placed on and are not given back until it is
	//    reaped, so a global count compared across a spawn is comparing two different machines.
	//
	//    AND THE PER-NODE FIGURE IS TAKEN ON THE FREEING CORE, not here. The stack is allocated
	//    PREFERRING the node the thread was placed on, and preference is not a guarantee: on riscv64
	//    it came out of node 1's pool, so a count taken before the spawn was short by the whole
	//    stack and the assertion failed on a free that had worked perfectly. Reading the count
	//    inside the thread, after its stack exists and immediately before the free, cancels the
	//    stack whichever pool it came from - and leaves exactly the claim being made: the free put
	//    this frame back in the pool that owns its ADDRESS, from a core of another node.
	let Some(travelling) = crate::mem::frame::allocate_strict(second) else {
		crate::serial_println!("numa-matrix: incomplete - node {} could not serve the frame for the cross-node free", second.0);
		return;
	};
	if !remote_free(first, second, travelling) {
		// SAFETY / NEVER-MAPPED: still this test's frame, never mapped, freed exactly once here
		// because no other core took it.
		unsafe { crate::mem::frame::deallocate(travelling) };
		crate::serial_println!("numa-matrix: incomplete - no core of node {} other than this one took the cross-node free, so it was made locally", first.0);
		return;
	}
	assert_eq!(crate::mem::frame::free_in_node(Some(second)), Some(freed_from() + 1), "a frame of node {} freed by a core of node {} did not go back to node {}'s pool", second.0, first.0, second.0);
	assert!(crate::mem::frame::pools_agree_with_total(), "and the pools still add up after a core of another node gave a frame back");
	crate::serial_println!("numa-matrix: complete - node {} exhausted, strict refused, preferred fell back to node {}, a span stayed on it, a core of node {} freed node {}'s frame, and every per-node total returned", first.0, second.0, first.0, second.0);
}

// FREE ONE FRAME FROM A CORE OF `node`, and say whether that core actually took it.
//
// The frame is handed over through a static because a kernel thread body is an `extern "C" fn(u64)`
// and the argument is the frame itself; the flag is what tells the caller whether the free happened
// there or has still to be made here. Bounded, like every other cross-core wait in this suite: a
// core that is busy or asleep must weaken the claim rather than hang the run.
fn remote_free(node: topology::NodeId, owner: topology::NodeId, frame: u64) -> bool {
	// SEEN BY THE CORE THAT DOES THE FREEING, so the thread's own kernel stack cancels out - see the
	// step that calls this.
	static OWNER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
	static BEFORE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
	use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
	static FREED: AtomicBool = AtomicBool::new(false);
	// WHO OWNS THE FREE, DECIDED BY A COMPARE-AND-EXCHANGE AND NOT BY A DEADLINE.
	//
	// The bounded wait below can expire while the spawned thread is between its own deallocate and
	// the flag that reports it - and the queued thread is not cancelled, so it can also run long
	// after the wait gave up. Both cases end with the caller freeing a frame the thread also freed,
	// which is a double free injected by the test that exists to prove ownership is respected.
	//
	// The frame has exactly one owner: whoever wins this CAS frees it, and the loser does nothing.
	static CLAIMED: AtomicBool = AtomicBool::new(false);
	extern "C" fn body(frame: u64) {
		if CLAIMED.compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Acquire).is_err() {
			// The caller gave up first and owns the frame. Nothing here may touch it.
			return;
		}
		let owner = topology::NodeId(OWNER.load(AtomicOrdering::SeqCst));
		BEFORE.store(crate::mem::frame::free_in_node(Some(owner)).unwrap_or(usize::MAX), AtomicOrdering::SeqCst);
		// SAFETY: this thread won the CAS above, so it is the only owner of this frame. The caller
		// allocated it and never mapped it.
		// NEVER-MAPPED: allocated by the matrix above and freed without entering a page table.
		unsafe { crate::mem::frame::deallocate(frame) };
		FREED.store(true, AtomicOrdering::SeqCst);
	}
	CLAIMED.store(false, AtomicOrdering::SeqCst);
	OWNER.store(owner.0, AtomicOrdering::SeqCst);

	crate::smp::numa::bind_online();
	// A CORE OF THIS NODE THAT IS NOT THE ONE ASKING.
	//
	// `place_on` answers with the node's first online core, which on the boot processor's node is
	// the core running this test - and a free made there proves exactly what freeing inline proves.
	// The profile this matters on gives each node two cores for this reason.
	let here = crate::sched::current_cpu_id();
	let mut chosen = None;
	for cpu in 0..crate::smp::cpu_count() {
		if cpu != here && crate::smp::numa::cpu_node(cpu) == topology::Affinity::Node(node) {
			chosen = Some(cpu);
			break;
		}
	}
	let Some(cpu) = chosen else { return false };
	FREED.store(false, AtomicOrdering::SeqCst);
	// THE FRAME IS THE ARGUMENT, and `spawn_on` is what carries one to a named core - the
	// `prepare_*` forms take an object rather than a value and would need the frame in a static
	// beside the flag. A remote core is kicked with a wake IPI here, so it does not wait for its
	// next timer tick to notice.
	crate::sched::spawn_on(cpu, body, frame);
	for _ in 0..2_000_000u64 {
		if FREED.load(AtomicOrdering::SeqCst) {
			FREED_FROM.store(BEFORE.load(AtomicOrdering::SeqCst), AtomicOrdering::SeqCst);
			return true;
		}
		core::hint::spin_loop();
	}
	// THE WAIT EXPIRED. Take the frame back through the same CAS the thread uses: winning it means
	// the thread has not started its free and never will be allowed to, so the caller may free it.
	// Losing means the thread is inside its free right now - so this waits for the flag rather than
	// touching the frame, and reports the free as the remote one it is.
	if CLAIMED.compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Acquire).is_ok() {
		return false;
	}
	while !FREED.load(AtomicOrdering::SeqCst) {
		core::hint::spin_loop();
	}
	FREED_FROM.store(BEFORE.load(AtomicOrdering::SeqCst), AtomicOrdering::SeqCst);
	true
}

// The owning node's free count as the FREEING core saw it, immediately before the free.
static FREED_FROM: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn freed_from() -> usize {
	FREED_FROM.load(core::sync::atomic::Ordering::SeqCst)
}
