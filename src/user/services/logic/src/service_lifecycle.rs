// Select the next pending service in this pass whose declared dependencies are ready.
// ServiceManager supplies its real manifest and states; host regressions supply outcomes.
pub fn next_startable<'a>(from: usize, count: usize, mut pending: impl FnMut(usize) -> bool, mut dependencies: impl FnMut(usize) -> &'a [&'a [u8]], mut ready: impl FnMut(&[u8]) -> bool) -> Option<usize> {
	(from..count).find(|&index| pending(index) && dependencies(index).iter().all(|dependency| ready(dependency)))
}

use alloc::vec;
use alloc::vec::Vec;

pub fn depends_on_any(node: usize, node_count: usize, mut selected: impl FnMut(usize) -> bool, mut depends_on: impl FnMut(usize, usize) -> bool) -> bool {
	(0..node_count).any(|dependency| selected(dependency) && depends_on(node, dependency))
}

pub fn has_active_dependent(node: usize, node_count: usize, mut active: impl FnMut(usize) -> bool, mut depends_on: impl FnMut(usize, usize) -> bool) -> bool {
	(0..node_count).any(|dependent| dependent != node && active(dependent) && depends_on(dependent, node))
}

pub fn reverse_dependency_order(node_count: usize, mut selected: impl FnMut(usize) -> bool, mut depends_on: impl FnMut(usize, usize) -> bool) -> Option<Vec<usize>> {
	let scope = (0..node_count).map(&mut selected).collect::<Vec<_>>();
	let scoped_count = scope.iter().filter(|included| **included).count();
	let mut dropped = vec![false; node_count];
	let mut order = Vec::with_capacity(scoped_count);
	loop {
		let mut progress = false;
		for node in 0..node_count {
			if !scope[node] || dropped[node] {
				continue;
			}
			let blocked = (0..node_count).any(|dependent| dependent != node && scope[dependent] && !dropped[dependent] && depends_on(dependent, node));
			if !blocked {
				dropped[node] = true;
				order.push(node);
				progress = true;
			}
		}
		if !progress {
			break;
		}
	}
	(order.len() == scoped_count).then_some(order)
}

pub fn verify_reverse_dependency_order(order: &[usize], node_count: usize, mut selected: impl FnMut(usize) -> bool, mut depends_on: impl FnMut(usize, usize) -> bool) -> bool {
	let mut positions = vec![None; node_count];
	for (position, &node) in order.iter().enumerate() {
		if node >= node_count || positions[node].replace(position).is_some() || !selected(node) {
			return false;
		}
	}
	for (node, position) in positions.iter().enumerate() {
		if selected(node) != position.is_some() {
			return false;
		}
	}
	for (dependent, dependent_position) in positions.iter().enumerate() {
		let Some(dependent_position) = dependent_position else { continue };
		for (dependency, dependency_position) in positions.iter().enumerate() {
			if depends_on(dependent, dependency) && dependency_position.is_some_and(|position| position < *dependent_position) {
				return false;
			}
		}
	}
	true
}

#[cfg(test)]
#[path = "service_lifecycle/tests.rs"]
mod tests;
