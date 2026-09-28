// driver.tco - the ICH9 chipset's TCO watchdog, on x86_64 q35.
//
// WHAT IT BINDS. The ICH9 LPC bridge (8086:2918), claimed with DMA policy `none` and no decode change: its row carries
// EXACTLY the TCO block - PM base + 0x60..0x7F, the LPC/TCO derivation's port range, and nothing else of the PM
// block - and DECLARES one chipset register, GCS at the root-complex base + 0x3410, of which only No-Reboot (bit 5)
// is writable. On a machine with a WDAT the row is not applied and the claim is refused: the ACPI watchdog drives
// this timer there.
//
// WHAT IT DOES. The TCO counts 0.6 s ticks from its 10-bit count and resets the machine only at its SECOND expiry,
// and only while No-Reboot is clear - so a count of N is N * 1.2 s to the reset. It has no running bit: at bind a
// clear halt bit with No-Reboot clear counts as running and is taken over, and with No-Reboot set the halt bit is
// set, since a count nobody armed would only record expiries that reset nothing. Arming clears No-Reboot and READS
// IT BACK - a strap or a locked firmware setting keeps it set, and the arm is then `unsupported`.

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(target_arch = "x86_64")]
mod tco {
	use drivers::common;
	use drivers::watchdog::{self, Timer};
	use proto::system::{Error, WatchdogDescription};
	use rt::*;

	// The TCO block's registers, from its base.
	const TCO_RLD: u16 = 0x00;
	const TCO2_STS: u16 = 0x06;
	const TCO1_CNT: u16 = 0x08;
	const TCO_TMR: u16 = 0x12;
	const SECOND_TO_STS: u16 = 1 << 1;
	const TMR_HLT: u16 = 1 << 11;
	// GCS, the one declared register, and its No-Reboot bit.
	const GCS: u64 = 0;
	const NO_REBOOT: u32 = 1 << 5;

	struct Tco {
		base: u16,
		registers: u64,
		running_at_bind: bool,
		last_reset: bool,
	}

	impl Tco {
		fn inw(&self, offset: u16) -> u16 {
			rt::port::inw(self.base + offset)
		}

		fn outw(&self, offset: u16, value: u16) {
			rt::port::outw(self.base + offset, value);
		}

		fn no_reboot(&self) -> Option<bool> {
			let gcs = device_register_read(self.registers, GCS);
			(gcs >= 0).then_some(gcs as u32 & NO_REBOOT != 0)
		}

		fn halt(&self) -> bool {
			self.outw(TCO1_CNT, self.inw(TCO1_CNT) | TMR_HLT);
			self.inw(TCO1_CNT) & TMR_HLT != 0
		}
	}

	impl Timer for Tco {
		fn describe(&mut self) -> WatchdogDescription {
			let (min, max, granularity) = watchdog::tco_range();
			WatchdogDescription { device: alloc::string::String::from("tco"), min_timeout_ms: min, max_timeout_ms: max, granularity_ms: granularity, can_disarm: true, survives_reset: false, stops_in_suspend_to_idle: false, stops_in_s3: true, running_at_bind: self.running_at_bind, last_reset_was_watchdog: self.last_reset }
		}

		fn arm(&mut self, timeout_ms: u32) -> Result<u32, Error> {
			let Some(division) = watchdog::tco(timeout_ms) else { return Err(Error::Invalid) };
			// NO-REBOOT CLEARED AND READ BACK before the count is written: a timer that cannot reset the machine is not
			// armed as though it could.
			if device_register_write(self.registers, GCS, 0) < 0 || self.no_reboot() != Some(false) {
				print(b"driver.tco: No-Reboot stays set - this TCO cannot reset the machine\n");
				return Err(Error::Unsupported);
			}
			self.outw(TCO_TMR, (self.inw(TCO_TMR) & !0x3FF) | division.count as u16);
			self.outw(TCO_RLD, 1);
			self.outw(TCO1_CNT, self.inw(TCO1_CNT) & !TMR_HLT);
			Ok(division.effective_ms)
		}

		fn pet(&mut self) -> Result<(), Error> {
			self.outw(TCO_RLD, 1);
			Ok(())
		}

		// DISARM: the halt bit set and read back - `unsupported` if it did not stay set.
		fn disarm(&mut self) -> Result<(), Error> {
			if self.halt() { Ok(()) } else { Err(Error::Unsupported) }
		}
	}

	#[unsafe(no_mangle)]
	pub extern "C" fn __user_main(bootstrap: u64) -> ! {
		let (bind, resources) = common::handshake(bootstrap);
		let unusable = driver_protocol::DriverFailureCode::ResourceUnusable;
		let block = (0..bind.info.port_count as usize).map(|at| bind.info.ports[at]).find(|port| port.source == abi::PORT_SOURCE_DERIVED && port.len == 32);
		let (Some(block), true) = (block, resources.registers != 0 && resources.port_range_count != 0) else {
			print(b"driver.tco: it was not handed the TCO block and GCS\n");
			common::failed(bootstrap, &bind, unusable)
		};
		let at = (0..bind.info.port_count as usize).position(|at| bind.info.ports[at] == block).unwrap_or(0);
		if at >= resources.port_range_count || port_range_map(resources.port_ranges[at]) < 0 {
			print(b"driver.tco: the TCO block could not be mapped\n");
			common::failed(bootstrap, &bind, unusable);
		}
		let mut tco = Tco { base: block.base, registers: resources.registers, running_at_bind: false, last_reset: false };
		// THE LAST RESET: the second-timeout status, which survives the reset it caused - read and cleared.
		tco.last_reset = tco.inw(TCO2_STS) & SECOND_TO_STS != 0;
		if tco.last_reset {
			tco.outw(TCO2_STS, SECOND_TO_STS);
		}
		let Some(no_reboot) = tco.no_reboot() else {
			print(b"driver.tco: GCS could not be read\n");
			common::failed(bootstrap, &bind, unusable)
		};
		let halted = tco.inw(TCO1_CNT) & TMR_HLT != 0;
		tco.running_at_bind = !halted && !no_reboot;
		// A COUNT THAT CANNOT RESET THE MACHINE IS HALTED rather than left to record expiries; one that can is taken over.
		if !halted && no_reboot {
			tco.halt();
		}
		let bridge = tco.running_at_bind;
		if bridge {
			let _ = tco.pet();
		}
		let mut report = common::Bounded::<160>::new();
		report.push(b"driver.tco: online - ");
		report.push(if tco.running_at_bind { b"running at bind" } else { b"stopped at bind" });
		report.push(if no_reboot { b", No-Reboot set" } else { b", No-Reboot clear" });
		if tco.last_reset {
			report.push(b", the last reset was its own");
		}
		watchdog::serve(bootstrap, &bind, report.as_bytes(), b"tco", &mut tco, bridge, 0)
	}
}

// THE TCO IS AN x86 CHIPSET'S: the other ports have no port space and no ICH9, and no row of theirs matches this
// driver.
#[cfg(not(target_arch = "x86_64"))]
#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let (bind, _resources) = drivers::common::handshake(bootstrap);
	rt::print(b"driver.tco: this machine has no port space - the ICH9 TCO is x86's\n");
	drivers::common::failed(bootstrap, &bind, driver_protocol::DriverFailureCode::UnsupportedDevice)
}
