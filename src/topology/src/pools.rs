// The multi-pool allocator, as a model a host can drive a million operations through.
//
// WHAT THIS IS FOR. The kernel's frame allocator routes a free by ADDRESS and an allocation by
// PREFERENCE, and both rules are pure functions of the topology - so they can be stated here, driven
// through long deterministic traces, and compared against totals that must hold after every step.
// What cannot be modelled here is the bitmap arithmetic underneath, which is `buddy`'s and has its
// own differential suite; what can be, and is, is the part this milestone added.
//
// THE FAILURE IT EXISTS TO CATCH is a frame returned to the pool of the CPU that freed it rather
// than the pool that owns its address. That is cross-node corruption: the frame leaves one node's
// accounting and appears in another's, both totals stay plausible, and the machine slowly moves its
// memory to whichever core does the freeing. `a_free_routed_by_the_freeing_cpu_corrupts_the_pools`
// drives exactly that mistake and requires the model to notice.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::{Affinity, NodeId, Topology};

// One pool's free list, as a count and a set. A set rather than a bitmap because this models
// ROUTING, not layout: which pool a frame is in is the question, and where inside it is not.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct PoolState {
	pub free: Vec<u64>,
}

// The reference allocator: one pool per memory-bearing node, plus one for unaffiliated memory.
#[derive(Clone)]
pub struct Pools {
	topology: Topology,
	// Keyed by node, with `None` for the unaffiliated pool.
	pools: BTreeMap<Option<NodeId>, PoolState>,
	// Every frame currently out on loan, and which pool it came from - so a free can be checked
	// against where the frame actually belongs rather than where the caller says.
	loaned: BTreeMap<u64, Option<NodeId>>,
	// Frames taken permanently out of circulation.
	retired: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FreeOutcome {
	// The frame went back to the pool that owns its address.
	Returned(Option<NodeId>),
	// The address was never handed out, or was freed twice.
	Refused,
}

impl Pools {
	// Build the pools and seed them from a list of frames, each routed by its address.
	pub fn new(topology: Topology, frames: &[u64]) -> Pools {
		let mut pools: BTreeMap<Option<NodeId>, PoolState> = BTreeMap::new();
		for node in topology.memory_bearing_nodes() {
			pools.insert(Some(node), PoolState::default());
		}
		pools.insert(None, PoolState::default());
		let mut built = Pools { topology, pools, loaned: BTreeMap::new(), retired: Vec::new() };
		for frame in frames {
			let owner = built.owner_of(*frame);
			built.pools.entry(owner).or_default().free.push(*frame);
		}
		built
	}

	// WHICH POOL OWNS AN ADDRESS. The one rule a free must obey.
	pub fn owner_of(&self, frame: u64) -> Option<NodeId> {
		match self.topology.node_of_address(frame) {
			Affinity::Node(node) if self.pools.contains_key(&Some(node)) => Some(node),
			_ => None,
		}
	}

	// A frame from this node or nothing.
	pub fn alloc_strict(&mut self, node: NodeId) -> Option<u64> {
		let frame = self.pools.get_mut(&Some(node))?.free.pop()?;
		self.loaned.insert(frame, Some(node));
		Some(frame)
	}

	// This node, then the nearest with memory, then unaffiliated memory.
	pub fn alloc_preferred(&mut self, node: NodeId) -> Option<u64> {
		let mut order: Vec<Option<NodeId>> = self.topology.fallback_order(node).into_iter().map(Some).collect();
		order.retain(|candidate| self.pools.contains_key(candidate));
		// Unaffiliated last: memory with no distance is not nearer than a node that has one.
		order.push(None);
		for candidate in order {
			if let Some(frame) = self.pools.get_mut(&candidate).and_then(|pool| pool.free.pop()) {
				self.loaned.insert(frame, candidate);
				return Some(frame);
			}
		}
		None
	}

	// Give a frame back. `by_node` is what a WRONG implementation would route by - the node of the
	// core doing the freeing - and passing `None` is the correct behaviour: route by the address.
	pub fn free(&mut self, frame: u64, by_node: Option<NodeId>) -> FreeOutcome {
		if self.loaned.remove(&frame).is_none() {
			return FreeOutcome::Refused;
		}
		let owner = by_node.map(Some).unwrap_or_else(|| self.owner_of(frame));
		self.pools.entry(owner).or_default().free.push(frame);
		FreeOutcome::Returned(owner)
	}

	// Out of circulation for good: an unconfirmed shootdown, a span outside an extent.
	pub fn retire(&mut self, frame: u64) -> bool {
		if self.loaned.remove(&frame).is_none() {
			return false;
		}
		self.retired.push(frame);
		true
	}

	pub fn free_in(&self, node: Option<NodeId>) -> usize {
		self.pools.get(&node).map_or(0, |pool| pool.free.len())
	}

	pub fn free_total(&self) -> usize {
		self.pools.values().map(|pool| pool.free.len()).sum()
	}

	pub fn loaned_total(&self) -> usize {
		self.loaned.len()
	}

	pub fn retired_total(&self) -> usize {
		self.retired.len()
	}

	// EVERY FRAME IS IN EXACTLY ONE PLACE, and every frame is where its address says it should be.
	//
	// The second half is what a routing bug breaks and a total does not: the counts stay right while
	// a frame sits in a pool that does not own it.
	pub fn consistent(&self) -> bool {
		let mut seen: Vec<u64> = Vec::new();
		for (node, pool) in &self.pools {
			for frame in &pool.free {
				if self.owner_of(*frame) != *node {
					return false;
				}
				seen.push(*frame);
			}
		}
		for frame in self.loaned.keys().chain(self.retired.iter()) {
			seen.push(*frame);
		}
		let before = seen.len();
		seen.sort_unstable();
		seen.dedup();
		seen.len() == before
	}
}
