// Valid tables, and then every way a table can be wrong.
//
// The tables here are BUILT rather than captured, so a case can change one byte and say which byte
// it changed. A captured table proves a machine boots; a constructed one proves what happens when
// the machine's firmware is not what it claims.

use super::acpi::{parse_slit, parse_srat};
use super::*;

// An ACPI table's header, with the checksum filled in last so the body can be written first.
fn finish(mut bytes: Vec<u8>, signature: &[u8; 4]) -> Vec<u8> {
	let length = bytes.len() as u32;
	bytes[0..4].copy_from_slice(signature);
	bytes[4..8].copy_from_slice(&length.to_le_bytes());
	bytes[9] = 0;
	let sum = bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte));
	bytes[9] = (0u8).wrapping_sub(sum);
	bytes
}

// The Memory Affinity Structure, offset by offset. NAMED, because three of ACPI's affinity
// structures put the proximity domain in three different places and a fixture written from the
// wrong one agrees with a parser written from the wrong one.
fn srat_memory(node: u32, base: u64, len: u64, flags: u32) -> Vec<u8> {
	let mut entry = alloc::vec![0u8; 40];
	entry[0] = 1;
	entry[1] = 40;
	// Offset 2: proximity domain. NOT 4 - that is the x2APIC structure's.
	entry[2..6].copy_from_slice(&node.to_le_bytes());
	entry[8..12].copy_from_slice(&(base as u32).to_le_bytes());
	entry[12..16].copy_from_slice(&((base >> 32) as u32).to_le_bytes());
	entry[16..20].copy_from_slice(&(len as u32).to_le_bytes());
	entry[20..24].copy_from_slice(&((len >> 32) as u32).to_le_bytes());
	entry[28..32].copy_from_slice(&flags.to_le_bytes());
	entry
}

fn srat_cpu(node: u32, apic: u8, flags: u32) -> Vec<u8> {
	let mut entry = alloc::vec![0u8; 16];
	entry[0] = 0;
	entry[1] = 16;
	entry[2] = (node & 0xFF) as u8;
	entry[3] = apic;
	entry[4..8].copy_from_slice(&flags.to_le_bytes());
	entry[9] = ((node >> 8) & 0xFF) as u8;
	entry[10] = ((node >> 16) & 0xFF) as u8;
	entry[11] = ((node >> 24) & 0xFF) as u8;
	entry
}

fn srat_x2apic(node: u32, id: u32, flags: u32) -> Vec<u8> {
	let mut entry = alloc::vec![0u8; 24];
	entry[0] = 2;
	entry[1] = 24;
	entry[4..8].copy_from_slice(&node.to_le_bytes());
	entry[8..12].copy_from_slice(&id.to_le_bytes());
	entry[12..16].copy_from_slice(&flags.to_le_bytes());
	entry
}

fn srat(entries: &[Vec<u8>]) -> Vec<u8> {
	let mut bytes = alloc::vec![0u8; 48];
	for entry in entries {
		bytes.extend_from_slice(entry);
	}
	finish(bytes, b"SRAT")
}

fn slit(size: usize, cells: &[u8]) -> Vec<u8> {
	let mut bytes = alloc::vec![0u8; 36];
	bytes.extend_from_slice(&(size as u64).to_le_bytes());
	bytes.extend_from_slice(cells);
	finish(bytes, b"SLIT")
}

fn two_node() -> Topology {
	let table = srat(&[srat_memory(0, 0, 0x8000_0000, 1), srat_memory(1, 0x1_0000_0000, 0x8000_0000, 1), srat_cpu(0, 0, 1), srat_cpu(0, 1, 1), srat_cpu(1, 2, 1), srat_cpu(1, 3, 1)]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("a valid SRAT");
	builder.build().expect("a valid topology")
}

#[test]
fn a_two_node_machine_reads_back_exactly_what_its_table_said() {
	let topology = two_node();
	assert_eq!(topology.nodes(), &[NodeId(0), NodeId(1)]);
	assert_eq!(topology.ranges().len(), 2);
	assert_eq!(topology.memory_of(NodeId(0)), 0x8000_0000);
	assert_eq!(topology.memory_of(NodeId(1)), 0x8000_0000);
	assert_eq!(topology.cpus().len(), 4);
	assert_eq!(topology.node_of_cpu(0), Affinity::Node(NodeId(0)));
	assert_eq!(topology.node_of_cpu(3), Affinity::Node(NodeId(1)));
}

#[test]
fn an_address_belongs_to_the_node_that_owns_it_and_nothing_else_does() {
	let topology = two_node();
	assert_eq!(topology.node_of_address(0), Affinity::Node(NodeId(0)));
	assert_eq!(topology.node_of_address(0x7FFF_FFFF), Affinity::Node(NodeId(0)), "the last byte of a range is inside it");
	// THE HOLE BETWEEN THE TWO NODES IS UNKNOWN, not node zero and not node one. Memory no table
	// describes is memory nothing may claim locality over.
	assert_eq!(topology.node_of_address(0x8000_0000), Affinity::Unknown);
	assert_eq!(topology.node_of_address(0xFFFF_FFFF), Affinity::Unknown);
	assert_eq!(topology.node_of_address(0x1_0000_0000), Affinity::Node(NodeId(1)));
	assert_eq!(topology.node_of_address(0x1_8000_0000), Affinity::Unknown, "and so is everything past the last range");
}

#[test]
fn a_processor_no_table_mentions_is_unknown_rather_than_node_zero() {
	let topology = two_node();
	assert_eq!(topology.node_of_cpu(99), Affinity::Unknown);
	assert_eq!(Affinity::Unknown.node(), None, "and the caller has to handle it rather than unwrap into a node");
}

#[test]
fn a_disabled_processor_and_a_hot_pluggable_range_create_no_affinity() {
	let table = srat(&[
		srat_memory(0, 0, 0x1000_0000, 1),
		// Enabled and hot-pluggable: the node exists, the memory is not seedable.
		srat_memory(1, 0x1000_0000, 0x1000_0000, 1 | 2),
		srat_cpu(0, 0, 1),
		// Present and disabled.
		srat_cpu(1, 1, 0),
	]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("valid");
	let topology = builder.build().expect("valid");
	assert_eq!(topology.nodes(), &[NodeId(0), NodeId(1)], "a node with no usable memory and no enabled CPU is still a node");
	assert_eq!(topology.memory_of(NodeId(1)), 0, "hot-pluggable memory is described and not seeded");
	assert_eq!(topology.node_of_cpu(1), Affinity::Unknown, "a disabled processor has no affinity");
}

#[test]
fn an_x2apic_affinity_reaches_ids_the_eight_bit_structure_cannot() {
	let table = srat(&[srat_memory(0, 0, 0x1000, 1), srat_x2apic(0, 300, 1), srat_x2apic(1, 0x1234_5678, 1)]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("valid");
	let topology = builder.build().expect("valid");
	assert_eq!(topology.node_of_cpu(300), Affinity::Node(NodeId(0)));
	assert_eq!(topology.node_of_cpu(0x1234_5678), Affinity::Node(NodeId(1)));
}

#[test]
fn each_affinity_structure_has_its_proximity_domain_where_the_specification_puts_it() {
	// THE THREE LAYOUTS, ASKED APART. A memory range at node 1 whose base address happens to put
	// something else at offset 4, and an x2APIC entry at node 2 whose bytes at offset 2 are
	// deliberately not its domain - a parser reading either from the other's place gets a plausible
	// number rather than an error, which is why this is a test and not a comment.
	let mut memory = srat_memory(1, 0x8000_0000, 0x1000, 1);
	// Offsets 6 and 7 are reserved and stay zero; a reader taking offsets 4..8 would see the low
	// half of the base address, which for this range is not 1.
	assert_eq!(&memory[2..6], &1u32.to_le_bytes(), "the fixture puts the domain where the specification does");
	memory[6] = 0;
	memory[7] = 0;
	let table = srat(&[memory, srat_x2apic(2, 9, 1)]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("valid");
	let topology = builder.build().expect("valid");
	assert_eq!(topology.node_of_address(0x8000_0000), Affinity::Node(NodeId(1)), "the memory structure's domain is at offset 2");
	assert_eq!(topology.node_of_cpu(9), Affinity::Node(NodeId(2)), "and the x2APIC structure's is at offset 4");
}

#[test]
fn a_proximity_domain_above_255_is_read_from_all_four_of_its_bytes() {
	// The classic APIC affinity structure splits the domain: one byte at offset 2 and three more at
	// offset 9. A reader that takes only the low byte is right for every machine with fewer than 256
	// nodes and silently wrong for the rest - it would put node 0x100 in node 0.
	let table = srat(&[srat_memory(0x100, 0, 0x1000, 1), srat_cpu(0x100, 7, 1)]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("valid");
	let topology = builder.build().expect("valid");
	assert_eq!(topology.node_of_cpu(7), Affinity::Node(NodeId(0x100)));
}

#[test]
fn a_table_that_is_not_what_it_claims_is_refused_three_different_ways() {
	let good = srat(&[srat_memory(0, 0, 0x1000, 1)]);
	let mut builder = Builder::new();
	assert!(parse_srat(&good, &mut builder).is_ok());

	// A signature that is not SRAT.
	let mut wrong = good.clone();
	wrong[0..4].copy_from_slice(b"XSDT");
	assert_eq!(parse_srat(&wrong, &mut Builder::new()), Err(Error::WrongSignature));

	// A length longer than the buffer holding it.
	let mut long = good.clone();
	long[4..8].copy_from_slice(&(good.len() as u32 + 64).to_le_bytes());
	assert_eq!(parse_srat(&long, &mut Builder::new()), Err(Error::Truncated));

	// One byte changed anywhere: the checksum is what notices.
	let mut corrupt = good.clone();
	corrupt[50] ^= 0xFF;
	assert_eq!(parse_srat(&corrupt, &mut Builder::new()), Err(Error::BadChecksum));

	// And a buffer too short to hold a header at all.
	assert_eq!(parse_srat(&[0u8; 8], &mut Builder::new()), Err(Error::Truncated));
}

#[test]
fn an_entry_that_runs_past_the_end_of_its_table_is_refused_rather_than_read() {
	let mut table = srat(&[srat_memory(0, 0, 0x1000, 1)]);
	// The entry claims to be longer than what is left of the table.
	table[49] = 200;
	let table = finish(table, b"SRAT");
	assert_eq!(parse_srat(&table, &mut Builder::new()), Err(Error::Truncated));

	// And an entry claiming zero length, which a reader that trusted it would loop on for ever.
	let mut table = srat(&[srat_memory(0, 0, 0x1000, 1)]);
	table[49] = 0;
	let table = finish(table, b"SRAT");
	assert_eq!(parse_srat(&table, &mut Builder::new()), Err(Error::Truncated));
}

#[test]
fn two_nodes_claiming_one_page_is_a_refusal_and_two_claims_that_agree_are_merged() {
	let mut builder = Builder::new();
	builder.add_memory(0, 0x2000, NodeId(0));
	builder.add_memory(0x1000, 0x2000, NodeId(1));
	assert_eq!(builder.build(), Err(Error::ContradictoryOverlap), "one of the two is wrong and there is no way to tell which");

	let mut builder = Builder::new();
	builder.add_memory(0, 0x2000, NodeId(0));
	builder.add_memory(0x1000, 0x2000, NodeId(0));
	let topology = builder.build().expect("agreeing claims are redundant, not contradictory");
	assert_eq!(topology.ranges().len(), 1);
	assert_eq!(topology.memory_of(NodeId(0)), 0x3000);
}

#[test]
fn a_range_that_wraps_or_holds_nothing_is_refused() {
	let mut builder = Builder::new();
	builder.add_memory(u64::MAX - 0x100, 0x1000, NodeId(0));
	assert_eq!(builder.build(), Err(Error::MalformedRange));

	let mut builder = Builder::new();
	builder.add_memory(0x1000, 0, NodeId(0));
	assert_eq!(builder.build(), Err(Error::MalformedRange));
}

#[test]
fn one_processor_in_two_places_is_refused_and_the_same_entry_twice_is_not() {
	let mut builder = Builder::new();
	builder.add_cpu(3, NodeId(0));
	builder.add_cpu(3, NodeId(1));
	assert_eq!(builder.build(), Err(Error::DuplicateCpu));

	let mut builder = Builder::new();
	builder.add_cpu(3, NodeId(0));
	builder.add_cpu(3, NodeId(0));
	let topology = builder.build().expect("a repeated identical entry is duplication, not contradiction");
	assert_eq!(topology.cpus().len(), 1);
}

#[test]
fn more_nodes_ranges_or_processors_than_the_kernel_bounds_at_are_refused() {
	let mut builder = Builder::new();
	for node in 0..=MAX_NODES as u32 {
		builder.note_node(NodeId(node));
	}
	assert_eq!(builder.build(), Err(Error::TooManyNodes));

	let mut builder = Builder::new();
	for index in 0..=MAX_RANGES as u64 {
		builder.add_memory(index * 0x2000, 0x1000, NodeId(0));
	}
	assert_eq!(builder.build(), Err(Error::TooManyRanges));

	let mut builder = Builder::new();
	for index in 0..=MAX_CPUS as u64 {
		builder.add_cpu(index, NodeId(0));
	}
	assert_eq!(builder.build(), Err(Error::TooManyCpus));
}

#[test]
fn a_slit_is_read_asymmetrically_because_a_real_one_is() {
	let table = slit(2, &[10, 21, 31, 10]);
	let mut builder = Builder::new();
	builder.note_node(NodeId(0));
	builder.note_node(NodeId(1));
	parse_slit(&table, &mut builder).expect("a valid SLIT");
	let topology = builder.build().expect("valid");
	assert!(topology.distances().is_measured());
	assert_eq!(topology.distance(NodeId(0), NodeId(0)), 10);
	assert_eq!(topology.distance(NodeId(0), NodeId(1)), 21);
	assert_eq!(topology.distance(NodeId(1), NodeId(0)), 31, "the way back need not be the way there");
}

#[test]
fn a_matrix_that_is_not_a_topology_is_refused() {
	// A diagonal that is not the local distance describes a node that is not local to itself.
	let table = slit(2, &[11, 20, 20, 10]);
	assert_eq!(parse_slit(&table, &mut Builder::new()), Err(Error::MalformedMatrix));

	// A matrix shorter than its own size claims.
	let mut short = slit(2, &[10, 20, 20, 10]);
	short.truncate(short.len() - 2);
	let short = finish(short, b"SLIT");
	assert_eq!(parse_slit(&short, &mut Builder::new()), Err(Error::Truncated));

	// A size the kernel does not bound at, before anything is allocated from it.
	let table = slit(0, &[]);
	assert_eq!(parse_slit(&table, &mut Builder::new()), Err(Error::TooManyNodes));
	let mut huge = alloc::vec![0u8; 36];
	huge.extend_from_slice(&(65_536u64).to_le_bytes());
	let huge = finish(huge, b"SLIT");
	assert_eq!(parse_slit(&huge, &mut Builder::new()), Err(Error::TooManyNodes));
}

#[test]
fn a_matrix_that_does_not_cover_a_node_the_srat_named_is_refused() {
	let mut builder = Builder::new();
	builder.note_node(NodeId(0));
	builder.note_node(NodeId(5));
	builder.set_matrix(2, alloc::vec![10, 20, 20, 10]);
	assert_eq!(builder.build(), Err(Error::UnknownNode), "node 5 is outside a two-by-two matrix");
}

#[test]
fn without_a_slit_every_other_node_is_simply_remote() {
	let topology = two_node();
	assert!(!topology.distances().is_measured());
	assert_eq!(topology.distance(NodeId(0), NodeId(0)), LOCAL_DISTANCE);
	assert_eq!(topology.distance(NodeId(0), NodeId(1)), REMOTE_DISTANCE, "an absent SLIT produces the documented default, not a fabricated number");
}

#[test]
fn the_fallback_order_starts_at_home_and_breaks_ties_by_node_id() {
	let mut builder = Builder::new();
	for node in 0..4u32 {
		builder.add_memory(node as u64 * 0x1000, 0x1000, NodeId(node));
	}
	// Node 0 is equally far from 2 and 3, and nearer to 1.
	builder.set_matrix(4, alloc::vec![10, 15, 30, 30, 15, 10, 30, 30, 30, 30, 10, 15, 30, 30, 15, 10]);
	let topology = builder.build().expect("valid");
	assert_eq!(topology.fallback_order(NodeId(0)), alloc::vec![NodeId(0), NodeId(1), NodeId(2), NodeId(3)]);
	// AND THE TIE IS BROKEN THE SAME WAY EVERY BOOT. Two nodes at equal distance tried in table
	// order would place differently on machines whose firmware wrote the table differently.
	assert_eq!(topology.fallback_order(NodeId(2)), alloc::vec![NodeId(2), NodeId(3), NodeId(0), NodeId(1)]);
}

#[test]
fn a_memoryless_node_and_a_cpuless_node_are_both_real_topologies() {
	let mut builder = Builder::new();
	// Node 0 has memory and processors; node 1 has processors only; node 2 has memory only.
	builder.add_memory(0, 0x1000, NodeId(0));
	builder.add_cpu(0, NodeId(0));
	builder.add_cpu(1, NodeId(1));
	builder.add_memory(0x1000, 0x1000, NodeId(2));
	let topology = builder.build().expect("valid");
	assert_eq!(topology.nodes(), &[NodeId(0), NodeId(1), NodeId(2)]);
	assert_eq!(topology.memory_bearing_nodes(), alloc::vec![NodeId(0), NodeId(2)], "an allocator must not be pointed at a node with no memory in it");
	assert_eq!(topology.memory_of(NodeId(1)), 0);
}

#[test]
fn records_arriving_in_any_order_produce_the_same_topology() {
	let forwards = srat(&[srat_memory(0, 0, 0x1000, 1), srat_cpu(0, 0, 1), srat_memory(1, 0x1000, 0x1000, 1), srat_cpu(1, 1, 1)]);
	let backwards = srat(&[srat_cpu(1, 1, 1), srat_memory(1, 0x1000, 0x1000, 1), srat_cpu(0, 0, 1), srat_memory(0, 0, 0x1000, 1)]);
	let mut first = Builder::new();
	let mut second = Builder::new();
	parse_srat(&forwards, &mut first).expect("valid");
	parse_srat(&backwards, &mut second).expect("valid");
	// The two ranges are adjacent and belong to different nodes, so they stay two ranges.
	assert_eq!(first.build().expect("valid"), second.build().expect("valid"));
}

#[test]
fn sparse_node_ids_are_kept_as_they_are_rather_than_renumbered() {
	// Firmware is entitled to number its nodes 0 and 7, and a kernel that renumbered them would
	// report a topology that does not match what anything else on the machine says.
	let table = srat(&[srat_memory(0, 0, 0x1000, 1), srat_memory(7, 0x1000, 0x1000, 1)]);
	let mut builder = Builder::new();
	parse_srat(&table, &mut builder).expect("valid");
	let topology = builder.build().expect("valid");
	assert_eq!(topology.nodes(), &[NodeId(0), NodeId(7)]);
	assert_eq!(topology.index_of(NodeId(7)), Some(1), "and the matrix index is the position, not the id");
}

#[test]
fn a_machine_with_no_tables_at_all_is_a_topology_rather_than_an_absence() {
	let topology = Topology::single_node();
	assert!(topology.is_empty());
	assert_eq!(topology.node_of_address(0x1000), Affinity::Unknown);
	assert_eq!(topology.node_of_cpu(0), Affinity::Unknown);
	assert!(topology.memory_bearing_nodes().is_empty());
}

#[test]
fn a_device_trees_banks_and_harts_become_the_same_topology_an_acpi_table_would() {
	let banks = [(0x4000_0000u64, 0x1000_0000u64, 0u32), (0x5000_0000, 0x1000_0000, 1)];
	let cpus = [(0u64, 0u32), (1, 0), (2, 1), (3, 1)];
	let distances = [(0u32, 0u32, 10u8), (0, 1, 21), (1, 0, 31), (1, 1, 10)];
	let topology = from_device_tree(&banks, &cpus, &distances).expect("a valid tree");
	assert_eq!(topology.nodes(), &[NodeId(0), NodeId(1)]);
	assert_eq!(topology.node_of_address(0x4000_0000), Affinity::Node(NodeId(0)));
	assert_eq!(topology.node_of_address(0x5000_0000), Affinity::Node(NodeId(1)), "two adjacent banks in two nodes are two ranges");
	assert_eq!(topology.node_of_cpu(2), Affinity::Node(NodeId(1)));
	assert_eq!(topology.distance(NodeId(0), NodeId(1)), 21);
	assert_eq!(topology.distance(NodeId(1), NodeId(0)), 31);
}

#[test]
fn a_bank_the_tree_gave_no_node_belongs_to_none() {
	let banks = [(0x4000_0000u64, 0x1000_0000u64, 0u32), (0x5000_0000, 0x1000_0000, UNKNOWN_NODE)];
	let cpus = [(0u64, 0u32), (1, UNKNOWN_NODE)];
	let topology = from_device_tree(&banks, &cpus, &[]).expect("valid");
	assert_eq!(topology.node_of_address(0x4000_0000), Affinity::Node(NodeId(0)));
	assert_eq!(topology.node_of_address(0x5000_0000), Affinity::Unknown, "unaffiliated memory stays unaffiliated");
	assert_eq!(topology.node_of_cpu(1), Affinity::Unknown);
	assert_eq!(topology.nodes(), &[NodeId(0)]);
}

#[test]
fn a_distance_the_tree_left_out_is_remote_rather_than_zero() {
	// A zero would read as "nearer than local", which is how a missing cell becomes a preference.
	let banks = [(0x1000u64, 0x1000u64, 0u32), (0x2000, 0x1000, 1)];
	let distances = [(0u32, 0u32, 10u8), (1, 1, 10)];
	let topology = from_device_tree(&banks, &[], &distances).expect("valid");
	assert_eq!(topology.distance(NodeId(0), NodeId(1)), REMOTE_DISTANCE);
	assert_eq!(topology.distance(NodeId(0), NodeId(0)), LOCAL_DISTANCE);
}

// ---------------------------------------------------------------------------------------------
// The multi-pool model: long deterministic traces, and the one mistake that must fail it.
// ---------------------------------------------------------------------------------------------

use super::pools::{FreeOutcome, Pools};

// A two-node machine with a hole between the nodes, so some frames belong to nobody - which is the
// case an allocator that assumed every address has a node would get wrong.
fn two_node_pools() -> Pools {
	let mut builder = Builder::new();
	builder.add_memory(0x0000, 0x4000, NodeId(0));
	builder.add_memory(0x8000, 0x4000, NodeId(1));
	builder.set_matrix(2, alloc::vec![10, 21, 31, 10]);
	let topology = builder.build().expect("valid");
	// Four frames in node 0, four in node 1, and four in the hole between them.
	let frames: Vec<u64> = (0..4).map(|i| i * 0x1000).chain((0..4).map(|i| 0x4000 + i * 0x1000)).chain((0..4).map(|i| 0x8000 + i * 0x1000)).collect();
	Pools::new(topology, &frames)
}

#[test]
fn every_frame_is_seeded_into_the_pool_that_owns_its_address() {
	let pools = two_node_pools();
	assert_eq!(pools.free_in(Some(NodeId(0))), 4);
	assert_eq!(pools.free_in(Some(NodeId(1))), 4);
	assert_eq!(pools.free_in(None), 4, "the frames in the hole belong to nobody, and there is a pool for exactly that");
	assert_eq!(pools.free_total(), 12);
	assert!(pools.consistent());
}

#[test]
fn strict_exhausts_one_node_and_then_fails_where_preferred_falls_back() {
	let mut pools = two_node_pools();
	let mut taken = Vec::new();
	for _ in 0..4 {
		taken.push(pools.alloc_strict(NodeId(0)).expect("node 0 has four frames"));
	}
	// EXHAUSTED, AND STRICT SAYS SO. The other pools have eight frames between them and not one of
	// them is an answer to this question.
	assert!(pools.alloc_strict(NodeId(0)).is_none(), "strict never reaches another pool");
	assert_eq!(pools.free_in(Some(NodeId(1))), 4, "and it did not touch the other node on the way past");

	// Preferred, asked for the same node, falls back - to the NEARER of what is left.
	let fell_back = pools.alloc_preferred(NodeId(0)).expect("preferred falls back while anything is free");
	assert_eq!(pools.owner_of(fell_back), Some(NodeId(1)), "node 1 is nearer than memory with no node at all");
	assert!(pools.consistent());
	for frame in taken {
		assert_eq!(pools.free(frame, None), FreeOutcome::Returned(Some(NodeId(0))));
	}
	assert_eq!(pools.free_in(Some(NodeId(0))), 4, "and every one of them went home");
}

#[test]
fn exhausting_every_pool_changes_no_count() {
	let mut pools = two_node_pools();
	let mut taken = Vec::new();
	while let Some(frame) = pools.alloc_preferred(NodeId(0)) {
		taken.push(frame);
	}
	assert_eq!(taken.len(), 12, "everything came out");
	assert_eq!(pools.free_total(), 0);
	// A REFUSAL COSTS NOTHING. Asking again when there is nothing left must not move a number.
	let before = (pools.free_total(), pools.loaned_total(), pools.retired_total());
	assert!(pools.alloc_preferred(NodeId(0)).is_none());
	assert!(pools.alloc_strict(NodeId(1)).is_none());
	assert_eq!((pools.free_total(), pools.loaned_total(), pools.retired_total()), before);
	assert!(pools.consistent());
}

#[test]
fn a_free_of_something_that_was_never_handed_out_is_refused() {
	let mut pools = two_node_pools();
	assert_eq!(pools.free(0x1000, None), FreeOutcome::Refused, "that frame is free, not on loan");
	let frame = pools.alloc_strict(NodeId(0)).expect("a frame");
	assert!(matches!(pools.free(frame, None), FreeOutcome::Returned(_)));
	assert_eq!(pools.free(frame, None), FreeOutcome::Refused, "and a second free of the same frame is a double free");
	assert!(pools.consistent());
}

#[test]
fn a_retired_frame_leaves_circulation_and_stays_out() {
	let mut pools = two_node_pools();
	let frame = pools.alloc_strict(NodeId(1)).expect("a frame");
	assert!(pools.retire(frame));
	assert_eq!(pools.retired_total(), 1);
	assert_eq!(pools.free_in(Some(NodeId(1))), 3, "it did not go back");
	assert!(!pools.retire(frame), "and it cannot be retired twice");
	assert_eq!(pools.free(frame, None), FreeOutcome::Refused, "nor freed afterwards");
	assert!(pools.consistent());
}

#[test]
fn a_long_deterministic_trace_keeps_every_total() {
	// TEN THOUSAND OPERATIONS, and the totals are checked after every one. The schedule is a fixed
	// stream rather than a random one so a failure is reproducible by its step number.
	let mut pools = two_node_pools();
	let mut held: Vec<u64> = Vec::new();
	let mut state: u64 = 0x2545_F491_4F6C_DD1D;
	for step in 0..10_000u32 {
		state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
		let roll = (state >> 33) % 10;
		match roll {
			0..=3 => {
				if let Some(frame) = pools.alloc_preferred(NodeId((state >> 5) as u32 % 2)) {
					held.push(frame);
				}
			}
			4..=5 => {
				if let Some(frame) = pools.alloc_strict(NodeId((state >> 7) as u32 % 2)) {
					held.push(frame);
				}
			}
			6..=8 => {
				if !held.is_empty() {
					let at = (state >> 9) as usize % held.len();
					let frame = held.remove(at);
					// FREED "ON ANOTHER CORE": the caller passes no node, so the routing is the
					// address's - which is the whole point, and what the mutation below breaks.
					assert!(matches!(pools.free(frame, None), FreeOutcome::Returned(_)), "step {step}");
				}
			}
			_ => {
				if !held.is_empty() {
					let at = (state >> 11) as usize % held.len();
					let frame = held.remove(at);
					assert!(pools.retire(frame), "step {step}");
				}
			}
		}
		assert!(pools.consistent(), "step {step}: a frame is in a pool that does not own it, or in two places");
		assert_eq!(pools.free_total() + pools.loaned_total() + pools.retired_total(), 12, "step {step}: frames appeared or vanished");
	}
}

#[test]
fn a_free_routed_by_the_freeing_cpu_corrupts_the_pools() {
	// THE MISTAKE THIS MODEL EXISTS FOR. A frame from node 0, freed on a core of node 1, routed by
	// the CORE rather than by the ADDRESS. Every count stays plausible - the totals add up, nothing
	// is lost - and the frame is now in a pool that does not own it, which is cross-node corruption
	// that no total would ever report.
	let mut pools = two_node_pools();
	let frame = pools.alloc_strict(NodeId(0)).expect("a frame from node 0");
	assert_eq!(pools.owner_of(frame), Some(NodeId(0)));
	assert_eq!(pools.free(frame, Some(NodeId(1))), FreeOutcome::Returned(Some(NodeId(1))), "the wrong routing accepts it");
	assert_eq!(pools.free_total(), 12, "and every count still adds up, which is why a count cannot catch this");
	assert!(!pools.consistent(), "the model must refuse a frame sitting in a pool that does not own it");
}
