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

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use bootproto::BootInfo;

use crate::arch;
use crate::mem;

// Total cores we manage (BSP + woken APs).
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);

// Cores that have completed per-CPU init and reported in (BSP starts counted).
static ONLINE: AtomicUsize = AtomicUsize::new(1);

// Each core's LAPIC id by CPU id, retained at report-in so the CPU topology stays
// inspectable at runtime - SYS_CPU_INFO reads it for `lscpu`. Allocated by init,
// sized by the machine's real core count.
static LAPIC_IDS: AtomicPtr<AtomicU32> = AtomicPtr::new(core::ptr::null_mut());

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
static AP_CPU_ID: AtomicUsize = AtomicUsize::new(0);
static AP_LAPIC_ID: AtomicU32 = AtomicU32::new(0);
// Even and stable, odd while the two slots above are being rewritten.
static AP_SEQ: AtomicUsize = AtomicUsize::new(0);
// The generation an arriving AP may claim, or 0 for "nothing is open".
static AP_INVITE: AtomicUsize = AtomicUsize::new(0);

// Each application processor's kernel stack in 16-byte words (64 KiB), 16-aligned
// (a Box<[u128]>) so the trampoline's `call` into Rust lands ABI-aligned.
const AP_STACK_WORDS: usize = 4096;

// Number of cores brought under kernel management.
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
		let mut ids: Vec<AtomicU32> = Vec::with_capacity(count);
		for i in 0..count {
			ids.push(AtomicU32::new(i as u32));
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

// The LAPIC id of the core with CPU id `cpu` (0 for a core that never reported in).
pub fn lapic_id(cpu: usize) -> u32 {
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
pub fn set_lapic_id(cpu: usize, id: u32) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	if !base.is_null() && cpu < cpu_count() {
		unsafe { (*base.add(cpu)).store(id, Ordering::Relaxed) };
	}
}

// Wake every application processor and wait for all cores to report in. Runs on
// the BSP after memory and interrupts are up.
pub fn init(boot_info: &BootInfo) {
	let bsp_lapic_id = arch::apic::local_id();

	// Enumerate the local APICs from the ACPI MADT. Fall back to a lone BSP if the
	// firmware exposed no RSDP or no MADT (the kernel then runs single-core).
	let mut lapics = madt_local_apics(boot_info.rsdp);
	if lapics.is_empty() {
		lapics.push(bsp_lapic_id);
	}

	// Size every per-CPU table from the enumerated core count before any core - the
	// BSP included - initializes its slot. Extra slots for any AP that fails to
	// come online stay unused; ids are handed out contiguously as APs report in.
	let total = lapics.len();
	let mut ids: Vec<AtomicU32> = Vec::with_capacity(total);
	ids.resize_with(total, || AtomicU32::new(0));
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

	arch::init_bsp_percpu(bsp_lapic_id);
	report(0, bsp_lapic_id);

	// Wake the application processors, one at a time, via the real-mode trampoline
	// the loader reserved a low page for. Nothing to do (and nowhere to land the
	// trampoline) on a single-core machine.
	let tramp_phys = boot_info.smp_trampoline;
	if total > 1 && tramp_phys != 0 {
		let tramp = (mem::hhdm_offset() + tramp_phys) as *mut u8;
		let vector = (tramp_phys >> 12) as u8;
		// The trampoline runs on the shared page tables (our CR3) and calls ap_entry.
		unsafe { arch::apboot::install(tramp, arch::context::read_cr3(), ap_entry as *const () as u64) };

		let mut online = 1usize; // the BSP
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
		CPU_COUNT.store(online, Ordering::Relaxed);
	}
}

// Entry point each application processor reaches from the trampoline, in 64-bit
// mode on the shared page tables and its own stack. It reads the id the BSP
// published, runs its per-CPU init, reports in, then parks in the scheduler idle
// loop so threads can be scheduled onto it.
extern "C" fn ap_entry() -> ! {
	// Read the identity and the generation it belongs to together, then claim that exact
	// generation. Losing the claim means this core's invitation was withdrawn or taken -
	// it is late, its slots now describe another core, and the only safe thing it can do
	// is stop. Halting costs one core; joining on someone else's stack costs the machine.
	let Some((cpu_id, lapic_id)) = claim_identity() else {
		arch::halt_loop();
	};
	arch::init_ap(cpu_id, lapic_id);
	// Publish the topology entry before counting the core online, so the BSP cannot
	// observe a completed bring-up while the entry is still stale.
	report(cpu_id, lapic_id);
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
fn alloc_ap_stack() -> u64 {
	let stack: Box<[u128]> = vec![0u128; AP_STACK_WORDS].into_boxed_slice();
	let base = Box::leak(stack).as_mut_ptr() as u64;
	base + (AP_STACK_WORDS as u64 * 16)
}

// Enumerate the enabled processors' LAPIC ids from the ACPI MADT, reachable via
// the RSDP the loader passed (0 if the firmware exposed none). All ACPI tables are
// read through the HHDM. Returns an empty vec if there is no RSDP or no MADT.
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
		find_table(hhdm, xsdt, 8)
	} else {
		let rsdt = unsafe { core::ptr::read_unaligned(rsdp.add(16) as *const u32) } as u64;
		find_table(hhdm, rsdt, 4)
	};
	let Some(madt) = madt else {
		return out;
	};
	parse_madt(hhdm, madt, &mut out);
	out
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
fn table_signature(hhdm: u64, phys: u64) -> Option<[u8; 4]> {
	if !mem::within_direct_map(phys, 36) {
		return None;
	}
	let p = (hhdm + phys) as *const u8;
	Some(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] })
}

// The `length` field (offset 4) of the ACPI table header at physical `phys`.
fn table_length(hhdm: u64, phys: u64) -> Option<u32> {
	if !mem::within_direct_map(phys, 36) {
		return None;
	}
	Some(unsafe { core::ptr::read_unaligned((hhdm + phys + 4) as *const u32) })
}

// Scan an RSDT/XSDT (entry pointers are `ptr_size` bytes each, after the 36-byte
// header) for the MADT (signature "APIC"), returning its physical address.
fn find_table(hhdm: u64, sdt_phys: u64, ptr_size: usize) -> Option<u64> {
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
		if table_signature(hhdm, phys) == Some(*b"APIC") && table_ok(hhdm, phys) {
			return Some(phys);
		}
	}
	None
}

// Walk the MADT's interrupt-controller structures, collecting the LAPIC id of each
// enabled Processor Local APIC (type 0, flags bit 0). Entries start at offset 44
// (36-byte header + 4-byte local APIC address + 4-byte flags).
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
				out.push(apic_id);
			}
		}
		off += elen;
	}
}

fn report(cpu_id: usize, lapic_id: u32) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	unsafe { (*base.add(cpu_id)).store(lapic_id, Ordering::Relaxed) };
}

#[cfg(test)]
mod tests;
