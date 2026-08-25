// Symmetric multiprocessing: bring the application processors online.
//
// The own loader hands the kernel the machine's ACPI RSDP and a low trampoline
// page. The kernel enumerates the local APICs from the ACPI MADT, then wakes each
// application processor itself with INIT-SIPI-SIPI: it copies the real-mode
// trampoline (arch::apboot) into the reserved page, points its mailbox at the
// shared page tables and a fresh per-core stack, and sends the wake sequence. The
// AP runs the trampoline up into 64-bit mode and calls `ap_entry`. Every per-CPU
// table is sized from the core count before any core initializes its slot, and
// indexed by our contiguous CPU id (the bootstrap processor is 0).

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

#[cfg(target_arch = "x86_64")]
use alloc::boxed::Box;
#[cfg(target_arch = "x86_64")]
use alloc::vec;
use alloc::vec::Vec;
// The MADT walk that reads it is x86_64's, so the type is too.
#[cfg(target_arch = "x86_64")]
use bootproto::BootInfo;

#[cfg(target_arch = "x86_64")]
use crate::arch;
// The ACPI table walk that reads through it is x86_64's.
#[cfg(target_arch = "x86_64")]
use crate::mem;

// Total cores we manage (BSP + woken APs).
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);

// Cores that have completed per-CPU init and reported in (BSP starts counted).
static ONLINE: AtomicUsize = AtomicUsize::new(1);

// Each core's LAPIC id by CPU id, retained at report-in so the CPU topology stays
// inspectable at runtime - SYS_CPU_INFO reads it for `lscpu`. Allocated by init,
// sized by the machine's real core count.
static LAPIC_IDS: AtomicPtr<AtomicU64> = AtomicPtr::new(core::ptr::null_mut());

// The CPU id and LAPIC id the next application processor reads on entry.
//
// "A single slot suffices, the AP has consumed both long before the next wake
// overwrites them" was the reasoning, and it holds for every AP that answers. The one
// that does NOT is the whole problem: the wait below gives up after a timeout and the
// BSP moves on, rewriting all three slots for the next core. An AP that was merely slow
// then wakes into someone else's identity - another core's id, another core's LAPIC,
// another core's stack - and initialises a per-CPU slot that is already taken, on a
// stack that is already in use.
//
// So the identity is published as a SEQLOCK and taken by a claim. `AP_SEQ` is odd while
// the slots are being written and even when they are stable, which lets an AP tell a torn
// read from a whole one; `AP_INVITE` names the generation currently open, and an AP joins
// only by winning a compare-exchange against it. That ties the identity it read to the
// invitation it is answering: a core whose invitation has been superseded loses the CAS
// and parks instead of joining, which is the outcome that costs a core rather than the
// machine.
// The published identity belongs to the x86 trampoline wake: a core started by PSCI or SBI HSM is
// given its id by the call that started it, so it has nothing to claim.
#[cfg(target_arch = "x86_64")]
static AP_CPU_ID: AtomicUsize = AtomicUsize::new(0);
#[cfg(target_arch = "x86_64")]
static AP_LAPIC_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
// Even and stable, odd while the two slots above are being rewritten.
#[cfg(target_arch = "x86_64")]
static AP_SEQ: AtomicUsize = AtomicUsize::new(0);
// The generation an arriving AP may claim, or 0 for "nothing is open".
#[cfg(target_arch = "x86_64")]
static AP_INVITE: AtomicUsize = AtomicUsize::new(0);

// Each application processor's kernel stack in 16-byte words (64 KiB), 16-aligned
// (a Box<[u128]>) so the trampoline's `call` into Rust lands ABI-aligned.
#[cfg(target_arch = "x86_64")]
const AP_STACK_WORDS: usize = 4096;

// Number of cores brought under kernel management.
pub mod numa;

pub fn cpu_count() -> usize {
	CPU_COUNT.load(Ordering::Relaxed)
}

// Register the machine's core count from the arch backend. aarch64 (PSCI) and
// riscv64 (SBI HSM) bring up SMP outside the ACPI/APIC path in `init`, so they record
// the count here before sizing the per-CPU scheduler slots.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn set_cpu_count(count: usize) {
	CPU_COUNT.store(count, Ordering::Relaxed);
	// Publish each core's interrupt-controller id for the cross-core wake-IPI path
	// (the x86 path fills this from the MADT as APs report in). On QEMU virt
	// (cortex-a72) the MPIDR affinity is the linear core index and the GICv2 SGI
	// target list addresses CPU interface N for core N, so the id is the index.
	if LAPIC_IDS.load(Ordering::Acquire).is_null() && count > 0 {
		// ALLOC-OK: boot, the core id table, built during SMP bring-up
		let mut ids: Vec<AtomicU64> = Vec::with_capacity(count);
		for i in 0..count {
			// ALLOC-OK: CPU bring-up at boot; bounded by MAX_CPUS.
			ids.push(AtomicU64::new(i as u64));
		}
		LAPIC_IDS.store(Vec::leak(ids).as_mut_ptr(), Ordering::Release);
	}
}

// Number of cores currently online.
pub fn online_count() -> usize {
	ONLINE.load(Ordering::Acquire)
}

// Count this core in the portable online tally. The x86 APs do this in ap_entry;
// aarch64 (PSCI) and riscv64 (SBI HSM) secondaries come up through their arch bring-up
// path and call this once their per-CPU state is initialized.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn mark_online() {
	ONLINE.fetch_add(1, Ordering::Release);
}

// The interrupt-controller id of the core with CPU id `cpu` (0 for a core that never reported in).
//
// WIDE ENOUGH FOR THE FIRMWARE THAT NAMES IT. An x86 APIC id is 32 bits, but an SBI hart id is an
// `unsigned long` and an MPIDR affinity is 40 bits (Aff3 sits at 39:32), and both used to reach this
// table through a `u32` - so a machine whose harts are numbered above 2^32, or whose cores carry a
// non-zero Aff3, had two cores answering to one id here. The narrow fields are real, but they are
// the CONTROLLER's rather than this table's: each `send_wake_ipi` validates at its own boundary.
pub fn lapic_id(cpu: usize) -> u64 {
	if cpu >= cpu_count() {
		return 0;
	}
	let base = LAPIC_IDS.load(Ordering::Acquire);
	if base.is_null() {
		return 0;
	}
	unsafe { (*base.add(cpu)).load(Ordering::Relaxed) }
}

// Record the real interrupt-controller id (hart id) of CPU `cpu`. riscv64 uses this
// because the SBI/OpenSBI boot hart is not necessarily hart 0, so the CPU-id -> hart-id
// map is not the identity `set_cpu_count` assumes; each hart records its own on entry so
// the cross-hart wake IPI targets the right hart.
#[cfg(target_arch = "riscv64")]
pub fn set_lapic_id(cpu: usize, id: u64) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	if !base.is_null() && cpu < cpu_count() {
		unsafe { (*base.add(cpu)).store(id, Ordering::Relaxed) };
	}
}

// Wake every application processor and wait for all cores to report in. Runs on
// the BSP after memory and interrupts are up.
//
// THE x86 WAKE SEQUENCE, and only that. It reads the ACPI MADT for local APIC ids and drives
// INIT-SIPI-SIPI through the real-mode trampoline the loader reserved a page for. The other two
// ports do not have any of those things: aarch64 asks PSCI to start a core at an address and
// riscv64 asks SBI HSM, both from their own prologue, and both then call the portable bookkeeping
// below - `set_cpu_count`, `report`, `mark_online`. Compiling this function for them made
// `send_init`, `send_startup` and the trampoline into symbols they had to define and could never
// run.
#[cfg(target_arch = "x86_64")]
pub fn init(boot_info: &BootInfo) {
	let bsp_lapic_id = arch::apic::local_id();

	// Enumerate the local APICs from the ACPI MADT. Fall back to a lone BSP if the
	// firmware exposed no RSDP or no MADT (the kernel then runs single-core).
	let mut lapics = madt_local_apics(boot_info.rsdp);
	if lapics.is_empty() {
		// ALLOC-OK: CPU bring-up at boot; bounded by MAX_CPUS.
		lapics.push(bsp_lapic_id);
	}

	// Size every per-CPU table from the enumerated core count before any core - the
	// BSP included - initializes its slot. Extra slots for any AP that fails to
	// come online stay unused; ids are handed out contiguously as APs report in.
	let total = lapics.len();
	// ALLOC-OK: boot, as above
	let mut ids: Vec<AtomicU64> = Vec::with_capacity(total);
	ids.resize_with(total, || AtomicU64::new(0));
	LAPIC_IDS.store(Vec::leak(ids).as_mut_ptr(), Ordering::Release);
	arch::percpu::allocate(total);
	crate::sched::allocate(total);
	// Publish the full core count before any AP parks: the scheduler bounds-checks
	// every cpu id against it, so it must cover every id we are about to hand out.
	// It is narrowed to the count that actually came online once bring-up finishes.
	CPU_COUNT.store(total, Ordering::Relaxed);

	// x2APIC honesty: our MSI message address encodes an 8-bit xAPIC destination,
	// so a core whose LAPIC id does not fit one byte (a >255-core machine) cannot
	// be targeted by device interrupts until x2APIC addressing lands. Say so
	// loudly rather than truncating ids silently.
	if lapics.iter().any(|&id| id > u8::MAX as u32) {
		crate::serial_println!("smp: WARNING: LAPIC ids beyond 255 present; MSI delivery (8-bit xAPIC destination) cannot target those cores - x2APIC addressing is not implemented yet");
	}

	// WIDENED HERE, where a 32-bit APIC id enters the portable path.
	arch::init_bsp_percpu(bsp_lapic_id as u64);
	report(0, bsp_lapic_id as u64);

	// Wake the application processors, one at a time, via the real-mode trampoline
	// the loader reserved a low page for. Nothing to do (and nowhere to land the
	// trampoline) on a single-core machine.
	let tramp_phys = boot_info.smp_trampoline;
	// THE COUNT THAT IS PUBLISHED IS THE COUNT THAT CAME ONLINE (KERN-ARCH-009).
	//
	// This starts at the boot processor, which is the only core known to be running, and rises only
	// as an AP reports in. It used to be declared inside the bring-up branch, so every path that
	// skipped bring-up - no trampoline page from the loader, or a root the trampoline cannot load -
	// left the firmware's discovered total published. The scheduler, the shootdown and the IPI
	// paths all read that number, and would then wait on and dispatch to cores that were never
	// started.
	let mut online = 1usize;
	if let Some(reason) = ap_boot_refusal(total, tramp_phys, arch::context::read_cr3()) {
		if total > 1 {
			crate::serial_println!("smp: WARNING: {reason}; running on the boot processor alone");
		}
	} else {
		let tramp = (mem::hhdm_offset() + tramp_phys) as *mut u8;
		let vector = (tramp_phys >> 12) as u8;
		// The trampoline runs on the shared page tables (our CR3) and calls ap_entry.
		let installed = unsafe { arch::apboot::install(tramp, arch::context::read_cr3(), ap_entry as *const () as u64) };
		debug_assert!(installed, "ap_boot_refusal already established the root is loadable");

		for &lapic in &lapics {
			if lapic == bsp_lapic_id {
				continue;
			}
			let cpu_id = online; // contiguous id for the next core to report in
			let stack = alloc_ap_stack();
			// close whatever was open, write the new identity, then publish it. A late
			// AP from the previous round can be anywhere in here and will fail its claim.
			AP_INVITE.store(0, Ordering::SeqCst);
			AP_SEQ.fetch_add(1, Ordering::SeqCst); // odd: writing
			AP_CPU_ID.store(cpu_id, Ordering::SeqCst);
			AP_LAPIC_ID.store(lapic, Ordering::SeqCst);
			unsafe { arch::apboot::set_stack(tramp, stack) };
			let generation = AP_SEQ.fetch_add(1, Ordering::SeqCst) + 1; // even: stable
			AP_INVITE.store(generation, Ordering::SeqCst);

			// INIT, then two STARTUP IPIs (with the Intel-prescribed pauses), then
			// wait for this AP to run its per-CPU init and report in.
			arch::apic::send_init(lapic);
			udelay(10_000);
			arch::apic::send_startup(lapic, vector);
			udelay(200);
			arch::apic::send_startup(lapic, vector);
			if wait_online(online + 1, 100_000) {
				online += 1;
			} else {
				// Withdraw the invitation before the next one is written. An AP that
				// turns up after this finds nothing open and parks.
				AP_INVITE.store(0, Ordering::SeqCst);
				crate::serial_println!("smp: WARNING: AP lapic_id {} did not come online", lapic);
			}
		}
	}
	// Narrowed to what is confirmed, on every path through the branch above.
	CPU_COUNT.store(online, Ordering::Relaxed);
}

// Why application processors cannot be started here, or None when they can.
//
// The three conditions are separated from `init` so they can be stated once and asked about
// directly: a discovered total of one is nothing to do, a trampoline of zero is the loader
// reporting it reserved no low page, and a root above 4 GB is one the 32-bit CR3 load in the
// trampoline would truncate (KERN-ARCH-009, KERN-ARCH-010).
// The reasons the x86 trampoline wake will not be attempted. It reads a low real-mode page and a
// 32-bit-reachable CR3, neither of which exists on a port whose firmware starts a core at an
// address for it.
#[cfg(target_arch = "x86_64")]
fn ap_boot_refusal(total: usize, trampoline: u64, cr3: u64) -> Option<&'static str> {
	if total <= 1 {
		return Some("the firmware reports a single core");
	}
	if trampoline == 0 {
		return Some("the loader reserved no real-mode trampoline page, so no application processor can be started");
	}
	if !arch::apboot::cr3_is_reachable(cr3) {
		return Some("the page-table root is above 4 GB and the trampoline loads CR3 with a 32-bit register");
	}
	None
}

// Entry point each application processor reaches from the trampoline, in 64-bit
// mode on the shared page tables and its own stack. It reads the id the BSP
// published, runs its per-CPU init, reports in, then parks in the scheduler idle
// loop so threads can be scheduled onto it.
//
// THE TRAMPOLINE IS x86's, so this is too. A core woken by PSCI or SBI HSM starts at an address its
// own prologue chose, in that port's own entry, and reaches the portable bookkeeping - `report`,
// `mark_online` - from there.
#[cfg(target_arch = "x86_64")]
extern "C" fn ap_entry() -> ! {
	// Read the identity and the generation it belongs to together, then claim that exact
	// generation. Losing the claim means this core's invitation was withdrawn or taken -
	// it is late, its slots now describe another core, and the only safe thing it can do
	// is stop. Halting costs one core; joining on someone else's stack costs the machine.
	let Some((cpu_id, lapic_id)) = claim_identity() else {
		arch::halt_loop();
	};
	arch::init_ap(cpu_id, lapic_id as u64);
	// Publish the topology entry before counting the core online, so the BSP cannot
	// observe a completed bring-up while the entry is still stale.
	report(cpu_id, lapic_id as u64);
	ONLINE.fetch_add(1, Ordering::Release);
	crate::sched::cpu_idle_loop()
}

// Read the published identity coherently and claim the invitation it belongs to, or None
// if this core arrived too late.
//
// The seqlock loop is what makes the pair coherent: an odd sequence means the BSP is
// mid-write, and a sequence that changed across the read means the snapshot is torn. The
// claim then ties that snapshot to the invitation - if the BSP has moved on, `AP_INVITE`
// no longer names this generation and the exchange fails.
// Reading the seqlock the x86 wake publishes, and claiming the invitation it belongs to.
#[cfg(target_arch = "x86_64")]
fn claim_identity() -> Option<(usize, u32)> {
	for _ in 0..1_000_000 {
		let before = AP_SEQ.load(Ordering::SeqCst);
		if before % 2 == 1 {
			core::hint::spin_loop();
			continue;
		}
		let cpu_id = AP_CPU_ID.load(Ordering::SeqCst);
		let lapic_id = AP_LAPIC_ID.load(Ordering::SeqCst);
		if AP_SEQ.load(Ordering::SeqCst) != before {
			continue;
		}
		return AP_INVITE.compare_exchange(before, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok().then_some((cpu_id, lapic_id));
	}
	None
}

// Spin until at least `target` cores are online or `spin_us` microseconds elapse.
// Returns whether the target was reached.
#[cfg(target_arch = "x86_64")]
fn wait_online(target: usize, spin_us: u64) -> bool {
	let hz = arch::tsc::hz();
	let deadline = arch::tsc::now().wrapping_add(hz / 1_000_000 * spin_us);
	while online_count() < target {
		if arch::tsc::now() >= deadline {
			return false;
		}
		core::hint::spin_loop();
	}
	true
}

// Busy-wait `us` microseconds against the calibrated TSC (up before SMP bring-up).
#[cfg(target_arch = "x86_64")]
fn udelay(us: u64) {
	let hz = arch::tsc::hz();
	let cycles = hz / 1_000_000 * us;
	let start = arch::tsc::now();
	while arch::tsc::now().wrapping_sub(start) < cycles {
		core::hint::spin_loop();
	}
}

// Allocate one application processor's kernel stack (16-aligned, leaked for the
// lifetime of the system) and return its top.
#[cfg(target_arch = "x86_64")]
fn alloc_ap_stack() -> u64 {
	// ALLOC-OK: boot, an AP's stack, allocated before that core starts
	let stack: Box<[u128]> = vec![0u128; AP_STACK_WORDS].into_boxed_slice();
	let base = Box::leak(stack).as_mut_ptr() as u64;
	base + (AP_STACK_WORDS as u64 * 16)
}

// Enumerate the enabled processors' LAPIC ids from the ACPI MADT, reachable via
// the RSDP the loader passed (0 if the firmware exposed none). All ACPI tables are
// read through the HHDM. Returns an empty vec if there is no RSDP or no MADT.
// The ACPI MADT walk, which is how x86_64 learns its local APIC ids. The device-tree ports read
// their CPU nodes in their own prologue.
#[cfg(target_arch = "x86_64")]
fn madt_local_apics(rsdp_phys: u64) -> Vec<u32> {
	let mut out = Vec::new();
	if rsdp_phys == 0 {
		return out;
	}
	let hhdm = mem::hhdm_offset();
	// THE RSDP ITSELF IS A FIRMWARE POINTER. The loader passes it through; nothing between there
	// and here has asked whether it is somewhere this kernel can read. The extended structure is 36
	// bytes, so that is what has to be inside the map before any of it is touched.
	if !mem::within_direct_map(rsdp_phys, 36) {
		crate::serial_println!("smp: the ACPI RSDP at {rsdp_phys:#x} is outside the direct map; running single-core");
		return out;
	}
	let rsdp = (hhdm + rsdp_phys) as *const u8;
	// RSDP: revision at offset 15; RSDT (u32) at 16 for revision 0/1, XSDT (u64) at
	// 24 for revision 2+.
	let mut sum: u8 = 0;
	for i in 0..20 {
		sum = sum.wrapping_add(unsafe { *rsdp.add(i) });
	}
	if sum != 0 {
		crate::serial_println!("smp: the ACPI RSDP failed its checksum; running single-core");
		return out;
	}
	let revision = unsafe { *rsdp.add(15) };
	// THE FIRST 20 BYTES DO NOT COVER THE XSDT POINTER, and the comment that used to sit here said
	// they did - "the first 20 cover the pointers this code reads". For revision 0 and 1 that is
	// true, because the RSDT pointer is at 16. For revision 2 the pointer is at 24, four bytes past
	// the end of what was summed, and the extended checksum that does cover it - over the first 36 -
	// was never asked for. The code's own justification was the thing that was wrong, which is why
	// it read as finished.
	//
	// A revision-2 RSDP whose extended checksum fails is not treated as fatal: the first 20 bytes
	// passed, so the RSDT pointer inside them is as vouched-for as it ever was, and falling back to
	// it finds the same MADT on every machine that has both. Refusing to follow a pointer nothing
	// has vouched for does not have to mean refusing to boot SMP.
	let extended_ok = revision < 2 || {
		let mut sum: u8 = 0;
		for i in 0..36 {
			sum = sum.wrapping_add(unsafe { *rsdp.add(i) });
		}
		if sum != 0 {
			crate::serial_println!("smp: the ACPI RSDP's v2 extended checksum failed; ignoring the XSDT and using the RSDT the checked bytes cover");
		}
		sum == 0
	};
	let madt = if revision >= 2 && extended_ok {
		let xsdt = unsafe { core::ptr::read_unaligned(rsdp.add(24) as *const u64) };
		find_table(hhdm, xsdt, 8, b"APIC")
	} else {
		let rsdt = unsafe { core::ptr::read_unaligned(rsdp.add(16) as *const u32) } as u64;
		find_table(hhdm, rsdt, 4, b"APIC")
	};
	let Some(madt) = madt else {
		return out;
	};
	parse_madt(hhdm, madt, &mut out);
	out
}

// ANY ACPI TABLE, BY SIGNATURE, as bytes this kernel may read.
//
// The RSDP walk is the same one SMP does for the MADT, and it is shared rather than copied: the
// checksum rules, the revision-2 fallback and the direct-map bound are all decisions that must not
// exist twice and drift. What differs is only which signature is wanted, and what comes back is a
// SLICE - so the parsers above this take bytes and can be driven by a host with a table nobody's
// firmware would produce.
#[cfg(target_arch = "x86_64")]
pub fn acpi_table(rsdp_phys: u64, want: &[u8; 4]) -> Option<&'static [u8]> {
	if rsdp_phys == 0 || !mem::within_direct_map(rsdp_phys, 36) {
		return None;
	}
	let hhdm = mem::hhdm_offset();
	let rsdp = (hhdm + rsdp_phys) as *const u8;
	let mut sum: u8 = 0;
	for i in 0..20 {
		sum = sum.wrapping_add(unsafe { *rsdp.add(i) });
	}
	if sum != 0 {
		return None;
	}
	let revision = unsafe { *rsdp.add(15) };
	let extended_ok = revision < 2 || {
		let mut sum: u8 = 0;
		for i in 0..36 {
			sum = sum.wrapping_add(unsafe { *rsdp.add(i) });
		}
		sum == 0
	};
	let found = if revision >= 2 && extended_ok {
		let xsdt = unsafe { core::ptr::read_unaligned(rsdp.add(24) as *const u64) };
		find_table(hhdm, xsdt, 8, want)
	} else {
		let rsdt = unsafe { core::ptr::read_unaligned(rsdp.add(16) as *const u32) } as u64;
		find_table(hhdm, rsdt, 4, want)
	}?;
	// `find_table` accepted it, so the length is sane and the whole table is inside the direct map.
	let len = table_length(hhdm, found)? as usize;
	// SAFETY: `table_ok` established that `len` bytes from `found` are inside the direct map, and
	// the direct map is a permanent mapping of physical memory.
	Some(unsafe { core::slice::from_raw_parts((hhdm + found) as *const u8, len) })
}

// Does the ACPI table at `phys` pass its own checksum, and is its length sane?
//
// Firmware tables were read on trust: a signature match and a minimum length, then the
// entries were walked. A damaged or hostile table can name a length that sends the walk
// off the end of the direct map, and ACPI's own answer to "is this table intact" - the
// bytes of the table sum to zero mod 256 - was never asked.
//
// This does not make firmware trustworthy; it makes a corrupt table fail as a corrupt
// table rather than as a wild read.
#[cfg(target_arch = "x86_64")]
fn table_ok(hhdm: u64, phys: u64) -> bool {
	let Some(len) = table_length(hhdm, phys) else {
		return false;
	};
	let len = len as usize;
	// The header alone is 36 bytes; a table claiming less is not one. The ceiling is a
	// sanity bound - no real ACPI table is megabytes - and it is what stops a bad length
	// turning into a long walk through unmapped memory.
	if !(36..=1024 * 1024).contains(&len) {
		return false;
	}
	// AND THE TABLE'S OWN LENGTH HAS TO BE INSIDE THE MAP TOO. A megabyte ceiling bounds how far a
	// bad length can walk; it does not say the walk stays somewhere readable, and the sum below
	// touches every byte of it.
	if !mem::within_direct_map(phys, len as u64) {
		return false;
	}
	let base = (hhdm + phys) as *const u8;
	let mut sum: u8 = 0;
	for i in 0..len {
		sum = sum.wrapping_add(unsafe { *base.add(i) });
	}
	sum == 0
}

// The 4-byte signature of the ACPI table at physical `phys`, or `None` if `phys` is not somewhere
// this kernel can read.
//
// BOUNDED, because `find_table` evaluates this BEFORE `table_ok` - `&&` is left to right - so the
// checksum that was meant to be the gate came second and the wild read came first. `table_ok`
// existed because this code had already decided not to take the firmware's word; the gap was an
// inconsistency rather than a policy.
#[cfg(target_arch = "x86_64")]
fn table_signature(hhdm: u64, phys: u64) -> Option<[u8; 4]> {
	if !mem::within_direct_map(phys, 36) {
		return None;
	}
	let p = (hhdm + phys) as *const u8;
	Some(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] })
}

// The `length` field (offset 4) of the ACPI table header at physical `phys`.
#[cfg(target_arch = "x86_64")]
fn table_length(hhdm: u64, phys: u64) -> Option<u32> {
	if !mem::within_direct_map(phys, 36) {
		return None;
	}
	Some(unsafe { core::ptr::read_unaligned((hhdm + phys + 4) as *const u32) })
}

// Scan an RSDT/XSDT (entry pointers are `ptr_size` bytes each, after the 36-byte
// header) for the MADT (signature "APIC"), returning its physical address.
#[cfg(target_arch = "x86_64")]
fn find_table(hhdm: u64, sdt_phys: u64, ptr_size: usize, want: &[u8; 4]) -> Option<u64> {
	if sdt_phys == 0 {
		return None;
	}
	if !table_ok(hhdm, sdt_phys) {
		crate::serial_println!("smp: the ACPI root table failed its own checksum; not walking it");
		return None;
	}
	// `table_ok` passed, so the length is present, sane and inside the map.
	let len = table_length(hhdm, sdt_phys).unwrap_or(0) as usize;
	let base = (hhdm + sdt_phys + 36) as *const u8;
	let count = (len - 36) / ptr_size;
	for i in 0..count {
		let entry = unsafe { base.add(i * ptr_size) };
		let phys = if ptr_size == 8 { unsafe { core::ptr::read_unaligned(entry as *const u64) } } else { unsafe { core::ptr::read_unaligned(entry as *const u32) as u64 } };
		// The bound says the pointer is somewhere readable, the signature says what it claims to
		// be, and the checksum says whether to believe it - in that order, because the first two
		// are reads of the thing the third is about.
		if table_signature(hhdm, phys) == Some(*want) && table_ok(hhdm, phys) {
			return Some(phys);
		}
	}
	None
}

// Walk the MADT's interrupt-controller structures, collecting the LAPIC id of each
// enabled Processor Local APIC (type 0, flags bit 0). Entries start at offset 44
// (36-byte header + 4-byte local APIC address + 4-byte flags).
#[cfg(target_arch = "x86_64")]
fn parse_madt(hhdm: u64, madt_phys: u64, out: &mut Vec<u32>) {
	// `table_ok` has passed for this table, so the length is readable and the table is inside the
	// direct map for the whole of it.
	let len = table_length(hhdm, madt_phys).unwrap_or(0) as usize;
	let base = (hhdm + madt_phys) as *const u8;
	let mut off = 44usize;
	while off + 2 <= len {
		let etype = unsafe { *base.add(off) };
		let elen = unsafe { *base.add(off + 1) } as usize;
		// The entry has to fit inside the table it claims to be in, and be at least a header.
		//
		// The walk checked `elen == 0` and then trusted the rest: a length running past the table
		// advanced `off` past the end (where the loop condition catches it, harmlessly) but a length
		// that merely OVERLAPS the end let the type-0 read below run to `off + 8` on the strength of
		// its own bound while `elen` said the entry was shorter. Checksum-valid firmware can be
		// structurally wrong, and this is a boot path with no one to complain to.
		if elen < 2 || off + elen > len {
			break;
		}
		// A Local APIC entry is 8 bytes by the specification. Requiring the DECLARED length to be at
		// least that - rather than only that the read fits in the table - is what makes the read
		// consistent with the entry it belongs to.
		if etype == 0 && elen >= 8 {
			let apic_id = unsafe { *base.add(off + 3) } as u32;
			let flags = unsafe { core::ptr::read_unaligned(base.add(off + 4) as *const u32) };
			if flags & 1 != 0 {
				// ALLOC-OK: CPU bring-up at boot; bounded by MAX_CPUS.
				out.push(apic_id);
			}
		}
		off += elen;
	}
}

// The x86 topology publication: an AP records the id it was woken with. The device-tree ports
// publish their own per-CPU identity in their prologue and only count themselves online here.
#[cfg(target_arch = "x86_64")]
fn report(cpu_id: usize, lapic_id: u64) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	unsafe { (*base.add(cpu_id)).store(lapic_id, Ordering::Relaxed) };
}

#[cfg(test)]
mod tests;
