// THE riscv64 HALF OF PROCESSOR POWER: what the hart offers an idle state, and the SBI's HART_SUSPEND - the entry the
// device tree's `idle-states` name (`riscv,sbi-suspend-param`). The `time` counter is the clock and runs on in every
// state; the hart's timer runs on in every state its tree does not mark `local-timer-stop`. No state that loses the
// hart's context (a non-retentive suspend type, bit 31) is entered: this port has no per-core resume path.
//
// NO PROCESSOR REGISTER IS MAPPED, and there is no port space: see the aarch64 half, whose reasons are this port's too.

pub const ARCH: procpower::Arch = procpower::Arch::Riscv64;

pub fn cpu() -> procpower::Cpu {
	procpower::Cpu { mwait: false, invariant_counter: true, timer_always_running: false, context_resume: false }
}

pub fn map_register(_phys: u64) -> Result<u64, &'static str> {
	Err("this port maps no processor register - a table naming one comes from ACPI, which it does not read")
}

pub fn unmap_register(_phys: u64) {}

pub fn port_read(_port: u16, _bytes: u8) -> u64 {
	0
}

pub fn port_write(_port: u16, _bytes: u8, _value: u64) {}

pub fn offers_mwait() -> bool {
	false
}

pub fn mwait(_hint: u32) {
	super::idle_halt();
}

pub fn after_register_entry() {
	super::idle_halt();
}

// WHETHER A TREE STATE'S SUSPEND TYPE LOSES THE HART'S CONTEXT: a non-retentive type, bit 31.
pub fn state_loses_context(parameter: u32) -> bool {
	parameter & (1 << 31) != 0
}

// HOW MANY ENTRIES THE FIRMWARE REFUSED - each one the halt instead - for the suite, which checks a state was entered
// through the firmware and not merely counted.
static REFUSED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
pub fn firmware_refusals() -> u64 {
	REFUSED.load(core::sync::atomic::Ordering::Relaxed)
}

// THE SBI's HSM HART_SUSPEND (EID 0x48534D, FID 3) with the state's suspend type, entered with interrupts masked: a
// retentive suspend returns once an interrupt this hart enables is pending, as WFI does, and the unmask after takes it.
// A firmware that refuses the call leaves the hart in the halt instead, said once.
pub fn firmware_suspend(parameter: u32) {
	let error: isize;
	// SAFETY: a retentive suspend, which returns here with the hart's state kept; the resume address is unused by one.
	unsafe {
		core::arch::asm!("ecall", in("a7") 0x48534Dusize, in("a6") 3usize, inout("a0") parameter as usize => error, inout("a1") 0usize => _, in("a2") 0usize, options(nostack));
	}
	if error != 0 {
		REFUSED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
		static SAID: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
		if !SAID.swap(true, core::sync::atomic::Ordering::Relaxed) {
			crate::serial_println!("processor: the SBI refused HART_SUSPEND({parameter:#x}) with {error} - the halt waits instead");
		}
		super::idle_halt();
		return;
	}
	// SAFETY: the idle path's own unmask, as the halt's is.
	unsafe { core::arch::asm!("csrsi sstatus, 2", options(nomem, nostack)) };
}
