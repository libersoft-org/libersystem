// Which node each core is on, once the cores have actually come up.
//
// AFTER BRING-UP, AND ONLY FOR CONFIRMED CORES. Firmware describes processors it believes exist;
// bring-up finds out which of them answer. A core that timed out, came up late or was never started
// has no logical id worth binding - and binding one anyway would put an online mask's bit on a core
// that is not there, which is worse than an empty mask because it looks like an answer.
//
// THE HARDWARE ID IS THE JOIN. The topology speaks APIC ids, MPIDRs and hart ids; the scheduler
// speaks logical ids assigned by bring-up. `lapic_id(cpu)` is the one place the two meet, and doing
// the join anywhere else would mean inventing a correspondence.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use topology::{Affinity, NodeId};

use crate::sync::SpinLock;

// The node of each logical CPU, or `UNBOUND`. Sized at bring-up and read-only afterwards.
const UNBOUND: u32 = u32::MAX;
static BINDINGS: SpinLock<alloc::vec::Vec<AtomicU32>> = SpinLock::new(alloc::vec::Vec::new());
static BOUND: AtomicUsize = AtomicUsize::new(0);

// Bind every core that is online to the node its hardware id belongs to.
//
// Returns how many bindings were made. Zero on a machine with no topology, which is not a failure:
// it is a machine where every core is equally near everything.
pub fn bind_online() -> usize {
	let count = crate::smp::cpu_count();
	let mut bindings = BINDINGS.lock();
	if bindings.is_empty() {
		// ALLOC-OK: once at boot, bounded by the core count the bring-up established.
		if bindings.try_reserve(count).is_err() {
			return 0;
		}
		for _ in 0..count {
			bindings.push(AtomicU32::new(UNBOUND));
		}
	}
	let mut bound = 0usize;
	for cpu in 0..bindings.len() {
		if bindings[cpu].load(Ordering::Acquire) != UNBOUND {
			bound += 1;
			continue;
		}
		// A CORE THAT IS NOT ONLINE GETS NO BINDING, asked of the fact rather than of a stand-in.
		//
		// This used to read the core's controller id and treat zero as "never answered", because
		// `online_count` was a tally and said nothing about WHICH cores. That is right on a machine
		// whose boot core is APIC 0 and wrong on one where the SBI boot hart is not hart zero - hart
		// 0 is then an ordinary online secondary, and it got no node binding at all.
		if !crate::smp::is_online(cpu) {
			continue;
		}
		let hardware = crate::smp::lapic_id(cpu);
		if let Affinity::Node(node) = crate::mem::topology_node_of_cpu(hardware) {
			bindings[cpu].store(node.0, Ordering::Release);
			bound += 1;
		}
	}
	BOUND.store(bound, Ordering::Release);
	bound
}

// Bind ONE core, called by that core as it comes online.
//
// THE SWEEP ABOVE HAPPENS ONCE AND SOME CORES ARRIVE AFTER IT.
//
// Bring-up gives a core a bounded window to report in, and a core that misses it is correctly kept
// online rather than disowned - the compare-exchange in `smpboot` exists for exactly that race. But
// the portable online bit is set by the core ITSELF, after bring-up has already moved on, so a core
// landing in that window was not online when `bind_online` swept and the sweep returned early on
// every later call. It was scheduled on, it allocated memory, and it belonged to no node for the
// rest of the boot. A core now binds itself when it arrives, and the sweep fills in whoever was
// already there.
pub fn bind_self(cpu: usize) {
	let bindings = BINDINGS.lock();
	// Before the sweep has sized the table there is nothing to write into, and nothing to miss
	// either: `bind_online` will reach this core because it is online by the time it runs.
	let Some(slot) = bindings.get(cpu) else { return };
	if slot.load(Ordering::Acquire) != UNBOUND {
		return;
	}
	let hardware = crate::smp::lapic_id(cpu);
	if let Affinity::Node(node) = crate::mem::topology_node_of_cpu(hardware) {
		slot.store(node.0, Ordering::Release);
		BOUND.fetch_add(1, Ordering::AcqRel);
		crate::serial_println!("numa: core {cpu} arrived after the topology sweep and bound itself to node {}", node.0);
	}
	drop(bindings);
}

// Which node a logical CPU is on. `Unknown` for a core firmware said nothing about, and for every
// core on a machine with no topology.
pub fn cpu_node(cpu: usize) -> Affinity {
	let bindings = BINDINGS.lock();
	match bindings.get(cpu).map(|slot| slot.load(Ordering::Acquire)) {
		Some(node) if node != UNBOUND => Affinity::Node(NodeId(node)),
		_ => Affinity::Unknown,
	}
}

// How many online cores this node has. An unconfirmed core is in no node's count.
//
// The per-node mask, in the one form anything in this kernel needs: a count. A bitmask would be an
// interface for a scheduler policy this tree explicitly does not add, and adding one to make a
// test easier is how a milestone grows an API nobody asked for.
//
// NO LONGER TEST-ONLY: the boot report is a production caller. It printed the count of FIRMWARE
// RECORDS per node, so a core that timed out or never came up was still reported under its node -
// and M1 says an absent or timed-out CPU creates no logical affinity. The two counts are different
// facts and the report now prints both.
pub fn online_on(node: NodeId) -> usize {
	let bindings = BINDINGS.lock();
	bindings.iter().filter(|slot| slot.load(Ordering::Acquire) == node.0).count()
}

// Why a placement could not be made. TYPED, because "no CPU" and "no such node" are different
// answers and a caller that cannot tell them apart cannot report either.
//
// STILL TEST-ONLY, AND THE REASON IS NOW WRITTEN AS A LIMIT RATHER THAN A PREFERENCE (2026-08-30).
// `place_on` is the NODE -> CPU direction, and nothing in this kernel asks it: there is no
// production kernel-thread spawner in this tree at all - `spawn_with_object` and its siblings are
// themselves `#[cfg(test)]` - so a production caller cannot be wired without adding the service M3's
// last bullet explicitly refuses ("the scheduler exposes enough placement to prove topology is used
// and stops there").
//
// What the SHIPPING kernel does use is the other direction: `Thread::new` names the creating core, so
// `cpu_node` decides the node its kernel stack is taken from. That is M3's third bullet and it is in
// the product; this hint is the half whose consumer does not exist yet, and saying so is more honest
// than inventing one to make it reachable.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	// This machine has no topology, so there is no node to place on.
	NoTopology,
	// The node exists and no core of it is online.
	NoOnlineCpu,
}

// A logical CPU on `node`, or a typed refusal.
//
// NO FALLBACK. A caller that would rather have any core than fail is not asking for placement; it is
// asking for `spawn`, which keeps its current semantics. Falling back silently would make "this
// thread runs on node 1" a statement nobody can rely on.
#[cfg(test)]
pub fn place_on(node: NodeId) -> Result<usize, Refusal> {
	if crate::mem::with_topology(|_| ()).is_none() {
		return Err(Refusal::NoTopology);
	}
	let bindings = BINDINGS.lock();
	for (cpu, slot) in bindings.iter().enumerate() {
		if slot.load(Ordering::Acquire) == node.0 {
			return Ok(cpu);
		}
	}
	Err(Refusal::NoOnlineCpu)
}

// The node the CALLING core is on, for an allocation that wants to be local.
//
// `None` before bring-up has bound anything, which is the readiness point the contract names: an
// allocation must not ask which CPU it is on before per-CPU state exists, and this answers `None`
// rather than guessing until it does.
pub fn local_node() -> Option<NodeId> {
	if BOUND.load(Ordering::Acquire) == 0 {
		return None;
	}
	cpu_node(crate::sched::current_cpu_id()).node()
}

#[cfg(test)]
mod tests;
