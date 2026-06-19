// Global Descriptor Table and Task State Segment (x86_64).
//
// In long mode segmentation is largely vestigial, but the CPU still requires a
// valid code and data segment plus a TSS. The TSS provides the Interrupt Stack
// Table (IST): a known-good stack the CPU switches to when taking a critical
// fault such as a double fault, even if the active kernel stack is corrupt.
//
// SMP: every core needs its OWN TSS, because `ltr` marks a TSS busy and loading
// an already-busy TSS on a second core faults. The GDT therefore carries one TSS
// descriptor per core (indexed by our contiguous CPU id), and each core loads
// the selector for its own slot.

use core::arch::asm;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

use super::percpu::MAX_CPUS;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;

// User-mode selectors. The layout is fixed by SYSRET: from STAR[63:48] = 0x18 the
// CPU derives SS = 0x18 + 8 = 0x20 and CS = 0x18 + 16 = 0x28 (RPL forced to 3).
// The 32-bit user code entry exists only to anchor that base; long mode runs the
// 64-bit user code segment.
pub const USER_CODE32_SELECTOR: u16 = 0x18;
pub const USER_DATA_SELECTOR: u16 = 0x20;
pub const USER_CODE64_SELECTOR: u16 = 0x28;

// IST slot (1-based, as encoded in an IDT gate) for the double-fault handler.
pub const DOUBLE_FAULT_IST_INDEX: u8 = 1;

const IST_STACK_SIZE: usize = 4096 * 5;
// Per-core ring-0 stack the CPU switches to (via TSS.RSP0) when an interrupt or
// exception is taken while running in ring 3.
const RSP0_STACK_SIZE: usize = 4096 * 5;

// null, kernel code, kernel data, three user segments, then two entries per
// per-core TSS descriptor.
const GDT_ENTRIES: usize = 6 + 2 * MAX_CPUS;

static mut DOUBLE_FAULT_STACKS: [[u8; IST_STACK_SIZE]; MAX_CPUS] = [[0; IST_STACK_SIZE]; MAX_CPUS];
static mut RSP0_STACKS: [[u8; RSP0_STACK_SIZE]; MAX_CPUS] = [[0; RSP0_STACK_SIZE]; MAX_CPUS];

#[repr(C, packed)]
struct Tss {
	reserved0: u32,
	privilege_stack_table: [u64; 3],
	reserved1: u64,
	interrupt_stack_table: [u64; 7],
	reserved2: u64,
	reserved3: u16,
	iomap_base: u16,
}

impl Tss {
	const fn new() -> Self {
		Self { reserved0: 0, privilege_stack_table: [0; 3], reserved1: 0, interrupt_stack_table: [0; 7], reserved2: 0, reserved3: 0, iomap_base: size_of::<Tss>() as u16 }
	}
}

static mut TSS: [Tss; MAX_CPUS] = [const { Tss::new() }; MAX_CPUS];

static mut GDT: [u64; GDT_ENTRIES] = [0; GDT_ENTRIES];

#[repr(C, packed)]
struct DescriptorPointer {
	limit: u16,
	base: u64,
}

// GDT byte offset (selector) of the TSS descriptor for a given CPU id.
fn tss_selector(cpu: usize) -> u16 {
	((6 + 2 * cpu) * size_of::<u64>()) as u16
}

// Build the whole GDT (called once on the BSP): code/data segments plus one TSS
// descriptor per core, each with its own double-fault IST stack. The BSP then
// loads it and selects its own TSS (CPU id 0).
pub fn init() {
	unsafe {
		let gdt = &mut *addr_of_mut!(GDT);
		gdt[0] = 0;
		gdt[1] = 0x00AF_9A00_0000_FFFF; // kernel code: present, ring0, exec/read, L=1
		gdt[2] = 0x00CF_9200_0000_FFFF; // kernel data: present, ring0, read/write
		gdt[3] = 0x00CF_FA00_0000_FFFF; // user code 32: present, ring3, exec/read, D=1
		gdt[4] = 0x00CF_F200_0000_FFFF; // user data: present, ring3, read/write
		gdt[5] = 0x00AF_FA00_0000_FFFF; // user code 64: present, ring3, exec/read, L=1

		for cpu in 0..MAX_CPUS {
			// IST1 points at the top of this core's double-fault stack (stacks
			// grow down).
			let stack_base = addr_of!(DOUBLE_FAULT_STACKS) as u64 + (cpu * IST_STACK_SIZE) as u64;
			let tss_base = addr_of!(TSS) as u64 + (cpu * size_of::<Tss>()) as u64;
			let tss = &mut *(tss_base as *mut Tss);
			tss.interrupt_stack_table[(DOUBLE_FAULT_IST_INDEX - 1) as usize] = stack_base + IST_STACK_SIZE as u64;
			// RSP0: the ring-0 stack used when entering the kernel from ring 3.
			let rsp0_base = addr_of!(RSP0_STACKS) as u64 + (cpu * RSP0_STACK_SIZE) as u64;
			tss.privilege_stack_table[0] = rsp0_base + RSP0_STACK_SIZE as u64;

			let (low, high) = tss_descriptor(tss_base, (size_of::<Tss>() - 1) as u32);
			gdt[6 + 2 * cpu] = low;
			gdt[7 + 2 * cpu] = high;
		}

		load_gdt();
		reload_segments();
		load_tss(0);
	}
}

// Per-core bring-up for an application processor: load the shared GDT, reload
// segments, and mark this core's own TSS busy.
pub fn load_ap(cpu: usize) {
	unsafe {
		load_gdt();
		reload_segments();
		load_tss(cpu);
	}
}

unsafe fn load_gdt() {
	unsafe {
		let ptr = DescriptorPointer { limit: (size_of::<[u64; GDT_ENTRIES]>() - 1) as u16, base: addr_of!(GDT) as u64 };
		asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
	}
}

unsafe fn load_tss(cpu: usize) {
	unsafe {
		asm!("ltr {0:x}", in(reg) tss_selector(cpu), options(nostack, preserves_flags));
	}
}

// Build the two 64-bit halves of a 64-bit TSS system descriptor.
fn tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
	let mut low: u64 = 0;
	low |= limit as u64 & 0xFFFF; // limit 0..15
	low |= ((limit as u64 >> 16) & 0xF) << 48; // limit 16..19
	low |= (base & 0xFF_FFFF) << 16; // base 0..23
	low |= ((base >> 24) & 0xFF) << 56; // base 24..31
	low |= 0x89u64 << 40; // present, type = available 64-bit TSS
	let high = (base >> 32) & 0xFFFF_FFFF; // base 32..63
	(low, high)
}

// Reload CS via a far return and the data/stack segments via plain moves, so the
// CPU uses our freshly installed descriptors instead of the bootloader's.
unsafe fn reload_segments() {
	unsafe {
		asm!(
			"push {sel}",
			"lea {tmp}, [rip + 2f]",
			"push {tmp}",
			"retfq",
			"2:",
			sel = in(reg) KERNEL_CODE_SELECTOR as u64,
			tmp = lateout(reg) _,
			options(preserves_flags),
		);
		asm!(
			"mov ds, {0:x}",
			"mov es, {0:x}",
			"mov ss, {0:x}",
			"mov fs, {0:x}",
			"mov gs, {0:x}",
			in(reg) KERNEL_DATA_SELECTOR,
			options(nostack, preserves_flags),
		);
	}
}
