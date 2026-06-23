//! Shared OS ABI - the single source of truth for the values the kernel
//! and userspace must agree on byte-for-byte: syscall numbers, error codes,
//! capability rights bits, and the PKGARCH1 package format. Both sides (and the
//! kernel's build script) depend on this crate, so the two halves can never drift
//! out of sync.
//!
//! It is intentionally pure constants plus a couple of `const fn`s, `no_std`, and
//! dependency-free, so it compiles for the kernel and userspace targets (under
//! build-std) and for the host (as a build-dependency) alike.

#![no_std]
#![allow(dead_code)]

// The canonical structured-log record type and its representations (text, JSON,
// CBOR), shared by emitters, LogService, and the kernel.
pub mod log;

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
pub const SYS_DMA_BUFFER_CREATE: u64 = 24;
pub const SYS_DEVICE_MEMORY_MAP: u64 = 25;
pub const SYS_RANDOM_GET: u64 = 26;
pub const SYS_INTERRUPT_BIND: u64 = 27;
pub const SYS_OBJECT_PROPERTY_SET: u64 = 28;
pub const SYS_PROCESS_CREATE: u64 = 29;
pub const SYS_PROCESS_LOAD: u64 = 30;
pub const SYS_THREAD_CREATE: u64 = 31;
pub const SYS_THREAD_START: u64 = 32;
pub const SYS_CONSOLE_ATTACH: u64 = 33;
pub const SYS_DEVICE_COUNT: u64 = 34;
pub const SYS_DEVICE_INFO: u64 = 35;
pub const SYS_DEVICE_ACQUIRE: u64 = 36;
pub const SYS_DMA_BUFFER_MAP: u64 = 37;
pub const SYS_DMA_BUFFER_PHYS: u64 = 38;
// Acquire an Interrupt capability for a discovered device's IRQ (the kernel routes
// its GSI through the I/O APIC), and acknowledge/re-arm a serviced interrupt.
pub const SYS_DEVICE_INTERRUPT_ACQUIRE: u64 = 39;
pub const SYS_INTERRUPT_ACK: u64 = 40;
// Inject one byte into the kernel console input (a userspace input driver feeds the
// interactive shell the same way the kernel's serial loop does).
pub const SYS_CONSOLE_FEED: u64 = 41;
// Block until ANY handle in a caller-supplied array is ready (or the deadline
// passes), returning the ready handle's index - `wait` over a set, so a driver can
// wait on its device interrupt and a control channel at once.
pub const SYS_WAIT_ANY: u64 = 42;
// Read the hardware real-time clock as a Unix timestamp (seconds since the epoch,
// UTC). Raw mechanism; the userspace TimeService is the wall-clock policy.
pub const SYS_CLOCK_RTC: u64 = 43;
// Map the boot framebuffer into the caller and report its geometry, handing the
// display to a userspace ConsoleService (the kernel console stops drawing to it).
pub const SYS_FRAMEBUFFER_MAP: u64 = 44;
// Deliver an asynchronous signal to a process (the typed, capability-gated equivalent
// of POSIX kill): a holder of the process's MANAGE capability requests a default
// disposition - interrupt / terminate, suspend, or resume.
pub const SYS_PROCESS_SIGNAL: u64 = 45;
// Acquire an MSI-X Interrupt capability for a discovered device: the kernel allocates
// a per-device LAPIC vector and programs the device's MSI-X table entry 0, so the
// driver gets its own edge-triggered interrupt instead of sharing a legacy INTx line.
pub const SYS_DEVICE_MSIX_ACQUIRE: u64 = 46;

// Signal numbers for SYS_PROCESS_SIGNAL (POSIX-like values, but our own typed set).
// The kernel applies the default disposition: INT / TERM / KILL terminate the target,
// STOP suspends it, CONT resumes a suspended one. User-installed handlers are not
// modelled (no async handler delivery in this milestone).
pub const SIG_INT: u64 = 2;
pub const SIG_KILL: u64 = 9;
pub const SIG_TERM: u64 = 15;
pub const SIG_CONT: u64 = 18;
pub const SIG_STOP: u64 = 19;

// The ring-3 stack top an ELF-loaded process runs on: the kernel's loader maps a
// stack just below this address, and a userspace spawner passes it to
// thread_create as the new thread's stack_top. Part of the spawn ABI, so it lives
// here next to the spawn syscall numbers.
pub const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000;

// object_property_set property selectors. PROP_NAME sets an object's label (arg2 =
// name pointer, arg3 = length); the PROP_*_LIMIT selectors set a Domain resource
// counter's limit (arg2 = the new limit).
pub const PROP_NAME: u64 = 0;
pub const PROP_MEMORY_LIMIT: u64 = 1;
pub const PROP_HANDLE_LIMIT: u64 = 2;
pub const PROP_THREAD_LIMIT: u64 = 3;
pub const PROP_DMA_LIMIT: u64 = 4;
pub const PROP_IPC_QUEUE_LIMIT: u64 = 5;

// virtio device type codes, as written into `DeviceInfo::virtio_type` (the modern
// virtio-pci `device_id - 0x1040`). The single source of truth for the kernel's PCI
// enumeration and the userspace DeviceManager/DeviceService that classify devices.
pub const VIRTIO_TYPE_NET: u32 = 1;
pub const VIRTIO_TYPE_BLOCK: u32 = 2;
pub const VIRTIO_TYPE_CONSOLE: u32 = 3;
pub const VIRTIO_TYPE_RNG: u32 = 4;
pub const VIRTIO_TYPE_GPU: u32 = 16;
pub const VIRTIO_TYPE_INPUT: u32 = 18;
pub const VIRTIO_TYPE_SOUND: u32 = 25;

// What `device_info` writes about one discovered virtio device. The kernel
// resolves these from the device's PCI capabilities at boot; a driver maps the
// device's MMIO BAR (via a DeviceMemory capability from `device_acquire`) and uses
// the offsets to reach each virtio structure within the mapping. `repr(C)` so the
// kernel and userspace agree on the layout byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DeviceInfo {
	// virtio device type (net = 1, blk = 2, console = 3, ...).
	pub virtio_type: u32,
	// length of the MMIO window the DeviceMemory capability covers.
	pub bar_len: u64,
	// byte offsets of the virtio structures within that window.
	pub common_offset: u32,
	pub notify_offset: u32,
	pub notify_multiplier: u32,
	pub isr_offset: u32,
	pub device_offset: u32,
}

// The framebuffer geometry framebuffer_map writes into the caller's buffer (the
// mapped virtual base is the syscall's return value): the pixel dimensions, the row
// stride in bytes, the bytes per pixel, and the per-channel shift/size of the pixel
// format. repr(C) so the kernel and a userspace ConsoleService agree byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Framebuffer {
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub bytes_per_pixel: u32,
	pub red_shift: u8,
	pub red_size: u8,
	pub green_shift: u8,
	pub green_size: u8,
	pub blue_shift: u8,
	pub blue_size: u8,
	pub _pad: [u8; 2],
}

// The introspection view object_info_get returns for a handle: the identity (koid)
// of the object behind it, its stable type code (ObjectType::code - Domain = 0,
// Process = 1, Thread = 2, ...), the rights the handle confers, and the object's
// generation. repr(C) with fixed-width fields so it marshals cleanly across the
// syscall boundary; the kernel writes it, userspace reads it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ObjectInfo {
	pub koid: u64,
	pub object_type: u64,
	pub rights: u32,
	pub generation: u32,
}

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

// PKGARCH1 archive format - a 16-byte header (8-byte magic, u32 entry count, u32
// reserved), then one 32-byte entry per file (24-byte NUL-padded name, u32 blob
// offset, u32 size), then the concatenated blobs. All integers little-endian.
// Written by the kernel build.rs, read by the kernel pkg.rs and the userspace
// storage runtime.
pub const PKG_MAGIC: &[u8; 8] = b"PKGARCH1";
pub const PKG_HEADER_LEN: usize = 16;
pub const PKG_ENTRY_LEN: usize = 32;
pub const PKG_NAME_LEN: usize = 24;

// A parsed PKGARCH1 archive borrowing the underlying bytes. The single reader for
// the format: the kernel (init/volume packages) and the userspace storage runtime
// both decode the layout above through this one implementation, so the on-disk
// format and its parser never drift apart.
pub struct Package<'a> {
	bytes: &'a [u8],
	count: usize,
}

impl<'a> Package<'a> {
	// Parse and validate a package header. Returns None if the bytes are too
	// short, the magic is wrong, or the entry table does not fit.
	pub fn parse(bytes: &'a [u8]) -> Option<Self> {
		if bytes.len() < PKG_HEADER_LEN {
			return None;
		}
		if &bytes[0..8] != PKG_MAGIC {
			return None;
		}
		let count = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
		let table_end = PKG_HEADER_LEN.checked_add(count.checked_mul(PKG_ENTRY_LEN)?)?;
		if table_end > bytes.len() {
			return None;
		}
		Some(Self { bytes, count })
	}

	// Number of files in the package.
	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	// The name of the `index`-th file (its stored name up to the first NUL), or
	// None if the index is out of range. Lets a caller enumerate the archive.
	pub fn name(&self, index: usize) -> Option<&'a [u8]> {
		if index >= self.count {
			return None;
		}
		let base = PKG_HEADER_LEN + index * PKG_ENTRY_LEN;
		let stored = &self.bytes[base..base + PKG_NAME_LEN];
		match stored.iter().position(|&b| b == 0) {
			Some(end) => Some(&stored[..end]),
			None => Some(stored),
		}
	}

	// Find a file by name, returning its blob. The stored name is compared up to
	// its first NUL. Returns None if absent, or if its byte range is out of bounds.
	pub fn lookup(&self, name: &[u8]) -> Option<&'a [u8]> {
		for index in 0..self.count {
			let base = PKG_HEADER_LEN + index * PKG_ENTRY_LEN;
			let entry = &self.bytes[base..base + PKG_ENTRY_LEN];
			let stored = &entry[0..PKG_NAME_LEN];
			let stored_name = match stored.iter().position(|&b| b == 0) {
				Some(end) => &stored[..end],
				None => stored,
			};
			if stored_name != name {
				continue;
			}
			let offset = u32::from_le_bytes(entry[PKG_NAME_LEN..PKG_NAME_LEN + 4].try_into().ok()?) as usize;
			let size = u32::from_le_bytes(entry[PKG_NAME_LEN + 4..PKG_NAME_LEN + 8].try_into().ok()?) as usize;
			let end = offset.checked_add(size)?;
			if end > self.bytes.len() {
				return None;
			}
			return Some(&self.bytes[offset..end]);
		}
		None
	}
}
