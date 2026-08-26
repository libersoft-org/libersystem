// Which memory is near which processor, as firmware says it - and as this kernel is prepared to
// believe it.
//
// WHY A CRATE. Everything here is parsing and arithmetic over tables a machine hands us: an ACPI
// SRAT, an ACPI SLIT, a device tree's `numa-node-id`. All three are attacker-adjacent in the sense
// that matters at boot - they come from firmware, they are frequently wrong, and there is nobody to
// complain to. A host can feed this a truncated table, a matrix that is not square, a range that
// wraps, two nodes claiming the same page, and a hundred nodes where the kernel bounds at sixteen.
// A booted kernel with QEMU cannot produce most of those on demand.
//
// `Unknown` IS A FIRST-CLASS ANSWER. Memory no table describes, and a CPU no table mentions, are not
// node zero: calling them node zero is inventing an affinity, and an allocation steered by an
// invented affinity is worse than one that was never steered at all. Every query here can answer
// "firmware did not say", and the allocator above has an explicit pool for exactly that.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod acpi;
pub mod pools;
#[cfg(test)]
mod tests;

// BOUNDS BEFORE ALLOCATION, because the counts come from the tables. A machine claiming four billion
// proximity domains is a machine with a broken table, and the answer is a refusal rather than an
// allocation sized by it.
pub const MAX_NODES: usize = 16;
pub const MAX_RANGES: usize = 64;
pub const MAX_CPUS: usize = 256;

// The distances a system with no SLIT gets. The ACPI specification fixes local at 10, and everything
// else is "further than local" without claiming to know how much further.
pub const LOCAL_DISTANCE: u8 = 10;
pub const REMOTE_DISTANCE: u8 = 20;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct NodeId(pub u32);

// What firmware said about something, including the case where it said nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Affinity {
	Node(NodeId),
	// No table covers this address or this processor. Not an error and not node zero.
	Unknown,
}

impl Affinity {
	pub fn node(self) -> Option<NodeId> {
		match self {
			Affinity::Node(id) => Some(id),
			Affinity::Unknown => None,
		}
	}
}

// One physical range firmware assigned to a node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
	pub base: u64,
	pub len: u64,
	pub node: NodeId,
}

impl Range {
	pub fn end(&self) -> u64 {
		self.base.saturating_add(self.len)
	}

	pub fn contains(&self, address: u64) -> bool {
		address >= self.base && address < self.end()
	}

	fn overlaps(&self, other: &Range) -> bool {
		self.base < other.end() && other.base < self.end()
	}
}

// Why a table was refused. Typed, because the boot report says which kind of wrong a machine's
// firmware is - and because "the table was bad" is not something a reader can act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
	// More nodes, ranges or processors than this kernel bounds at.
	TooManyNodes,
	TooManyRanges,
	TooManyCpus,
	// The same processor reported twice, with different affinities.
	DuplicateCpu,
	// Two ranges covering the same address for different nodes. One of them is wrong and there is no
	// way to tell which.
	ContradictoryOverlap,
	// A range of zero length, or one whose end wraps past the top of the address space.
	MalformedRange,
	// A distance matrix that is not square, or whose diagonal is not the local distance.
	MalformedMatrix,
	// A record naming a node no other record defines.
	UnknownNode,
	// The table is shorter than its own header says, or its checksum does not add up.
	Truncated,
	BadChecksum,
	// The table is not the one that was asked for.
	WrongSignature,
}

// The distance matrix, over the topology's own node ORDER rather than over raw ids - firmware node
// ids are sparse, and indexing a matrix by them directly is how a table with ids 0 and 7 becomes an
// eight-by-eight allocation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Distances {
	// Row-major, `order.len()` square. Empty when no SLIT was present, and `distance` then answers
	// from the local/remote default.
	cells: Vec<u8>,
	size: usize,
}

impl Distances {
	pub fn none() -> Distances {
		Distances { cells: Vec::new(), size: 0 }
	}

	pub fn is_measured(&self) -> bool {
		self.size > 0
	}
}

// The normalized answer: nodes, the memory each owns, the processors each owns, and how far apart
// they are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Topology {
	// Sorted and unique. The index into this is the matrix's index.
	nodes: Vec<NodeId>,
	// Sorted by base, non-overlapping.
	memory: Vec<Range>,
	// Sorted by hardware id, unique.
	cpus: Vec<(u64, NodeId)>,
	distances: Distances,
}

impl Topology {
	pub fn nodes(&self) -> &[NodeId] {
		&self.nodes
	}

	pub fn ranges(&self) -> &[Range] {
		&self.memory
	}

	pub fn cpus(&self) -> &[(u64, NodeId)] {
		&self.cpus
	}

	pub fn distances(&self) -> &Distances {
		&self.distances
	}

	// A single node with no memory ranges and no processors: what a machine with no topology tables
	// has, said as a topology rather than as an absence. Callers can then have one code path.
	pub fn single_node() -> Topology {
		Topology { nodes: Vec::new(), memory: Vec::new(), cpus: Vec::new(), distances: Distances::none() }
	}

	pub fn is_empty(&self) -> bool {
		self.nodes.is_empty()
	}

	// Which node owns this physical address.
	//
	// A BINARY SEARCH OVER SORTED, NON-OVERLAPPING RANGES, which is what `build` guarantees. The
	// allocator calls this on every free, so it is on a hot path - and it is also the reason a free
	// can never send a frame to the wrong node: the answer comes from the address, never from the
	// CPU doing the freeing.
	pub fn node_of_address(&self, physical: u64) -> Affinity {
		let at = self.memory.partition_point(|range| range.end() <= physical);
		match self.memory.get(at) {
			Some(range) if range.contains(physical) => Affinity::Node(range.node),
			_ => Affinity::Unknown,
		}
	}

	// Which node a processor belongs to, by its HARDWARE id - an APIC id, an MPIDR or a hart id.
	// Logical ids are not used here on purpose: a logical id is assigned by bring-up, and bring-up
	// happens after this.
	pub fn node_of_cpu(&self, hardware_id: u64) -> Affinity {
		match self.cpus.binary_search_by_key(&hardware_id, |(id, _)| *id) {
			Ok(at) => Affinity::Node(self.cpus[at].1),
			Err(_) => Affinity::Unknown,
		}
	}

	pub fn index_of(&self, node: NodeId) -> Option<usize> {
		self.nodes.binary_search(&node).ok()
	}

	// How far `to` is from `from`, as firmware measured it or as the default says.
	pub fn distance(&self, from: NodeId, to: NodeId) -> u8 {
		if from == to {
			return LOCAL_DISTANCE;
		}
		let (Some(a), Some(b)) = (self.index_of(from), self.index_of(to)) else {
			return REMOTE_DISTANCE;
		};
		if !self.distances.is_measured() {
			return REMOTE_DISTANCE;
		}
		self.distances.cells[a * self.distances.size + b]
	}

	// The order a preferred allocation walks: the node itself, then the rest by increasing distance.
	//
	// ONE DETERMINISTIC TIE RULE, and it is the node id. Two nodes at equal distance must be tried in
	// an order that does not depend on how the table happened to be written, or the same machine
	// gives different placements on different boots for no reason anybody can see.
	pub fn fallback_order(&self, from: NodeId) -> Vec<NodeId> {
		let mut order: Vec<NodeId> = self.nodes.clone();
		order.sort_by_key(|node| (self.distance(from, *node), node.0));
		order
	}

	// Every node that owns at least one byte of memory. A CPU-only node is a real thing and an
	// allocator must not be pointed at one.
	pub fn memory_bearing_nodes(&self) -> Vec<NodeId> {
		let mut out: Vec<NodeId> = Vec::new();
		for range in &self.memory {
			if !out.contains(&range.node) {
				out.push(range.node);
			}
		}
		out.sort();
		out
	}

	// The total memory firmware assigned to a node.
	pub fn memory_of(&self, node: NodeId) -> u64 {
		self.memory.iter().filter(|range| range.node == node).map(|range| range.len).sum()
	}
}

// "The tree did not say", in the numbering a device tree uses. Mirrors `fdt::NUMA_NODE_UNKNOWN`
// without depending on that crate: this one is fed by an ACPI reader too, and neither should have to
// know about the other.
pub const UNKNOWN_NODE: u32 = u32::MAX;

// Build a topology from what a device tree reported.
//
// THE UNKNOWN ENTRIES ARE DROPPED RATHER THAN DEFAULTED. A bank with no `numa-node-id` belongs to no
// node, and `node_of_address` answers `Unknown` for it - which is what the allocator's unknown pool
// is for. Turning it into node zero would be inventing the one thing this crate exists not to
// invent.
pub fn from_device_tree(banks: &[(u64, u64, u32)], cpus: &[(u64, u32)], distances: &[(u32, u32, u8)]) -> Result<Topology, Error> {
	let mut builder = Builder::new();
	for (base, len, node) in banks {
		if *node != UNKNOWN_NODE {
			builder.add_memory(*base, *len, NodeId(*node));
		}
	}
	for (hardware_id, node) in cpus {
		if *node != UNKNOWN_NODE {
			builder.add_cpu(*hardware_id, NodeId(*node));
		}
	}
	// The tree's matrix arrives as directed triples rather than as a square, so the square is rebuilt
	// from them - OVER THE NODES THE TRIPLES NAME, in order, rather than over a range of raw ids.
	// Sizing it by the largest id turned a legal two-node tree numbered 0 and 16 into a
	// seventeen-row square, which `MAX_NODES` refused as too many nodes.
	if !distances.is_empty() {
		let mut ids: Vec<NodeId> = Vec::new();
		for (from, to, _) in distances {
			for raw in [*from, *to] {
				let id = NodeId(raw);
				if !ids.contains(&id) {
					ids.push(id);
				}
				// A TRIPLE DEFINES ITS NODES. The comment here has always said a triple naming a
				// node no bank or processor mentions still defines it, because a memoryless
				// CPU-less node is a legal topology - and the loop never told the builder, so such
				// a node was absent from the topology it was supposed to define.
				builder.note_node(id);
			}
		}
		ids.sort();
		let size = ids.len();
		if size > MAX_NODES {
			return Err(Error::TooManyNodes);
		}
		let mut cells = Vec::new();
		if cells.try_reserve(size * size).is_err() {
			return Err(Error::TooManyNodes);
		}
		// REMOTE UNTIL THE TABLE SAYS OTHERWISE, and local on the diagonal. A triple the tree left
		// out is a distance the tree did not state, and the default is the documented one rather
		// than zero - a zero distance would read as "nearer than local".
		for from in 0..size {
			for to in 0..size {
				cells.push(if from == to { LOCAL_DISTANCE } else { REMOTE_DISTANCE });
			}
		}
		// TWO TRIPLES FOR ONE PAIR THAT DISAGREE ARE A CONTRADICTION, not a last-writer-wins. The
		// builder already refuses a memory range claimed by two nodes and a CPU placed on two; the
		// distance table was the one place where the later record silently replaced the earlier one.
		let mut stated: Vec<(usize, usize)> = Vec::new();
		for (from, to, distance) in distances {
			let a = ids.iter().position(|id| id.0 == *from).expect("collected above");
			let b = ids.iter().position(|id| id.0 == *to).expect("collected above");
			if stated.contains(&(a, b)) && cells[a * size + b] != *distance {
				return Err(Error::MalformedMatrix);
			}
			stated.push((a, b));
			cells[a * size + b] = *distance;
		}
		builder.set_matrix_for(ids, cells);
	}
	builder.build()
}

// Records arrive in any order and from more than one table, so they are collected and then checked
// once - a validation that ran per record could not see a contradiction between two of them.
#[derive(Default)]
pub struct Builder {
	memory: Vec<Range>,
	cpus: Vec<(u64, NodeId)>,
	nodes: Vec<NodeId>,
	matrix: Vec<u8>,
	matrix_size: usize,
	// Which node each row and column of `matrix` belongs to, in order.
	matrix_ids: Vec<NodeId>,
}

impl Builder {
	pub fn new() -> Builder {
		Builder::default()
	}

	// A node firmware named, whether or not anything is in it. A CPU-less or memory-less node is a
	// real topology and this is how one gets recorded.
	pub fn note_node(&mut self, node: NodeId) {
		if !self.nodes.contains(&node) {
			self.nodes.push(node);
		}
	}

	pub fn add_memory(&mut self, base: u64, len: u64, node: NodeId) {
		self.note_node(node);
		self.memory.push(Range { base, len, node });
	}

	pub fn add_cpu(&mut self, hardware_id: u64, node: NodeId) {
		self.note_node(node);
		self.cpus.push((hardware_id, node));
	}

	// The firmware's distance table, and WHICH NODE EACH ROW IS FOR.
	//
	// THE ROWS CARRY THEIR IDS RATHER THAN BEING INDEXED BY THEM. A SLIT is dense by construction -
	// row N is proximity domain N - so for that reader the two are the same thing. A device tree's
	// is not: it arrives as directed triples naming arbitrary `numa-node-id` values, and the reader
	// that rebuilt a square from them sized it by the LARGEST id it saw. Two nodes numbered 0 and 16
	// then needed a seventeen-row square, which `MAX_NODES` refused as too many nodes - a legal
	// two-node topology turned away over its numbering.
	pub fn set_matrix_for(&mut self, ids: Vec<NodeId>, cells: Vec<u8>) {
		self.matrix_size = ids.len();
		self.matrix_ids = ids;
		self.matrix = cells;
	}

	// The same table from a reader whose rows ARE its ids, which is every ACPI SLIT.
	pub fn set_matrix(&mut self, size: usize, cells: Vec<u8>) {
		self.set_matrix_for((0..size).map(|i| NodeId(i as u32)).collect(), cells);
	}

	// Check everything at once, and refuse rather than repair.
	//
	// A REPAIRED TOPOLOGY IS A FABRICATED ONE. Dropping the contradictory half of an overlap, or
	// filling in a missing matrix row, produces a table that looks valid and describes a machine
	// that does not exist - and every allocation steered by it is wrong in a way nothing will ever
	// report. The whole table is refused and the system runs with none, which is a state the boot
	// report names and the allocator has a pool for.
	pub fn build(mut self) -> Result<Topology, Error> {
		if self.nodes.len() > MAX_NODES {
			return Err(Error::TooManyNodes);
		}
		if self.memory.len() > MAX_RANGES {
			return Err(Error::TooManyRanges);
		}
		if self.cpus.len() > MAX_CPUS {
			return Err(Error::TooManyCpus);
		}
		for range in &self.memory {
			if range.len == 0 || range.base.checked_add(range.len).is_none() {
				return Err(Error::MalformedRange);
			}
		}
		self.memory.sort_by_key(|range| (range.base, range.len));
		// OVERLAPS THAT AGREE ARE MERGED; overlaps that disagree are the refusal. Firmware describing
		// one page twice for one node is redundant and harmless; describing it twice for two nodes
		// means one of the two is wrong and there is no way to tell which.
		let mut merged: Vec<Range> = Vec::new();
		for range in self.memory.drain(..) {
			match merged.last_mut() {
				Some(previous) if previous.overlaps(&range) || previous.end() == range.base => {
					if previous.node != range.node {
						if previous.overlaps(&range) {
							return Err(Error::ContradictoryOverlap);
						}
						merged.push(range);
						continue;
					}
					let end = previous.end().max(range.end());
					previous.len = end - previous.base;
				}
				_ => merged.push(range),
			}
		}
		self.memory = merged;

		self.cpus.sort_by_key(|(id, _)| *id);
		for pair in self.cpus.windows(2) {
			if pair[0].0 == pair[1].0 {
				// The same processor twice. Identical entries are harmless duplication; different
				// ones mean the table contradicts itself about where a CPU lives.
				if pair[0].1 != pair[1].1 {
					return Err(Error::DuplicateCpu);
				}
			}
		}
		self.cpus.dedup_by_key(|(id, _)| *id);

		self.nodes.sort();
		self.nodes.dedup();

		// The matrix, rearranged from the firmware's numbering into this topology's node order.
		let distances = if self.matrix_size == 0 {
			Distances::none()
		} else {
			if self.matrix.len() != self.matrix_size * self.matrix_size {
				return Err(Error::MalformedMatrix);
			}
			if self.matrix_ids.len() != self.matrix_size {
				return Err(Error::MalformedMatrix);
			}
			for index in 0..self.matrix_size {
				if self.matrix[index * self.matrix_size + index] != LOCAL_DISTANCE {
					// A diagonal that is not the local distance describes a node that is not local
					// to itself, which is not a topology.
					return Err(Error::MalformedMatrix);
				}
			}
			// AND NOTHING IS NEARER THAN LOCAL. Only the diagonal was checked, so firmware stating
			// `distance(0, 1) = 0` produced a fallback order in which a REMOTE node sorted ahead of
			// the local one - every allocation steered away from the memory it was steering towards.
			// A table that says that is malformed, not a preference to honour.
			for from in 0..self.matrix_size {
				for to in 0..self.matrix_size {
					if from != to && self.matrix[from * self.matrix_size + to] < LOCAL_DISTANCE {
						return Err(Error::MalformedMatrix);
					}
				}
			}
			let size = self.nodes.len();
			let mut cells = Vec::new();
			if cells.try_reserve(size * size).is_err() {
				return Err(Error::TooManyNodes);
			}
			// The row a node's distances are in, found by its id rather than assumed to BE its id.
			let row_of = |node: &NodeId| self.matrix_ids.iter().position(|id| id == node);
			for from in &self.nodes {
				for to in &self.nodes {
					let (Some(a), Some(b)) = (row_of(from), row_of(to)) else {
						// A node the matrix does not cover. The table describes a machine whose
						// distance matrix is smaller than its node list.
						return Err(Error::UnknownNode);
					};
					cells.push(self.matrix[a * self.matrix_size + b]);
				}
			}
			Distances { cells, size }
		};

		Ok(Topology { nodes: self.nodes, memory: self.memory, cpus: self.cpus, distances })
	}
}
