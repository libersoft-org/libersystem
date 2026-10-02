// THE riscv64 HALF OF THE SLEEP ENTRY. SUSPEND TO IDLE is what this port's entry takes: the portable entry parks every core
// with the timed wake as the only timer (`crate::sleep`), and this masks every claimed line and every MSI-X entry the
// kernel programmed - each driver has quiesced its device - but the wake set's, and puts back exactly what it masked.
// No fixed button exists here, so nothing is pending before an entry but what the wake set itself raises.
//
// SUSPEND TO RAM would be the SBI's System Suspend extension (SUSP), asked of the SBI's probe: whether this firmware
// offers it is said once at boot (`report_offers`); QEMU's OpenSBI does not, and the entry does not take it.
// HIBERNATION IS OFFERED where the SBI's HSM extension stops and starts harts: its snapshot holds every hart in its
// hold with its context saved, the image's kernel is resumed on the hart the image's boot hart was, and every other hart
// is started again at its record (`sleep::image`). The APLIC domain, every function's configuration and the MSI-X entries
// the kernel programmed are saved with it and written back (`save_machine`, `restore_machine`); each hart's interrupt file
// is its own record's.

use abi::{ERR_UNSUPPORTED, SleepReport};

use crate::sleep::boot_core::{self, Kind};

pub fn wake_pending() -> bool {
	false
}

pub fn mask_device_lines() {
	super::interrupts::mask_claimed_lines();
	super::interrupts::mask_msi_entries();
}

pub fn unmask_device_lines() {
	super::interrupts::unmask_claimed_lines();
	super::interrupts::unmask_msi_entries();
}

pub fn offers_idle() -> bool {
	true
}

pub fn offers_ram() -> bool {
	false
}

pub fn fixed_buttons() -> u64 {
	0
}

// WHAT THE FIRMWARE OFFERS AND THE ENTRY TAKES, said once at boot - checked, not assumed.
pub fn report_offers() {
	let offered = super::sbi_probe_extension(0x5355_5350);
	crate::serial_println!("sleep: the SBI System Suspend extension is {} by this firmware - this port's entry takes suspend to idle", if offered { "offered" } else { "not offered" });
	match disk_refused() {
		None => crate::serial_println!("sleep: hibernation is offered - every hart's context through the HSM extension"),
		Some(why) => crate::serial_println!("sleep: hibernation is not offered - {why}"),
	}
}

// WHY HIBERNATION IS NOT OFFERED, where it is not: the harts are stopped and started through the HSM extension.
pub fn disk_refused() -> Option<&'static str> {
	(!super::sbi_probe_extension(0x48_534D)).then_some("the SBI does not offer the HSM extension, which stops and starts the harts")
}

// THE MACHINE'S SETTINGS A RESTORE'S FRESH BOOT RESETS: the APLIC domain, every function's configuration, the MSI-X
// entries the kernel programmed.
pub fn save_machine() -> bool {
	if !super::aplic::save_domain() || !super::pci::save_config_all() {
		return false;
	}
	super::interrupts::save_msi_entries();
	true
}

pub fn restore_machine() {
	super::aplic::restore_domain();
	super::pci::restore_config_all();
	super::interrupts::restore_msi_entries();
}

// THE JUMP TO THE TRAMPOLINE: the boot page tables, which map the kernel's text where it runs and the low identity the
// trampoline runs at, and then it - from its physical address, with `params` its physical address too.
pub fn replace_jump(params: u64) -> ! {
	// SAFETY: interrupts are masked and every other hart is stopped; the trampoline never returns.
	unsafe {
		core::arch::asm!(
			"srli t0, t2, 12",
			"li t1, 8",
			"slli t1, t1, 60",
			"or t0, t0, t1",
			"sfence.vma",
			"csrw satp, t0",
			"sfence.vma",
			"jr t3",
			in("t2") super::resume::boot_tables(),
			in("t3") super::resume::trampoline(),
			in("a0") params,
			options(noreturn),
		)
	}
}

// HIBERNATION, through the per-core resume path (`sleep::image`): its snapshot and a restore's replacement asked of the
// boot core's idle context (`sleep::boot_core`), and the machine off at the end of the write. SUSPEND TO RAM is not
// this port's - see the head of this file - and a request for it is answered `ERR_UNSUPPORTED` there.
pub fn offers_disk() -> bool {
	disk_refused().is_none()
}

pub fn suspend_to_ram(pair: (u8, u8), after: Option<u64>) -> Result<SleepReport, i64> {
	boot_core::ask(Kind::Ram, pair, after)
}

pub fn snapshot() -> Result<SleepReport, i64> {
	if !offers_disk() {
		return Err(ERR_UNSUPPORTED);
	}
	boot_core::ask(Kind::Snapshot, (0, 0), None)
}

pub fn enter_disk() -> Result<SleepReport, i64> {
	crate::sleep::image::enter_disk()
}

pub fn replace() -> i64 {
	match boot_core::ask(Kind::Replace, (0, 0), None) {
		Ok(_) => ERR_UNSUPPORTED,
		Err(error) => error,
	}
}

pub use crate::sleep::image::{context_is_ours, replace_memory};
// The resume context a snapshot carries, which the suite's restore cases hand the kernel as an image's.
#[cfg(test)]
pub use crate::sleep::image::context;

pub fn s3_pending() -> bool {
	boot_core::pending()
}

pub fn run_pending() {
	boot_core::run_pending()
}

// THE BOOT CORE'S RUN OF A REQUEST.
pub fn run(kind: Kind, _pair: (u8, u8), _after: Option<u64>) -> Result<SleepReport, i64> {
	match kind {
		Kind::Ram => Err(ERR_UNSUPPORTED),
		Kind::Snapshot => crate::sleep::image::run_snapshot(),
		Kind::Replace => Err(crate::sleep::disk::replace_now()),
	}
}

unsafe extern "C" {
	static __kernel_image_start: u8;
	static __kernel_image_end: u8;
}

// The kernel's code and read-only data as loaded: what the system image's digest reads of the kernel - which
// `SYS_SYSTEM_FINGERPRINT` answers here as on x86_64.
pub fn kernel_image() -> (u64, usize) {
	// Two linker symbols' addresses, never dereferenced here.
	let (start, end) = (&raw const __kernel_image_start as u64, &raw const __kernel_image_end as u64);
	(start, end.saturating_sub(start) as usize)
}

// THE DEVELOPMENT SWITCH FOR ANOTHER SYSTEM IMAGE, as on x86_64: on a development profile, fw-cfg's
// `opt/org.libersystem/system-variant` is a part of the system image's digest.
pub fn development_variant(out: &mut [u8; 64]) -> usize {
	if super::boot_profile().is_none() {
		return 0;
	}
	super::fwcfg_read(b"opt/org.libersystem/system-variant", out).unwrap_or(0)
}
