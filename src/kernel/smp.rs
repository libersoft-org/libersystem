// Symmetric multiprocessing: bring application processors online.
//
// Limine starts each application processor (AP) for us and parks it until we
// write its per-CPU `goto_address`. We size every per-CPU table from the MP
// response first - the heap is up long before any core initializes its slot -
// then assign every core a contiguous CPU id (the bootstrap processor is 0),
// wake each AP into `ap_entry`, and wait until all of them have run their
// per-CPU init and reported in.

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use alloc::vec::Vec;
use limine::mp::Cpu;
use limine::response::MpResponse;

use crate::arch;
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
pub fn init(mp: &MpResponse) {
	let bsp_lapic_id = mp.bsp_lapic_id();

	// Size every per-CPU table from the machine's real core count before any
	// core - the BSP included - initializes its slot. Counted the same way the
	// wake loop below assigns ids, so the tables and the id space always agree.
	let total = 1 + mp.cpus().iter().filter(|cpu| cpu.lapic_id != bsp_lapic_id).count();
	let mut ids: Vec<AtomicU32> = Vec::with_capacity(total);
	ids.resize_with(total, || AtomicU32::new(0));
	LAPIC_IDS.store(Vec::leak(ids).as_mut_ptr(), Ordering::Release);
	arch::percpu::allocate(total);
	crate::sched::allocate(total);
	CPU_COUNT.store(total, Ordering::Relaxed);

	// x2APIC honesty: our MSI message address encodes an 8-bit xAPIC destination,
	// so a core whose LAPIC id does not fit one byte (a >255-core machine) cannot
	// be targeted by device interrupts until x2APIC addressing lands. Say so
	// loudly rather than truncating ids silently.
	if mp.cpus().iter().any(|cpu| cpu.lapic_id > u8::MAX as u32) {
		crate::serial_println!("smp: WARNING: LAPIC ids beyond 255 present; MSI delivery (8-bit xAPIC destination) cannot target those cores - x2APIC addressing is not implemented yet");
	}

	arch::init_bsp_percpu(bsp_lapic_id);
	report(0, bsp_lapic_id, true);

	let mut next_id = 1usize;
	for cpu in mp.cpus() {
		if cpu.lapic_id == bsp_lapic_id {
			continue;
		}
		let cpu_id = next_id;
		next_id += 1;
		// Publish the assigned id before waking the core; GotoAddress::write
		// synchronizes, so the AP is guaranteed to observe this store.
		cpu.extra.store(cpu_id as u64, Ordering::SeqCst);
		cpu.goto_address.write(ap_entry);
	}
	CPU_COUNT.store(next_id, Ordering::Relaxed);

	while online_count() < cpu_count() {
		core::hint::spin_loop();
	}
}

// Entry point each application processor jumps to. Limine gives it its own
// stack; we set up the per-core state, report in, then park the core.
unsafe extern "C" fn ap_entry(cpu: &Cpu) -> ! {
	let cpu_id = cpu.extra.load(Ordering::SeqCst) as usize;
	arch::init_ap(cpu_id, cpu.lapic_id);
	// Report (under the lock) before counting online, so the BSP - which waits
	// on the online count - does not resume and print until every core's
	// report-in line has been emitted.
	report(cpu_id, cpu.lapic_id, false);
	ONLINE.fetch_add(1, Ordering::Release);
	// Park in the scheduler idle loop so threads can be scheduled onto this core.
	crate::sched::cpu_idle_loop()
}

fn report(cpu_id: usize, lapic_id: u32, is_bsp: bool) {
	let base = LAPIC_IDS.load(Ordering::Acquire);
	unsafe { (*base.add(cpu_id)).store(lapic_id, Ordering::Relaxed) };
	let _guard = REPORT_LOCK.lock();
	let role = if is_bsp { "BSP" } else { "AP" };
	crate::serial_println!("cpu {} ({}) online, lapic_id {}", cpu_id, role, lapic_id);
}
