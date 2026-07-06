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
use crate::sync::SpinLock;

// Total cores we manage (BSP + woken APs).
static CPU_COUNT: AtomicUsize = AtomicUsize::new(1);

// Cores that have completed per-CPU init and reported in (BSP starts counted).
static ONLINE: AtomicUsize = AtomicUsize::new(1);

// Each core's LAPIC id by CPU id, retained at report-in so the CPU topology stays
// inspectable at runtime - SYS_CPU_INFO reads it for `lscpu`. Allocated by init,
// sized by the machine's real core count.
static LAPIC_IDS: AtomicPtr<AtomicU32> = AtomicPtr::new(core::ptr::null_mut());

// Serializes report-in lines so concurrent cores do not interleave their output.
static REPORT_LOCK: SpinLock<()> = SpinLock::new(());

// The CPU id and LAPIC id the next application processor reads on entry. APs are
// woken one at a time and each is waited on before the next, so a single slot
// suffices (the AP has consumed both long before the next wake overwrites them).
static AP_CPU_ID: AtomicUsize = AtomicUsize::new(0);
static AP_LAPIC_ID: AtomicU32 = AtomicU32::new(0);

// Each application processor's kernel stack in 16-byte words (64 KiB), 16-aligned
// (a Box<[u128]>) so the trampoline's `call` into Rust lands ABI-aligned.
const AP_STACK_WORDS: usize = 4096;

// Number of cores brought under kernel management.
pub fn cpu_count() -> usize {
	CPU_COUNT.load(Ordering::Relaxed)
}

// Number of cores currently online.
pub fn online_count() -> usize {
	ONLINE.load(Ordering::Acquire)
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
	report(0, bsp_lapic_id, true);

	// Wake the application processors, one at a time, via the real-mode trampoline
	// the loader reserved a low page for. Nothing to do (and nowhere to land the
	// trampoline) on a single-core machine.
	let tramp_phys = boot_info.smp_trampoline;
	if total > 1 && tramp_phys != 0 {
		let tramp = (mem::hhdm_offset() + tramp_phys) as *mut u8;
		let vector = (tramp_phys >> 12) as u8;
		// The trampoline runs on the shared page tables (our CR3) and calls ap_entry.
		unsafe { arch::apboot::install(tramp, read_cr3(), ap_entry as *const () as u64) };

		let mut online = 1usize; // the BSP
		for &lapic in &lapics {
			if lapic == bsp_lapic_id {
				continue;
			}
			let cpu_id = online; // contiguous id for the next core to report in
			let stack = alloc_ap_stack();
			AP_CPU_ID.store(cpu_id, Ordering::SeqCst);
			AP_LAPIC_ID.store(lapic, Ordering::SeqCst);
			unsafe { arch::apboot::set_stack(tramp, stack) };

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
	let cpu_id = AP_CPU_ID.load(Ordering::SeqCst);
	let lapic_id = AP_LAPIC_ID.load(Ordering::SeqCst);
	arch::init_ap(cpu_id, lapic_id);
	// Report (under the lock) before counting online, so the BSP - which waits on
	// the online count - does not resume and print until this core's report-in line
	// has been emitted.
	report(cpu_id, lapic_id, false);
	ONLINE.fetch_add(1, Ordering::Release);
	crate::sched::cpu_idle_loop()
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

// The current CR3 (the shared page tables the loader built), for the AP mailbox.
fn read_cr3() -> u64 {
	let cr3: u64;
	unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)) };
	cr3
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
	let rsdp = (hhdm + rsdp_phys) as *const u8;
	// RSDP: revision at offset 15; RSDT (u32) at 16 for revision 0/1, XSDT (u64) at
	// 24 for revision 2+.
	let revision = unsafe { *rsdp.add(15) };
	let madt = if revision >= 2 {
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

// The 4-byte signature of the ACPI table at physical `phys` (read via the HHDM).
fn table_signature(hhdm: u64, phys: u64) -> [u8; 4] {
	let p = (hhdm + phys) as *const u8;
	unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] }
}

// The `length` field (offset 4) of the ACPI table header at physical `phys`.
fn table_length(hhdm: u64, phys: u64) -> u32 {
	unsafe { core::ptr::read_unaligned((hhdm + phys + 4) as *const u32) }
}

// Scan an RSDT/XSDT (entry pointers are `ptr_size` bytes each, after the 36-byte
// header) for the MADT (signature "APIC"), returning its physical address.
fn find_table(hhdm: u64, sdt_phys: u64, ptr_size: usize) -> Option<u64> {
	if sdt_phys == 0 {
		return None;
	}
	let len = table_length(hhdm, sdt_phys) as usize;
	if len < 36 {
		return None;
	}
	let base = (hhdm + sdt_phys + 36) as *const u8;
	let count = (len - 36) / ptr_size;
	for i in 0..count {
		let entry = unsafe { base.add(i * ptr_size) };
		let phys = if ptr_size == 8 { unsafe { core::ptr::read_unaligned(entry as *const u64) } } else { unsafe { core::ptr::read_unaligned(entry as *const u32) as u64 } };
		if table_signature(hhdm, phys) == *b"APIC" {
			return Some(phys);
		}
	}
	None
}

// Walk the MADT's interrupt-controller structures, collecting the LAPIC id of each
// enabled Processor Local APIC (type 0, flags bit 0). Entries start at offset 44
// (36-byte header + 4-byte local APIC address + 4-byte flags).
fn parse_madt(hhdm: u64, madt_phys: u64, out: &mut Vec<u32>) {
	let len = table_length(hhdm, madt_phys) as usize;
	let base = (hhdm + madt_phys) as *const u8;
	let mut off = 44usize;
	while off + 2 <= len {
		let etype = unsafe { *base.add(off) };
		let elen = unsafe { *base.add(off + 1) } as usize;
		if elen == 0 {
			break;
		}
		if etype == 0 && off + 8 <= len {
			let apic_id = unsafe { *base.add(off + 3) } as u32;
			let flags = unsafe { core::ptr::read_unaligned(base.add(off + 4) as *const u32) };
			if flags & 1 != 0 {
				out.push(apic_id);
			}
		}
		off += elen;
	}
}

fn report(cpu_id: usize, lapic_id: u32, is_bsp: bool) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	unsafe { (*base.add(cpu_id)).store(lapic_id, Ordering::Relaxed) };
	let _guard = REPORT_LOCK.lock();
	let role = if is_bsp { "BSP" } else { "AP" };
	crate::serial_println!("cpu {} ({}) online, lapic_id {}", cpu_id, role, lapic_id);
}
