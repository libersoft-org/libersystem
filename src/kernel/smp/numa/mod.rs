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
	if !bindings.is_empty() {
		return BOUND.load(Ordering::Acquire);
	}
	// ALLOC-OK: once at boot, bounded by the core count the bring-up established.
	if bindings.try_reserve(count).is_err() {
		return 0;
	}
	for _ in 0..count {
		bindings.push(AtomicU32::new(UNBOUND));
	}
	let mut bound = 0usize;
	for cpu in 0..count {
		// A CORE THAT IS NOT ONLINE GETS NO BINDING. `online_count` is a tally rather than a mask,
		// so what stands in for "this core answered" is its hardware id being recorded - which is
		// what bring-up does when a core reports in.
		let hardware = crate::smp::lapic_id(cpu);
		if cpu > 0 && hardware == 0 {
			continue;
		}
		if let Affinity::Node(node) = crate::mem::topology_node_of_cpu(hardware) {
			bindings[cpu].store(node.0, Ordering::Release);
			bound += 1;
		}
	}
	BOUND.store(bound, Ordering::Release);
	bound
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
#[cfg(test)]
pub fn online_on(node: NodeId) -> usize {
	let bindings = BINDINGS.lock();
	bindings.iter().filter(|slot| slot.load(Ordering::Acquire) == node.0).count()
}

// Why a placement could not be made. TYPED, because "no CPU" and "no such node" are different
// answers and a caller that cannot tell them apart cannot report either.
// TEST-ONLY UNTIL SOMETHING ASKS FOR A PLACEMENT. The hint is what M3 owes and it is exercised
// directly; the callers that will use it are a service asking for a thread on a node, which
// this tree explicitly does not add - "the scheduler exposes enough placement to prove topology is
// used and stops there". A public API with no caller would be the opposite of that.
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
