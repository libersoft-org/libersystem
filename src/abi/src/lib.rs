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

// The ABI revision this crate defines: the version the kernel and every userspace
// binary agree on. Bump it whenever the ABI changes in a way an old binary would
// misread - a grown or reordered struct, a changed argument meaning. New syscalls
// only ever append (a higher SYS_ number) and old ones never renumber, so appending a
// call does NOT require a bump; a binary carrying an older version simply never issues
// the newer call. A starting process reports the version it was built against through
// SYS_ABI_CHECK, and the kernel refuses a mismatch (ERR_ABI_MISMATCH) so a binary built
// against a different revision is stopped at startup instead of misbehaving.
pub const ABI_VERSION: u32 = 1;

// Control messages intercepted by the userspace runtime before typed LSIDL
// dispatch. Typed interface opcodes must stay at or below TYPED_OP_MAX.
pub const TYPED_OP_MAX: u16 = 0xfffb;
pub const GOODBYE_OP: u16 = 0xfffc;
pub const RESOLVE_OP: u16 = 0xfffd;
pub const HEARTBEAT_OP: u16 = 0xfffe;
pub const CONNECT_OP: u16 = 0xffff;

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
// a2 = the queue depth per endpoint in messages (0 = the default), so a channel's
// backpressure point is a creation parameter rather than one hardwired constant.
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
// Acknowledge and re-arm a serviced device interrupt (39 retired: device interrupts
// are MSI-X now, see SYS_DEVICE_MSIX_ACQUIRE).
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
// Reboot or power the machine off (the argument is POWER_REBOOT or POWER_OFF). The
// machine resets or enters ACPI soft-off; restricting it to an authorized component
// is a future PermissionManager concern.
pub const SYS_SYSTEM_POWER: u64 = 47;
// Read the kernel boot console's content as logical text lines into the caller's
// buffer, returning the byte count. The kernel hands its on-screen boot log across to
// a userspace ConsoleService at takeover, which replays it so the boot log survives.
pub const SYS_CONSOLE_READLOG: u64 = 48;
// Read the monotonic clock in nanoseconds since boot (the calibrated TSC), the
// fine-grained companion to SYS_CLOCK_GET's 100 Hz ticks. Resolves latencies far
// below a tick - an IPC round-trip, a ping RTT - that the tick counter cannot.
pub const SYS_CLOCK_MONO_NS: u64 = 49;
// Arm the calling process to catch a signal (SIG_INT only for now): a
// subsequent SIG_INT then sets a pending flag the process polls with SYS_SIGNAL_TAKE
// instead of terminating it, so a long-running tool can stop cleanly on Ctrl+C.
pub const SYS_SIGNAL_CATCH: u64 = 50;
// Poll-and-clear a pending caught signal on the calling process: returns 1 if the
// signal (SIG_INT) was delivered since the last take (clearing it), else 0.
pub const SYS_SIGNAL_TAKE: u64 = 51;
// Read live per-process counters and state into the caller's buffer (a ProcessStats),
// for a Process handle that carries RIGHT_READ. Surfaces the kernel's per-process IPC
// volume, handle and memory usage, and liveness so a userspace SystemGraphService can
// build the live observability graph without each component having to self-report.
pub const SYS_PROCESS_STATS_GET: u64 = 52;
// Read live per-Domain resource counters into the caller's buffer (a DomainStats), for a
// Domain handle that carries RIGHT_READ. Surfaces the kernel's per-Domain used/limit pair
// for each accounted resource - memory, handles, threads, IPC queue bytes and DMA - so a
// userspace ResourceManager can observe usage against the budgets it sets without the
// governed component having to self-report.
pub const SYS_DOMAIN_STATS_GET: u64 = 53;
// Read the online CPU set: copies one u32 LAPIC id per core into the caller's buffer
// (as many as fit) and returns the core count. A free syscall - the CPU topology is
// public identity, not a capability - feeding the `lscpu` inventory command.
pub const SYS_CPU_INFO: u64 = 54;
// Read the physical-memory and kernel-heap totals into the caller's buffer (a
// MemoryStats): total and free 4 kB frames, and the heap's total and free bytes. A
// free syscall feeding the `free` inventory command.
pub const SYS_MEMORY_STATS: u64 = 55;
// Read one retained boot memory-map region (a MemmapRegion) by index into the
// caller's buffer, returning the region count - ERR_INVALID past the end, so a caller
// can walk the map without knowing its size up front. A free syscall feeding `lsmem`.
pub const SYS_MEMMAP_GET: u64 = 56;
// Read one device-interrupt vector's state (an IrqInfo) by index into the caller's
// buffer, returning the vector count: the fixed INTx window first, then the MSI-X
// window with the owning device's index. A free syscall feeding `lsirq`.
pub const SYS_IRQ_INFO: u64 = 57;
// Read one PCI function's identity (a PciInfo) by index into the caller's buffer,
// returning the function count - ERR_INVALID past the end. The kernel retains the
// full boot bus scan (every present function, not just the ones drivers bind), so
// the bus stays inspectable. A free syscall feeding `lspci`.
pub const SYS_PCI_INFO: u64 = 58;
// Report the byte length of the next pending message on a channel WITHOUT
// dequeuing it (ERR_WOULD_BLOCK when nothing is queued, ERR_PEER_CLOSED once the
// queue is empty and the peer is gone), so a receiver can size its buffer exactly
// instead of guessing a ceiling.
pub const SYS_CHANNEL_PEEK: u64 = 59;
// Report the ABI revision the caller was built against (a0 = its abi::ABI_VERSION); the
// kernel returns 0 on a match and ERR_ABI_MISMATCH otherwise. The runtime issues it as
// its first syscall, so a binary built against a different ABI is refused before it runs.
pub const SYS_ABI_CHECK: u64 = 60;
// Write the CPU's model / brand string into the caller's buffer, returning the byte
// length written (as many bytes as fit). A free syscall - the CPU model is public
// identity, not a capability - feeding the `lscpu` model field. x86 returns the CPUID
// brand string (the host CPU under KVM); aarch64 decodes MIDR_EL1; riscv64 queries the
// SBI vendor id (a generic QEMU rv64 falls back to "riscv64").
pub const SYS_CPU_NAME: u64 = 61;
// Remove the calling process's DmaBuffer mapping. Shared DMA backings can be mapped
// by a driver and a display server independently; each owner releases its own mapping.
pub const SYS_DMA_BUFFER_UNMAP: u64 = 62;
// Map one ET_DYN provider into a created process before its main image. `a0` is a
// MANAGE-capable Process handle, `a1/a2` the caller's ELF bytes, and `a3` the
// explicit page-aligned load bias selected by ProcessService's dependency order.
// The module receives no stack or thread; SYS_PROCESS_LOAD finalizes the main image.
pub const SYS_PROCESS_LOAD_MODULE: u64 = 63;
// Actions for SYS_SYSTEM_POWER.
pub const POWER_REBOOT: u64 = 0;
pub const POWER_OFF: u64 = 1;

// Flag for SYS_WAIT (arg 2) / SYS_WAIT_ANY (arg 3): the deadline is a PERIODIC
// housekeeping wake (a display poll, a blink tick), not pending progress. The
// kernel still wakes the caller when it is due, but the scheduler's boot driver
// may consider the system idle while only periodic waits remain - so a service
// can tick forever without holding the boot path (or the tests) hostage.
pub const WAIT_PERIODIC: u64 = 1;

// Flag for SYS_WAIT (arg 2): wait for a Channel to become WRITABLE (the peer's
// queue has room, or the peer is gone - the send then reports the close) instead
// of readable. A sender that got WOULD_BLOCK blocks here until the receiver
// drains, which is what backpressure means: the sender waits, it never spins.
// Ignored for non-Channel objects.
pub const WAIT_WRITABLE: u64 = 2;

// Signal numbers for SYS_PROCESS_SIGNAL (POSIX-like values, but our own typed set).
// The kernel applies the default disposition: INT / TERM / KILL terminate the target,
// STOP suspends it, CONT resumes a suspended one. User-installed handlers are not
// modelled (no async handler delivery yet).
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
// counter's limit (arg2 = the new limit). PROP_STACK_LIMIT is the per-thread stack
// ceiling: the VA span (bytes, below USER_STACK_TOP) the kernel's fault handler
// demand-pages a thread's stack into.
pub const PROP_NAME: u64 = 0;
pub const PROP_MEMORY_LIMIT: u64 = 1;
pub const PROP_HANDLE_LIMIT: u64 = 2;
pub const PROP_THREAD_LIMIT: u64 = 3;
pub const PROP_DMA_LIMIT: u64 = 4;
pub const PROP_IPC_QUEUE_LIMIT: u64 = 5;
pub const PROP_STACK_LIMIT: u64 = 6;

// virtio device type codes, as written into `DeviceInfo::device_type` (the modern
// virtio-pci `device_id - 0x1040`). The single source of truth for the kernel's PCI
// enumeration and the userspace DeviceManager/DeviceService that classify devices.
pub const VIRTIO_TYPE_NET: u32 = 1;
pub const VIRTIO_TYPE_BLOCK: u32 = 2;
pub const VIRTIO_TYPE_CONSOLE: u32 = 3;
pub const VIRTIO_TYPE_RNG: u32 = 4;
pub const VIRTIO_TYPE_GPU: u32 = 16;
pub const VIRTIO_TYPE_INPUT: u32 = 18;
pub const VIRTIO_TYPE_SOUND: u32 = 25;

// virtio-pci modern wire format, shared by the kernel's minimal boot driver and the
// userspace drivers so the register offsets, status bits and ring flags have one
// source of truth (each side aliases these to its own ergonomic short names).
//
// virtio_pci_common_cfg field offsets, relative to the common-config structure.
pub const VIRTIO_CFG_DEVICE_FEATURE_SELECT: u64 = 0x00;
pub const VIRTIO_CFG_DEVICE_FEATURE: u64 = 0x04;
pub const VIRTIO_CFG_DRIVER_FEATURE_SELECT: u64 = 0x08;
pub const VIRTIO_CFG_DRIVER_FEATURE: u64 = 0x0c;
pub const VIRTIO_CFG_CONFIG_MSIX_VECTOR: u64 = 0x10;
pub const VIRTIO_CFG_NUM_QUEUES: u64 = 0x12;
pub const VIRTIO_CFG_DEVICE_STATUS: u64 = 0x14;
pub const VIRTIO_CFG_QUEUE_SELECT: u64 = 0x16;
pub const VIRTIO_CFG_QUEUE_SIZE: u64 = 0x18;
pub const VIRTIO_CFG_QUEUE_MSIX_VECTOR: u64 = 0x1a;
pub const VIRTIO_CFG_QUEUE_ENABLE: u64 = 0x1c;
pub const VIRTIO_CFG_QUEUE_NOTIFY_OFF: u64 = 0x1e;
pub const VIRTIO_CFG_QUEUE_DESC: u64 = 0x20;
pub const VIRTIO_CFG_QUEUE_DRIVER: u64 = 0x28;
pub const VIRTIO_CFG_QUEUE_DEVICE: u64 = 0x30;

// device_status register bits.
pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
pub const VIRTIO_STATUS_FAILED: u8 = 128;

// split-virtqueue descriptor flags.
pub const VIRTIO_DESC_F_NEXT: u16 = 1; // the buffer continues in the `next` descriptor
pub const VIRTIO_DESC_F_WRITE: u16 = 2; // the device writes this buffer (device-writable)

// available-ring flag: suppress the device's used-buffer interrupt (polling drivers).
pub const VIRTIO_AVAIL_F_NO_INTERRUPT: u16 = 1;

// VIRTIO_F_VERSION_1 (feature bit 32) = bit 0 of the second feature word; every modern
// virtio device offers it and a modern driver must accept it.
pub const VIRTIO_F_VERSION_1: u32 = 1 << 0;

// The MSI-X vector fields' reset value: no vector mapped (the device raises legacy INTx).
pub const VIRTIO_MSI_NO_VECTOR: u16 = 0xffff;

// Non-virtio device type codes live above the virtio id space (modern virtio types
// are below 0x40), so one `device_type` field classifies every discovered device.
pub const DEVICE_TYPE_XHCI: u32 = 0x100;

// What `device_info` writes about one discovered device. The kernel resolves these
// from the device's PCI configuration at boot; a driver maps the device's MMIO BAR
// (via a DeviceMemory capability from `device_acquire`) and, for a virtio device,
// uses the offsets to reach each virtio structure within the mapping (a non-virtio
// device such as the xHCI controller carries zero offsets - its register layout
// starts at the BAR base). `repr(C)` so the kernel and userspace agree on the
// layout byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DeviceInfo {
	// device type (virtio net = 1, blk = 2, console = 3, ...; xHCI = 0x100).
	pub device_type: u32,
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
// Process = 1, Thread = 2, ...), the rights the handle confers, the object's
// generation, and - for memory-backed objects (MemoryObject, DmaBuffer) - its byte
// size (0 for other types), so a service can validate a claimed transfer length
// against the real object instead of a guessed cap. repr(C) with fixed-width
// fields so it marshals cleanly across the syscall boundary; the kernel writes
// it, userspace reads it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ObjectInfo {
	pub koid: u64,
	pub object_type: u64,
	pub rights: u32,
	pub generation: u32,
	pub size: u64,
}

// The live per-process view process_stats_get returns for a Process handle: the IPC
// volume the process has done (channel messages sent and received), how many handles
// its table currently holds, how many bytes of user memory it has mapped, and its
// liveness state (PROC_STATE_RUNNING / PROC_STATE_STOPPED / PROC_STATE_FAILED). The
// kernel derives state from the live process - threads still running, a clean exit,
// or a fault/kill - so a SystemGraphService sees crash and stop transitions at the
// next snapshot without the component reporting them. repr(C) so it marshals cleanly.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ProcessStats {
	pub messages_sent: u64,
	pub messages_received: u64,
	pub handle_count: u64,
	pub memory_bytes: u64,
	pub state: u64,
}

// Liveness states reported in ProcessStats::state.
pub const PROC_STATE_RUNNING: u64 = 0;
pub const PROC_STATE_STOPPED: u64 = 1;
pub const PROC_STATE_FAILED: u64 = 2;

// The live per-Domain view domain_stats_get returns for a Domain handle: the used and
// limit of each resource counter the kernel accounts - memory held, live handles, live
// threads, in-transit IPC queue bytes and pinned DMA memory. A limit of u64::MAX means
// the counter is uncapped. The kernel reads these straight off the Domain's account, so a
// ResourceManager sees real consumption against the budgets it sets without the governed
// component reporting them. repr(C) so it marshals cleanly across the syscall boundary.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DomainStats {
	pub memory_used: u64,
	pub memory_limit: u64,
	pub handles_used: u64,
	pub handles_limit: u64,
	pub threads_used: u64,
	pub threads_limit: u64,
	pub ipc_used: u64,
	pub ipc_limit: u64,
	pub dma_used: u64,
	pub dma_limit: u64,
	// Stack: used = the stack bytes currently mapped across the Domain's processes
	// (initial pages plus demand-paged growth); limit = the per-thread ceiling (the
	// VA span a stack may grow into), not a cap on the sum.
	pub stack_used: u64,
	pub stack_limit: u64,
}

// The memory totals memory_stats writes into the caller's buffer: the physical frame
// allocator's total and free 4 kB frames (the total is fixed at boot from the usable
// memory-map regions), and the kernel heap's total and free bytes. repr(C) so the
// kernel and userspace agree on the layout byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MemoryStats {
	pub total_frames: u64,
	pub free_frames: u64,
	pub heap_total: u64,
	pub heap_free: u64,
}

// One boot memory-map region memmap_get writes into the caller's buffer: its physical
// base, byte length, and kind (the MEMMAP_* codes below, the kernel's own stable
// mapping of the bootloader's entry types). repr(C) so both sides agree byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MemmapRegion {
	pub base: u64,
	pub length: u64,
	pub kind: u32,
	pub _pad: u32,
}

// Region kinds reported in MemmapRegion::kind.
pub const MEMMAP_USABLE: u32 = 0;
pub const MEMMAP_RESERVED: u32 = 1;
pub const MEMMAP_ACPI_RECLAIMABLE: u32 = 2;
pub const MEMMAP_ACPI_NVS: u32 = 3;
pub const MEMMAP_BAD: u32 = 4;
pub const MEMMAP_BOOTLOADER: u32 = 5;
pub const MEMMAP_KERNEL: u32 = 6;
pub const MEMMAP_FRAMEBUFFER: u32 = 7;

// One device-interrupt vector's state irq_info writes into the caller's buffer: the
// vector number, its window (IRQ_KIND_FIXED for the legacy INTx window, IRQ_KIND_MSI
// for the per-device MSI-X window), whether it is in use (a kernel handler or a live
// driver binding), and for an owned MSI-X vector the discovered device's index
// (IRQ_NO_DEVICE otherwise). repr(C) so both sides agree byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IrqInfo {
	pub vector: u32,
	pub kind: u32,
	pub bound: u32,
	pub device: u32,
}

// Vector windows reported in IrqInfo::kind.
pub const IRQ_KIND_FIXED: u32 = 0;
pub const IRQ_KIND_MSI: u32 = 1;
// IrqInfo::device when no device owns the vector.
pub const IRQ_NO_DEVICE: u32 = u32::MAX;

// One PCI function's identity pci_info writes into the caller's buffer: its bus
// address, vendor and device ids, and class triple - the boot bus scan the kernel
// retains in full. repr(C) so both sides agree byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PciInfo {
	pub vendor: u16,
	pub device: u16,
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	pub _pad: u16,
}

// Error codes (a successful call returns its value, an error returns
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
// The caller was built against a different ABI revision than the kernel implements
// (SYS_ABI_CHECK): the runtime refuses to run rather than issue calls against a
// mismatched syscall table or struct layout.
pub const ERR_ABI_MISMATCH: i64 = -12;

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

pub const EXECUTABLE_SUFFIX: &str = ".lsexe";

pub fn executable_aliases_ambiguous(first: &[u8], second: &[u8]) -> bool {
	fn expands_to(shorter: &[u8], longer: &[u8]) -> bool {
		longer.len() == shorter.len() + EXECUTABLE_SUFFIX.len() && longer.starts_with(shorter) && longer[shorter.len()..] == *EXECUTABLE_SUFFIX.as_bytes()
	}
	expands_to(first, second) || expands_to(second, first)
}

// PKGARCH1 archive format - a 16-byte header (8-byte magic, u32 entry count, u32
// reserved), then one 40-byte entry per file (32-byte NUL-padded name, u32 blob
// offset, u32 size), then the concatenated blobs. All integers little-endian.
// Written by the kernel build.rs, read by the kernel pkg.rs and the userspace
// storage runtime.
pub const PKG_MAGIC: &[u8; 8] = b"PKGARCH1";
pub const PKG_HEADER_LEN: usize = 16;
pub const PKG_ENTRY_LEN: usize = 40;
pub const PKG_NAME_LEN: usize = 32;

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

#[cfg(test)]
mod tests {
	use super::executable_aliases_ambiguous;

	#[test]
	fn executable_alias_collision_is_exactly_one_suffix_level() {
		assert!(executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe"));
		assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"bin/ping.lsexe.lsexe.lsexe"));
		assert!(!executable_aliases_ambiguous(b"bin/ping.lsexe", b"drivers/ping.lsexe.lsexe"));
	}
}
