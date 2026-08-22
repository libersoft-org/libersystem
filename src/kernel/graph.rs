// The live object tree under one Domain: the processes accounted to it and the handles each holds.
//
// It is built by walking a Domain and reading each process's handle table under its own lock, so
// the result is a snapshot rather than a live cursor.
//
// THE MODULE IS `cfg(test)`. Nothing in a running kernel builds one: there is no syscall behind it,
// and the renderer that printed a tree to the serial log had no caller either - the CLI's `graph`
// command is answered by SystemGraphService in userspace. What is left is what the domain
// accounting tests walk, and it carries exactly what they read: a Domain's koid, its live
// processes, and their handles. It used to carry the Domain's resource counters and its children
// too, and nothing ever looked at either.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::KernelObject;
use crate::object::domain::Domain;
use crate::object::handle::HandleInfo;

// One process in the graph: its identity and the handles it holds.
pub struct ProcessNode {
	pub koid: u64,
	pub handles: Vec<HandleInfo>,
}

// One Domain in the graph: its identity and the processes accounted to it.
pub struct DomainNode {
	pub koid: u64,
	pub processes: Vec<ProcessNode>,
}

// Collect the processes under `domain`.
pub fn collect_from(domain: &Arc<Domain>) -> DomainNode {
	// ALLOC-OK: the System Graph dump, called from the kernel test suites and from no syscall.
	let processes: Vec<ProcessNode> = domain.live_processes().iter().map(|p| ProcessNode { koid: p.header().koid(), handles: p.handles().lock().entries() }).collect();
	DomainNode { koid: domain.header().koid(), processes }
}
