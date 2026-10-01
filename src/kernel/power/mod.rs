// THE FORCED POWER-OFF DEADLINE - `SYS_SYSTEM_POWER`'s `POWER_OFF_WITHIN`, which a thermal policy at `_CRT` and the
// power-state service at a critical battery arm before they ask for the orderly power-off: whatever the processes that
// sequence stops are doing - hung, spinning, or ServiceManager and SystemManager gone - the machine is off by then.
//
// THE KERNEL KEEPS THE EARLIEST ONE ARMED, never moves it later and never cancels it: arming twice is harmless, and a
// policy that comes back inside the bound finds it still armed. IT IS CHECKED IN THE TIMER INTERRUPT ITSELF - the
// periodic tick every busy core keeps, and the idle boot core's one-shot, which includes it - not on the deadline path,
// which the boot core reaches only when its run queue empties: a thread that keeps it busy cannot postpone it. While one
// is armed every reset the kernel would make powers the machine off instead, and the sleep entry is refused, so a
// sleep's held clock cannot postpone it either.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch;

// The deadline, as an absolute tick; zero for none.
static DEADLINE: AtomicU64 = AtomicU64::new(0);

// ARMED `seconds` from now, or left where it is when one already armed is earlier. Answers the deadline in force.
pub fn arm_within(seconds: u64) -> u64 {
	let at = arch::apic::ticks().saturating_add(seconds.saturating_mul(abi::TICKS_PER_SECOND)).max(1);
	let mut current = DEADLINE.load(Ordering::Acquire);
	loop {
		if current != 0 && current <= at {
			return current;
		}
		match DEADLINE.compare_exchange(current, at, Ordering::AcqRel, Ordering::Acquire) {
			Ok(_) => {
				crate::serial_println!("power: a forced power-off deadline is armed for tick {at} ({seconds} s from now)");
				return at;
			}
			Err(seen) => current = seen,
		}
	}
}

pub fn deadline() -> Option<u64> {
	let at = DEADLINE.load(Ordering::Acquire);
	(at != 0).then_some(at)
}

pub fn armed() -> bool {
	deadline().is_some()
}

// THE CHECK, from the timer interrupt: past the deadline, the terminal power-off, which waits for no thread or process.
pub fn check(now: u64) {
	if let Some(at) = deadline()
		&& now >= at
	{
		fire();
	}
}

// A RESET THE KERNEL WOULD MAKE: the machine powered off instead while a deadline is armed - a reboot would be the
// deadline cancelled.
pub fn reset() -> ! {
	if armed() {
		crate::serial_println!("power: a reset was asked for with a forced power-off deadline armed - powering off instead");
		arch::poweroff();
	}
	arch::reset()
}

#[cfg(not(test))]
fn fire() {
	crate::serial_println!("power: the forced power-off deadline passed - powering off");
	arch::poweroff();
}

// THE SUITE RECORDS THE POWER-OFF INSTEAD OF PERFORMING IT, and takes the deadline down so the next test starts clean.
#[cfg(test)]
static FIRED: core::sync::atomic::AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn fire() {
	if let Some(at) = deadline() {
		FIRED.store(at, Ordering::Release);
	}
	DEADLINE.store(0, Ordering::Release);
}

#[cfg(test)]
mod tests;
