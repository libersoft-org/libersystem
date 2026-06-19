// Virtual-address-space layout (x86_64, 4-level paging, 48-bit canonical).
//
// The single place that fixes where each region of the address space lives, so
// the windows below cannot silently overlap. The lower half [0, USER_VA_END) is
// user space (per-process page tables); the higher half is the kernel, shared
// across every address space.
//
//   lower half (user, per-process page tables)
//     0x0000_0000_4000_0000  USER_CODE_VA      M8 embedded-demo code page (1 GiB)
//     0x0000_0000_4001_0000  USER_STACK_VA     M8 embedded-demo stack page
//     0x0000_0000_8000_0000  USER_STACK_TOP    ELF-loaded program's ring-3 stack top (2 GiB)
//     0x0000_4000_0000_0000  USER_MMAP_BASE    ring-3 syscall-mapped objects (bump up)
//     0x0000_8000_0000_0000  USER_VA_END       exclusive top of the user half
//   higher half (kernel, shared across address spaces)
//     0xffff_e800_0000_0000  KERNEL_MMAP_BASE  kernel syscall-mapped objects (bump up)

// M8 embedded ring-3 demo/test: one page for the program, one for its stack,
// mapped into the low half of the shared address space (per-process CR3
// isolation is a later milestone).
pub(crate) const USER_CODE_VA: u64 = 0x0000_0000_4000_0000;
pub(crate) const USER_STACK_VA: u64 = 0x0000_0000_4001_0000;

// ELF-loaded process ring-3 stack: it lives just below the 2 GiB line (well above
// the program's load address and clear of the kernel's higher half) and grows
// down from USER_STACK_TOP over USER_STACK_PAGES pages.
pub(crate) const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;
pub(crate) const USER_STACK_PAGES: u64 = 4;

// Ring-3 syscall-mapped MemoryObjects bump up from here. The base sits far above
// the program and stack the loader places below the 2 GiB line, yet within the
// user (lower) half, so user_buf_ok still accepts buffers carved from it.
pub(crate) const USER_MMAP_BASE: u64 = 0x0000_4000_0000_0000;

// Exclusive upper bound of the user (lower-half) virtual-address range: a ring-3
// syscall may only hand the kernel pointers below this.
pub(crate) const USER_VA_END: u64 = 0x0000_8000_0000_0000;

// Kernel virtual-address window for syscall-mapped MemoryObjects (the kernel-side
// counterpart of USER_MMAP_BASE). A bump pointer hands out non-overlapping ranges.
pub(crate) const KERNEL_MMAP_BASE: u64 = 0xffff_e800_0000_0000;
