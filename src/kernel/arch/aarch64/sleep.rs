// THE aarch64 HALF OF THE SLEEP ENTRY. SUSPEND TO IDLE is what this port's entry takes: the portable entry parks every core
// with the timed wake as the only timer (`crate::sleep`), and this masks every claimed line and every MSI-X entry the
// kernel programmed - each driver has quiesced its device - but the wake set's, and puts back exactly what it masked.
// No fixed button exists here, so nothing is pending before an entry but what the wake set itself raises.
//
// SUSPEND TO RAM would be PSCI's SYSTEM_SUSPEND, asked of PSCI_FEATURES: whether this firmware offers it is said once
// at boot (`report_offers`), and the entry does not take it on this port, which has no resume path for a machine whose
// cores lost their context. HIBERNATION needs that same path - the image's kernel resumed on a core a fresh boot handed
// over - and is not offered here either: `SYS_SLEEP_STATES` names neither, and asking for one is answered
// `ERR_UNSUPPORTED`.

use abi::{ERR_UNSUPPORTED, SNAPSHOT_CONTEXT, SleepReport};

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

pub fn offers_disk() -> bool {
	false
}

pub fn fixed_buttons() -> u64 {
	0
}

// WHAT THE FIRMWARE OFFERS AND THE ENTRY TAKES, said once at boot - checked, not assumed.
pub fn report_offers() {
	let offered = super::psci::system_suspend_offered();
	crate::serial_println!("sleep: PSCI SYSTEM_SUSPEND is {} by this firmware - this port's entry takes suspend to idle", if offered { "offered" } else { "not offered" });
}

pub fn suspend_to_ram(_pair: (u8, u8), _after: Option<u64>) -> Result<SleepReport, i64> {
	Err(ERR_UNSUPPORTED)
}

pub fn snapshot() -> Result<SleepReport, i64> {
	Err(ERR_UNSUPPORTED)
}

pub fn enter_disk() -> Result<SleepReport, i64> {
	Err(ERR_UNSUPPORTED)
}

pub fn replace() -> i64 {
	ERR_UNSUPPORTED
}

// No request ever waits for the boot core's idle context on this port.
pub fn s3_pending() -> bool {
	false
}

pub fn run_pending() {}

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

// THE SUITE'S RESUME CONTEXT: the magic, the kernel's page tables, the direct map's offset and the top of RAM - the bytes
// the snapshot's own cases carry through `sleep::disk`. No resume entry exists on this port, so no context is ours
// (`context_is_ours`) and a restore is refused before it starts.
#[cfg(test)]
const CONTEXT_MAGIC: u64 = u64::from_le_bytes(*b"LSHIBCTX");

#[cfg(test)]
pub fn context() -> [u8; SNAPSHOT_CONTEXT] {
	let words = [CONTEXT_MAGIC, crate::sched::kernel_cr3(), 0, 0, crate::mem::hhdm_offset(), crate::sleep::disk::ram_top(), kernel_image().0, 0];
	let mut out = [0u8; SNAPSHOT_CONTEXT];
	for (at, word) in words.iter().enumerate() {
		out[at * 8..at * 8 + 8].copy_from_slice(&word.to_le_bytes());
	}
	out
}

pub fn context_is_ours(_context: &[u8; SNAPSHOT_CONTEXT]) -> bool {
	false
}
