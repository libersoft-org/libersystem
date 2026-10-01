//! THE ORDER THE DRIVERS' STEP ASKS ITS BINDINGS IN - DeviceManager's, one binding at a time.
//!
//! SUSPEND: reverse bind order - a binding that consumes another's provider before that provider, whatever order they
//! bound in - and every binding that publishes a `watchdog` LAST, so the timer is disarmed or bounded once nothing else
//! can hang the sleep. RESUME: the reverse - every `watchdog` publisher FIRST, re-armed with a timeout that also catches a
//! resume that hangs, then providers before their consumers, in bind order.
//!
//! A WATCHDOG PUBLISHER THAT CONSUMES A PROVIDER CAN BE NEITHER: a child of a controller is suspended before its
//! controller and resumed after it, or its own resume waits on a controller not yet serving. So the watchdog's place
//! is a leaf's alone - a binding that consumes nothing - and one that consumes is ordered by its depth like any other.

use alloc::vec::Vec;

/// One online binding, as the order sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
	/// It publishes a `watchdog`.
	pub watchdog: bool,
	/// How many providers deep it consumes: zero for a binding that consumes none.
	pub depth: u32,
	/// When it bound.
	pub bind_at: u64,
}

/// The suspend order, as indices into `bindings`.
pub fn suspend_order(bindings: &[Binding]) -> Vec<usize> {
	let mut order: Vec<usize> = (0..bindings.len()).collect();
	order.sort_by_key(|&at| (bindings[at].watchdog_leaf(), core::cmp::Reverse(bindings[at].depth), core::cmp::Reverse(bindings[at].bind_at), core::cmp::Reverse(at)));
	order
}

impl Binding {
	// Whether it takes the watchdog's place: a publisher that consumes nothing.
	fn watchdog_leaf(&self) -> bool {
		self.watchdog && self.depth == 0
	}
}

/// The resume order of the bindings `suspended` names.
pub fn resume_order(bindings: &[Binding], suspended: &[usize]) -> Vec<usize> {
	let mut order: Vec<usize> = suspended.to_vec();
	order.sort_by_key(|&at| (!bindings[at].watchdog_leaf(), bindings[at].depth, bindings[at].bind_at, at));
	order
}

#[cfg(test)]
mod tests;
