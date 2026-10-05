//! THE PAIRING INTERACTION AND THE SECURITY LEVELS, ONE RULE FOR BOTH RADIOS.
//!
//! WHO ANSWERS. Every model that needs a person - Numeric Comparison, Passkey Entry, legacy PIN entry, and consent to
//! an incoming pairing - is a PROMPT the service originates on the operator authority, answered by a prompt watcher (a
//! running `btctl pair` or `btctl pairable`, one per controller). The deadline is the protocol's: 25 seconds, five
//! inside the 30-second Security Manager and LMP response timers, after which the service answers no itself.
//!
//! WHAT THIS HOST DECLARES depends on whether a watcher is attached: KeyboardDisplay on LE and DisplayYesNo on BR/EDR -
//! whose IO capabilities have no KeyboardDisplay, so on BR/EDR this host shows the passkey a keyboard types and never
//! types one itself - and with no watcher NoInputNoOutput on both, so no model is offered that nobody can answer.
//!
//! THE LEVELS are compared on two axes - the key agreement and the authentication - and a bond is NEVER DOWNGRADED on
//! either: a bonded peer that pairs again lower on either axis is refused and keeps its bond.

use crate::hci_bredr::{IoCapability, KeyType};

#[cfg(test)]
mod tests;

/// How long a prompt waits for its answer, in milliseconds.
pub const PROMPT_DEADLINE_MS: u64 = 25_000;

/// The radio a pairing runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Radio {
	Classic,
	Le,
}

/// The IO capability this host declares, as each radio's protocol encodes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Declared {
	/// BR/EDR: DisplayYesNo with a watcher, NoInputNoOutput without.
	Classic(IoCapability),
	/// LE: KeyboardDisplay (`0x04`) with a watcher, NoInputNoOutput (`0x03`) without.
	Le(u8),
}

pub const LE_KEYBOARD_DISPLAY: u8 = 0x04;
pub const LE_NO_INPUT_NO_OUTPUT: u8 = 0x03;

pub const fn declared(radio: Radio, watcher: bool) -> Declared {
	match (radio, watcher) {
		(Radio::Classic, true) => Declared::Classic(IoCapability::DisplayYesNo),
		(Radio::Classic, false) => Declared::Classic(IoCapability::NoInputNoOutput),
		(Radio::Le, true) => Declared::Le(LE_KEYBOARD_DISPLAY),
		(Radio::Le, false) => Declared::Le(LE_NO_INPUT_NO_OUTPUT),
	}
}

/// THE KEY AGREEMENT a bond was made with, lowest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Agreement {
	/// BR/EDR legacy pairing with a PIN.
	Legacy,
	/// LE legacy pairing.
	LeLegacy,
	/// BR/EDR Secure Simple Pairing on P-192.
	P192,
	/// P-256 Secure Connections, either radio.
	SecureConnections,
}

/// A bond's level: its key agreement and whether a person authenticated it (Numeric Comparison or Passkey Entry, or a
/// PIN) rather than Just Works.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Level {
	pub agreement: Agreement,
	pub authenticated: bool,
}

impl Level {
	/// The level a BR/EDR link key's type states.
	pub const fn of_link_key(kind: KeyType) -> Option<Level> {
		match kind {
			KeyType::Combination | KeyType::ChangedCombination => Some(Level { agreement: Agreement::Legacy, authenticated: true }),
			KeyType::UnauthenticatedP192 => Some(Level { agreement: Agreement::P192, authenticated: false }),
			KeyType::AuthenticatedP192 => Some(Level { agreement: Agreement::P192, authenticated: true }),
			KeyType::UnauthenticatedP256 => Some(Level { agreement: Agreement::SecureConnections, authenticated: false }),
			KeyType::AuthenticatedP256 => Some(Level { agreement: Agreement::SecureConnections, authenticated: true }),
			// A DEBUG KEY IS NO KEY: its private half is published in the specification.
			KeyType::DebugCombination | KeyType::Other(_) => None,
		}
	}

	/// Whether a new pairing at `self` may replace a bond at `held`: neither axis lower.
	pub fn may_replace(&self, held: &Level) -> bool {
		self.agreement >= held.agreement && (self.authenticated || !held.authenticated)
	}

	/// Whether a cross-transport key may be derived from a key at this level: Secure Connections only, at its own level.
	pub const fn derives_across(&self) -> bool {
		matches!(self.agreement, Agreement::SecureConnections)
	}
}

/// What a person is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Question {
	/// Do the six digits on both screens match?
	Compare(u32),
	/// Type these six digits on the peer.
	ShowPasskey(u32),
	/// Type the six digits the peer shows.
	EnterPasskey,
	/// Type the peer's PIN (legacy pairing, only when the operator asked for it).
	EnterPin,
	/// May this peer pair? - an incoming Just Works pairing.
	Consent,
}

/// What a person answered.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
	Yes,
	No,
	Passkey(u32),
	Pin(alloc::vec::Vec<u8>),
}

/// ONE PROMPT IN FLIGHT: the question, when it was asked, and keypress notifications as they arrive.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prompt {
	pub question: Question,
	pub asked_ms: u64,
	pub keypresses: u8,
}

impl Prompt {
	pub const fn new(question: Question, now_ms: u64) -> Prompt {
		Prompt { question, asked_ms: now_ms, keypresses: 0 }
	}

	pub const fn deadline(&self) -> u64 {
		self.asked_ms + PROMPT_DEADLINE_MS
	}

	pub const fn expired(&self, now_ms: u64) -> bool {
		now_ms >= self.deadline()
	}

	/// Whether `reply` answers this question at all: a passkey to a passkey entry, a PIN to a PIN entry, yes or no to
	/// anything else - and no to everything.
	pub fn fits(&self, reply: &Reply) -> bool {
		match (self.question, reply) {
			(_, Reply::No) => true,
			(Question::EnterPasskey, Reply::Passkey(value)) => *value <= 999_999,
			(Question::EnterPin, Reply::Pin(pin)) => (1..=16).contains(&pin.len()),
			(Question::Compare(_) | Question::ShowPasskey(_) | Question::Consent, Reply::Yes) => true,
			_ => false,
		}
	}
}

/// WHAT A BR/EDR USER CONFIRMATION REQUEST IS, from both sides' IO capabilities and authentication requirements: Numeric
/// Comparison when both can show and confirm, Just Works otherwise - which an incoming pairing turns into a consent and
/// an outgoing one this host started accepts without asking.
pub fn classic_confirmation(ours: IoCapability, theirs: IoCapability, value: u32, outgoing: bool) -> Option<Question> {
	let both_display_yes_no = ours == IoCapability::DisplayYesNo && theirs == IoCapability::DisplayYesNo;
	if both_display_yes_no {
		return Some(Question::Compare(value % 1_000_000));
	}
	if outgoing { None } else { Some(Question::Consent) }
}

/// Whether an incoming pairing may begin at all: only while a watcher is attached (the controller is pairable).
pub const fn pairable(watcher: bool) -> bool {
	watcher
}

/// Legacy PIN pairing: only for a device the operator named for it - one that cannot do better - and never for a peer
/// already bonded higher.
pub fn legacy_allowed(operator_asked: bool, held: Option<&Level>) -> bool {
	operator_asked && held.is_none_or(|held| Level { agreement: Agreement::Legacy, authenticated: true }.may_replace(held))
}
