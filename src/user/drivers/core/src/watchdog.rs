// THE WATCHDOG DRIVERS' ARITHMETIC: one timeout - the time from the last pet to the reset - divided across each
// device's stages and units, rounded DOWN to what the device can count and refused below its minimum.
//
// Every driver answers the effective timeout its division gives, which is what its consumer schedules against.

#[cfg(test)]
mod tests;

// What a division gave: the effective timeout, and the count the device is programmed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Division {
	pub effective_ms: u32,
	pub count: u32,
}

// THE i6300ESB: two stages, each a 20-bit preload of units of 32768 PCI clocks of 30 ns (the 1 kHz scale) -
// 983,040 ns - and the reset at the end of the second. The timeout is split into two equal preloads.
pub const ESB_UNIT_NS: u64 = 32_768 * 30;
pub const ESB_PRELOAD_MAX: u32 = (1 << 20) - 1;

pub fn i6300esb(timeout_ms: u32) -> Option<Division> {
	let stage_ns = timeout_ms as u64 * 1_000_000 / 2;
	let preload = (stage_ns / ESB_UNIT_NS).min(ESB_PRELOAD_MAX as u64) as u32;
	if preload == 0 {
		return None;
	}
	Some(Division { effective_ms: (2 * preload as u64 * ESB_UNIT_NS / 1_000_000) as u32, count: preload })
}

// The i6300esb's range as the contract states it: the shortest two stages that count at all, the longest, and the
// granularity - two units.
pub fn i6300esb_range() -> (u32, u32, u32) {
	let granularity = (2 * ESB_UNIT_NS).div_ceil(1_000_000) as u32;
	(granularity, (2 * ESB_PRELOAD_MAX as u64 * ESB_UNIT_NS / 1_000_000) as u32, granularity)
}

// THE ICH9 TCO: a 0.6 s tick and a 10-bit count, and the reset only at the SECOND expiry - both expiries run the
// same count - so a count of N is N * 1.2 s to the reset: 2 to 1023, 2.4 s to 1227.6 s.
pub const TCO_TICK_PAIR_MS: u32 = 1_200;
pub const TCO_COUNT_MIN: u32 = 2;
pub const TCO_COUNT_MAX: u32 = 1_023;

pub fn tco(timeout_ms: u32) -> Option<Division> {
	let count = (timeout_ms / TCO_TICK_PAIR_MS).min(TCO_COUNT_MAX);
	if count < TCO_COUNT_MIN {
		return None;
	}
	Some(Division { effective_ms: count * TCO_TICK_PAIR_MS, count })
}

pub fn tco_range() -> (u32, u32, u32) {
	(TCO_COUNT_MIN * TCO_TICK_PAIR_MS, TCO_COUNT_MAX * TCO_TICK_PAIR_MS, TCO_TICK_PAIR_MS)
}

// A WDAT: the table's timer period times its count, within the table's count range - the table describes the time
// to its action, so a WDAT over the TCO states 1200 ms per count.
pub fn wdat(timeout_ms: u32, period_ms: u32, min_count: u32, max_count: u32) -> Option<Division> {
	if period_ms == 0 || min_count > max_count {
		return None;
	}
	let count = (timeout_ms / period_ms).min(max_count);
	if count < min_count.max(1) {
		return None;
	}
	Some(Division { effective_ms: count.checked_mul(period_ms)?, count })
}

// A BMC's watchdog: one countdown in 100 ms units, sixteen bits of them.
pub const BMC_UNIT_MS: u32 = 100;

pub fn bmc(timeout_ms: u32) -> Option<Division> {
	let count = (timeout_ms / BMC_UNIT_MS).min(u16::MAX as u32);
	if count == 0 {
		return None;
	}
	Some(Division { effective_ms: count * BMC_UNIT_MS, count })
}

// ------------------------------------------------------------------ the provider every watchdog driver serves

use crate::common;
use proto::system::{Error, WatchdogDescription, watchdog};
use rt::*;

// THE BRIDGE: a timer found running at bind that CAN reset the machine is petted at once and kept fed by its driver
// until its consumer's first pet (an arm is one) or disarm - for at most this long, 120 s, the boot bound, which a
// boot must beat from the driver's bind to its consumer's first pet. After it the driver stops feeding, so a boot
// that never brings the watchdog service up still resets the machine.
pub const BRIDGE_TICKS: u64 = 120 * rt::TICKS_PER_SECOND;
// How often the bridge pets: once a second, far inside any timeout a device takes.
pub const BRIDGE_PET_TICKS: u64 = rt::TICKS_PER_SECOND;

// ONE WATCHDOG, as its driver drives it.
pub trait Timer {
	fn describe(&mut self) -> WatchdogDescription;
	// Arm with a timeout, answering the effective one; the count starts at the arm.
	fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error>;
	fn pet(&mut self) -> Result<(), Error>;
	// `unsupported` where the device cannot stop.
	fn disarm(&mut self) -> Result<(), Error>;
}

struct Provider<'a, T: Timer> {
	timer: &'a mut T,
	// The consumer has petted, armed or disarmed: the bridge is over.
	consumer_acted: bool,
}

impl<T: Timer> watchdog::Service for Provider<'_, T> {
	fn describe(&mut self) -> Result<WatchdogDescription, Error> {
		Ok(self.timer.describe())
	}

	// AN ARM OR A DISARM THAT DID NOT HAPPEN ENDS NOTHING: the timer is as it was, and a bridged one stays bridged.
	fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
		let armed = self.timer.arm(timeout_ms);
		self.consumer_acted |= armed.is_ok();
		armed
	}

	fn pet(&mut self) -> Result<(), Error> {
		self.consumer_acted = true;
		self.timer.pet()
	}

	fn disarm(&mut self) -> Result<(), Error> {
		let disarmed = self.timer.disarm();
		self.consumer_acted |= disarmed.is_ok();
		disarmed
	}
}

// PUBLISH THE ONE `watchdog` PROVIDER under the device's name and serve it for the life of the binding. `bridge` says
// the timer was found running and able to reset the machine: the driver feeds it until its consumer acts, at most
// `BRIDGE_TICKS`. A planned stop writes nothing to the timer - the release writes nothing either - so a watchdog
// armed across a stop is still armed; the next binding takes it over. `device` is the capability `finish_stop`
// hands back (0 for none).
pub fn serve<T: Timer>(bootstrap: u64, bind: &common::Bind, report: &[u8], name: &[u8], timer: &mut T, bridge: bool, device: u64) -> ! {
	let Some((mine, far)) = channel() else { common::failed(bootstrap, bind, driver_protocol::DriverFailureCode::OutOfMemory) };
	if !common::online_named(bootstrap, bind, report, &[(driver_protocol::provider::WATCHDOG, far, name)]) {
		exit();
	}
	let mut serving = common::Serving::from_offers(&[(0, mine)]);
	let mut provider = Provider { timer, consumer_acted: false };
	// THE BRIDGE'S CLOCK: a timer object woken once a second while it lasts.
	let bridge_until = if bridge { clock() + BRIDGE_TICKS } else { 0 };
	let alarm = timer_create();
	let alarm = if alarm > 0 { alarm as u64 } else { 0 };
	let mut next_pet = clock();
	let mut buf = alloc::vec![0u8; 512];
	let mut reply = alloc::vec![0u8; 512];
	loop {
		let bridging = bridge && !provider.consumer_acted && clock() < bridge_until;
		if bridging && clock() >= next_pet {
			let _ = provider.timer.pet();
			next_pet = clock() + BRIDGE_PET_TICKS;
		}
		if bridging && alarm != 0 {
			timer_set(alarm, next_pet.min(bridge_until));
		}
		let devices: &[u64] = if bridging && alarm != 0 { core::slice::from_ref(&alarm) } else { &[] };
		match common::wait_providers(bootstrap, bind, &mut serving, devices, true) {
			None => {
				if common::stop_requested() {
					common::finish_stop(bootstrap, bind, device, true);
				}
				exit();
			}
			Some(common::ProviderReady::Connected(_)) | Some(common::ProviderReady::Device(_)) => {}
			Some(common::ProviderReady::Consumer(index)) => {
				let Some((len, handle)) = common::recv_from_consumer(bootstrap, bind, &mut serving, index, &mut buf) else { continue };
				if handle != 0 {
					close(handle);
				}
				let mut handles = wire::Handles::new();
				let mut reply_handles = wire::Handles::new();
				match watchdog::dispatch(&mut provider, &buf[..len], &mut handles, &mut reply, &mut reply_handles) {
					Some(written) => {
						send_blocking(serving.at(index), &reply[..written], 0);
					}
					// A request the contract does not carry ends that connection - and not the timer.
					None => {
						let token = serving.close_at(index);
						if !common::disconnected(bootstrap, bind, token) {
							exit();
						}
					}
				}
			}
		}
	}
}
