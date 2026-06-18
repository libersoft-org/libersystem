//! Shared OS ABI - the single source of truth for the values the kernel
//! and userspace must agree on byte-for-byte: syscall numbers, error codes,
//! capability rights bits, and the LIBERPK1 package format. Both sides (and the
//! kernel's build script) depend on this crate, so the two halves can never drift
//! out of sync.
//!
//! It is intentionally pure constants plus a couple of `const fn`s, `no_std`, and
//! dependency-free, so it compiles for the kernel and userspace targets (under
//! build-std) and for the host (as a build-dependency) alike.

#![no_std]
#![allow(dead_code)]

// Syscall numbers (the stable ABI index). Handlers live in the kernel's
// syscall.rs; userspace issues them through its syscall wrapper.
pub const SYS_DEBUG_NOOP: u64 = 0;
pub const SYS_CLOCK_GET: u64 = 1;
pub const SYS_DEBUG_WRITE: u64 = 2;
pub const SYS_MEMORY_OBJECT_CREATE: u64 = 3;
pub const SYS_MEMORY_MAP: u64 = 4;
pub const SYS_MEMORY_UNMAP: u64 = 5;
pub const SYS_HANDLE_DUPLICATE: u64 = 6;
pub const SYS_HANDLE_CLOSE: u64 = 7;
pub const SYS_CHANNEL_CREATE: u64 = 8;
pub const SYS_CHANNEL_SEND: u64 = 9;
pub const SYS_CHANNEL_RECV: u64 = 10;
pub const SYS_EVENT_CREATE: u64 = 11;
pub const SYS_EVENT_SIGNAL: u64 = 12;
pub const SYS_EVENT_POLL: u64 = 13;
pub const SYS_TIMER_CREATE: u64 = 14;
pub const SYS_TIMER_SET: u64 = 15;
pub const SYS_TIMER_POLL: u64 = 16;
pub const SYS_USER_EXIT: u64 = 17;
pub const SYS_FAULT_INFO_GET: u64 = 18;
pub const SYS_DOMAIN_CREATE: u64 = 19;
pub const SYS_DOMAIN_KILL: u64 = 20;
pub const SYS_YIELD: u64 = 21;
pub const SYS_OBJECT_INFO_GET: u64 = 22;
pub const SYS_WAIT: u64 = 23;

// Error codes (Linux-style: a successful call returns its value, an error returns
// a small negative in the reserved band [-4095, -1]).
pub const ERR_BAD_SYSCALL: i64 = -1;
pub const ERR_NO_THREAD: i64 = -2;
pub const ERR_NO_MEMORY: i64 = -3;
pub const ERR_BAD_HANDLE: i64 = -4;
pub const ERR_ACCESS_DENIED: i64 = -5;
pub const ERR_INVALID: i64 = -6;
pub const ERR_NOT_MAPPED: i64 = -7;
pub const ERR_WOULD_BLOCK: i64 = -8;
pub const ERR_PEER_CLOSED: i64 = -9;
pub const ERR_RESOURCE_EXHAUSTED: i64 = -10;
pub const ERR_TIMED_OUT: i64 = -11;

// True if a syscall return value encodes an error (the reserved band [-4095, -1]).
// A higher-half kernel address has its top bit set and so is never mistaken for
// an error.
pub const fn sys_is_err(ret: u64) -> bool {
	let signed: i64 = ret as i64;
	signed >= -4095 && signed < 0
}

// Capability rights bits - a 12-bit set. The kernel wraps these in the `Rights`
// newtype (object/rights.rs); userspace passes the raw bits at the syscall
// boundary.
pub const RIGHT_READ: u32 = 1 << 0;
pub const RIGHT_WRITE: u32 = 1 << 1;
pub const RIGHT_EXECUTE: u32 = 1 << 2;
pub const RIGHT_MAP: u32 = 1 << 3;
pub const RIGHT_SEND: u32 = 1 << 4;
pub const RIGHT_RECEIVE: u32 = 1 << 5;
pub const RIGHT_DUPLICATE: u32 = 1 << 6;
pub const RIGHT_TRANSFER: u32 = 1 << 7;
pub const RIGHT_REVOKE: u32 = 1 << 8;
pub const RIGHT_GET_INFO: u32 = 1 << 9;
pub const RIGHT_MANAGE: u32 = 1 << 10;
pub const RIGHT_WAIT: u32 = 1 << 11;
// Every currently defined right.
pub const RIGHTS_ALL: u32 = 0xfff;

// LIBERPK1 archive format - a 16-byte header (8-byte magic, u32 entry count, u32
// reserved), then one 32-byte entry per file (24-byte NUL-padded name, u32 blob
// offset, u32 size), then the concatenated blobs. All integers little-endian.
// Written by the kernel build.rs, read by the kernel pkg.rs and the userspace
// storage runtime.
pub const PKG_MAGIC: &[u8; 8] = b"LIBERPK1";
pub const PKG_HEADER_LEN: usize = 16;
pub const PKG_ENTRY_LEN: usize = 32;
pub const PKG_NAME_LEN: usize = 24;
