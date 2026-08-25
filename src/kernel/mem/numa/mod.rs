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
pub fn discover() -> bool {
	let Some(found) = read() else {
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
fn read() -> Option<Topology> {
	let rsdp = crate::boot_info().rsdp;
	let mut builder = topology::Builder::new();
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
fn read() -> Option<Topology> {
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
	let distances = &info.numa_distances[..info.numa_distance_count];
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
			crate::serial_println!("numa:   node {}: {} MiB, {} processor(s)", node.0, memory / (1024 * 1024), cpus);
		}
	}) else {
		crate::serial_println!("numa: no memory topology - one pool, no locality{}", absence_clause());
		return;
	};
	crate::mem::frame::report_pools();
}

#[cfg(test)]
mod tests;
