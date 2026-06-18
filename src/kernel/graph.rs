// System Graph: a point-in-time snapshot of the live kernel object tree - the
// Domains, the processes accounted to each, and the handles those processes hold.
//
// It is built by walking the Domain tree from a root and reading each process's
// handle table, and it is the introspection view the CLI's `graph` command
// prints. Each handle table is read under its lock, but the tree can change after
// collection, so the result is a snapshot rather than a live cursor.

#![allow(dead_code)]

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::domain::{Domain, UNLIMITED};
use crate::object::handle::HandleInfo;
use crate::object::KernelObject;
use crate::sched;

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

// Collect the whole System Graph, rooted at the kernel's root Domain.
pub fn collect() -> DomainNode {
	collect_from(&sched::root_domain())
}

// Collect the subtree rooted at `domain`.
pub fn collect_from(domain: &Arc<Domain>) -> DomainNode {
	let processes: Vec<ProcessNode> = domain.live_processes().iter().map(|p| ProcessNode { koid: p.header().koid(), handles: p.handles().lock().entries() }).collect();
	let children: Vec<DomainNode> = domain.child_domains().iter().map(collect_from).collect();
	let account = domain.account();
	DomainNode { koid: domain.header().koid(), killed: domain.is_killed(), memory_used: account.memory().used(), memory_limit: account.memory().limit(), handles_used: account.handles().used(), threads_used: account.threads().used(), processes, children }
}

// Print the graph rooted at `node` to the log, indented by tree depth.
pub fn render(node: &DomainNode) {
	render_domain(node, 0);
}

fn indent(depth: usize) {
	for _ in 0..depth {
		crate::serial_print!("  ");
	}
}

fn render_domain(node: &DomainNode, depth: usize) {
	indent(depth);
	let limit: String = if node.memory_limit == UNLIMITED { String::from("inf") } else { alloc::format!("{}", node.memory_limit) };
	let killed: &str = if node.killed { " (killed)" } else { "" };
	crate::serial_println!("domain koid={} mem {}/{} handles {} threads {}{}", node.koid, node.memory_used, limit, node.handles_used, node.threads_used, killed);
	for process in &node.processes {
		indent(depth + 1);
		crate::serial_println!("process koid={} ({} handles)", process.koid, process.handles.len());
		for handle in &process.handles {
			indent(depth + 2);
			crate::serial_println!("handle koid={} {} rights={:#05x} badge={}", handle.koid, handle.object_type.name(), handle.rights.bits(), handle.badge);
		}
	}
	for child in &node.children {
		render_domain(child, depth + 1);
	}
}

// Total number of processes in the subtree (a summary used by tests and callers).
pub fn count_processes(node: &DomainNode) -> usize {
	node.processes.len() + node.children.iter().map(count_processes).sum::<usize>()
}
