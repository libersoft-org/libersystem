// Global Descriptor Table and Task State Segment (x86_64).
//
// In long mode segmentation is largely vestigial, but the CPU still requires a
// valid code and data segment plus a TSS. The TSS provides the Interrupt Stack
// Table (IST): a known-good stack the CPU switches to when taking a critical
// fault such as a double fault, even if the active kernel stack is corrupt.
//
// SMP: every core needs its OWN TSS, because `ltr` marks a TSS busy and loading
// an already-busy TSS on a second core faults. Each core gets its own small GDT:
// a single shared GDT would cap the machine at ~4093 cores
// (16-byte TSS descriptors against the 64 kB GDT limit), and per-core GDTs also
// let every core use the same TSS selector. The BSP's area (GDT + TSS + fault
// stacks) is static - it is needed before any allocator runs - while each AP's
// is allocated from the kernel heap when the core is brought online, so absent
// cores cost nothing.

use core::arch::asm;
use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut};

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;

// User-mode selectors. The layout is fixed by SYSRET: from STAR[63:48] = 0x18 the
// CPU derives SS = 0x18 + 8 = 0x20 and CS = 0x18 + 16 = 0x28 (RPL forced to 3).
// The 32-bit user code entry exists only to anchor that base; long mode runs the
// 64-bit user code segment.
pub const USER_CODE32_SELECTOR: u16 = 0x18;
pub const USER_DATA_SELECTOR: u16 = 0x20;
pub const USER_CODE64_SELECTOR: u16 = 0x28;

// Every core's TSS descriptor sits at the same slot of its own GDT.
const TSS_SELECTOR: u16 = (6 * size_of::<u64>()) as u16;

// IST slot (1-based, as encoded in an IDT gate) for the double-fault handler.
pub const DOUBLE_FAULT_IST_INDEX: u8 = 1;

const IST_STACK_SIZE: usize = 4096 * 5;
// Per-core ring-0 stack the CPU switches to (via TSS.RSP0) when an interrupt or
// exception is taken while running in ring 3. Only a fallback: once a thread
// enters ring 3, TSS.RSP0 tracks that thread's own parked kernel stack (set by
// usermode::enter and restored by the scheduler on every context switch), so a
// ring-3 interrupt frame lands on a stack that travels with the thread.
const RSP0_STACK_SIZE: usize = 4096 * 5;

// null, kernel code, kernel data, three user segments, then the two entries of
// this core's own TSS descriptor.
const GDT_ENTRIES: usize = 6 + 2;

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

// Everything one core needs: its own GDT, TSS, and the fault / ring-entry stacks
// the TSS points at. The BSP's is the static instance below (pre-allocator); each
// AP gets one leaked from the kernel heap at bring-up.
#[repr(C)]
struct CpuArea {
	gdt: [u64; GDT_ENTRIES],
	tss: Tss,
	double_fault_stack: [u8; IST_STACK_SIZE],
	rsp0_stack: [u8; RSP0_STACK_SIZE],
}

impl CpuArea {
	const fn new() -> Self {
		Self { gdt: [0; GDT_ENTRIES], tss: Tss::new(), double_fault_stack: [0; IST_STACK_SIZE], rsp0_stack: [0; RSP0_STACK_SIZE] }
	}
}

static mut BSP_AREA: CpuArea = CpuArea::new();

#[repr(C, packed)]
struct DescriptorPointer {
	limit: u16,
	base: u64,
}

// Fill one core's area: the fixed segment descriptors, its TSS descriptor, and
// the TSS's IST / RSP0 stack pointers - then load the GDT, reload the segments,
// and mark the TSS active on this core.
unsafe fn install(area: *mut CpuArea) {
	unsafe {
		let gdt = &mut (*area).gdt;
		gdt[0] = 0;
		gdt[1] = 0x00AF_9A00_0000_FFFF; // kernel code: present, ring0, exec/read, L=1
		gdt[2] = 0x00CF_9200_0000_FFFF; // kernel data: present, ring0, read/write
		gdt[3] = 0x00CF_FA00_0000_FFFF; // user code 32: present, ring3, exec/read, D=1
		gdt[4] = 0x00CF_F200_0000_FFFF; // user data: present, ring3, read/write
		gdt[5] = 0x00AF_FA00_0000_FFFF; // user code 64: present, ring3, exec/read, L=1

		// IST1 points at the top of this core's double-fault stack (stacks grow
		// down); RSP0 at the top of its ring-entry stack.
		let stack_base = addr_of!((*area).double_fault_stack) as u64;
		(*area).tss.interrupt_stack_table[(DOUBLE_FAULT_IST_INDEX - 1) as usize] = stack_base + IST_STACK_SIZE as u64;
		let rsp0_base = addr_of!((*area).rsp0_stack) as u64;
		(*area).tss.privilege_stack_table[0] = rsp0_base + RSP0_STACK_SIZE as u64;
		// No I/O permission bitmap: point past the TSS limit (set explicitly - a
		// heap-allocated area arrives zeroed, not through Tss::new).
		(*area).tss.iomap_base = size_of::<Tss>() as u16;

		let tss_base = addr_of!((*area).tss) as u64;
		let (low, high) = tss_descriptor(tss_base, (size_of::<Tss>() - 1) as u32);
		gdt[6] = low;
		gdt[7] = high;

		load_gdt(area);
		reload_segments();
		load_tss();
	}
}

// BSP bring-up (called once, before any allocator): install the static area.
pub fn init() {
	unsafe {
		install(addr_of_mut!(BSP_AREA));
	}
}

// Per-core bring-up for an application processor: allocate the core's own area
// straight on the kernel heap (alive by the time APs start) and install it. The
// allocation is raw and zeroed on purpose - a Box::new would construct the ~41 kB
// value on the AP's small bootstrap stack first and overflow it. The area is
// leaked deliberately: a core's GDT/TSS live for the life of the machine.
pub fn load_ap(_cpu: usize) {
	let area = unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<CpuArea>()) } as *mut CpuArea;
	if area.is_null() {
		panic!("out of memory: AP GDT/TSS area");
	}
	unsafe {
		install(area);
	}
}

unsafe fn load_gdt(area: *const CpuArea) {
	unsafe {
		let ptr = DescriptorPointer { limit: (size_of::<[u64; GDT_ENTRIES]>() - 1) as u16, base: addr_of!((*area).gdt) as u64 };
		asm!("lgdt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
	}
}

unsafe fn load_tss() {
	unsafe {
		asm!("ltr {0:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
	}
}

// The address of the running core's TSS.RSP0 slot, derived from the live GDTR:
// read the TSS descriptor out of this core's GDT and offset to
// privilege_stack_table[0] (right after the leading reserved u32). The per-CPU
// block keeps this pointer so the scheduler and the ring-3 entry path can retarget
// RSP0 at the current thread's kernel stack without knowing the CpuArea's address.
pub fn rsp0_slot_addr() -> u64 {
	let mut ptr = DescriptorPointer { limit: 0, base: 0 };
	unsafe {
		asm!("sgdt [{}]", in(reg) &mut ptr, options(nostack, preserves_flags));
	}
	let gdt = ptr.base as *const u64;
	let low = unsafe { gdt.add(6).read() };
	let high = unsafe { gdt.add(7).read() };
	let tss_base = ((low >> 16) & 0xFF_FFFF) | (((low >> 56) & 0xFF) << 24) | ((high & 0xFFFF_FFFF) << 32);
	tss_base + 4
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
