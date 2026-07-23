// uname - print the system identity, run as its own sandboxed ELF.
//
// PermissionManager launches this program under an empty permission manifest - it
// needs no capability at all - and forwards it the shell's stdout console and an
// (empty) argument string. uname prints the product name, version and architecture
// (all baked in at build time from product.conf and the target, the same single
// source of truth the boot banner renders) and exits. A standalone command, not a
// shell built-in: the system identity is compile-time data, so the emptiest
// manifest in the store is enough.

#![no_std]
#![no_main]

use rt::*;

// The target architecture, from the compile target (the userspace programs build
// per-arch alongside the kernel).
#[cfg(target_arch = "x86_64")]
const ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const ARCH: &str = "aarch64";
#[cfg(target_arch = "riscv64")]
const ARCH: &str = "riscv64";

#[unsafe(no_mangle)]
pub extern "C" fn __user_main(bootstrap: u64) -> ! {
	let mut buf: [u8; 64] = [0u8; 64];
	unsafe {
		// 1. adopt the forwarded stdout console (the first bootstrap message), so our
		//    output renders on the same terminal as the shell that launched us.
		inherit_stdout(bootstrap);
		// 2. receive the argument string (uname takes none, but the launch protocol
		//    sends one).
		let _ = recv_blocking(bootstrap, &mut buf);
		// 3. print the identity: "<name> <version> <arch>".
		print(env!("PRODUCT_NAME").as_bytes());
		print(b" ");
		print(env!("PRODUCT_VERSION").as_bytes());
		print(b" ");
		print(ARCH.as_bytes());
		print(b"\n");
	}
	exit();
}
