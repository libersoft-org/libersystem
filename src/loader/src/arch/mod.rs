// Architecture dispatch for the loader. Each architecture provides the same small
// surface the common driver (main.rs) calls: the `serial` diagnostic console, a
// `halt` for the panic path, and `hand_off`, which does the arch-specific work of
// placing the kernel in memory and jumping into it. The UEFI protocol bindings
// (uefi.rs), the ELF reader (elf.rs) and the boot-volume file I/O (main.rs) are
// architecture-neutral and shared.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{halt, hand_off, serial};

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{halt, hand_off, serial};

#[cfg(target_arch = "riscv64")]
pub mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use riscv64::{halt, hand_off, serial};
