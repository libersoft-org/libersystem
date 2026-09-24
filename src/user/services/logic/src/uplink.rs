//! WHICH LINK NETWORKSERVICE RUNS ON: the Ethernet publications it has seen, the one it selected, and
//! the modem context that may be installed in its place.
//!
//! ONE UPLINK AT A TIME. At most sixteen Ethernet publications are tracked; the one selected stays
//! selected while it is usable, and otherwise the lowest live publication identity is chosen - a stable
//! rule, so two boots with the same devices pick the same NIC. A modem is never chosen automatically:
//! it is INSTALLED, through a reservation made before the modem is asked to activate anything, and a
//! reservation is refused - `busy` - when another link is selected and the caller may not replace it.
//!
//! EVERY SWITCH IS A NEW INTERFACE GENERATION. What was bound to the old link - sockets, addresses,
//! routes, DNS - belongs to a generation that is over, and a stale event naming it cannot restore it.
//! When the modem goes, the NIC it replaced comes back if it is still live; otherwise the lowest live
//! one; otherwise nothing, and the service serves unlinked.

use alloc::vec::Vec;

/// The most Ethernet publications tracked at once.
pub const MAX_NICS: usize = 16;
/// The IPv4 MTU range a raw-IP link may be installed with: the smallest datagram every host must accept,
/// and the largest this system's packet channels carry.
pub const MIN_MTU: u16 = 576;
pub const MAX_MTU: u16 = 4096;

/// A publication as DeviceManager identified it. Ordered by slot, then generation, then binding
/// generation: "lowest" is well defined and stable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Publication {
	pub slot: u32,
	pub generation: u32,
	pub binding_generation: u64,
}

/// What the service is running on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Selected {
	None,
	Nic(Publication),
	/// A raw-IP link installed under this reservation.
	Modem(u64),
}

/// What the service has to do about a change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
	/// Nothing about the selected link changed.
	Keep,
	/// Tear the current link down - every socket and address with it - and run on this one, as
	/// interface generation `generation`.
	Switch { to: Selected, generation: u64 },
}

/// Why a reservation or an installation was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
	/// Another link is selected and replacing it was not allowed, or a modem link already exists.
	Busy,
	/// No such reservation, or not in the state the request needs.
	NotFound,
	/// The attachment's configuration is not one this service can install.
	Invalid,
	/// A family this service does not install on a raw-IP link.
	Unsupported,
	/// Past the publication bound.
	Exhausted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reservation {
	Reserved { id: u64, replace: bool },
	Installed { id: u64 },
}

pub struct Uplinks {
	nics: Vec<Publication>,
	// A NIC whose channel failed stays out of selection until it is published again.
	failed: Vec<Publication>,
	selected: Selected,
	// The NIC a modem replaced, to go back to.
	replaced: Option<Publication>,
	reservation: Option<Reservation>,
	next_reservation: u64,
	generation: u64,
}

impl Default for Uplinks {
	fn default() -> Self {
		Self::new()
	}
}

impl Uplinks {
	pub fn new() -> Self {
		Self { nics: Vec::new(), failed: Vec::new(), selected: Selected::None, replaced: None, reservation: None, next_reservation: 1, generation: 0 }
	}

	pub fn selected(&self) -> Selected {
		self.selected
	}

	pub fn generation(&self) -> u64 {
		self.generation
	}

	pub fn nics(&self) -> &[Publication] {
		&self.nics
	}

	fn usable(&self) -> impl Iterator<Item = &Publication> {
		self.nics.iter().filter(|nic| !self.failed.contains(nic))
	}

	fn switch(&mut self, to: Selected) -> Decision {
		self.selected = to;
		self.generation += 1;
		Decision::Switch { to, generation: self.generation }
	}

	// When nothing a modem holds is selected: the lowest usable NIC, or none.
	fn fallback(&mut self) -> Decision {
		let lowest = self.usable().min().copied();
		match lowest {
			Some(nic) => self.switch(Selected::Nic(nic)),
			None if self.selected == Selected::None => Decision::Keep,
			None => self.switch(Selected::None),
		}
	}

	/// A publication appeared - at boot or late. A service with no link takes it at once; one with a
	/// link keeps it. Refused past sixteen, and a repeat changes nothing.
	pub fn published(&mut self, nic: Publication) -> Result<Decision, Refusal> {
		if self.nics.contains(&nic) {
			return Ok(Decision::Keep);
		}
		if self.nics.len() >= MAX_NICS {
			return Err(Refusal::Exhausted);
		}
		self.nics.push(nic);
		self.failed.retain(|failed| *failed != nic);
		// A LINK NOW IS BETTER THAN A RESERVATION LATER: an unlinked service takes the NIC even while a modem
		// is being activated, and that modem's installation then needs the authority to replace it.
		if self.selected == Selected::None {
			return Ok(self.switch(Selected::Nic(nic)));
		}
		Ok(Decision::Keep)
	}

	/// A publication was withdrawn. If it was the selected link, the service moves on; if it was the NIC
	/// a modem replaced, there is nothing to go back to any more.
	pub fn withdrawn(&mut self, nic: Publication) -> Decision {
		self.nics.retain(|held| *held != nic);
		self.failed.retain(|failed| *failed != nic);
		if self.replaced == Some(nic) {
			self.replaced = None;
		}
		if self.selected == Selected::Nic(nic) { self.fallback() } else { Decision::Keep }
	}

	/// The selected NIC's channel failed: it is torn down and not chosen again until it is republished.
	pub fn failed(&mut self, nic: Publication) -> Decision {
		if !self.failed.contains(&nic) {
			self.failed.push(nic);
		}
		if self.replaced == Some(nic) {
			self.replaced = None;
		}
		if self.selected == Selected::Nic(nic) { self.fallback() } else { Decision::Keep }
	}

	/// ADMISSION, BEFORE THE MODEM DOES ANYTHING. One modem link at a time; and while another link is
	/// selected, only a caller allowed to replace it may reserve.
	pub fn reserve(&mut self, replace: bool) -> Result<u64, Refusal> {
		if self.reservation.is_some() || matches!(self.selected, Selected::Modem(_)) {
			return Err(Refusal::Busy);
		}
		if matches!(self.selected, Selected::Nic(_)) && !replace {
			return Err(Refusal::Busy);
		}
		let id = self.next_reservation;
		self.next_reservation += 1;
		self.reservation = Some(Reservation::Reserved { id, replace });
		Ok(id)
	}

	/// Commit a reserved link, whose configuration `validate` accepted. The selected NIC, if any, is
	/// remembered so it can come back.
	pub fn install(&mut self, reservation: u64) -> Result<Decision, Refusal> {
		let Some(Reservation::Reserved { id, replace }) = self.reservation else { return Err(Refusal::NotFound) };
		if id != reservation {
			return Err(Refusal::NotFound);
		}
		// The selection may have changed since the reservation: a NIC that appeared meanwhile is still
		// not replaceable by a caller that could not replace one.
		if let Selected::Nic(nic) = self.selected {
			if !replace {
				self.reservation = None;
				return Err(Refusal::Busy);
			}
			self.replaced = Some(nic);
		}
		self.reservation = Some(Reservation::Installed { id });
		Ok(self.switch(Selected::Modem(id)))
	}

	/// Give a reservation back, or remove the link installed under it: the NIC it replaced returns if it
	/// is still live, otherwise the lowest live NIC, otherwise none. A reservation that is not the
	/// current one - an old event - changes nothing.
	pub fn release(&mut self, reservation: u64) -> Decision {
		match self.reservation {
			Some(Reservation::Reserved { id, .. }) if id == reservation => {
				self.reservation = None;
				Decision::Keep
			}
			Some(Reservation::Installed { id }) if id == reservation => {
				self.reservation = None;
				let back = self.replaced.take().filter(|nic| self.usable().any(|usable| usable == nic));
				match back {
					Some(nic) => self.switch(Selected::Nic(nic)),
					None => {
						self.selected = Selected::None;
						match self.fallback() {
							Decision::Keep => self.switch(Selected::None),
							switched => switched,
						}
					}
				}
			}
			_ => Decision::Keep,
		}
	}
}

/// The configuration a raw-IP link arrives with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attachment {
	/// 4 or 6.
	pub family: u8,
	pub address: [u8; 4],
	pub prefix: u8,
	pub gateway: Option<[u8; 4]>,
	pub dns: [Option<[u8; 4]>; 2],
	pub mtu: u16,
}

fn unicast(address: [u8; 4]) -> bool {
	let first = address[0];
	address != [0; 4] && address != [255; 4] && first != 127 && !(224..=239).contains(&first) && first < 240
}

fn mask(prefix: u8) -> u32 {
	if prefix == 0 { 0 } else { u32::MAX << (32 - u32::from(prefix)) }
}

/// WHETHER A RAW-IP LINK'S CONFIGURATION IS ONE TO INSTALL. IPv4 only - an IPv6 context is unsupported,
/// not quietly half-configured. A unicast address with a prefix that leaves room for it; a gateway, when
/// there is one, on the same subnet and not the address itself; at most two unicast DNS servers; an MTU
/// between 576 and 4096.
pub fn validate(attachment: &Attachment) -> Result<(), Refusal> {
	if attachment.family == 6 {
		return Err(Refusal::Unsupported);
	}
	if attachment.family != 4 {
		return Err(Refusal::Invalid);
	}
	if !unicast(attachment.address) || attachment.prefix == 0 || attachment.prefix > 32 {
		return Err(Refusal::Invalid);
	}
	let address = u32::from_be_bytes(attachment.address);
	let network = mask(attachment.prefix);
	// A prefix shorter than /31 has a network and a broadcast address, and the host is neither.
	if attachment.prefix < 31 {
		let host = address & !network;
		if host == 0 || host == !network {
			return Err(Refusal::Invalid);
		}
	}
	if let Some(gateway) = attachment.gateway {
		let gateway_bits = u32::from_be_bytes(gateway);
		if !unicast(gateway) || gateway == attachment.address || gateway_bits & network != address & network {
			return Err(Refusal::Invalid);
		}
	}
	if attachment.dns.iter().flatten().any(|server| !unicast(*server)) {
		return Err(Refusal::Invalid);
	}
	if !(MIN_MTU..=MAX_MTU).contains(&attachment.mtu) {
		return Err(Refusal::Invalid);
	}
	Ok(())
}

#[cfg(test)]
mod tests;
