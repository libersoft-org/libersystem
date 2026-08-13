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

extern crate alloc;

// The ABI revision this crate defines: the version the kernel and every userspace
// binary agree on. Bump it whenever the ABI changes in a way an old binary would
// misread - a grown or reordered struct, a changed argument meaning. New syscalls
// only ever append (a higher SYS_ number) and old ones never renumber, so appending a
// call does NOT require a bump; a binary carrying an older version simply never issues
// the newer call. A starting process reports the version it was built against through
// SYS_ABI_CHECK, and the kernel refuses a mismatch (ERR_ABI_MISMATCH) so a binary built
// against a different revision is stopped at startup instead of misbehaving.
pub const ABI_VERSION: u32 = 1;
// ONE, and it stays one. Nothing in this system is versioned yet, because there has been no release.
//
// A version number is a promise to something already out there, and nothing is out there. Moving it
// records compatibility with binaries nobody has, and costs a rebuild of every userspace artifact
// and a rebuilt image to say it. The handshake below still does its job within a build: kernel and
// userspace carry the same constant, so a stale artifact in the image is refused at startup rather
// than misbehaving - which is what it caught the one time this number was moved.
//
// The rule above describes what to do AFTER the first final release, and the two changes worth
// naming when that day comes are already in the tree: `SYS_WAITSET_WAIT` answers with the ready
// member's koid rather than its index, and `SYS_WAITSET_REMOVE` takes that koid where it once took
// the object's handle. Both change what a number MEANS rather than where an argument sits, which is
// the shape the wording below reads past most easily.
//
// Until then: do not bump this. An audit that calls an unbumped breaking change a defect is applying
// a rule that has not started.

// Control messages intercepted by the userspace runtime before typed LSIDL
// dispatch. Typed interface opcodes must stay at or below TYPED_OP_MAX.
//
// PROTOCOL_INFO_OP is answered by the GENERATED dispatch rather than by rt, because rt serves
// any interface and cannot know which package a given service implements - the generated code
// does. It is stateless by construction: the request is the opcode and a correlation id, the
// reply is the package identity and version, and no existing method frame changes.
//
// Lowering TYPED_OP_MAX from 0xfffb to 0xfffa to make room is safe as of 2026-08-01 and was
// checked rather than assumed: the highest `@op` across all 15 `.lsidl` schemas is 16.
pub const TYPED_OP_MAX: u16 = 0xfffa;
pub const PROTOCOL_INFO_OP: u16 = 0xfffb;
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
// Create a DMA buffer.
//   a0 = size in bytes
//   a1 = a DeviceMemory handle with WRITE, naming the device the buffer is for (0 = none)
//
// The second argument arrived with P02M0133's DMA lifetime rule - the kernel holds a dead driver's
// frames until that device is confirmed stopped - and this line did not mention it. Three stale
// syscall comments were corrected in this milestone and a fourth appeared within the day, which is
// the argument for generating these definitions rather than for a fifth correction.
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
// Inject one byte into the kernel console input.
//   a0 = the byte
//   a1 = 0 for a keystroke, non-zero for serial input (accepted even unfocused)
//   a2 = a handle to a ConsoleInputSource privilege
// Returns 0 when the console took it, ERR_WOULD_BLOCK when its queue is full or no console
// service is attached, and ERR_ACCESS_DENIED without the privilege.
//
// The privilege argument was missing from this comment while the handler required it, in the file
// whose reason for existing is to be the definition. Without it any process could type into a
// privileged console - shell commands, a password, a confirmation - as if a person had.
pub const SYS_CONSOLE_FEED: u64 = 41;
// Block until ANY handle in a caller-supplied array is ready (or the deadline
// passes), returning the ready handle's index - `wait` over a set, so a driver can
// wait on its device interrupt and a control channel at once.
pub const SYS_WAIT_ANY: u64 = 42;
// Read the hardware real-time clock as a Unix timestamp (seconds since the epoch,
// UTC). Raw mechanism; the userspace TimeService is the wall-clock policy.
pub const SYS_CLOCK_RTC: u64 = 43;
// Map the boot framebuffer into the caller and report its geometry, handing the display to a
// userspace ConsoleService (the kernel console stops drawing to it).
//   a0 = pointer to a Framebuffer to fill
//   a1 = its length in bytes
//   a2 = a handle to a DisplayController privilege
// Returns 0, or ERR_ACCESS_DENIED without the privilege. Without it the display went to whoever
// asked first, and asking first is a race any process at boot could try to win.
pub const SYS_FRAMEBUFFER_MAP: u64 = 44;
// Deliver an asynchronous signal to a process (the typed, capability-gated equivalent
// of POSIX kill): a holder of the process's MANAGE capability requests a default
// disposition - interrupt / terminate, suspend, or resume.
pub const SYS_PROCESS_SIGNAL: u64 = 45;
// Acquire an MSI-X Interrupt capability for a discovered device: the kernel allocates
// a per-device LAPIC vector and programs the device's MSI-X table entry 0, so the
// driver gets its own edge-triggered interrupt instead of sharing a legacy INTx line.
pub const SYS_DEVICE_MSIX_ACQUIRE: u64 = 46;
// Reboot or power the machine off.
//   a0 = a MANAGE-capable handle to the ROOT Domain
//   a1 = POWER_REBOOT or POWER_OFF
// Does not return on success; ERR_ACCESS_DENIED for a handle that is not the root domain's or
// lacks MANAGE, ERR_INVALID for an unknown action.
//
// The comment here said the argument was the action and that restricting the call was "a future
// PermissionManager concern". It has not been future since the handle argument landed - and
// the definition beside it was wrong. (`ABI_VERSION` was briefly moved for this and reverted:
// nothing here is versioned until the first final release, so the constant is 1 and the two changes
// worth naming when that day comes are recorded at it.)
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
// Report the boot profile the firmware selected (a0 = buffer, a1 = length), returning the
// bytes written, or 0 on an ordinary boot that named none. The kernel already reads it to
// decide what to print; userspace needs the same answer to decide what a build is allowed
// to do, and a development-only facility cannot gate itself on anything softer.
pub const SYS_BOOT_PROFILE: u64 = 64;

// Create a ProcessGroup over a set of Process handles, and signal one. A group is how a
// pipeline is one job: `a | b | c` is interrupted as a whole, not one stage at a time.
// Membership is fixed at creation and cannot be joined, and authority to signal comes from
// holding the group handle with RIGHT_MANAGE - being a member grants nothing, so one stage
// cannot signal its siblings.
// Send and receive a message carrying SEVERAL transferred capabilities. The ordinary
// `SYS_CHANNEL_SEND` / `SYS_CHANNEL_RECV` move exactly one, which is what stopped an interface
// op from handing over two - a pipeline stage needs its stdin AND its stdout, and there was no
// way to express that however the interface was written.
//
// Separate syscalls rather than widened ones: the single-capability path is what every one of
// the hundred-odd call sites in the tree uses, and it already fills all four argument
// registers. `caps_ptr` points at `[count, handle0, handle1, ...]`, so the count travels with
// the list instead of needing a fifth argument.
pub const SYS_CHANNEL_SEND_CAPS: u64 = 67;
pub const SYS_CHANNEL_RECV_CAPS: u64 = 68;

// A wait set the kernel keeps: create one, add and remove the objects it watches, and wait on the
// set rather than on an array handed over afresh every time. `SYS_WAIT_ANY` registers a waiter on
// every handle in its array on every call, so the cost of one pass grows with how many things a
// service listens to; a set registers each member once.
pub const SYS_WAITSET_CREATE: u64 = 69;
pub const SYS_WAITSET_ADD: u64 = 70;
pub const SYS_WAITSET_REMOVE: u64 = 71;
pub const SYS_WAITSET_WAIT: u64 = 72;

// Random bytes that are NOT cryptographic, asked for by that name.
//
// `SYS_RANDOM_GET` answers from a hardware source or refuses; this one always answers, from a
// deterministic generator seeded by the clock, and a caller reaching for it is saying that guessable
// is fine. Two syscalls rather than one that silently changes what it gives you: userspace sees one
// answer and cannot tell a hardware draw from a formula, so the moment anything derives a key or a
// token from it on a machine with no hardware source, the result is guessable and nothing says so.
//
// The name is the whole point. What is wrong with the single syscall is not the formula - a boot
// identifier, a jitter, a hash seed all want exactly this - it is that the formula arrives under a
// name that promises otherwise.
pub const SYS_RANDOM_INSECURE: u64 = 73;

// "I have stopped this device." Called by a driver once it has reset the device its DeviceMemory
// capability names - which is the first thing every virtio bring-up does and what `HCRST` is for
// xHCI - and it releases the DMA frames the kernel is holding for that device.
//
// It exists because a device is not a process. When a driver dies with a descriptor live, the
// kernel can close its handles, unmap its address space and refund its quota, and none of that
// tells the device to stop writing: there is no IOMMU, and the physical address it was given is
// still just an address. So those frames are held rather than recycled, and this is the one thing
// that can say they are safe - because the caller has just reset the hardware.
pub const SYS_DEVICE_QUIESCED: u64 = 74;

// The most capabilities one message may carry: stdin, stdout, stderr and one spare. Bounded
// like everything else here, so a sender cannot make the receiver allocate by asking.
pub const MAX_MESSAGE_CAPS: usize = 4;

// The most bytes one message may carry. There was no limit at all: `sys_channel_send` sized a
// kernel `Vec` straight off the caller's length and built the payload BEFORE the message was
// charged to any quota, so one syscall could ask the kernel for an allocation of any size, and
// an infallible `vec!` answers exhaustion by aborting rather than returning. The quota bounded
// what could be QUEUED and not what could be allocated on the way there.
//
// 1 MiB is far above what the services exchange (a launch context is capped at 64 KiB) and far
// below anything that threatens the kernel heap.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

// The largest a MemoryObject or a DmaBuffer may be, in bytes.
//
// The same lesson one object over. IPC, ELF images and wait sets all had ceilings and these did
// not: the size went from the syscall to `pages_for` to `pages as u64 * PAGE_SIZE` with no checked
// arithmetic and then to `Vec::with_capacity(pages)`, which answers an impossible request by
// ABORTING the kernel. The Domain's memory quota bounds what a caller may HOLD, and it is checked
// after the arithmetic that can wrap - so it was never the thing standing between a number and the
// allocator.
//
// 1 GiB is far above anything the system allocates in one object (the largest is a 4K framebuffer
// at about 32 MiB) and far below what the arithmetic can lose.
pub const MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;

// The most handles one `SYS_WAIT_ANY` may name. It was bounded by how many handles the caller
// holds, which is a limit that another finding shows is itself reachable past its ceiling - so
// this is the fixed one the audit asked for.
pub const MAX_WAIT_HANDLES: usize = 256;

// The most objects one wait SET may hold - the persistent form of the same question, and the same
// number for the same reason. Stated here rather than only in the kernel because a service sizing
// its client table against it needs to read it: StorageService's ceiling used to be a number picked
// around a defect, and deriving it from the set's own limit is what replaced that.
pub const MAX_WAIT_SET_MEMBERS: usize = 256;

// The largest ELF image `SYS_PROCESS_LOAD` will read out of a caller's buffer. A program that
// does not fit is refused rather than sized into a kernel allocation.
pub const MAX_ELF_BYTES: usize = 64 * 1024 * 1024;

pub const SYS_PROCESS_GROUP_CREATE: u64 = 65;
pub const SYS_PROCESS_GROUP_SIGNAL: u64 = 66;
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
	// EXPLICIT, because the four bytes are there either way and the kernel copies this
	// struct to userspace as raw bytes.
	//
	// `repr(C)` inserts them to align `bar_len`, and Rust does not promise that padding in an
	// otherwise initialised value is initialised - so `write_user`, which copies `size_of::<T>()`
	// bytes, was handing userspace four bytes of whatever the kernel stack held there. Naming the
	// field and assigning it 0 makes them a value rather than a gap.
	//
	// This does NOT move anything: the padding occupied exactly these four bytes already, so
	// `bar_len`'s offset, the size and the alignment are all unchanged. Every other struct in this
	// file that needed one already had an explicit `_pad`; this one was missed.
	pub _pad0: u32,
	// length of the MMIO window the DeviceMemory capability covers.
	pub bar_len: u64,
	// byte offsets of the virtio structures within that window.
	pub common_offset: u32,
	pub notify_offset: u32,
	pub notify_multiplier: u32,
	pub isr_offset: u32,
	pub device_offset: u32,
	// The device's PCI address. Two devices of one type are otherwise indistinguishable
	// to userspace, so this is what lets a second instance of a device class be bound to
	// a different program than the first without relying on enumeration order.
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
	// THE STANDARDS IDENTITY, which discovery already had and did not pass on.
	//
	// The kernel's PCI scan resolves class, subclass and programming interface for every function
	// and retains them for `lspci` - so the bytes existed, and the one consumer that most needs
	// them could not see them. Binding by `device_type` alone means every driver is selected by a
	// vendor-defined number, which is what P02M0098 exists to stop: standard class/subclass/
	// interface is how a driver claims a FAMILY of hardware rather than one model.
	//
	// THE STRUCT GREW: 40 bytes to 48, and the comment here used to say it did not.
	//
	// It claimed these three "occupy the byte that was `_pad` plus the two the struct's tail
	// alignment already held". There was no tail padding to use - 40 is already 8-aligned, so the
	// old layout ended exactly at `_pad` - and three bytes past `func` take the struct to 42, which
	// rounds to 48. Nothing BEFORE them moves, which is the half that was true.
	//
	// The layout assertion is what caught it, by failing on the size rather than on the offsets:
	// this is the test doing the job it was reopened to do, on the change that landed next.
	//
	// It is an ABI change and it is permitted here for the reason the whole file states: nothing is
	// versioned before the first final release, and kernel and userspace are built and shipped
	// together, with `ABI_VERSION` refusing a stale artifact at startup if they ever are not.
	pub class: u8,
	pub subclass: u8,
	pub prog_if: u8,
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
// The stable ABI codes `ObjectInfo::object_type` carries.
//
// The mapping lived in `ObjectType::code()` in `src/kernel/object/mod.rs`, so changing it there
// moved nothing here and userspace had no way to find out at compile time - a value documented as
// a stable ABI code whose definition was outside the ABI. The kernel's `code()` returns these now,
// which makes the two the same fact rather than two facts that agree.
pub const OBJECT_TYPE_DOMAIN: u64 = 0;
pub const OBJECT_TYPE_PROCESS: u64 = 1;
pub const OBJECT_TYPE_THREAD: u64 = 2;
pub const OBJECT_TYPE_ADDRESS_SPACE: u64 = 3;
pub const OBJECT_TYPE_MEMORY_OBJECT: u64 = 4;
pub const OBJECT_TYPE_CHANNEL: u64 = 5;
pub const OBJECT_TYPE_EVENT: u64 = 6;
pub const OBJECT_TYPE_TIMER: u64 = 7;
pub const OBJECT_TYPE_INTERRUPT: u64 = 8;
pub const OBJECT_TYPE_DEVICE_MEMORY: u64 = 9;
pub const OBJECT_TYPE_DMA_BUFFER: u64 = 10;
pub const OBJECT_TYPE_PROCESS_GROUP: u64 = 11;
pub const OBJECT_TYPE_PRIVILEGE: u64 = 12;
pub const OBJECT_TYPE_WAIT_SET: u64 = 13;

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
	// What a finished process reported, which `state` alone cannot say. For PROC_STATE_STOPPED
	// - a clean exit - this is the status the program passed to `exit_with`, and
	// `completion_valid` is 1. For a process still running, or one that faulted or was killed
	// and so never got to report anything, `completion_valid` is 0 and this is meaningless.
	//
	// The pair exists because 0 is both the most common success value and the natural "nothing
	// here" value, and a caller deciding whether a command succeeded must not have to guess
	// which one it is looking at.
	pub completion: u64,
	pub completion_valid: u64,
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
	pub memory_peak: u64,
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

// The machine cannot do this, and no retry or smaller request will change that. Distinct from
// `ERR_INVALID` (the request was malformed) and from `ERR_RESOURCE_EXHAUSTED` (there was not enough
// of something right now): the caller asked a reasonable question of a machine that has no answer.
pub const ERR_UNSUPPORTED: i64 = -13;

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
// Every right, computed rather than written. `0xfff` was a hand-written literal beside twelve
// individually defined bits, so the thirteenth right would have been silently absent from it -
// a right that exists and that "all rights" does not grant.
pub const RIGHTS_ALL: u32 = RIGHT_READ | RIGHT_WRITE | RIGHT_EXECUTE | RIGHT_MAP | RIGHT_SEND | RIGHT_RECEIVE | RIGHT_DUPLICATE | RIGHT_TRANSFER | RIGHT_REVOKE | RIGHT_GET_INFO | RIGHT_MANAGE | RIGHT_WAIT;

pub const EXECUTABLE_SUFFIX: &str = ".lsexe";

pub fn executable_aliases_ambiguous(first: &[u8], second: &[u8]) -> bool {
	fn expands_to(shorter: &[u8], longer: &[u8]) -> bool {
		longer.len() == shorter.len() + EXECUTABLE_SUFFIX.len() && longer.starts_with(shorter) && longer[shorter.len()..] == *EXECUTABLE_SUFFIX.as_bytes()
	}
	expands_to(first, second) || expands_to(second, first)
}

// PKGARCH1 archive format - a 16-byte header (8-byte magic, u32 entry count, u32
// reserved), then one 72-byte entry per file (64-byte NUL-padded name, u32 blob
// offset, u32 size), then the concatenated blobs. All integers little-endian.
// Written by the kernel build.rs, read by the kernel pkg.rs and the userspace
// storage runtime.
pub const PKG_MAGIC: &[u8; 8] = b"PKGARCH1";
pub const PKG_HEADER_LEN: usize = 16;
pub const PKG_ENTRY_LEN: usize = 72;
pub const PKG_NAME_LEN: usize = 64;

// A parsed PKGARCH1 archive borrowing the underlying bytes. The single reader for
// the format: the kernel (init/volume packages) and the userspace storage runtime
// both decode the layout above through this one implementation, so the on-disk
// format and its parser never drift apart.
pub struct Package<'a> {
	bytes: &'a [u8],
	count: usize,
}

// What a package entry's name may be, for the reader AND the writer.
//
// The two disagreed. `Package::parse` required a canonical name - non-empty, everything past the
// terminator zero - and `build_package` checked only `is_empty()` and the length, so the SSOT crate
// could produce archives its own reader called invalid: duplicate names, and a NUL inside a name,
// where `b"foo\0bar"` was written as one thing and read back as `foo` with non-zero padding after
// it. `b"foo\0"` was worse: a name the writer treated as distinct from `b"foo"` and the reader
// parsed as exactly that.
// The most entries any package may hold - the READER's bound, which is not the bootstrap writer's.
//
// These are two different limits and conflating them broke the guest immediately: the bootstrap set
// is a handful of boot programs and `bootstrap::MAX_ENTRIES` bounds it at 64, while the system
// VOLUME package is every program that ships and has 146 today. A reader ceiling of 64 refused it,
// which the guest reported as "file missing from the volume package" - measured rather than
// reasoned about, and the reason this number is generous.
//
// What it is for is the quadratic pass below: each entry is compared against every earlier one, so
// an archive declaring an enormous table drove that work with only the buffer bounding it. 4096 is
// far above anything this system packages and far below where n^2 costs anything.
pub const MAX_PACKAGE_ENTRIES: usize = 4096;

pub fn valid_package_name(name: &[u8]) -> bool {
	!name.is_empty() && name.len() <= PKG_NAME_LEN && !name.contains(&0)
}

impl<'a> Package<'a> {
	// Parse and validate the WHOLE archive, not just its header.
	//
	// The header checks and the checked arithmetic were right as far as they went, and they went to
	// the end of the entry table: the reserved field was not examined, a blob offset could point
	// into the header or into the table itself, blob ranges could overlap, two entries could share
	// a name, a name could carry garbage after its first NUL, an empty name was accepted, and a bad
	// offset on an entry nobody looked up was not found until somebody did.
	//
	// As a trusted build artifact that is survivable. As a parser reading bytes off a disk it is
	// not, and this is the same crate either way - so every entry is walked here, once, and after
	// `Some(Package)` a caller may treat the archive as well formed.

	pub fn parse(bytes: &'a [u8]) -> Option<Self> {
		if bytes.len() < PKG_HEADER_LEN {
			return None;
		}
		if &bytes[0..8] != PKG_MAGIC {
			return None;
		}
		// The RESERVED word, which the format reserves and no reader looked at. A field nothing
		// checks is a field a writer may fill with anything, and then it is not reserved.
		if bytes[12..16] != [0, 0, 0, 0] {
			return None;
		}
		let count = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
		// AND A CEILING ON THE READER. The strict pass compares each entry against every earlier
		// one, which is O(n^2), and `bootstrap::MAX_ENTRIES` bounds the WRITER only - so a hostile
		// archive with a large enough entry table drove quadratic work. The bound is the same one
		// the writer keeps, stated once here for both.
		if count > MAX_PACKAGE_ENTRIES {
			return None;
		}
		let table_end = PKG_HEADER_LEN.checked_add(count.checked_mul(PKG_ENTRY_LEN)?)?;
		if table_end > bytes.len() {
			return None;
		}
		let package = Self { bytes, count };
		// Every entry: a canonical name, a blob inside the data region, and no two entries naming
		// the same file or claiming the same bytes.
		for index in 0..count {
			let base = PKG_HEADER_LEN + index * PKG_ENTRY_LEN;
			let stored = &bytes[base..base + PKG_NAME_LEN];
			let name = match stored.iter().position(|&b| b == 0) {
				Some(end) => {
					// CANONICAL: everything past the terminator must be zero. Otherwise one archive
					// has two byte-level spellings of the same name, and a checksum over the file
					// says they are different while every reader says they are the same.
					if stored[end..].iter().any(|&b| b != 0) {
						return None;
					}
					&stored[..end]
				}
				None => stored,
			};
			if !valid_package_name(name) {
				return None;
			}
			let (offset, size) = package.extent(index)?;
			// A blob may not start inside the header or the entry table - that would have an entry
			// describing the structure that describes it - and must end inside the buffer.
			if offset < table_end || offset.checked_add(size)? > bytes.len() {
				return None;
			}
			for earlier in 0..index {
				if package.name(earlier)? == name {
					return None;
				}
				let (other_offset, other_size) = package.extent(earlier)?;
				// Overlapping blobs mean two files share bytes: rewriting one silently rewrites the
				// other, and no reader of either can tell.
				if size != 0 && other_size != 0 && offset < other_offset + other_size && other_offset < offset + size {
					return None;
				}
			}
		}
		Some(package)
	}

	// The (offset, size) of the `index`-th blob, straight off the entry table.
	fn extent(&self, index: usize) -> Option<(usize, usize)> {
		if index >= self.count {
			return None;
		}
		let base = PKG_HEADER_LEN + index * PKG_ENTRY_LEN;
		let entry = &self.bytes[base..base + PKG_ENTRY_LEN];
		let offset = u32::from_le_bytes(entry[PKG_NAME_LEN..PKG_NAME_LEN + 4].try_into().ok()?) as usize;
		let size = u32::from_le_bytes(entry[PKG_NAME_LEN + 4..PKG_NAME_LEN + 8].try_into().ok()?) as usize;
		Some((offset, size))
	}

	// The `index`-th file's blob. Every extent was validated at parse time, so this cannot be out
	// of bounds - which is what lets `entries()` yield without a second scan.
	pub fn blob(&self, index: usize) -> Option<&'a [u8]> {
		let (offset, size) = self.extent(index)?;
		self.bytes.get(offset..offset + size)
	}

	// Every (name, blob) in order, without a lookup per entry.
	//
	// `lookup` scans from zero, so enumerate-plus-lookup is quadratic in the entry count. It does
	// not matter for the init package and it is the shape that stops mattering only until it does.
	pub fn entries(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + '_ {
		(0..self.count).filter_map(|index| Some((self.name(index)?, self.blob(index)?)))
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

// The bootstrap list and the archive built from it. Here rather than in the loader because the
// loader is a UEFI binary and nothing in it can be tested on the host.
pub mod bootstrap;

#[cfg(test)]
mod tests;
