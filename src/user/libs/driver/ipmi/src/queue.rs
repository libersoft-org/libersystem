//! ONE MESSAGE AT A TIME, AND THE PET FIRST. KCS, BT (QEMU's reports one outstanding request) and SSIF carry one request
//! at a time, so a binding keeps ONE queue in front of its interface. A watchdog pet or re-arm goes to its head and never
//! waits behind a queued read; every long read is already cut into bounded transactions by its caller, so the longest a
//! pet waits is one transaction and its recovery.

use alloc::collections::VecDeque;

/// Whether an entry jumps the queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority {
	Ordinary,
	/// A watchdog pet, arm or disarm.
	Watchdog,
}

/// The queue in front of one interface.
#[derive(Clone, Debug)]
pub struct Queue<T> {
	entries: VecDeque<T>,
	bound: usize,
}

impl<T> Queue<T> {
	pub fn new(bound: usize) -> Queue<T> {
		Queue { entries: VecDeque::new(), bound }
	}

	/// Queue `entry`: a watchdog entry at the head, behind any watchdog entry already there; an ordinary one at the tail.
	/// False when an ordinary entry finds the queue full; a watchdog entry is never refused.
	pub fn push(&mut self, entry: T, priority: Priority, is_watchdog: impl Fn(&T) -> bool) -> bool {
		match priority {
			Priority::Watchdog => {
				let at = self.entries.iter().take_while(|queued| is_watchdog(queued)).count();
				self.entries.insert(at, entry);
				true
			}
			Priority::Ordinary => {
				if self.entries.len() >= self.bound {
					return false;
				}
				self.entries.push_back(entry);
				true
			}
		}
	}

	pub fn pop(&mut self) -> Option<T> {
		self.entries.pop_front()
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}
