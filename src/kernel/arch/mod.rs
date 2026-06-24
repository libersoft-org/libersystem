// architecture selection based on the compile target

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use self::x86_64::*;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use self::aarch64::*;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;
#[cfg(target_arch = "riscv64")]
pub use self::riscv64::*;

// The human-readable name of the compile-target architecture, for the boot log.
#[cfg(target_arch = "x86_64")]
pub const NAME: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub const NAME: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
pub const NAME: &str = "riscv64";
