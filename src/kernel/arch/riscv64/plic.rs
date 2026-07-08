// riscv64 PLIC - the Platform-Level Interrupt Controller (M117).
//
// The PLIC (QEMU virt: base from the device tree, "plic@c000000") routes wired device
// interrupts (PCI INTx, the 16550 UART, etc.) to a hart's S-mode external-interrupt
// line (SCAUSE code 9). Each (hart, privilege) pair is a PLIC "context"; on QEMU virt
// context 2*H + 1 is hart H's S-mode context. A source fires when its priority is
// non-zero, it is enabled in a context, and its priority exceeds that context's
// threshold. The S-mode handler claims the highest-priority pending source, services
// it, then writes the id back to complete it.
//
// Register map (offsets from the PLIC base):
//   priority[source]        = 0x0000 + 4 * source
//   enable[context][word]   = 0x2000 + 0x80 * context + 4 * word   (bit = source % 32)
//   threshold[context]      = 0x20_0000 + 0x1000 * context
//   claim/complete[context] = 0x20_0004 + 0x1000 * context

#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

// Highest interrupt source id the QEMU virt PLIC supports.
const MAX_SOURCES: usize = 96;

// PLIC MMIO base (from the device tree). Reached through the higher-half direct map.
static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);

// The boot hart id, captured at init. Device INTx sources are enabled and claimed on
// this hart's S-mode context only, so their completion (from a driver's ack, which may
// run on any hart) always targets the same context.
static BOOT_HART: AtomicUsize = AtomicUsize::new(0);

// Per-source kernel handlers (indexed by source id). A device driver registers one and
// enables its source; the S-mode external-interrupt trap dispatches through here.
static mut HANDLERS: [Option<fn()>; MAX_SOURCES] = [None; MAX_SOURCES];

// The S-mode PLIC context for hart `hartid` on QEMU virt.
fn s_context(hartid: u64) -> usize {
	2 * hartid as usize + 1
}

fn base() -> usize {
	PLIC_BASE.load(Ordering::Relaxed)
}

unsafe fn reg(off: usize) -> *mut u32 {
	super::paging::phys_to_virt((base() + off) as u64) as *mut u32
}

// Record the PLIC base discovered in the device tree.
pub fn set_base(addr: u64) {
	PLIC_BASE.store(addr as usize, Ordering::Relaxed);
}

// True once the PLIC base is known (from the device tree).
pub fn ready() -> bool {
	base() != 0
}

// Bring up the PLIC on the boot hart: clear every source priority (all masked) and open
// this hart's S-mode threshold so any enabled, non-zero-priority source can fire.
pub fn init(hartid: u64) {
	BOOT_HART.store(hartid as usize, Ordering::Relaxed);
	if !ready() {
		return;
	}
	unsafe {
		for source in 1..MAX_SOURCES {
			reg(4 * source).write_volatile(0);
		}
	}
	init_hart(hartid);
}

// The boot hart id (the S-mode PLIC context device INTx sources are routed to).
pub fn boot_hart() -> u64 {
	BOOT_HART.load(Ordering::Relaxed) as u64
}

// Open a hart's S-mode PLIC threshold (accept any priority > 0). Called on every hart.
pub fn init_hart(hartid: u64) {
	if !ready() {
		return;
	}
	unsafe {
		reg(0x20_0000 + 0x1000 * s_context(hartid)).write_volatile(0);
	}
}

// Register a kernel handler for `source` and route + unmask it on `hartid`'s S-context.
pub fn register(source: u32, hartid: u64, handler: fn()) {
	if source == 0 || source as usize >= MAX_SOURCES {
		return;
	}
	unsafe {
		(*(&raw mut HANDLERS))[source as usize] = Some(handler);
	}
	enable_source(source, hartid);
}

// Give `source` a non-zero priority and enable it in `hartid`'s S-mode context.
pub fn enable_source(source: u32, hartid: u64) {
	if !ready() || source == 0 || source as usize >= MAX_SOURCES {
		return;
	}
	let ctx = s_context(hartid);
	unsafe {
		reg(4 * source as usize).write_volatile(1); // priority 1
		let word = source as usize / 32;
		let bit = source % 32;
		let addr = reg(0x2000 + 0x80 * ctx + 4 * word);
		addr.write_volatile(addr.read_volatile() | (1 << bit));
	}
}

// Mask `source`: clear its priority and disable it in `hartid`'s S-mode context, so a
// gone driver's device cannot re-assert its wired line into the PLIC.
pub fn disable_source(source: u32, hartid: u64) {
	if !ready() || source == 0 || source as usize >= MAX_SOURCES {
		return;
	}
	let ctx = s_context(hartid);
	unsafe {
		let word = source as usize / 32;
		let bit = source % 32;
		let addr = reg(0x2000 + 0x80 * ctx + 4 * word);
		addr.write_volatile(addr.read_volatile() & !(1 << bit));
		reg(4 * source as usize).write_volatile(0); // priority 0
	}
}

// Claim the highest-priority pending source for `hartid` (0 if none).
pub fn claim(hartid: u64) -> u32 {
	if !ready() {
		return 0;
	}
	unsafe { reg(0x20_0004 + 0x1000 * s_context(hartid)).read_volatile() }
}

// Signal completion of `source` on `hartid`.
pub fn complete(hartid: u64, source: u32) {
	if !ready() {
		return;
	}
	unsafe {
		reg(0x20_0004 + 0x1000 * s_context(hartid)).write_volatile(source);
	}
}

// Service an S-mode external interrupt (SCAUSE code 9): claim the pending source,
// dispatch its kernel handler, then complete it. Called from the trap handler.
pub fn handle_external(hartid: u64) {
	loop {
		let source = claim(hartid);
		if source == 0 {
			break;
		}
		// A device INTx bound to a userspace driver is LEVEL-triggered: signal the driver
		// and leave the source claimed (the PLIC gateway masks it meanwhile) so it cannot
		// re-fire. The driver deasserts its device line (reads the virtio ISR / clears the
		// xHCI interrupt bit) and completes the source through SYS_INTERRUPT_ACK. Any other
		// source runs its kernel handler and completes at once.
		if super::interrupts::dispatch_intx(source) {
			continue;
		}
		let handler = unsafe { (*(&raw const HANDLERS))[source as usize] };
		if let Some(f) = handler {
			f();
		}
		complete(hartid, source);
	}
}
