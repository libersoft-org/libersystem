// Init package: a tiny read-only archive of the userspace programs the kernel
// loads at boot. The bootloader hands it to the kernel as a boot module (a loaded
// blob referenced from the BootInfo handoff); the
// kernel parses it in place (the module memory is 'static) and looks up programs
// by name. The on-disk format is produced by the kernel's build.rs.
//
// Layout (all integers little-endian):
//   header  : magic [8] = b"PKGARCH1", count u32, reserved u32        (16 bytes)
//   entries : count * { name [24] NUL-padded, offset u32, size u32 }  (32 bytes each)
//   blobs   : the file contents, concatenated; each entry's offset/size is an
//             absolute byte range into the package.

// The PKGARCH1 archive reader lives in the shared `abi` crate, next to the format
// constants, so the kernel and the userspace storage runtime decode the format
// through one implementation. Re-exported here as `pkg::Package` for the kernel's
// existing call sites.
pub use abi::Package;
