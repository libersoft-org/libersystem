// Virtual-address-space layout (x86_64, 4-level paging, 48-bit canonical).
//
// The single place that fixes where each region of the address space lives, so
// the windows below cannot silently overlap. The lower half [0, USER_VA_END) is
// user space (per-process page tables); the higher half is the kernel, shared
// across every address space.
//
//   lower half (user, per-process page tables)
//     0x0000_0000_4000_0000  USER_CODE_VA      ring-3 test code page (1 GB)
//     0x0000_0000_4001_0000  USER_STACK_VA     ring-3 test stack page
//     0x0000_0000_8000_0000  USER_STACK_TOP    ELF-loaded program's ring-3 stack top (2 GB)
//     0x0000_4000_0000_0000  USER_MMAP_BASE    ring-3 syscall-mapped objects (pooled)
//     0x0000_8000_0000_0000  USER_VA_END       exclusive top of the user half
//   higher half (kernel, shared across address spaces)
//     0xffff_e800_0000_0000  KERNEL_MMAP_BASE  kernel syscall-mapped objects (pooled)

// In-kernel ring-3 test: one page for the program, one for its stack, mapped into
// the low half of the shared address space (per-process CR3 isolation is a later
// milestone).
#[cfg(test)]
pub(crate) const USER_CODE_VA: u64 = 0x0000_0000_4000_0000;
#[cfg(test)]
pub(crate) const USER_STACK_VA: u64 = 0x0000_0000_4001_0000;

// ELF-loaded process ring-3 stack: it lives just below the 2 GB line (well above
// the program's load address and clear of the kernel's higher half) and grows
// down from USER_STACK_TOP. USER_STACK_TOP is part of the spawn ABI (a userspace
// spawner passes it to thread_create), so its value is sourced from the abi
// crate. Only the top USER_STACK_PAGES are mapped eagerly; the rest of the span -
// up to the owning Domain's per-thread stack ceiling (PROP_STACK_LIMIT, 8 MB by
// default) - is demand-paged by the fault handler as the stack grows into it, so
// a deep call chain costs only the pages it actually touches.
pub(crate) const USER_STACK_TOP: u64 = abi::USER_STACK_TOP;
pub(crate) const USER_STACK_PAGES: u64 = 8;

// Ring-3 syscall-mapped MemoryObjects are allocated from here (the user window's
// pool: reused released ranges first, then the bump). The base sits far above
// the program and stack the loader places below the 2 GB line, yet within the
// user (lower) half, so user_buf_ok still accepts buffers carved from it. On
// riscv64 the user half is Sv39's 39-bit low canonical range [0, 0x40_0000_0000),
// so the 48-bit x86/aarch64 base is non-canonical there and the window is moved
// down to 128 GiB (still well clear of the sub-2-GiB program, stack and heap).
#[cfg(not(target_arch = "riscv64"))]
pub(crate) const USER_MMAP_BASE: u64 = 0x0000_4000_0000_0000;
#[cfg(target_arch = "riscv64")]
pub(crate) const USER_MMAP_BASE: u64 = 0x0000_0020_0000_0000;

// Exclusive upper bound of the user (lower-half) virtual-address range: a ring-3
// syscall may only hand the kernel pointers below this. On riscv64 it is the top of
// the Sv39 low canonical half (256 GiB).
#[cfg(not(target_arch = "riscv64"))]
pub(crate) const USER_VA_END: u64 = 0x0000_8000_0000_0000;
#[cfg(target_arch = "riscv64")]
pub(crate) const USER_VA_END: u64 = 0x0000_0040_0000_0000;

// Kernel virtual-address window for syscall-mapped MemoryObjects (the kernel-side
// counterpart of USER_MMAP_BASE). Its pool hands out non-overlapping ranges and
// reclaims released ones.
pub(crate) const KERNEL_MMAP_BASE: u64 = 0xffff_e800_0000_0000;
