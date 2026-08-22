// The loader's ELF reader is the shared `bootproto::elf` parser (the loader and the
// kernel load ELF images the same way; only the mapping differs). Re-exported here so
// the arch backends keep referring to it as `crate::elf::*`; not every arch backend
// names every symbol.
pub use bootproto::elf::{Elf, PT_LOAD};
// The segment permission bits, read by the x86_64 backend when it describes each kernel segment to
// the kernel. The other two place segments by `p_paddr` and never look at the flags.
#[cfg(target_arch = "x86_64")]
pub use bootproto::elf::{PF_W, PF_X};
