//! INBOUND POLICY, PER RADIO, AND TRUST PER PEER AND PER PROFILE.
//!
//! BR/EDR is CONNECTABLE (page scan) while the radio is on and a bonded peer is trusted for an inbound profile, while a
//! `btctl receive` waits, or while pairable; DISCOVERABLE (inquiry scan) only under `btctl discoverable`, for at most
//! 180 seconds; PAIRABLE only while a prompt watcher is attached. On LE this host is always the central and never
//! advertises; bonded peripherals reconnect through the controller's filter accept list.
//!
//! TRUST IS PER PEER AND PER PROFILE, kept in the bond record and set by the operator: it admits an inbound connection
//! for a profile this system serves, and it is what an application grant for SPP or GATT is checked against. OPP HAS NO
//! STANDING TRUST: an inbound OPP connection is admitted only from the peer a waiting `btctl receive` names. ENFORCED
//! WHERE THE PROFILE IS FIRST NAMED: at L2CAP by PSM, and on RFCOMM by server channel. SDP answers any connected peer.

use crate::hci_bredr::Scan;
use crate::l2cap_bredr::psm;

#[cfg(test)]
mod tests;

/// How long `btctl discoverable` may keep the radio discoverable.
pub const DISCOVERABLE_MAX_MS: u64 = 180_000;

/// A profile trust is granted for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
	Input,
	Audio,
	Voice,
	Pan,
	Spp,
	Gatt,
}

impl Profile {
	pub const ALL: [Profile; 6] = [Profile::Input, Profile::Audio, Profile::Voice, Profile::Pan, Profile::Spp, Profile::Gatt];

	pub const fn bit(self) -> u8 {
		match self {
			Profile::Input => 1 << 0,
			Profile::Audio => 1 << 1,
			Profile::Voice => 1 << 2,
			Profile::Pan => 1 << 3,
			Profile::Spp => 1 << 4,
			Profile::Gatt => 1 << 5,
		}
	}

	pub const fn name(self) -> &'static str {
		match self {
			Profile::Input => "input",
			Profile::Audio => "audio",
			Profile::Voice => "voice",
			Profile::Pan => "pan",
			Profile::Spp => "spp",
			Profile::Gatt => "gatt",
		}
	}

	pub fn from_name(name: &str) -> Option<Profile> {
		Profile::ALL.into_iter().find(|profile| profile.name() == name)
	}
}

/// A peer's trust: one bit per profile.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Trust(pub u8);

impl Trust {
	pub const fn has(&self, profile: Profile) -> bool {
		self.0 & profile.bit() != 0
	}

	pub const fn with(self, profile: Profile, on: bool) -> Trust {
		if on { Trust(self.0 | profile.bit()) } else { Trust(self.0 & !profile.bit()) }
	}

	/// Whether any profile this system serves inbound is trusted: what keeps the radio connectable for this peer. SPP and
	/// GATT are client roles here and admit nothing inbound.
	pub const fn any_inbound(&self) -> bool {
		self.has(Profile::Input) || self.has(Profile::Audio) || self.has(Profile::Voice) || self.has(Profile::Pan)
	}
}

/// What an inbound L2CAP connection reaches, by its PSM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Inbound {
	/// SDP: any connected peer.
	Sdp,
	/// RFCOMM: its channels are checked one by one.
	Rfcomm,
	/// A profile's own channel.
	Profile(Profile),
	/// OBEX over L2CAP, on the PSM the OPP record names.
	Opp,
	/// Nothing here serves it.
	None,
}

/// The OBEX L2CAP PSM this system's OPP record names.
pub const OPP_PSM: u16 = 0x1001;

pub const fn inbound_by_psm(value: u16) -> Inbound {
	match value {
		psm::SDP => Inbound::Sdp,
		psm::RFCOMM => Inbound::Rfcomm,
		psm::HID_CONTROL | psm::HID_INTERRUPT => Inbound::Profile(Profile::Input),
		psm::AVDTP | psm::AVCTP | psm::AVCTP_BROWSING => Inbound::Profile(Profile::Audio),
		psm::BNEP => Inbound::Profile(Profile::Pan),
		OPP_PSM => Inbound::Opp,
		_ => Inbound::None,
	}
}

/// The RFCOMM server channels this system serves, and the profile each belongs to.
pub const HFP_CHANNEL: u8 = 1;
pub const HSP_CHANNEL: u8 = 2;
pub const OPP_CHANNEL: u8 = 9;

pub const fn inbound_by_channel(channel: u8) -> Inbound {
	match channel {
		HFP_CHANNEL | HSP_CHANNEL => Inbound::Profile(Profile::Voice),
		OPP_CHANNEL => Inbound::Opp,
		_ => Inbound::None,
	}
}

/// The decision for one inbound connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
	Accept,
	/// Nothing here serves it.
	NotServed,
	/// Not trusted, or not the peer a waiting receive names, or not encrypted for a profile that carries user data.
	Refused,
}

/// THE PEER'S STANDING for one decision: bonded and trusted, whether it is the peer a waiting `btctl receive` names, and
/// whether its link is encrypted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Peer {
	pub trust: Trust,
	pub receive_waits_for_it: bool,
	pub encrypted: bool,
}

/// ONE INBOUND DECISION. SDP needs nothing; every profile that carries user data needs encryption and the trust; OPP
/// needs the waiting receive; RFCOMM's session is admitted when the peer may open any channel served here.
pub fn decide(inbound: Inbound, peer: &Peer) -> Decision {
	match inbound {
		Inbound::None => Decision::NotServed,
		Inbound::Sdp => Decision::Accept,
		Inbound::Profile(profile) => {
			if peer.encrypted && peer.trust.has(profile) {
				Decision::Accept
			} else {
				Decision::Refused
			}
		}
		Inbound::Opp => {
			if peer.encrypted && peer.receive_waits_for_it {
				Decision::Accept
			} else {
				Decision::Refused
			}
		}
		Inbound::Rfcomm => {
			if peer.encrypted && (peer.trust.has(Profile::Voice) || peer.receive_waits_for_it) {
				Decision::Accept
			} else {
				Decision::Refused
			}
		}
	}
}

/// THE RADIO'S SCAN MODE right now: connectable when any bonded peer is trusted for an inbound profile, a receive waits,
/// or the controller is pairable; discoverable only while `btctl discoverable` lasts.
pub fn scan(radio_on: bool, any_trusted: bool, receive_waits: bool, pairable: bool, discoverable_until: Option<u64>, now_ms: u64) -> Scan {
	if !radio_on {
		return Scan::None;
	}
	let connectable = any_trusted || receive_waits || pairable;
	let discoverable = discoverable_until.is_some_and(|until| now_ms < until);
	match (connectable, discoverable) {
		(true, true) => Scan::Both,
		(true, false) => Scan::Page,
		// DISCOVERABLE IS ALWAYS CONNECTABLE TOO: a device found and then unreachable is no use.
		(false, true) => Scan::Both,
		(false, false) => Scan::None,
	}
}

/// When a `btctl discoverable` asked at `now_ms` for `seconds` ends: never past 180 seconds.
pub fn discoverable_until(now_ms: u64, seconds: u32) -> u64 {
	now_ms + (u64::from(seconds) * 1000).min(DISCOVERABLE_MAX_MS)
}
