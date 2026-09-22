//! WHICH PROVIDER KINDS A MINTED CATALOGUE CONNECTION MAY REACH.
//!
//! The provider catalogue's root answers the reserved CONNECT opcode with a connection that can
//! subscribe to every kind the machine publishes and open any provider of any of them. That was the
//! only shape there was, so a service needing one kind held the same authority as one driving eight,
//! and an inventory client that only ever lists devices held it too.
//!
//! THE SUBSET IS THE SERVER'S RECORD OF WHAT IT MINTED AND NEVER THE CALLER'S CLAIM. A request names
//! a kind; the check is against the scope stored beside the connection the request arrived on. A
//! caller that could be believed about its own kind could name a different one, which is the whole
//! of the difference between a capability and a convention.
//!
//! AN INVENTORY SCOPE IS A REAL SCOPE AND NOT AN ABSENCE. Two consumers here - `lsdev`'s service and
//! the System Graph - read the binding snapshot and nothing else, and the binding snapshot is not a
//! transport: it names what is bound, it opens nothing. So they get a scope that admits no kind at
//! all, which is expressible, rather than being handed the whole vocabulary because there was no way
//! to say "none".

/// The widest kind value a scope can express, bounded by the mask's own width.
///
/// THIS IS NOT THE VOCABULARY'S SIZE and deliberately does not track it. Whether a number is a kind
/// this system has is the protocol decoder's question and it already answers it - an unknown
/// discriminant does not decode, so it never reaches here. What this bounds is the mask, and a kind
/// allocated past it is a mask that silently wraps.
pub const MAX_KIND: u16 = 31;

/// The most kinds one connection may be minted for, which is the interface's own bound.
pub const MAX_SCOPE_KINDS: usize = 16;

/// Why a scope could not be minted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// No kinds at all. A consumer with an empty set is a manifest row that forgot one, and a
	/// connection that refuses every request turns that into a runtime mystery - so it is refused
	/// where it is asked for. `Scope::inventory` is how a caller says "none" on purpose.
	EmptySet,
	/// A kind value this mask cannot express, including zero - which is no kind at all.
	UnknownKind(u16),
	/// More entries than the interface admits, whether or not they are distinct.
	TooManyKinds,
}

/// What one minted connection may do.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Scope {
	allowed: u32,
}

impl Scope {
	/// The read-only scope: the binding snapshot, and no kind.
	pub const fn inventory() -> Self {
		Self { allowed: 0 }
	}

	/// The scope the catalogue's own root holds, which is every kind.
	///
	/// IT IS NOT HANDED TO A CONSUMER. The root is a service registration rather than a connection,
	/// and what reaches it is the supervisor minting on somebody else's behalf.
	pub const fn unrestricted() -> Self {
		Self { allowed: u32::MAX }
	}

	/// Mint a scope over these kinds. Duplicates are the same subset and are accepted.
	pub fn of(kinds: &[u16]) -> Result<Self, Refusal> {
		if kinds.is_empty() {
			return Err(Refusal::EmptySet);
		}
		if kinds.len() > MAX_SCOPE_KINDS {
			return Err(Refusal::TooManyKinds);
		}
		let mut allowed: u32 = 0;
		let mut at = 0;
		while at < kinds.len() {
			let kind = kinds[at];
			if kind == 0 || kind > MAX_KIND {
				return Err(Refusal::UnknownKind(kind));
			}
			allowed |= 1 << kind;
			at += 1;
		}
		Ok(Self { allowed })
	}

	/// Whether a request naming this kind may be answered on this connection.
	pub const fn admits(&self, kind: u16) -> bool {
		if kind == 0 || kind > MAX_KIND {
			return false;
		}
		self.allowed & (1 << kind) != 0
	}

	/// Whether this scope reaches no provider at all.
	pub const fn is_inventory(&self) -> bool {
		self.allowed == 0
	}

	/// The mask, for a table that stores one per slot.
	pub const fn bits(&self) -> u32 {
		self.allowed
	}

	/// The scope a stored mask describes. THE INVERSE OF `bits` AND NOTHING MORE: it is how a table
	/// gives a scope back, not a second way to construct one, so it performs no validation and a
	/// caller must not reach it with a number that did not come from `bits`.
	pub const fn from_bits(allowed: u32) -> Self {
		Self { allowed }
	}
}

#[cfg(test)]
mod tests;
