//! What a listener claims, and when two claims collide.
//!
//! THE MATRIX IS A FUNCTION, NOT A CONVENTION. "Which binds may share a port" is the sort of rule
//! that gets written three times - once in the admission check, once in the demultiplexer, once in a
//! comment - and the three disagree the first time a case is added. There is one of it here, and
//! both the admission and the lookup are written against it.
//!
//! NO REUSE FLAG EXISTS IN THIS MILESTONE, and that is what makes the matrix small. A wildcard and a
//! specific address on the same port CONFLICT: the narrower bind is not a carve-out from the wider
//! one, because nothing in the contract lets a caller ask for one.
//!
//! AN IPv4-MAPPED IPv6 ADDRESS IS NOT AN IPv4 BIND. It is refused as a listen address in every mode:
//! an IPv4 endpoint is expressible directly, so a second spelling for it buys nothing and costs every
//! consumer a check it will sometimes forget - and the one that forgets grants IPv4 reach to a
//! listener that asked for IPv6.

/// Which families a listener binds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindMode {
	Ipv4Only,
	Ipv6Only,
	/// Both families at once, and only ever on the unspecified IPv6 address.
	DualStack,
}

/// A local address a bind or an arriving segment names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Local {
	V4([u8; 4]),
	V6([u8; 16]),
}

impl Local {
	pub fn is_v4(&self) -> bool {
		matches!(self, Local::V4(_))
	}

	pub fn is_unspecified(&self) -> bool {
		match self {
			Local::V4(octets) => *octets == [0; 4],
			Local::V6(octets) => *octets == [0; 16],
		}
	}

	/// `::ffff:a.b.c.d`, the form this system refuses as a listen address.
	pub fn is_ipv4_mapped(&self) -> bool {
		match self {
			Local::V4(_) => false,
			Local::V6(octets) => octets[..10] == [0; 10] && octets[10] == 0xff && octets[11] == 0xff,
		}
	}
}

/// Why a bind was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindRefusal {
	/// The address's family disagrees with the mode, or a dual-stack bind named a specific address.
	Mismatch,
	/// An IPv4-mapped IPv6 address, which is not a way to express an IPv4 bind.
	Mapped,
	/// Something already holds this port in a way that overlaps.
	InUse,
}

/// One listener's claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
	pub mode: BindMode,
	pub address: Local,
	pub port: u16,
}

impl Binding {
	/// Is this claim well formed on its own terms, before anything else is considered?
	pub fn well_formed(&self) -> Result<(), BindRefusal> {
		if self.address.is_ipv4_mapped() {
			return Err(BindRefusal::Mapped);
		}
		match (self.mode, self.address) {
			(BindMode::Ipv4Only, Local::V4(_)) => Ok(()),
			(BindMode::Ipv6Only, Local::V6(_)) => Ok(()),
			// A BIND COVERING TWO FAMILIES CANNOT NAME ONE ADDRESS IN ONE OF THEM.
			(BindMode::DualStack, Local::V6(_)) if self.address.is_unspecified() => Ok(()),
			_ => Err(BindRefusal::Mismatch),
		}
	}

	/// Do two claims collide?
	///
	/// DIFFERENT PORTS NEVER COLLIDE, and on the same port the rule is the matrix:
	///
	///   IPv4-only + IPv6-only    BOTH ADMITTED - two families, two wildcards, no overlap
	///   dual-stack + anything    REFUSED, either order - it covers both families
	///   the same mode twice      REFUSED
	///   wildcard + a specific    REFUSED while there is no reuse rule
	pub fn conflicts_with(&self, other: &Binding) -> bool {
		if self.port != other.port {
			return false;
		}
		match (self.mode, other.mode) {
			(BindMode::DualStack, _) | (_, BindMode::DualStack) => true,
			(BindMode::Ipv4Only, BindMode::Ipv6Only) | (BindMode::Ipv6Only, BindMode::Ipv4Only) => false,
			// The same family: the same mode twice, and a wildcard beside a specific address, are
			// both refused - the narrower bind is not a carve-out from the wider one.
			_ => true,
		}
	}

	/// Would a segment arriving for `local` on this port be this listener's?
	pub fn accepts(&self, local: Local, port: u16) -> bool {
		if self.port != port {
			return false;
		}
		match self.mode {
			BindMode::DualStack => true,
			BindMode::Ipv4Only if !local.is_v4() => false,
			BindMode::Ipv6Only if local.is_v4() => false,
			_ => self.address.is_unspecified() || self.address == local,
		}
	}
}

/// The listeners a service is holding.
#[derive(Debug, Default)]
pub struct BindTable {
	entries: alloc::vec::Vec<Binding>,
}

impl BindTable {
	pub fn new() -> BindTable {
		BindTable::default()
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	/// Admit a claim, or say why not.
	pub fn bind(&mut self, binding: Binding) -> Result<(), BindRefusal> {
		binding.well_formed()?;
		if self.entries.iter().any(|held| held.conflicts_with(&binding)) {
			return Err(BindRefusal::InUse);
		}
		self.entries.push(binding);
		Ok(())
	}

	pub fn unbind(&mut self, binding: &Binding) -> bool {
		let before: usize = self.entries.len();
		self.entries.retain(|held| held != binding);
		before != self.entries.len()
	}

	/// Which listener a segment for `local`:`port` belongs to.
	///
	/// THE MOST SPECIFIC CLAIM WINS. With no reuse rule a wildcard and a specific address cannot both
	/// be held, so at most one can match - but the order is written down rather than left to
	/// insertion order, because insertion order is not a rule anybody can rely on.
	pub fn lookup(&self, local: Local, port: u16) -> Option<&Binding> {
		self.entries.iter().filter(|held| held.accepts(local, port)).min_by_key(|held| u8::from(held.address.is_unspecified()))
	}
}

#[cfg(test)]
mod tests;
