// THE aarch64 HALF OF PROCESSOR POWER: what the core offers an idle state, and PSCI's CPU_SUSPEND - the entry the device
// tree's `idle-states` name. The generic timer's counter is the clock and runs on in every state; the core's own timer
// runs on in every state its tree does not mark `local-timer-stop`, which is that state's own flag here. No state that
// loses the core's context is entered: this port has no per-core resume path.
//
// NO PROCESSOR REGISTER IS MAPPED. A register-entered idle state or a performance table comes from ACPI's processor
// objects, which this port does not read (it boots from the device tree), so a table naming a register is refused at
// install - said in `map_register`'s answer - and the port space does not exist here at all.

use super::psci;

pub const ARCH: procpower::Arch = procpower::Arch::Aarch64;

pub fn cpu() -> procpower::Cpu {
	procpower::Cpu { mwait: false, invariant_counter: true, timer_always_running: false, context_resume: false }
}

pub fn map_register(_phys: u64) -> Result<u64, &'static str> {
	Err("this port maps no processor register - a table naming one comes from ACPI, which it does not read")
}

pub fn unmap_register(_phys: u64) {}

// NO PORT SPACE ON THIS ARCHITECTURE: a table naming a port is refused at install (`procpower::check_register`), so the
// idle path and the governor never reach these.
pub fn port_read(_port: u16, _bytes: u8) -> u64 {
	0
}

pub fn port_write(_port: u16, _bytes: u8, _value: u64) {}

pub fn offers_mwait() -> bool {
	false
}

// MWAIT and a register's entry are refused at install here; were either reached, the halt stands in for it.
pub fn mwait(_hint: u32) {
	super::idle_halt();
}

pub fn after_register_entry() {
	super::idle_halt();
}

// WHETHER A TREE STATE'S PARAMETER LOSES THE CORE'S CONTEXT: PSCI's StateType bit, in the format this firmware takes.
pub fn state_loses_context(parameter: u32) -> bool {
	super::psci::state_loses_context(parameter)
}

// HOW MANY ENTRIES THE FIRMWARE REFUSED - each one the halt instead - for the suite, which checks a state was entered
// through the firmware and not merely counted.
static REFUSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
pub fn firmware_refusals() -> u64 {
	REFUSED.load(core::sync::atomic::Ordering::Relaxed)
}

// A TREE'S STATE, ENTERED with interrupts masked: CPU_SUSPEND returns once an interrupt is pending - masked or not, as WFI
// does - and the unmask after takes it. A firmware that refuses the call leaves the core in the halt instead, said once.
pub fn firmware_suspend(parameter: u32) {
	let status = psci::cpu_suspend(parameter);
	if status != 0 {
		REFUSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
		static SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
		if !SAID.swap(true, core::sync::atomic::Ordering::Relaxed) {
			crate::serial_println!("processor: PSCI refused CPU_SUSPEND({parameter:#x}) with {status} - the halt waits instead");
		}
		super::idle_halt();
		return;
	}
	// SAFETY: the idle path's own unmask, as the halt's `daifclr` is.
	unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags)) };
}
