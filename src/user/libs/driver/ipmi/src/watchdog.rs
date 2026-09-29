//! THE BMC WATCHDOG (IPMI 2.0, 27): one stage, timer use "SMS/OS", a hard reset at expiry, no pre-timeout, and "don't
//! stop" set - so a Set Watchdog Timer that re-arms it never stops it on the way. Reset Watchdog Timer starts it and pets
//! it. Set Watchdog Timer with "don't stop" CLEAR stops it, which is how it is disarmed.
//!
//! The countdown is in 100 ms steps: `MIN_TIMEOUT_MS` to `MAX_TIMEOUT_MS`. "The last reset was the watchdog's" is the
//! SMS/OS bit of Get Watchdog Timer's expiration flags, read at bind and then cleared.

use crate::Request;

pub const RESET_WATCHDOG_TIMER: u8 = 0x22;
pub const SET_WATCHDOG_TIMER: u8 = 0x24;
pub const GET_WATCHDOG_TIMER: u8 = 0x25;

/// Timer use 4: SMS/OS.
pub const USE_SMS_OS: u8 = 0x04;
/// Timer use bit 6: don't stop the timer on this Set.
pub const DONT_STOP: u8 = 0x40;
/// Timeout action 1: hard reset.
pub const HARD_RESET: u8 = 0x01;
/// The expiration flag of the SMS/OS use.
pub const EXPIRED_SMS_OS: u8 = 1 << 4;

pub const MIN_TIMEOUT_MS: u64 = 15_000;
pub const MAX_TIMEOUT_MS: u64 = 6_553_500;
/// Completion code 0x80 to Reset Watchdog Timer: the timer was never initialised.
pub const NOT_INITIALISED: u8 = 0x80;

/// The timeout an arm for `timeout_ms` takes effect with: rounded DOWN to 100 ms, at most `MAX_TIMEOUT_MS`, and none
/// below `MIN_TIMEOUT_MS` - the `watchdog` contract refuses a timeout under the minimum rather than raising it.
pub fn effective_ms(timeout_ms: u64) -> Option<u64> {
	(timeout_ms >= MIN_TIMEOUT_MS).then(|| timeout_ms.min(MAX_TIMEOUT_MS) / 100 * 100)
}

/// Arm (or re-arm) for `timeout_ms`, clearing the expiration flags in `clear`. None below the minimum.
pub fn arm(timeout_ms: u64, clear: u8) -> Option<Request> {
	let countdown = (effective_ms(timeout_ms)? / 100) as u16;
	let [low, high] = countdown.to_le_bytes();
	Some(Request::new(crate::netfn::APP, SET_WATCHDOG_TIMER, &[DONT_STOP | USE_SMS_OS, HARD_RESET, 0, clear, low, high]))
}

/// Stop it: "don't stop" clear, and the flags in `clear` cleared.
pub fn disarm(clear: u8) -> Request {
	Request::new(crate::netfn::APP, SET_WATCHDOG_TIMER, &[USE_SMS_OS, 0, 0, clear, 0xFF, 0xFF])
}

pub fn pet() -> Request {
	Request::new(crate::netfn::APP, RESET_WATCHDOG_TIMER, &[])
}

pub fn get() -> Request {
	Request::new(crate::netfn::APP, GET_WATCHDOG_TIMER, &[])
}

/// Get Watchdog Timer's answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
	pub timer_use: u8,
	pub running: bool,
	pub action: u8,
	pub expiration_flags: u8,
	pub initial_ms: u64,
	pub present_ms: u64,
}

/// RUNNING is the "started" bit WITH a present countdown above zero. QEMU's simulated BMC answers the bit as the last Set
/// left "don't stop", whatever the timer did since: after an expiry it still says started, with a present countdown of
/// zero - and a timer taken over as running would be started again by the take-over's pet.
pub fn state(data: &[u8]) -> Option<State> {
	if data.len() < 8 {
		return None;
	}
	let present_ms = u64::from(u16::from_le_bytes([data[6], data[7]])) * 100;
	Some(State { timer_use: data[0] & 0x07, running: data[0] & 0x40 != 0 && present_ms > 0, action: data[1] & 0x07, expiration_flags: data[3], initial_ms: u64::from(u16::from_le_bytes([data[4], data[5]])) * 100, present_ms })
}

impl State {
	/// Whether the last reset was this watchdog's.
	pub fn expired(&self) -> bool {
		self.expiration_flags & EXPIRED_SMS_OS != 0
	}
}
