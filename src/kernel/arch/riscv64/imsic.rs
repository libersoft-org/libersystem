// RISC-V AIA IMSIC (Incoming MSI Controller) - per-hart MSI target.
//
// With QEMU's `virt,aia=aplic-imsic`, PCIe devices deliver MSI-X messages instead of
// wired INTx: a device signals by DMA-writing its interrupt identity (EID) to the target
// hart's IMSIC S-mode interrupt file (a 4 KiB MMIO page). The IMSIC then sets that EID's
// pending bit and raises the hart's S-mode external-interrupt line (SCAUSE code 9). This
// gives every device its own edge-triggered EID - no INTx line sharing - so, unlike the
// PLIC's four shared PCIe INTx sources, interrupt delivery to the full device set is
// reliable (mirroring the x86 LAPIC-MSI and aarch64 GICv2m backends).
//
// The IMSIC's registers (EIDELIVERY, EITHRESHOLD, the EIP/EIE arrays) are accessed per
// hart through the indirect S-mode CSRs siselect (0x150) / sireg (0x151); the top pending
// EID is claimed through stopei (0x15C). Each hart programs only its own file, so a
// device's MSI targets the hart that acquired it (the one running the setup syscall).

use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

// The S-mode IMSIC files: one 4 KiB page per hart, HART_STRIDE apart. A device MSI targets hart H by
// writing its EID to that hart's page.
//
// FROM THE MACHINE DESCRIPTION NOW. This was a fixed address with a comment saying the device tree
// was deliberately not consulted, so the port ran only where QEMU's `virt,aia=aplic-imsic` puts the
// files - and a tree that says otherwise was ignored rather than followed. `set_base` is called by
// the prologue with the address the tree named, once it has checked it lies inside the direct map.
//
// The default remains as the ONE no-DT descriptor, for the same reason aarch64 keeps one: a boot
// with no tree to read is a regression profile worth keeping and not a discovery claim.
const IMSIC_S_DEFAULT: usize = 0x2800_0000;
const HART_STRIDE: usize = 0x1000;
static IMSIC_S_BASE: AtomicUsize = AtomicUsize::new(IMSIC_S_DEFAULT);

// Indirect-CSR register selects for siselect.
const EIDELIVERY: usize = 0x70; // interrupt delivery enable
const EITHRESHOLD: usize = 0x72; // priority threshold (0 = accept all)
const EIE0: usize = 0xC0; // enable bits for EIDs 0..63 (RV64: one 64-bit register)

// How many interrupt files the controller has, which is how many harts can be an MSI target.
//
// The initial value belongs to the no-DT descriptor above rather than to any machine: a boot with
// no tree to read keeps exactly the behaviour it had, and a boot WITH a tree replaces this with what
// the tree stated.
static FILE_COUNT: AtomicU32 = AtomicU32::new(fdt::MAX_CPUS as u32);

// The identities this kernel arms: EIE0 holds EIDs 0..63 and EID 0 is "no interrupt", so a
// controller carrying fewer than this cannot deliver everything the MSI window hands out.
const IDENTITIES_USED: u32 = 63;

// Point this port at the interrupt files the machine described, or say why it cannot.
//
// EVERY REFUSAL LEAVES THE PREVIOUS VALUE ALONE and returns the reason, because the alternative to
// naming an unsupported layout is computing an address inside it. A file's address here is `base +
// hart * 4096`, which is true only of a flat, hart-indexed array whose file N belongs to hart N -
// and the AIA binding describes machines where none of that holds.
pub fn configure(info: &fdt::BootInfo) -> Result<(), &'static str> {
	if info.imsic_base == 0 {
		return Err("the tree describes no supervisor IMSIC");
	}
	if !crate::mem::within_direct_map(info.imsic_base, info.imsic_size) {
		// An address `phys_to_virt` does not translate; writing an MSI there would be a store into
		// whatever the arithmetic produced.
		return Err("the interrupt files lie outside the direct map");
	}
	if info.imsic_guest_index_bits != 0 {
		return Err("the files are guest-indexed, which this kernel does not address");
	}
	if info.imsic_group_index_bits != 0 {
		return Err("the files are group-indexed, which this kernel does not address");
	}
	if info.imsic_num_ids != 0 && info.imsic_num_ids < IDENTITIES_USED {
		return Err("the controller carries fewer identities than the MSI window arms");
	}
	let files = info.imsic_hart_count;
	if files == 0 {
		return Err("the tree ties the interrupt files to no hart");
	}
	if u64::from(files) * HART_STRIDE as u64 > info.imsic_size {
		return Err("the region is smaller than the files the tree declares");
	}
	for (index, &hart) in info.imsic_harts[..files as usize].iter().enumerate() {
		if hart != index as u64 {
			// A machine whose harts are sparse or listed out of order. Addressing it needs the file
			// INDEX rather than the hart id, which is a translation this port does not carry.
			return Err("a file's index is not its hart id, which is the only layout this kernel addresses");
		}
	}
	IMSIC_S_BASE.store(info.imsic_base as usize, Ordering::Relaxed);
	FILE_COUNT.store(files, Ordering::Release);
	Ok(())
}

// Whether `hart` has an interrupt file, and so can be the target of a device MSI.
pub fn has_file(hart: u64) -> bool {
	hart < u64::from(FILE_COUNT.load(Ordering::Acquire))
}

fn base() -> usize {
	IMSIC_S_BASE.load(Ordering::Relaxed)
}

// The physical MSI target address for hart `hart`'s S-mode interrupt file - what a
// device's MSI-X table entry stores so its DMA write lands in that hart's IMSIC.
pub fn msi_address(hart: u64) -> u64 {
	(base() + hart as usize * HART_STRIDE) as u64
}

// Select an IMSIC register on THIS hart and write it (siselect then sireg).
unsafe fn ireg_write(select: usize, val: usize) {
	unsafe {
		core::arch::asm!(
			"csrw 0x150, {s}",
			"csrw 0x151, {v}",
			s = in(reg) select,
			v = in(reg) val,
			options(nostack, preserves_flags),
		);
	}
}

// Select an IMSIC register on THIS hart and read it.
unsafe fn ireg_read(select: usize) -> usize {
	let val: usize;
	unsafe {
		core::arch::asm!(
			"csrw 0x150, {s}",
			"csrr {v}, 0x151",
			s = in(reg) select,
			v = out(reg) val,
			options(nostack, preserves_flags),
		);
	}
	val
}

// Bring up THIS hart's IMSIC S-file: enable interrupt delivery and accept any priority,
// so an EID a device targets here raises the hart's S-mode external interrupt.
pub fn init_hart() {
	unsafe {
		ireg_write(EIDELIVERY, 1);
		ireg_write(EITHRESHOLD, 0);
	}
}

// WHICH HART'S INTERRUPT FILE HOLDS EACH EID'S ENABLE BIT.
//
// An IMSIC enable bit lives in ONE hart's file, and the only way to touch a file is to be the hart
// that owns it: `siselect`/`sireg` address the running hart's, always. So an EID enabled by the hart
// that ran the acquire could only ever be disabled BY THAT HART - and teardown runs wherever the
// last reference happened to be dropped. `disable_eid` on another hart cleared a bit in ITS file,
// where the EID was never enabled, and reported success: the device kept delivering to the owner,
// and the "best-effort" disable the unbind path documented had in fact done nothing at all.
//
// The owner is therefore recorded with the EID, by logical CPU id (`u32::MAX` = nobody).
static EID_CPU: [AtomicU32; 64] = [const { AtomicU32::new(u32::MAX) }; 64];

// Disable requests waiting for a hart to run them, one bit per EID. EIDs are 0..63 here, so one
// word per hart is the whole mailbox.
static PENDING_DISABLE: [AtomicU64; fdt::MAX_CPUS] = [const { AtomicU64::new(0) }; fdt::MAX_CPUS];

// Enable EID `eid` on THIS hart's IMSIC (set its EIE bit) and record this hart as its owner. The
// device's MSI-X entry is programmed with the same hart's file, so the two cannot disagree.
pub fn enable_eid(eid: u32) {
	if eid == 0 || eid >= 64 {
		return;
	}
	EID_CPU[eid as usize].store(super::percpu::this_cpu().cpu_id(), Ordering::Release);
	unsafe {
		let cur = ireg_read(EIE0);
		ireg_write(EIE0, cur | (1usize << eid));
	}
}

// Clear EID `eid` in the interrupt file that owns it, wherever that is.
//
// True means the enable bit is CLEARED and the EID can be handed to another device. False means it
// is not, and the caller must treat the EID as spoken for: the owning hart did not answer, so a
// device may still be delivering to a file where the identity is live. Reusing it then hands a
// stray message to whoever gets it next, which is the whole reason vectors are retired rather than
// freed on this path.
pub fn disable_eid_on_owner(eid: u32) -> bool {
	if eid == 0 || eid >= 64 {
		return true;
	}
	let owner = EID_CPU[eid as usize].load(Ordering::Acquire);
	if owner == u32::MAX {
		return true; // never enabled anywhere
	}
	if owner == super::percpu::this_cpu().cpu_id() {
		disable_local(eid);
		return true;
	}
	let Some(mailbox) = PENDING_DISABLE.get(owner as usize) else {
		return false;
	};
	mailbox.fetch_or(1u64 << eid, Ordering::AcqRel);
	// The wake IPI is the request: the owning hart leaves `wfi`, takes the software interrupt and
	// services its mailbox before anything else.
	super::apic::send_wake_ipi(crate::smp::lapic_id(owner as usize));
	// A BOUND, AND WHAT IT COSTS WHEN IT EXPIRES. Long enough for a hart that is running to reach
	// its trap handler; short enough that a hart wedged with interrupts off does not hold a device
	// teardown open. On expiry the EID stays enabled AND stays owned, so nothing can be armed under
	// it again.
	let mut spins: u64 = 0;
	while mailbox.load(Ordering::Acquire) & (1u64 << eid) != 0 && spins < 200_000_000 {
		core::hint::spin_loop();
		spins += 1;
	}
	if mailbox.load(Ordering::Acquire) & (1u64 << eid) != 0 {
		crate::serial_println!("imsic: cpu {owner} did not answer the disable of EID {eid}; it stays enabled and out of circulation");
		return false;
	}
	true
}

// Run the disable requests addressed to this hart. Called from the software-interrupt handler - the
// same interrupt the request's wake IPI raises - and it is the ONLY place an EID belonging to this
// hart is cleared on behalf of somebody else.
pub fn service_pending_disables() {
	let cpu = super::percpu::this_cpu().cpu_id() as usize;
	let Some(mailbox) = PENDING_DISABLE.get(cpu) else {
		return;
	};
	let mut requests = mailbox.load(Ordering::Acquire);
	while requests != 0 {
		let eid = requests.trailing_zeros();
		disable_local(eid);
		// Cleared only after the enable bit is gone, because the requester takes the clear as the
		// answer to its question.
		mailbox.fetch_and(!(1u64 << eid), Ordering::AcqRel);
		requests &= !(1u64 << eid);
	}
}

// Clear EID `eid` in THIS hart's file and give up its ownership.
fn disable_local(eid: u32) {
	unsafe {
		let cur = ireg_read(EIE0);
		ireg_write(EIE0, cur & !(1usize << eid));
	}
	EID_CPU[eid as usize].store(u32::MAX, Ordering::Release);
}

// Claim the top pending-and-enabled external interrupt through stopei, clearing its
// pending bit (edge-triggered). Returns its EID (identity in bits 26:16), 0 if none.
pub fn claim() -> u32 {
	let top: usize;
	unsafe {
		// csrrw rd, stopei, rs1 with rd == rs1 (seeded 0): writes 0 (claims the top
		// interrupt, clearing its pending bit) and reads the pre-claim top into rd.
		core::arch::asm!(
			"csrrw {t}, 0x15c, {t}",
			t = inout(reg) 0usize => top,
			options(nostack, preserves_flags),
		);
	}
	(top >> 16) as u32
}

// Service an S-mode external interrupt (SCAUSE code 9): claim each pending EID and wake
// its bound driver, until none remain. Called from the trap handler.
pub fn handle_external() {
	loop {
		let eid = claim();
		if eid == 0 {
			break;
		}
		super::interrupts::dispatch_msi(eid);
	}
}
