// System Graph: a point-in-time snapshot of the live kernel object tree - the
// Domains, the processes accounted to each, and the handles those processes hold.
//
// It is built by walking the Domain tree from a root and reading each process's
// handle table, and it is the introspection view the CLI's `graph` command
// prints. Each handle table is read under its lock, but the tree can change after
// collection, so the result is a snapshot rather than a live cursor.
//
// THE MODULE IS `cfg(test)`. Nothing in a running kernel builds a graph: there is no syscall behind
// it, and the renderer that printed one to the serial log had no caller either - the CLI's `graph`
// command is answered by SystemGraphService in userspace. What is left is what the domain-accounting
// tests walk, and it is compiled when they are.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::KernelObject;
use crate::object::domain::{Domain, UNLIMITED};
use crate::object::handle::HandleInfo;

// One process in the graph: its identity and the handles it holds.
pub struct ProcessNode {
	pub koid: u64,
	pub handles: Vec<HandleInfo>,
}

// One Domain in the graph: its identity, resource usage, processes, and children.
pub struct DomainNode {
	pub koid: u64,
	pub killed: bool,
	pub memory_used: u64,
	pub memory_limit: u64,
	pub handles_used: u64,
	pub threads_used: u64,
	pub processes: Vec<ProcessNode>,
	pub children: Vec<DomainNode>,
}

// Collect the subtree rooted at `domain`.
pub fn collect_from(domain: &Arc<Domain>) -> DomainNode {
	// ALLOC-OK: the System Graph dump, called from the kernel test suites and from no syscall.
	let processes: Vec<ProcessNode> = domain.live_processes().iter().map(|p| ProcessNode { koid: p.header().koid(), handles: p.handles().lock().entries() }).collect();
	// ALLOC-OK: the System Graph dump, as above.
	let children: Vec<DomainNode> = domain.child_domains().iter().map(collect_from).collect();
	let account = domain.account();
	DomainNode { koid: domain.header().koid(), killed: domain.is_killed(), memory_used: account.memory().used(), memory_limit: account.memory().limit(), handles_used: account.handles().used(), threads_used: account.threads().used(), processes, children }
}
