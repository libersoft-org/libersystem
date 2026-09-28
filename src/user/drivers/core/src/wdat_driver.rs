// driver.wdat - the ACPI Watchdog Action Table's watchdog, run as the register reads and writes the table lists.
//
// WHAT IT BINDS. The static table's platform row, `table:WDAT#0`: the port ranges the kernel minted from the table's
// I/O registers - after refusing every range in the reserved set or already claimed - the table's system-memory
// registers as DECLARED registers, and the table itself as the row's property block. It never runs AML and never
// reaches a register the kernel did not mint: an action naming one is dropped, and a table whose required actions
// cannot run is not a watchdog.
//
// WHAT IT DOES. At bind: the status read and cleared (the last reset was the watchdog's), `SET_REBOOT` run - which on
// a WDAT over q35's TCO is the action that clears No-Reboot - and the running state read. A running timer is taken
// over as every watchdog driver takes one over. Arming sets the countdown to the timeout divided by the table's
// period, starts the timer and pets it; a pet is `RESET`; a disarm is `SET_STOPPED` where the table has it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use drivers::common;
use drivers::watchdog::{self, Timer};
use drivers::wdat::{self, Bus, Reach, Table};
use proto::system::{Error, WatchdogDescription};
use rt::*;

// The registers the table reaches: the row's ports, mapped into this process, and its declared registers.
struct Registers {
	declared: u64,
}

impl Bus for Registers {
	fn read(&mut self, reach: Reach) -> Option<u32> {
		match reach {
			Reach::Port { port, width } => port_read(port, width),
			Reach::Declared { index, .. } => {
				let value = device_register_read(self.declared, index as u64);
				(value >= 0).then_some(value as u32)
			}
		}
	}

	fn write(&mut self, reach: Reach, value: u32) -> bool {
		match reach {
			Reach::Port { port, width } => port_write(port, width, value),
			Reach::Declared { index, .. } => device_register_write(self.declared, index as u64, value) >= 0,
		}
	}
}

#[cfg(target_arch = "x86_64")]
fn port_read(port: u16, width: u8) -> Option<u32> {
	Some(match width {
		1 => rt::port::inb(port) as u32,
		2 => rt::port::inw(port) as u32,
		_ => rt::port::inl(port),
	})
}

#[cfg(target_arch = "x86_64")]
fn port_write(port: u16, width: u8, value: u32) -> bool {
	match width {
		1 => rt::port::outb(port, value as u8),
		2 => rt::port::outw(port, value as u16),
		_ => rt::port::outl(port, value),
	}
	true
}

// No port space: a WDAT is an ACPI table, and only x86_64 boots with ACPI here - no row of the other ports names
// this driver, and a port register there is one it cannot reach.
#[cfg(not(target_arch = "x86_64"))]
fn port_read(_port: u16, _width: u8) -> Option<u32> {
	None
}

#[cfg(not(target_arch = "x86_64"))]
fn port_write(_port: u16, _width: u8, _value: u32) -> bool {
	false
}

struct Wdat {
	table: Table,
	bus: Registers,
	running_at_bind: bool,
	last_reset: bool,
}

impl Timer for Wdat {
	fn describe(&mut self) -> WatchdogDescription {
		let min = self.table.period_ms.saturating_mul(self.table.min_count.max(1));
		let max = self.table.period_ms.saturating_mul(self.table.max_count);
		// THE TIMER STOPS IN THE ACPI SLEEP STATES when the table says so - S3 and deeper - and never in suspend to
		// idle, where no firmware runs.
		let stops_in_s3 = self.table.flags & wdat::FLAG_STOPPED_IN_SLEEP != 0;
		WatchdogDescription { device: alloc::string::String::from("wdat"), min_timeout_ms: min, max_timeout_ms: max, granularity_ms: self.table.period_ms, can_disarm: self.table.has(wdat::SET_STOPPED), survives_reset: false, stops_in_suspend_to_idle: false, stops_in_s3, running_at_bind: self.running_at_bind, last_reset_was_watchdog: self.last_reset }
	}

	fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
		let Some(division) = watchdog::wdat(timeout_ms, self.table.period_ms, self.table.min_count, self.table.max_count) else { return Err(Error::Invalid) };
		if self.table.has(wdat::SET_REBOOT) && self.table.run(wdat::SET_REBOOT, 0, &mut self.bus).is_none() {
			return Err(Error::Io);
		}
		if self.table.has(wdat::QUERY_REBOOT) && self.table.run(wdat::QUERY_REBOOT, 0, &mut self.bus) != Some(1) {
			print(b"driver.wdat: the table's reboot setting does not hold - this watchdog cannot reset the machine\n");
			return Err(Error::Unsupported);
		}
		self.table.run(wdat::SET_COUNTDOWN, division.count, &mut self.bus).ok_or(Error::Io)?;
		self.table.run(wdat::SET_RUNNING, 0, &mut self.bus).ok_or(Error::Io)?;
		self.table.run(wdat::RESET, 0, &mut self.bus).ok_or(Error::Io)?;
		Ok(division.effective_ms)
	}

	fn pet(&mut self) -> Result<(), Error> {
		self.table.run(wdat::RESET, 0, &mut self.bus).map(|_| ()).ok_or(Error::Io)
	}

	fn disarm(&mut self) -> Result<(), Error> {
		if !self.table.has(wdat::SET_STOPPED) {
			return Err(Error::Unsupported);
		}
		self.table.run(wdat::SET_STOPPED, 0, &mut self.bus).map(|_| ()).ok_or(Error::Io)
	}
}

// The table, out of the row's property block: one VALUE record named `WDAT`.
fn table_bytes(handle: u64) -> Option<Vec<u8>> {
	let mut block = alloc::vec![0u8; abi::MAX_DEVICE_PROPERTIES];
	let len = device_properties(handle, &mut block);
	if len < 12 {
		return None;
	}
	let block = &block[..(len as usize).min(abi::MAX_DEVICE_PROPERTIES)];
	let name_len = u16::from_le_bytes([block[2], block[3]]) as usize;
	let value_len = u32::from_le_bytes([block[4], block[5], block[6], block[7]]) as usize;
	if block[0] != abi::DEVICE_PROPERTY_VALUE || name_len != 4 || &block[8..12] != b"WDAT" || 12 + value_len > block.len() {
		return None;
	}
	Some(block[12..12 + value_len].to_vec())
}

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, resources) = common::handshake(bootstrap);
	let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
	let refuse = |what: &[u8]| -> ! {
		print(b"driver.wdat: ");
		print(what);
		print(b"\n");
		common::failed(bootstrap, &bind, unusable)
	};
	if resources.registers == 0 && resources.port_range_count == 0 {
		refuse(b"it was handed neither the table's ports nor its registers");
	}
	// THE TABLE, through the declared registers - a WDAT row has no window.
	let Some(bytes) = table_bytes(resources.registers) else { refuse(b"its row carries no WDAT table") };
	let mut minted = wdat::Minted::default();
	for at in 0..resources.port_range_count {
		if port_range_map(resources.port_ranges[at]) < 0 {
			refuse(b"a port range of the table could not be mapped");
		}
		let port = bind.info.ports[at];
		minted.ports.push((port.base, port.len));
	}
	// THE MEMORY REGISTERS, in the kernel's declaration order - each distinct one at its first appearance. A table
	// naming one the kernel refused is one whose indices would not line up, and it is not run at all.
	minted.memory = wdat::memory_registers(&bytes);
	if !minted.memory.is_empty() && resources.registers == 0 {
		refuse(b"the kernel declared none of the table's memory registers");
	}
	let table = match Table::load(&bytes, &minted) {
		Ok(table) => table,
		Err(wdat::Refusal::NotEnabled) => refuse(b"the table says the watchdog is not enabled"),
		Err(wdat::Refusal::Missing(_)) => refuse(b"the table lacks an action every watchdog needs"),
		Err(wdat::Refusal::Unrunnable(_)) => refuse(b"a required action names a register outside what the kernel minted"),
		Err(wdat::Refusal::Malformed) => refuse(b"the table is malformed"),
	};
	let mut driver = Wdat { table, bus: Registers { declared: resources.registers }, running_at_bind: false, last_reset: false };
	// THE LAST RESET, read and cleared.
	if driver.table.has(wdat::QUERY_STATUS) {
		driver.last_reset = driver.table.run(wdat::QUERY_STATUS, 0, &mut driver.bus) == Some(1);
		if driver.last_reset && driver.table.has(wdat::SET_STATUS) {
			let _ = driver.table.run(wdat::SET_STATUS, 0, &mut driver.bus);
		}
	}
	// `SET_REBOOT` AT EVERY BIND: the reset is this contract's action.
	if driver.table.has(wdat::SET_REBOOT) {
		let _ = driver.table.run(wdat::SET_REBOOT, 0, &mut driver.bus);
	}
	driver.running_at_bind = driver.table.run(wdat::QUERY_RUNNING, 0, &mut driver.bus) == Some(1);
	let can_reset = !driver.table.has(wdat::QUERY_REBOOT) || driver.table.run(wdat::QUERY_REBOOT, 0, &mut driver.bus) == Some(1);
	let bridge = driver.running_at_bind && can_reset;
	if bridge {
		let _ = driver.pet();
	} else if driver.running_at_bind && driver.table.has(wdat::SET_STOPPED) {
		// A running count that cannot reset the machine records expiries that reset nothing: it is stopped.
		let _ = driver.table.run(wdat::SET_STOPPED, 0, &mut driver.bus);
	}
	let mut report = common::Bounded::<200>::new();
	report.push(b"driver.wdat: online - ");
	report.decimal(driver.table.period_ms as u64);
	report.push(b" ms per count, ");
	report.push(if driver.running_at_bind { b"running at bind" } else { b"stopped at bind" });
	if driver.last_reset {
		report.push(b", the last reset was its own");
	}
	if !driver.table.dropped.is_empty() {
		report.push(b", some actions not run");
	}
	watchdog::serve(bootstrap, &bind, report.as_bytes(), b"wdat", &mut driver, bridge, 0)
}
