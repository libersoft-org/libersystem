// The loader's ELF reader is the shared `bootproto::elf` parser (the loader and the
// kernel load ELF images the same way; only the mapping differs). Re-exported here so
// the arch backends keep referring to it as `crate::elf::*`; not every arch backend
// names every symbol.
#[allow(unused_imports)]
pub use bootproto::elf::{Elf, PF_R, PF_W, PF_X, PT_LOAD, ProgramHeader};
