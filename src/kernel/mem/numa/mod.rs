// Reading the machine's topology, once, at the one moment it can be read.
//
// THE ORDER IS THE MILESTONE'S POINT. An SRAT pointer cannot be dereferenced before the direct-map
// bound exists - the check that says "this address is somewhere I can read" is the bound itself -
// and the frame allocator cannot be divided into node pools after it has started handing frames out.
// So bring-up is: publish the map bounds, seed the neutral boot pool, read the topology, and only
// then upgrade to the heap and partition. This module is the third step, and it is called from
// exactly one place for that reason.
//
// WHAT IT WILL NOT DO. It will not invent an affinity. A machine with no tables, a table that does
// not add up, or a topology whose extents interleave all end the same way: no topology is published,
// the allocator keeps one pool, and the boot report says so.

use core::sync::atomic::{AtomicU8, Ordering};

use topology::Topology;

// WHY THIS MACHINE HAS NO TOPOLOGY, recorded where it is found and printed where the baseline is.
//
// Discovery said its piece at the moment it looked and the boot report said the baseline afterwards,
// so an ordinary machine - one with no SRAT at all, which is every ordinary QEMU boot - printed two
// lines about one fact:
//
//     numa: this machine's firmware published no SRAT (rsdp 0x7f77e014)
//     numa: this machine reported no memory topology - one pool, no locality
//
// The reasons that carry a PAYLOAD keep their own lines: a refused table names what was wrong with
// it, appears only on unusual machines, and is not what the baseline could have said anyway. What is
// folded in here is the ordinary silence, which is the case that repeats.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Absence {
	// Nothing looked yet, or a topology was published and there is no absence to explain.
	None = 0,
	// x86: the firmware published no SRAT.
	NoSrat = 1,
	// The device-tree ports: this boot kept no tree to read one from.
	NoDeviceTree = 2,
	// A tree that named no node for any of its memory banks.
	NoNodeIds = 3,
	// A table or tree that was read and refused; the refusal printed its own line, with the reason
	// in it, where it happened.
	Refused = 4,
}

static ABSENCE: AtomicU8 = AtomicU8::new(Absence::None as u8);

fn record(absence: Absence) {
	ABSENCE.store(absence as u8, Ordering::Relaxed);
}

// The recorded reason as a clause the baseline line can carry, or nothing when there is none to
// carry - a refusal already said more than this could.
fn absence_clause() -> &'static str {
	match ABSENCE.load(Ordering::Relaxed) {
		x if x == Absence::NoSrat as u8 => " (this machine's firmware published no SRAT)",
		x if x == Absence::NoDeviceTree as u8 => " (this boot kept no device tree to read one from)",
		x if x == Absence::NoNodeIds as u8 => " (its device tree named no node for any memory bank)",
		_ => "",
	}
}

// Read whatever this machine's firmware offers, and publish it if it is coherent.
//
// Returns whether a topology was published. The caller reports it; nothing else changes behaviour
// on the answer, because the allocator asks `mem::with_topology` when it partitions.
// `regions` is the boot memory map, so both readers can intersect the firmware's affinity ranges with
// the memory this machine actually seeds its allocator from - see `Builder::restrict_to_seedable`.
// The spans this machine seeds its frame allocator with, from the loader's own map. One list, used
// by both readers, so the ACPI and device-tree paths intersect against the same thing the allocator
// was actually given.
fn seedable_spans(regions: &[bootproto::MemRegion]) -> alloc::vec::Vec<(u64, u64)> {
	let mut spans: alloc::vec::Vec<(u64, u64)> = alloc::vec::Vec::new();
	for region in regions {
		if crate::mem::frame::seeds_the_pool(region.kind) && region.length > 0 {
			// ALLOC-OK: read once at boot, bounded by the memory map's region count.
			spans.push((region.base, region.length));
		}
	}
	spans
}

pub fn discover(regions: &[bootproto::MemRegion]) -> bool {
	let Some(found) = read(regions) else {
		return false;
	};
	if found.is_empty() {
		return false;
	}
	crate::mem::set_topology(found);
	true
}

// x86: the ACPI SRAT for affinity and the SLIT for distances.
//
// THE SLIT IS OPTIONAL AND ITS ABSENCE IS NOT A FAILURE. A machine with an SRAT and no SLIT has
// nodes and no measured distances, and the documented local/remote default is what it gets - not a
// fabricated matrix, and not a refusal to use the affinity it did report.
#[cfg(target_arch = "x86_64")]
fn read(regions: &[bootproto::MemRegion]) -> Option<Topology> {
	let rsdp = crate::boot_info().rsdp;
	let mut builder = topology::Builder::new();
	builder.restrict_to_seedable(&seedable_spans(regions));
	let Some(srat) = crate::smp::acpi_table(rsdp, b"SRAT") else {
		// SAID, RATHER THAN SILENT. "No SRAT" and "an SRAT this kernel could not read" are different
		// facts about a machine, and a boot report that showed neither would make the two look the
		// same from the outside. Recorded rather than printed: the baseline line carries it, so the
		// ordinary machine gets one line instead of two saying the same thing.
		record(Absence::NoSrat);
		return None;
	};
	if let Err(reason) = topology::acpi::parse_srat(srat, &mut builder) {
		record(Absence::Refused);
		crate::serial_println!("numa: the SRAT was refused ({reason:?}); this machine runs with no topology (rsdp {rsdp:#x})");
		return None;
	}
	if let Some(slit) = crate::smp::acpi_table(rsdp, b"SLIT")
		&& let Err(reason) = topology::acpi::parse_slit(slit, &mut builder)
	{
		// The distances are refused and the affinity is kept: a bad matrix says nothing about which
		// memory is on which node, and discarding that too would lose information the machine did
		// report correctly.
		crate::serial_println!("numa: the SLIT was refused ({reason:?}); node affinity is kept and distances are the local/remote default");
	}
	match builder.build() {
		Ok(found) => Some(found),
		Err(reason) => {
			record(Absence::Refused);
			crate::serial_println!("numa: the firmware topology does not hold together ({reason:?}); this machine runs with no topology");
			None
		}
	}
}

// The device-tree ports: `numa-node-id` on banks and harts, and `/distance-map`.
#[cfg(not(target_arch = "x86_64"))]
fn read(regions: &[bootproto::MemRegion]) -> Option<Topology> {
	let Some(info) = crate::arch::device_tree_boot_info() else {
		// WHICH OF THE TWO SILENCES THIS IS. "No tree was kept" and "the tree said nothing about
		// nodes" are different facts about a boot, and a reader that showed neither could not tell a
		// UEFI/no-DT profile from a discovery that failed. Both are recorded, and the baseline line
		// carries whichever it was.
		record(Absence::NoDeviceTree);
		return None;
	};
	let mut banks: alloc::vec::Vec<(u64, u64, u32)> = alloc::vec::Vec::new();
	for index in 0..info.ram_region_count {
		let (base, len) = info.ram_regions[index];
		// ALLOC-OK: read once at boot, bounded by `fdt::MAX_RAM_REGIONS`.
		banks.push((base, len, info.ram_region_nodes[index]));
	}
	let mut cpus: alloc::vec::Vec<(u64, u32)> = alloc::vec::Vec::new();
	for index in 0..info.cpu_count as usize {
		if index < fdt::MAX_CPUS {
			// ALLOC-OK: read once at boot, bounded by `fdt::MAX_CPUS`.
			cpus.push((info.cpu_ids[index], info.cpu_node_ids[index]));
		}
	}
	// A MALFORMED MATRIX IS REFUSED, AND THE AFFINITY IS KEPT.
	//
	// The reader used to hand back whatever prefix of a bad matrix it had managed to parse - a partial
	// triple at the end, more cells than the bound, a distance above 255, a `distance-map` node in a
	// format this kernel has never read - and a prefix of a false table is not a table. It now says so,
	// and the split here is the one the ACPI path already makes for a bad SLIT: bad distances say
	// nothing about which memory is on which node, so the banks and harts keep their nodes and the
	// distances fall back to the local/remote default.
	let distances: &[(u32, u32, u8)] = if info.numa_distance_malformed {
		crate::serial_println!("numa: the device tree's distance map was refused; node affinity is kept and distances are the local/remote default");
		&[]
	} else {
		&info.numa_distances[..info.numa_distance_count]
	};
	// THE SAME INTERSECTION THE ACPI READER MAKES. A device tree's `/memory` banks are the machine's,
	// and what this kernel seeds is the loader's map - on a direct boot they are close and on a UEFI
	// boot they are not, so the banks are clipped rather than assumed.
	let seedable = seedable_spans(regions);
	if !seedable.is_empty() {
		let mut clipped: alloc::vec::Vec<(u64, u64, u32)> = alloc::vec::Vec::new();
		for (base, len, node) in &banks {
			for (span_base, span_len) in &seedable {
				let low = (*base).max(*span_base);
				let high = (base + len).min(span_base + span_len);
				if high > low {
					// ALLOC-OK: read once at boot, bounded by the map's region count.
					clipped.push((low, high - low, *node));
				}
			}
		}
		banks = clipped;
	}
	let described = banks.iter().filter(|(_, _, node)| *node != topology::UNKNOWN_NODE).count();
	if described == 0 {
		record(Absence::NoNodeIds);
	}
	match topology::from_device_tree(&banks, &cpus, distances) {
		Ok(found) => Some(found),
		Err(reason) => {
			record(Absence::Refused);
			crate::serial_println!("numa: the device tree's topology does not hold together ({reason:?}); this machine runs with no topology");
			None
		}
	}
}

// What the boot report says about memory topology.
//
// PRINTED ON EVERY BOOT, like the free-frame count beside it: a baseline that is always there is
// what makes a later change worth reading, and "this machine has one node" is a fact about the
// machine rather than an absence of news.
pub fn report() {
	let Some(()) = crate::mem::with_topology(|found| {
		crate::serial_println!("numa: {} node(s), distances {}", found.nodes().len(), if found.distances().is_measured() { "from firmware" } else { "local/remote default" });
		for node in found.nodes() {
			let memory = found.memory_of(*node);
			let cpus = found.cpus().iter().filter(|(_, owner)| owner == node).count();
			// DESCRIBED AND ONLINE ARE TWO NUMBERS, and printing only the first was wrong.
			//
			// `found.cpus()` is what the firmware DESCRIBED. A core that timed out during bring-up,
			// or that never answered at all, is still in that list - so a node whose second core
			// never came up reported two processors and had one, and M1's rule that an absent or
			// timed-out CPU creates no logical affinity was invisible in the one place a reader
			// looks. The confirmed bindings are `smp::numa::online_on`, which is what the scheduler
			// actually places against.
			let online = crate::smp::numa::online_on(*node);
			crate::serial_println!("numa:   node {}: {} MiB, {} processor(s) described, {} online", node.0, memory / (1024 * 1024), cpus, online);
			// WHICH PROCESSORS, AND WHERE THE MEMORY IS - not just how many and how much.
			//
			// A gate can compare a COUNT with the profile it launched and still accept a graph whose
			// assignments are swapped: two nodes of equal size with two cores each satisfy every
			// count on a machine that has them the wrong way round. These are the assignments
			// themselves, which is what M5 means by the exact normalized graph and what M6 allows
			// the report to carry - bounded by the node and CPU counts, which are small.
			for (hardware_id, owner) in found.cpus().iter().filter(|(_, owner)| owner == node) {
				crate::serial_println!("numa:     node {} cpu {}", owner.0, hardware_id);
			}
			// THE RANGES ONLY IN A TEST BUILD. A machine's firmware describes memory in many small
			// pieces - fifteen on the x86_64 profile - and a line each would triple the length of
			// every ordinary boot's report for a fact only a gate compares. M6 allows the exact
			// ranges to be exposed under test, which is where they are read.
			if cfg!(test) {
				for range in found.ranges().iter().filter(|range| range.node == *node) {
					crate::serial_println!("numa:     node {} range {:#x}..{:#x}", node.0, range.base, range.end());
				}
			}
		}
		// AND HOW FAR EACH NODE IS FROM EACH, which decides every fallback and was never printed.
		for from in found.nodes() {
			for to in found.nodes() {
				crate::serial_println!("numa:   distance {} -> {}: {}", from.0, to.0, found.distance(*from, *to));
			}
		}
		// AND THE RULE THOSE DISTANCES ARE USED BY, said once rather than left to be inferred.
		//
		// M6 asks for the fallback policy in the report and the report carried only its INPUT. A
		// reader given a distance matrix and no rule cannot tell a machine that prefers the nearest
		// node from one that round-robins, and both would print exactly these lines.
		crate::serial_println!("numa:   fallback: the requested node first, then the rest by ascending distance, ties by ascending node id; unaffiliated memory last");
	}) else {
		crate::serial_println!("numa: no memory topology - one pool, no locality{}", absence_clause());
		return;
	};
	crate::mem::frame::report_pools();
}

#[cfg(test)]
mod tests;
