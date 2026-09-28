// driver.i6300esb - the Intel 6300ESB watchdog, a PCI function on all three targets.
//
// WHAT IT BINDS. A plain PCI function (8086:25ab) whose kernel row resolves BAR 0 - the two stage preloads and the
// reload register, behind an unlock sequence - and DECLARES its two arming registers in configuration space: 0x60,
// answered only as a WORD (reboot enable, clock scale, stage-one interrupt type), and 0x68, answered only as a BYTE
// (enable, lock, free-run). A dword access to either falls through and does nothing, which is why they are the
// kernel's to reach at their exact widths.
//
// WHAT IT DOES. It runs two stages of the timeout each and resets the machine at the end of the second; the
// stage-one interrupt is left off. At bind it reads and clears the last-reset record (BAR 0 + 0xC, bit 9) and
// whether the timer runs and is locked; a running timer that can reset the machine is taken over - petted at once
// and fed until its consumer acts - and one that cannot is left alone. It never sets the lock.

#![no_std]
#![no_main]

extern crate alloc;

use drivers::common;
use drivers::watchdog::{self, Timer};
use proto::system::{Error, WatchdogDescription};
use rt::*;

// Configuration registers, by their index in the row's declaration.
const CONFIG: u64 = 0;
const LOCK: u64 = 1;
// 0x60's bits: reboot DISABLED when set, the 1 MHz clock when set, the stage-one interrupt type.
const CONFIG_REBOOT_DISABLED: u32 = 1 << 5;
const CONFIG_INT_DISABLED: u32 = 0x3;
// 0x68's bits.
const LOCK_ENABLE: u32 = 1 << 1;
const LOCK_LOCKED: u32 = 1 << 0;
// BAR 0's registers.
const TIMER1: u64 = 0x00;
const TIMER2: u64 = 0x04;
const RELOAD: u64 = 0x0C;
const UNLOCK1: u16 = 0x80;
const UNLOCK2: u16 = 0x86;
const RELOAD_BIT: u16 = 1 << 8;
const TIMEOUT_BIT: u16 = 1 << 9;

struct Esb {
	registers: u64,
	window: u64,
	running_at_bind: bool,
	last_reset: bool,
}

impl Esb {
	fn read16(&self, offset: u64) -> u16 {
		// SAFETY: BAR 0 of this function, mapped by this process; the offset is one of its registers.
		unsafe { core::ptr::read_volatile((self.window + offset) as *const u16) }
	}

	fn write16(&self, offset: u64, value: u16) {
		// SAFETY: as `read16`.
		unsafe { core::ptr::write_volatile((self.window + offset) as *mut u16, value) }
	}

	fn write32(&self, offset: u64, value: u32) {
		// SAFETY: as `read16`.
		unsafe { core::ptr::write_volatile((self.window + offset) as *mut u32, value) }
	}

	// The unlock sequence, before every write behind it.
	fn unlock(&self) {
		self.write16(RELOAD, UNLOCK1);
		self.write16(RELOAD, UNLOCK2);
	}

	fn lock_register(&self) -> Option<u32> {
		let value = device_register_read(self.registers, LOCK);
		(value >= 0).then_some(value as u32)
	}
}

impl Timer for Esb {
	fn describe(&mut self) -> WatchdogDescription {
		let (min, max, granularity) = watchdog::i6300esb_range();
		let locked = self.lock_register().is_some_and(|lock| lock & LOCK_LOCKED != 0);
		WatchdogDescription { device: alloc::string::String::from("i6300esb"), min_timeout_ms: min, max_timeout_ms: max, granularity_ms: granularity, can_disarm: !locked, survives_reset: false, stops_in_suspend_to_idle: false, stops_in_s3: true, running_at_bind: self.running_at_bind, last_reset_was_watchdog: self.last_reset }
	}

	// ARMING: 0x60 (reboot enabled, 1 kHz scale, the stage-one interrupt off), both preloads through the unlock
	// sequence, then 0x68's enable - which starts the count.
	fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
		let Some(division) = watchdog::i6300esb(timeout_ms) else { return Err(Error::Invalid) };
		if device_register_write(self.registers, CONFIG, CONFIG_INT_DISABLED) < 0 {
			return Err(Error::Io);
		}
		self.unlock();
		self.write32(TIMER1, division.count);
		self.unlock();
		self.write32(TIMER2, division.count);
		let lock = self.lock_register().ok_or(Error::Io)?;
		if lock & LOCK_LOCKED != 0 {
			// A LOCKED TIMER KEEPS WHAT IT WAS ARMED WITH: the new preloads take effect at its next reload.
			self.pet()?;
			return Ok(division.effective_ms);
		}
		if device_register_write(self.registers, LOCK, LOCK_ENABLE) < 0 {
			return Err(Error::Io);
		}
		self.pet()?;
		Ok(division.effective_ms)
	}

	fn pet(&mut self) -> Result<(), Error> {
		self.unlock();
		self.write16(RELOAD, RELOAD_BIT);
		Ok(())
	}

	// DISARM: 0x68's enable cleared - unless the lock bit is set, which this driver never sets and which makes the
	// timer one that cannot be stopped.
	fn disarm(&mut self) -> Result<(), Error> {
		let lock = self.lock_register().ok_or(Error::Io)?;
		if lock & LOCK_LOCKED != 0 {
			return Err(Error::Unsupported);
		}
		if device_register_write(self.registers, LOCK, 0) < 0 {
			return Err(Error::Io);
		}
		Ok(())
	}
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
	if resources.device == 0 || resources.registers == 0 {
		print(b"driver.i6300esb: it was not handed its BAR 0 window and its declared registers\n");
		common::failed(bootstrap, &bind, unusable);
	}
	let mapped: u64 = unsafe { syscall(SYS_DEVICE_MEMORY_MAP, resources.device, 0, 0, 0) };
	if sys_is_err(mapped) {
		print(b"driver.i6300esb: its BAR 0 window could not be mapped\n");
		common::failed(bootstrap, &bind, unusable);
	}
	// THE WINDOW AS MAPPED: the kernel maps from the page and answers the BAR's own address within it.
	let window = mapped;
	let mut esb = Esb { registers: resources.registers, window, running_at_bind: false, last_reset: false };
	// THE LAST RESET, read and cleared: bit 9 of the reload register survives the reset it caused.
	esb.last_reset = esb.read16(RELOAD) & TIMEOUT_BIT != 0;
	if esb.last_reset {
		esb.unlock();
		esb.write16(RELOAD, TIMEOUT_BIT);
	}
	let Some(lock) = esb.lock_register() else {
		print(b"driver.i6300esb: its lock register could not be read\n");
		common::failed(bootstrap, &bind, unusable)
	};
	let config = device_register_read(esb.registers, CONFIG);
	let can_reset = config >= 0 && config as u32 & CONFIG_REBOOT_DISABLED == 0;
	esb.running_at_bind = lock & LOCK_ENABLE != 0;
	// TAKEOVER AT BIND, NOT A RACE: a running timer that can reset the machine is petted at once and kept fed.
	let bridge = esb.running_at_bind && can_reset;
	if bridge {
		let _ = esb.pet();
	}
	let mut report = common::Bounded::<160>::new();
	report.push(b"driver.i6300esb: online - ");
	report.push(if esb.running_at_bind { b"running at bind" } else { b"stopped at bind" });
	if lock & LOCK_LOCKED != 0 {
		report.push(b", locked");
	}
	if esb.last_reset {
		report.push(b", the last reset was its own");
	}
	let device = resources.device;
	watchdog::serve(bootstrap, &bind, report.as_bytes(), b"i6300esb", &mut esb, bridge, device)
}
