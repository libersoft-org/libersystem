// THE aarch64 HALF OF THE SLEEP ENTRY. SUSPEND TO IDLE is what this port's entry takes: the portable entry parks every core
// with the timed wake as the only timer (`crate::sleep`), and this masks every claimed line and every MSI-X entry the
// kernel programmed - each driver has quiesced its device - but the wake set's, and puts back exactly what it masked.
// No fixed button exists here, so nothing is pending before an entry but what the wake set itself raises.
//
// SUSPEND TO RAM would be PSCI's SYSTEM_SUSPEND, asked of PSCI_FEATURES: whether this firmware offers it is said once
// at boot (`report_offers`); QEMU's does not, and the entry does not take it. HIBERNATION IS OFFERED where PSCI turns
// cores off and on and the MSIs do not go through an ITS: its snapshot holds every core in its hold with its context
// saved, the image's kernel is resumed on the core the image's boot core was, and every other core is turned on again at
// its record (`sleep::image`). The distributor, every function's configuration and the MSI-X entries the kernel
// programmed are saved with it and written back (`save_machine`, `restore_machine`); each core's CPU interface and
// redistributor are put back by its own resume.

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
	let offered = super::psci::system_suspend_offered();
	crate::serial_println!("sleep: PSCI SYSTEM_SUSPEND is {} by this firmware - this port's entry takes suspend to idle", if offered { "offered" } else { "not offered" });
	match disk_refused() {
		None => crate::serial_println!("sleep: hibernation is offered - every core's context through PSCI"),
		Some(why) => crate::serial_println!("sleep: hibernation is not offered - {why}"),
	}
}

// WHY HIBERNATION IS NOT OFFERED, where it is not: the cores are turned off and on through PSCI, and an ITS's tables and
// mappings are not what a restore puts back.
pub fn disk_refused() -> Option<&'static str> {
	if !super::psci::conduit_present() {
		return Some("no PSCI conduit turns the cores off and on");
	}
	super::interrupts::using_its().then_some("the MSIs go through an ITS, whose tables and mappings a restore does not put back")
}

// THE MACHINE'S SETTINGS A RESTORE'S FRESH BOOT RESETS: the distributor, every function's configuration, the MSI-X
// entries the kernel programmed.
pub fn save_machine() -> bool {
	if !super::gic::save_distributor() || !super::pci::save_config_all() {
		return false;
	}
	super::interrupts::save_msi_entries();
	true
}

pub fn restore_machine() {
	super::gic::restore_distributor();
	super::pci::restore_config_all();
	super::interrupts::restore_msi_entries();
}

// THE JUMP TO THE TRAMPOLINE: the boot tables' low half, the identity map the trampoline runs at, as TTBR0 - this code
// runs on through TTBR1 - and then it, from its physical address, with `params` its physical address too.
pub fn replace_jump(params: u64) -> ! {
	let low = super::resume::boot_tables() + 4096;
	// SAFETY: interrupts are masked and every other core is off; the trampoline never returns.
	unsafe {
		core::arch::asm!(
			"msr ttbr0_el1, {low}",
			"dsb sy",
			"tlbi vmalle1",
			"dsb sy",
			"isb",
			"br {trampoline}",
			low = in(reg) low,
			trampoline = in(reg) super::resume::trampoline(),
			in("x0") params,
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
