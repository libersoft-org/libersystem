// Capability rights: the set of operations a handle is allowed to perform.
//
// Rights are a bitset bound into every capability. They can only be narrowed
// (attenuated), never widened: a derived capability must carry a subset of the
// original's rights. This is the structural basis for least privilege when
// capabilities are passed around the system.

#![allow(dead_code)]

use core::ops::{BitAnd, BitOr};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rights(u32);

impl Rights {
	pub const NONE: Rights = Rights(0);

	pub const READ: Rights = Rights(abi::RIGHT_READ);
	pub const WRITE: Rights = Rights(abi::RIGHT_WRITE);
	pub const EXECUTE: Rights = Rights(abi::RIGHT_EXECUTE);
	pub const MAP: Rights = Rights(abi::RIGHT_MAP);
	pub const SEND: Rights = Rights(abi::RIGHT_SEND);
	pub const RECEIVE: Rights = Rights(abi::RIGHT_RECEIVE);
	pub const DUPLICATE: Rights = Rights(abi::RIGHT_DUPLICATE);
	pub const TRANSFER: Rights = Rights(abi::RIGHT_TRANSFER);
	pub const REVOKE: Rights = Rights(abi::RIGHT_REVOKE);
	pub const GET_INFO: Rights = Rights(abi::RIGHT_GET_INFO);
	pub const MANAGE: Rights = Rights(abi::RIGHT_MANAGE);
	pub const WAIT: Rights = Rights(abi::RIGHT_WAIT);

	// Every currently defined right.
	pub const ALL: Rights = Rights(abi::RIGHTS_ALL);

	pub const fn bits(self) -> u32 {
		self.0
	}

	// Build a rights set from raw bits, dropping any outside the defined set
	// (boundary hygiene for a value arriving as a syscall argument).
	pub const fn from_bits(bits: u32) -> Rights {
		Rights(bits & Self::ALL.0)
	}

	pub const fn is_empty(self) -> bool {
		self.0 == 0
	}

	// True if every right in `other` is also present in `self`.
	pub const fn contains(self, other: Rights) -> bool {
		self.0 & other.0 == other.0
	}
}

impl BitOr for Rights {
	type Output = Rights;
	fn bitor(self, rhs: Rights) -> Rights {
		Rights(self.0 | rhs.0)
	}
}

impl BitAnd for Rights {
	type Output = Rights;
	fn bitand(self, rhs: Rights) -> Rights {
		Rights(self.0 & rhs.0)
	}
}
