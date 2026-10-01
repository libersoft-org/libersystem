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
//
// DISCONNECT_OP took 0xfffa the same way on 2026-08-20, and the same check still holds: the highest
// `@op` across the sixteen schemas is 16. It is the one control message a service RECEIVES without
// a client having sent it - `serve_multi` synthesises it for the handler when a client's channel
// closes, because a service that holds per-client state (a prepared launch, a lease, a scope) has
// no other way to learn that the client is gone (IDL-001).
pub const TYPED_OP_MAX: u16 = 0xfff9;
pub const DISCONNECT_OP: u16 = 0xfffa;
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
// `SYS_DOMAIN_CREATE(memory, handles, threads, parent)`: a child Domain of `parent` - a Domain handle holding MANAGE -
// or, for zero, of the caller's own Domain.
pub const SYS_DOMAIN_CREATE: u64 = 19;
pub const SYS_DOMAIN_KILL: u64 = 20;
pub const SYS_YIELD: u64 = 21;
pub const SYS_OBJECT_INFO_GET: u64 = 22;
pub const SYS_WAIT: u64 = 23;
// Create a DMA buffer.
//   a0 = size in bytes
//   a1 = a DeviceMemory handle with WRITE, naming the device the buffer is for (0 = none)
//
// The second argument arrived with the DMA lifetime rule - the kernel holds a dead driver's
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
//   a1 = POWER_REBOOT, POWER_OFF or POWER_OFF_WITHIN
//   a2 = for POWER_OFF_WITHIN, the seconds from now
// Does not return on success, but for POWER_OFF_WITHIN, which answers 0 once the deadline is armed; ERR_ACCESS_DENIED for
// a handle that is not the root domain's or lacks MANAGE, ERR_INVALID for an unknown action or a zero bound.
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
// THE UNIT EVERY SYSCALL DEADLINE IS IN.
//
// `SYS_CLOCK_GET` answers the scheduler's coarse tick counter and every `deadline` argument in this
// ABI - `SYS_WAIT`, `SYS_WAIT_ANY`, `SYS_WAITSET_WAIT`, `SYS_TIMER_SET` - is an absolute value on
// that counter. `SYS_CLOCK_MONO_NS` answers NANOSECONDS, which is the unit every TIMING CONTRACT in
// this system is expressed in, because a frame deadline measured in ten-millisecond steps is not a
// frame deadline at all.
//
// SO THE TWO UNITS MEET IN USERSPACE, and the conversion needs the rate. It was written out as a
// private `100` in the one service that needed it, and the next caller that needed it got the
// conversion wrong in the direction that never wakes up: a nanosecond value passed as a tick
// deadline is a wait of about four months. It is stated here, once, beside the calls that take it.
pub const TICKS_PER_SECOND: u64 = 100;
pub const NANOS_PER_TICK: u64 = 1_000_000_000 / TICKS_PER_SECOND;

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
// Per-member `ProcessStats` for a ProcessGroup, in CREATION ORDER - which for a pipeline is the
// order of the line, so a caller can say which stage failed rather than that something in it did.
//
// It reads the group's own record of what each member finished as, captured when that member
// reached a terminal state. Reading the live processes instead would be a race dressed as an API:
// a group holds its members weakly so it never keeps a dead one alive, so by the time anybody asks
// about a finished pipeline the processes may be gone, and the answer would depend on how quickly
// the question was asked.
pub const SYS_PROCESS_GROUP_STATS: u64 = 75;

// THIS BOOT'S IDENTITY, so an event can say which boot it came from.
//
// It belongs to the KERNEL and not to the first userspace process, because the one event it has to
// survive is that process being replaced: an id owned by SystemManager would be re-minted by its
// successor, and every record from before the replacement would then claim to be from a boot that
// no longer exists. The kernel boots once per id by construction.
//
// Not a secret and not a sequence. It is an arbitrary nonzero number drawn once from the insecure
// source - which is the use that source's own comment names first - and its only property is that
// two boots do not share one.
pub const SYS_BOOT_ID: u64 = 76;

// THE DEVICE CLAIM. `SYS_DEVICE_ACQUIRE` answers with one `DeviceMemory` handle and nothing else,
// and that handle is precisely what gets sent on to the driver - so after a successful bind the
// manager holds nothing about the claim at all, and there is no way for it to learn which binding
// of the device this is, to read what state the device is in, or to take the device back from a
// driver that has stopped cooperating.
//
// `SYS_DEVICE_CLAIM` answers with BOTH: a `ClaimKey` copied into the caller's memory and a Claim
// handle installed in its table. The key is what stamps every capability derived from this binding;
// the handle is what STAYS with the manager - minted without RIGHT_TRANSFER and without
// RIGHT_DUPLICATE, so it cannot leave the Domain that made it - and it is waitable, because a
// manager built on one `wait_any` loop cannot spin on a status.
//
// `SYS_DEVICE_RELEASE` takes the whole key and starts the teardown; a key naming a generation that
// is no longer current is refused rather than applied to whoever holds the device now.
// `SYS_DEVICE_CLAIM_INFO` reads the claim's state and its terminal result out of the handle.
//
// THE FOURTH ARGUMENT NAMES THE REGISTRY ENTRY. `SYS_DEVICE_CLAIM(index, privilege, grant_ptr,
// entry_ptr)`: `entry_ptr` points at `ENTRY_NAME_LEN` bytes holding the NUL-padded program name of
// the registry entry DeviceManager selected for this device - the exact priority or fallback
// candidate it is attempting, which is the identity the manifest already validates and the
// operator's stored `select=` already carries. The kernel validates that the entry EXISTS in the
// table generated from the same manifest, that its match rules admit THIS device, and that its
// declared DMA policy is admissible under the boot's DMA mode; it does not recompute priority, so an
// operator's deliberate lower-priority choice is honoured. The grant carries the entry, the policy
// and the newly minted generation back, and the claim handle reports the same three.
pub const SYS_DEVICE_CLAIM: u64 = 77;
pub const SYS_DEVICE_RELEASE: u64 = 78;
pub const SYS_DEVICE_CLAIM_INFO: u64 = 79;

// Move ONE capability with less authority than the sender holds.
//
// `SYS_CHANNEL_SEND` moves a handle with the rights it already has; there is no way to hand
// something over with less than you hold, so "arrives without RIGHT_TRANSFER" was not a property of
// the existing primitive and had to become one. There is no room for a fifth argument - the ABI
// carries exactly four and the ordinary send spends all of them - so the last one is a POINTER to a
// `CapTransfer` in the caller's memory rather than a handle.
//
// The receiver gets the INTERSECTION of the capability's rights and the mask. A mask naming a right
// the capability does not hold is not an error, because an intersection cannot widen. Inside, this
// is the SAME transactional move the ordinary send performs - a mask applied to an existing
// transfer, not a second way of moving a capability - so a send that fails leaves the sender's
// handle open at the same value with its rights unchanged.
pub const SYS_CHANNEL_SEND_ATTENUATED: u64 = 80;

// WHAT A NEW MANAGER MAY ASK ABOUT A DEVICE IT HOLDS NO CLAIM ON.
//
// `SYS_DEVICE_CLAIM_INFO` reads a claim through its HANDLE, which is exactly what a reconstruction
// does not have: the old manager died and its handle died with its Domain. This reads by device
// INDEX, gated on the DeviceManager privilege, and answers the generation, the claim state and the
// deadline a teardown under way must confirm by - so "may I bind this?" and "how long is it
// reasonable to wait?" are one read rather than two sources that can disagree.
pub const SYS_DEVICE_CLAIM_SNAPSHOT: u64 = 81;
// WHAT A PROCESS MUST RUN BEFORE ITS FIRST LINE AND AFTER ITS LAST.
//
// An image may carry a table of functions to call once before anything else runs and once after
// everything else has. Nothing in this system ran them: `liber_rt_start` performed the ABI check and
// called the entry point directly, so a constructor in a loaded module was an initialisation that
// silently did not happen - and an image whose initialisation did not happen is not the image the
// audit checked.
//
// THE CALLER READS ITS OWN TABLE AND NOBODY ELSE-S. The entries are in LOAD order, which is the
// provider order the loader was given - so running them in order runs a provider-s constructor
// before its consumer-s, and running them in reverse does the same for destructors. It takes no
// handle because there is no other process it could name: a table of addresses in another process-s
// address space would mean nothing here.
pub const SYS_PROCESS_LIFECYCLE: u64 = 82;

// ENTROPY IN, AND THE SUBMITTER DOES NOT SAY WHAT IT IS WORTH.
//
// A driver holding a live claim on an entropy device hands the kernel BYTES; the kernel decides what
// they are credited, from the kind of source, at a rate below one bit per bit. That division is the
// whole security of the call: a submitter that could name its own credit could seed a machine to
// "fully healthy" with a constant, and nothing downstream of `SYS_RANDOM_GET` could tell.
//
// The authority is the device capability, checked against the CURRENT claim on a function whose type
// is the entropy device's. A capability from a previous binding is held by somebody who is no longer
// driving that device, which is the same rule `SYS_DEVICE_QUIESCED` applies for the same reason.
//
// Answers the number of BITS credited - which is not the number of bytes submitted and is often
// zero, because a device that answered a request with nothing has still said something.
pub const SYS_ENTROPY_ADD: u64 = 83;

// What the pool holds and where it came from, as `EntropyHealth`. CREDIT AND PROVENANCE, NEVER A
// VERDICT: whether a machine seeded only by a paravirtual device is good enough for a given purpose
// is the caller's question, and a kernel that answered it would be answering for every future caller
// too.
pub const SYS_ENTROPY_HEALTH: u64 = 84;

// Move SEVERAL capabilities, each with less authority than the sender holds.
//
// `SYS_CHANNEL_SEND_ATTENUATED` moves exactly one, and one is not enough for a reply that hands over
// a PAIR: a present queue's two completion endpoints travel in the same answer, and sending them one
// at a time would mean two messages for a record the schema says is one. `caps_ptr` points at
// `[count, CapTransfer * count]` - the count travels with the list for the same reason the ordinary
// multi-cap send's does, which is that the four argument registers are already spent.
//
// Each receiver end gets the INTERSECTION of that capability's rights and its own mask, and the masks
// are per capability because the two halves of a completion pair are not the same authority: one may
// only SEND and the other may only RECEIVE and WAIT. A mask naming a right the capability does not
// hold is not an error, because an intersection cannot widen.
//
// EVERY handle must carry TRANSFER before anything is sent, exactly as in the unattenuated form, so a
// list with one bad entry moves nothing - and a refused send leaves every one of the sender's handles
// open at the same value with its rights unchanged.
pub const SYS_CHANNEL_SEND_CAPS_ATTENUATED: u64 = 85;

// Register the channel the kernel reports BUS ARRIVALS AND DEPARTURES on, taking a DeviceManager
// privilege because the caller is the one component that binds drivers.
//
// A NOTIFICATION AND NOT A POLL, and the same shape as `SYS_CONSOLE_ATTACH`: the kernel holds the
// sending end and the caller waits on the other. Each message is a kind byte - `DEVICE_ARRIVED` or
// `DEVICE_DEPARTED` - and the device index it is about, little-endian. The INVENTORY is the truth
// and the event is the prompt: a manager that missed one and later has reason to look up that index
// finds the same answer.
pub const SYS_DEVICE_EVENTS: u64 = 86;

// Register the channel the kernel reports FIXED-HARDWARE PLATFORM EVENTS on - today the power and
// sleep buttons, decoded from the ACPI PM1 event block.
//
// THE KERNEL DECODES AND THE HOLDER ACTS, and the split is forced rather than chosen. The buttons
// arrive on a shared, level-triggered interrupt whose status register must be acknowledged inside
// the handler or the machine makes no further progress, and that register lives in an address space
// the public driver resource vocabulary has no object for. So the kernel is already holding the
// register when the event is decoded; what is left to hand on is the EVENT.
//
// IT TAKES THE SAME PRIVILEGE AS `SYS_DEVICE_EVENTS` AND THAT IS THE DECISION. A fifth privilege
// kind would name a narrower authority, and it would be narrower than nothing: the holder of this
// one already receives every arrival and departure on the bus and may take any device's BAR out of
// the kernel. Learning that somebody pressed the power button is strictly less than what it can
// already do, so a separate capability would add a name without adding a boundary.
//
// Each message is ONE BYTE: the kind. A press has no payload, and the two buttons are separate
// kinds rather than one with a flag, because this system can act on exactly one of them.
pub const SYS_PLATFORM_EVENTS: u64 = 87;

// A HANDLE TO THE CALLING PROCESS ITSELF, carrying MANAGE.
//
// A PROCESS COULD NOT NAME ITSELF, AND THAT WAS THE WHOLE OF WHAT WAS MISSING. `SYS_THREAD_CREATE`
// takes the entry and the stack top from its caller and validates both, and `SYS_THREAD_START` is a
// separate gated step beside it - so a program making a thread of its own was already expressible
// in every respect except one: the syscall wants a `Process` handle carrying MANAGE, and nothing
// handed a program one for itself. Every caller in this tree was a spawner naming a CHILD.
//
// WHY THE AUTHORITY IS BOUNDED WITHOUT WITHHOLDING THE HANDLE. MANAGE on your own process is the
// authority to create threads in it for ever, and this kernel already bounds that with
// `PROP_THREAD_LIMIT` - a limit a process cannot raise for itself, because raising it is
// `SYS_OBJECT_PROPERTY_SET` on a handle the process does not hold. So the bound that matters was
// already in place and was not the one being enforced by the missing handle.
//
// AND MANAGING YOURSELF DOES NOT INCLUDE LOADING CODE INTO YOURSELF. That is the one authority in
// MANAGE that would have meant something new: `SYS_PROCESS_LOAD` and `SYS_PROCESS_LOAD_MODULE` map
// executable pages, so a process holding MANAGE over itself could turn any bytes it had into
// instructions and the W^X the loader enforces would be a formality. Both refuse a handle that
// names the caller's own process. Nothing in this tree wanted that: every load is a spawner
// building a child before the child runs.
//
// It takes no argument. There is no other process it could name and none it would be allowed to.
pub const SYS_PROCESS_SELF: u64 = 88;

// THE FRAME ACCOUNT'S RECORD BUFFER, and the two calls that reach it. They
// answer `ERR_UNSUPPORTED` on every boot but a `development-trace` one, which is the only boot the
// kernel gives the buffer its storage on - so the ordinary path pays one cached-flag test per site in
// the runtime and never reaches the kernel at all.
//
// `SYS_PERF_RECORD(site, cycles, value)` appends one record: an eight-byte ASCII site tag packed
// little-endian into `site`, the caller's own clock reading and one value; the kernel adds the thread
// and the core. It answers 0 when the record was taken, `PERF_RECORD_UNARMED` when no window is open
// (dropped, not counted) and `PERF_RECORD_REFUSED` when the buffer is full (refused AND counted - it
// never wraps over an earlier record). An append never blocks, allocates or formats.
//
// `SYS_PERF_CONTROL(op)` ARMS the buffer (empties it and starts accepting - and from then on the
// scheduler appends a record at every context switch and every wake), DISARMS it, or DRAINS it:
// disarms, then writes every record to the debug serial as a `\x1ePERF` line, a table naming each
// thread's process and one line of counts, and answers the number of records written.
pub const SYS_PERF_RECORD: u64 = 89;
pub const SYS_PERF_CONTROL: u64 = 90;
pub const PERF_CONTROL_ARM: u64 = 1;
pub const PERF_CONTROL_DISARM: u64 = 2;
pub const PERF_CONTROL_DRAIN: u64 = 3;
pub const PERF_RECORD_UNARMED: i64 = 1;
pub const PERF_RECORD_REFUSED: i64 = 2;

// PORT I/O AS A CAPABILITY. x86_64 reaches a device's registers through the separate 64 KiB port space
// as well as through memory, and ring 3 may execute `in` and `out` only for the ports its task-state
// segment's permission bitmap allows. The authority to a RANGE of ports is an object, `PortRange`, and
// holding one grants nothing until its holder maps it: mapping sets the range's bits in the holder's
// process, and a thread of that process may then use those ports and no others. ARM and RISC-V have no
// port space, so every call below answers `ERR_UNSUPPORTED` there and no row ever carries a port
// resource.
//
// `SYS_DEVICE_RESOURCE_ACQUIRE(claim, kind, index)` mints ONE RESOURCE OF A CLAIMED ROW, named by its
// kind and its index on that row and never by address, for a claim handle carrying `RIGHT_MANAGE` (as
// `SYS_DEVICE_MSIX_ACQUIRE` takes it). The one kind so far is `RESOURCE_KIND_PORT_RANGE`, whose index is
// a position in `DeviceInfo::ports`; it answers a `PortRange` handle with `RIGHT_MAP | RIGHT_TRANSFER`
// and registers it as a derived object of the claim, so the claim's release revokes it. A range whose
// ports are reserved to the kernel, are in another live grant, or were retired this boot is refused.
//
// `SYS_PORT_RANGE_MAP(range)` grants the range to the CALLER's process - a range is mapped into at most
// one process at a time - and answers 0. `SYS_PORT_RANGE_UNMAP(range)` takes it back from the caller and
// answers 1 when every core confirmed it no longer lets the caller use those ports, or 0 when one did not
// answer in time: the range is then out of the caller's process all the same, and its ports stay out of
// every later grant this boot.
//
// `SYS_PORT_RANGE_FIRMWARE(privilege, base, len)` is the ONE call that takes a base and a length from its
// caller, for the firmware interpreter's SystemIO operation regions, gated by the `FirmwareInterpreter`
// privilege and held to the same reserved set and exclusivity as a claim's range.
pub const SYS_DEVICE_RESOURCE_ACQUIRE: u64 = 91;
pub const SYS_PORT_RANGE_MAP: u64 = 92;
pub const SYS_PORT_RANGE_UNMAP: u64 = 93;
pub const SYS_PORT_RANGE_FIRMWARE: u64 = 94;

// The kinds `SYS_DEVICE_RESOURCE_ACQUIRE` mints.
pub const RESOURCE_KIND_PORT_RANGE: u64 = 1;
// ONE MMIO RANGE OF A PLATFORM ROW, `index` a position in `PlatformPart::mmio`, answered as a
// `DeviceMemory` with `RIGHT_READ | RIGHT_WRITE | RIGHT_MAP | RIGHT_TRANSFER` and registered as derived
// from the claim. Range 0 is the claim's own memory - `ClaimGrant::memory` - so this kind answers
// indices from 1, and refuses 0 rather than minting the same registers twice.
pub const RESOURCE_KIND_MMIO: u64 = 2;
// ONE WIRED INTERRUPT LINE OF A PLATFORM ROW, `index` a position in `PlatformPart::lines`, answered as an
// `Interrupt` carrying the rights an MSI's does, bound to the line on its controller - configured with
// the row's trigger and polarity, routed to the boot core and enabled - and registered as derived from
// the claim, whose release masks the line and gives its vector or identity back. A LEVEL line is masked
// at its controller when it fires and unmasked by `SYS_INTERRUPT_ACK`, so a source that stays asserted
// until its driver runs fires once rather than for ever. A line another live claim holds is refused.
pub const RESOURCE_KIND_LINE: u64 = 3;
// THE KERNEL CONSOLE'S TAP, for a platform row carrying `PLATFORM_FLAG_CONSOLE` - `index` 0 - answered as a
// `ConsoleTap` with `RIGHT_READ | RIGHT_WAIT | RIGHT_TRANSFER` and registered as derived from the claim. The
// claim of such a row took the console UART from the kernel; the kernel's output still goes into its
// transmit ring, and the tap is how it leaves: `SYS_CONSOLE_TAP_READ` moves it out in order. The tap is
// ready to wait on when the ring goes from empty to holding bytes. The release revokes it and gives the
// UART back to the kernel.
pub const RESOURCE_KIND_CONSOLE_TAP: u64 = 4;
// A CLAIMED ROW'S DECLARED REGISTERS - `index` 0 - for a row that declares any: a PCI function whose (vendor,
// device) row names configuration registers (the i6300esb's 0x60 as a word and 0x68 as a byte) or a chipset
// register in memory (the ICH9's GCS), and a platform row whose firmware names system-memory registers (a WDAT's).
// Answered as a `Registers` capability with `RIGHT_READ | RIGHT_WRITE | RIGHT_TRANSFER`, derived from the claim;
// `SYS_DEVICE_REGISTER_READ` and `SYS_DEVICE_REGISTER_WRITE` reach one register at a time, by its index in the
// row's declaration, at EXACTLY its declared width. ERR_INVALID for a row that declares none.
pub const RESOURCE_KIND_REGISTERS: u64 = 5;

// Where a row's port resource came from.
//
// AN I/O BAR of the function, recorded by the boot scan; minted with the ordinary claim, which enables
// the function's I/O decode and whose release disables it.
pub const PORT_SOURCE_IO_BAR: u8 = 1;
// A CHIPSET SUB-RANGE the kernel derived from a configuration register of the function (a block's base)
// and a derivation row. It never toggles the function's decode: the block is the chipset's.
pub const PORT_SOURCE_DERIVED: u8 = 2;
// A PLATFORM DEVICE's resource: a static table the kernel parses, its own declaration of a port it
// drives, or a firmware-reported `_CRS` descriptor.
pub const PORT_SOURCE_PLATFORM: u8 = 3;

// How many port resources one row can record: a function's six BARs and the sub-ranges and platform
// descriptors beside them.
pub const MAX_PORT_RESOURCES: usize = 8;

// One port resource of a row: `len` ports from `base`, where it came from, and its index in that source
// (the BAR number for an I/O BAR, the sub-range for a derived one).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PortResource {
	pub base: u16,
	pub len: u16,
	pub source: u8,
	pub index: u8,
	pub _pad: [u8; 2],
}

// What a platform event's one byte says.
pub const PLATFORM_EVENT_POWER_BUTTON: u8 = 1;
pub const PLATFORM_EVENT_SLEEP_BUTTON: u8 = 2;

// What a device event's first byte says.
pub const DEVICE_EVENT_ARRIVED: u8 = 1;
pub const DEVICE_EVENT_DEPARTED: u8 = 2;
/// The device is still on the bus and must not be used: a fatal PCIe error was reported against it
/// and the kernel has stopped it mastering the bus. Distinct from a departure because the address is
/// still valid and nothing has been unplugged, and distinct from a driver fault because the DEVICE is
/// what stopped being trustworthy.
pub const DEVICE_EVENT_FAULTED: u8 = 3;
/// THE ACPI SERVICE'S WALK IS PUBLISHED: every row, reservation and companion it reported, and every withdrawal of
/// what it did not report again, came before this. The eight bytes after the kind are the instance's number, not a
/// device index. DeviceManager reads again the row of every live binding at it (`SYS_DEVICE_NODE`).
pub const DEVICE_EVENT_NAMESPACE_LOADED: u8 = 4;

// The largest submission `SYS_ENTROPY_ADD` will read in one call. A bound rather than a buffer size:
// the credit is capped far below this anyway, so a larger call would be a larger copy for no more
// credit - and an unbounded copy driven by a device interrupt is a kernel stall a driver can ask for.
pub const MAX_ENTROPY_SUBMISSION: u64 = 4096;

// WHAT THE ENTROPY POOL SAYS ABOUT ITSELF. Every field is a count; none of them is an opinion.
//
// `seeded` is the one boolean and it says the credit threshold was reached - NOT that this machine
// has cryptographic-quality randomness. A guest resumed from a snapshot has a seeded pool and a host
// that is about to hand it the same bytes again, and no field here can detect that: what the numbers
// are for is letting an operator see that a machine is running on one paravirtual source.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EntropyHealth {
	pub credited_bits: u32,
	pub submissions: u32,
	pub paravirtual_submissions: u32,
	pub hardware_submissions: u32,
	pub draws: u64,
	// 1 when the credit threshold has been reached, 0 otherwise. A `u8` rather than a `bool` because
	// this crosses the syscall boundary, where a `bool` with a value other than 0 or 1 is undefined
	// behaviour the moment it is read.
	pub seeded: u8,
	// 1 when this machine has a hardware random instruction at all. Two of this system's three
	// architectures do not, which is the fact that makes the pool worth having.
	pub hardware_available: u8,
	pub _pad: [u8; 6],
}
// Actions for SYS_SYSTEM_POWER.
pub const POWER_REBOOT: u64 = 0;
pub const POWER_OFF: u64 = 1;
// A FORCED POWER-OFF DEADLINE, a2 seconds from now - see the kernel's `power` module. Returns 0; the earliest armed
// stands, and none is ever cancelled.
pub const POWER_OFF_WITHIN: u64 = 2;

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

// THE REGISTRY ENTRY IDENTITY ON THE CLAIM BOUNDARY: the manifest's program name, which
// `system-manifest` bounds to 1-64 bytes of `[A-Za-z0-9_-]`, carried as a FIXED 64-byte NUL-padded
// field. Fixed because the claim ABI is `repr(C)` and a variable-length field there is a second
// parser; 64 because that is the manifest's own bound, so the two cannot drift. A name is canonical
// or it was refused at manifest validation - there is no normalisation step anywhere.
pub const ENTRY_NAME_LEN: usize = 64;

// THE THREE DMA POLICIES A REGISTRY ENTRY DECLARES, as `ClaimGrant::policy` and `ClaimInfo::policy`
// report them. The same numbers `system_manifest::DmaPolicy::wire` produces.
//
// `none`: the driver receives its MMIO and interrupt resources, bus mastering stays OFF, and the
// binding cannot mint a DMA buffer. `iommu-required`: binds only behind an enforcing controller.
// `trusted-untranslated`: translated where translation exists, and admitted untranslated - loudly,
// into the degraded inventory - only on a boot whose mode says it has no controller.
pub const DMA_POLICY_NONE: u32 = 0;
pub const DMA_POLICY_IOMMU_REQUIRED: u32 = 1;
pub const DMA_POLICY_TRUSTED_UNTRANSLATED: u32 = 2;

// virtio device type codes, as written into `DeviceInfo::device_type` (the modern
// virtio-pci `device_id - 0x1040`). The single source of truth for the kernel's PCI
// enumeration and the userspace DeviceManager/DeviceService that classify devices.
pub const VIRTIO_TYPE_NET: u32 = 1;
pub const VIRTIO_TYPE_BLOCK: u32 = 2;
pub const VIRTIO_TYPE_CONSOLE: u32 = 3;
// The SCSI host controller. It carries the SCSI command set to targets behind it, which is what a
// machine with a real HBA looks like, rather than the tiny request format `virtio-blk` defines.
pub const VIRTIO_TYPE_SCSI: u32 = 8;
pub const VIRTIO_TYPE_RNG: u32 = 4;
pub const VIRTIO_TYPE_GPU: u32 = 16;
pub const VIRTIO_TYPE_INPUT: u32 = 18;
pub const VIRTIO_TYPE_SOUND: u32 = 25;
// THE HOST STREAM TRANSPORT. Not a network device: a vsock connection reaches one peer - the host -
// by context id and port, with no routing and no addressing of its own, which is why the driver
// publishes `local-stream` and deliberately not `net`.
pub const VIRTIO_TYPE_VSOCK: u32 = 19;
// AN I2C CONTROLLER and A GPIO CONTROLLER, whose device side is a vhost-user process in this system's
// harness: the controllers laptops carry have no emulation, and these are what the bus contracts are proved on.
pub const VIRTIO_TYPE_I2C: u32 = 34;
pub const VIRTIO_TYPE_GPIO: u32 = 41;

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

// VIRTIO_F_ACCESS_PLATFORM (feature bit 33) = bit 1 of the second feature word.
//
// "THE ADDRESSES I GIVE YOU ARE NOT PHYSICAL ONES." A device behind an IOMMU is configured with
// `iommu_platform=on` and then REQUIRES this bit: a driver that does not acknowledge it is telling
// the device it will program raw physical addresses, and the device refuses `FEATURES_OK` rather
// than accept descriptors it would translate wrongly. It is what makes a driver's buffers the
// kernel's revocable IOVAs rather than integers nothing constrains.
pub const VIRTIO_F_ACCESS_PLATFORM: u32 = 1 << 1;

// The MSI-X vector fields' reset value: no vector mapped (the device raises legacy INTx).
pub const VIRTIO_MSI_NO_VECTOR: u16 = 0xffff;

// WHAT `DeviceInfo::transport` SAYS, and it is deliberately a small closed set.
//
// A transport is how a driver TALKS to a function, which is a different question from what the
// function is. Both are needed to select a driver: "a virtio-pci function whose virtio type is 1"
// names a network device a virtio driver can drive, while class `02/00` alone names every Ethernet
// controller ever made - and handing a virtio driver one of those is the bug this separation exists
// to stop.
// A function on the PCI bus that speaks no virtio transport: its class triple is its whole
// identity. A PREDICATE, not an absence - a rule may name it, and the xHCI row does, because
// "virtio-pci" and "not virtio-pci" are what keep a virtio rule and a class rule from being able
// to match one function.
pub const TRANSPORT_PLAIN_PCI: u8 = 0;
// A virtio-pci function, so `device_type` is the virtio specification's own device type.
pub const TRANSPORT_VIRTIO_PCI: u8 = 1;
// A PLATFORM DEVICE - one the firmware describes rather than one that announces itself on a bus: the
// kernel declares it, a static table names it, the device tree has a node for it, or the ACPI namespace
// does. Its identity is `DeviceInfo::platform`, and `device_type` is `DEVICE_TYPE_PLATFORM`.
pub const TRANSPORT_PLATFORM: u8 = 2;

// Non-virtio device type codes live above the virtio id space (modern virtio types
// are below 0x40), so one `device_type` field classifies every discovered device.
pub const DEVICE_TYPE_XHCI: u32 = 0x100;

// THE CLASS TRIPLES THEMSELVES, here rather than in whichever file asks first. A standard PCI
// function's whole identity is `class / subclass / programming interface`, so the triple is the
// device's NAME - and the NVMe one was three bare literals inside the IOMMU bypass transition while
// the PCI resolver was about to spell it a second time. A number that means "NVMe" in two files is
// two numbers that agree until one of them is edited.
pub const PCI_CLASS_MASS_STORAGE: u8 = 0x01;
pub const PCI_SUBCLASS_NVM: u8 = 0x08;
pub const PCI_PROG_IF_NVME: u8 = 0x02;
// AHCI is the same mass-storage class as NVMe, a different subclass, and a programming interface
// that is the whole distinction between a controller this driver can drive and one in a legacy IDE
// compatibility mode that it must not touch.
pub const PCI_SUBCLASS_SATA: u8 = 0x06;
pub const PCI_PROG_IF_AHCI: u8 = 0x01;
// An SD host controller is a base system peripheral rather than a mass-storage controller, which is
// its own small surprise: the class says what the silicon IS and not what it carries.
// HD Audio is a multimedia controller with an audio-device subclass and no programming interface of
// its own, which is why the row below names zero rather than leaving it out: a triple with a hole in
// it would match the vendor-specific audio devices that carry other interfaces.
pub const PCI_CLASS_MULTIMEDIA: u8 = 0x04;
pub const PCI_SUBCLASS_AUDIO_DEVICE: u8 = 0x03;
pub const PCI_PROG_IF_HDA: u8 = 0x00;
pub const PCI_CLASS_BASE_PERIPHERAL: u8 = 0x08;
pub const PCI_SUBCLASS_SD_HOST: u8 = 0x05;
pub const PCI_PROG_IF_SD_HOST: u8 = 0x01;
pub const PCI_CLASS_SERIAL_BUS: u8 = 0x0C;
pub const PCI_SUBCLASS_USB: u8 = 0x03;
pub const PCI_PROG_IF_XHCI: u8 = 0x30;
// An NVM Express controller. 0x101 is taken by the unresolved row below, which was allocated before
// a second resolved family existed; the numbers are a namespace and not an order.
pub const DEVICE_TYPE_NVME: u32 = 0x102;
// A SATA controller in AHCI mode.
pub const DEVICE_TYPE_AHCI: u32 = 0x103;
// An Intel High Definition Audio controller.
pub const DEVICE_TYPE_HDA: u32 = 0x105;
// An SD/eMMC host controller conforming to the SD Host Controller specification.
pub const DEVICE_TYPE_SDHCI: u32 = 0x104;
// An IPMI system interface on PCI (class 0x0C, subclass 0x07): KCS (interface 0x01) or BT (0x02). Its register file
// is BAR 0 - resolved here when BAR 0 is memory; an I/O BAR 0 reaches its driver as a port range like any I/O BAR.
pub const PCI_SUBCLASS_IPMI: u8 = 0x07;
pub const PCI_PROG_IF_IPMI_KCS: u8 = 0x01;
pub const PCI_PROG_IF_IPMI_BT: u8 = 0x02;
pub const DEVICE_TYPE_IPMI_KCS: u32 = 0x107;
pub const DEVICE_TYPE_IPMI_BT: u32 = 0x108;
// QEMU'S SHARED-MEMORY FUNCTION, `ivshmem-plain` (1af4:1110), resolved by its identity rather than its class - its class
// is a memory controller's, which names nothing: BAR 2 is the memory a host file backs. What the harness's fixtures reach
// the guest through; no shipping device resolves this way.
pub const PCI_VENDOR_REDHAT: u16 = 0x1af4;
pub const PCI_DEVICE_IVSHMEM: u16 = 0x1110;
pub const DEVICE_TYPE_SHARED_MEMORY: u32 = 0x109;

// A FUNCTION THIS KERNEL RESOLVED NO PROFILE FOR, and that is a device type of its own rather than an
// absence. Every PCI function is in the inventory; the ones outside the two resolvers carry their
// standards identity - vendor, product, the class triple, the address - and no resources, so a rule
// can match one and nothing can claim what it does not have.
pub const DEVICE_TYPE_UNKNOWN: u32 = 0x101;

// A platform row: its identity is the row's platform part, not a number this table assigns.
pub const DEVICE_TYPE_PLATFORM: u32 = 0x106;

// The name for a device-type code, beside the codes it names.
//
// It lived in `lsirq` and nowhere else, so every other reporter of a device printed the raw number:
// the kernel's DMA isolation report said "device type 2 at 00:01.0" two lines above the same log
// writing `driver.virtio-blk`, and "type 256" for the xHCI - a number that is a name in exactly one
// place in the tree. One table, beside the constants, and both readers use it.
pub fn device_type_name(device_type: u32) -> &'static str {
	match device_type {
		VIRTIO_TYPE_NET => "virtio-net",
		VIRTIO_TYPE_BLOCK => "virtio-blk",
		VIRTIO_TYPE_SCSI => "virtio-scsi",
		VIRTIO_TYPE_CONSOLE => "virtio-console",
		VIRTIO_TYPE_RNG => "virtio-rng",
		VIRTIO_TYPE_GPU => "virtio-gpu",
		VIRTIO_TYPE_INPUT => "virtio-input",
		VIRTIO_TYPE_SOUND => "virtio-snd",
		VIRTIO_TYPE_VSOCK => "virtio-vsock",
		VIRTIO_TYPE_I2C => "virtio-i2c",
		VIRTIO_TYPE_GPIO => "virtio-gpio",
		DEVICE_TYPE_XHCI => "xhci",
		DEVICE_TYPE_NVME => "nvme",
		DEVICE_TYPE_AHCI => "ahci",
		DEVICE_TYPE_SDHCI => "sdhci",
		DEVICE_TYPE_HDA => "hda",
		DEVICE_TYPE_IPMI_KCS => "ipmi-kcs",
		DEVICE_TYPE_IPMI_BT => "ipmi-bt",
		DEVICE_TYPE_SHARED_MEMORY => "shared-memory",
		DEVICE_TYPE_UNKNOWN => "unresolved-pci-function",
		DEVICE_TYPE_PLATFORM => "platform",
		// A code this build does not classify. The NUMBER is kept, because a reader chasing an
		// unrecognised device needs it and "unknown" alone sends them back to the source.
		_ => "unknown",
	}
}

// ------------------------------------------------------------------ platform rows
//
// A ROW OF THE DEVICE TABLE IS A PCI FUNCTION OR A PLATFORM DEVICE, and the two are told apart by
// `PlatformPart::kind` rather than by squeezing one into the other: a platform device has several MMIO
// ranges, wired lines with a trigger and a polarity, connections on another controller, and an identity
// that is a NAME - `kernel:com1`, `table:TPM2#0`, `dt:/soc/serial@10000000`, `acpi:\_SB_.COM0` - where a
// PCI function has a bus address. A PCI row's platform part is all zeros.

pub const ROW_KIND_PCI: u8 = 0;
pub const ROW_KIND_PLATFORM: u8 = 1;

// Where a platform row's first description came from, which is also its identity's form.
pub const PLATFORM_SOURCE_KERNEL: u8 = 1;
pub const PLATFORM_SOURCE_TABLE: u8 = 2;
pub const PLATFORM_SOURCE_TREE: u8 = 3;
pub const PLATFORM_SOURCE_ACPI: u8 = 4;

// WHO MAY TAKE A PLATFORM ROW. Only a CLAIMABLE one can be claimed. A KERNEL-HELD row is a device the
// kernel drives itself - an interrupt controller, a timer, its console until a handoff releases it -
// published so every device is accounted for. A FIRMWARE-HELD row is one the firmware keeps for its own
// methods. A RESERVATION is a range a description holds back (`PNP0C01`/`PNP0C02`), never a device.
pub const PLATFORM_STATE_CLAIMABLE: u8 = 1;
pub const PLATFORM_STATE_KERNEL_HELD: u8 = 2;
pub const PLATFORM_STATE_FIRMWARE_HELD: u8 = 3;
pub const PLATFORM_STATE_RESERVATION: u8 = 4;

// WHAT A DRIVER MATCHES A PLATFORM ROW BY. `HID` and `CID` are ACPI's hardware and compatible ids (a
// `PRP0001` node's `_DSD` `compatible` strings are `COMPATIBLE`, so a driver matches the same way on
// ACPI and the tree); `TABLE` is a static table's signature; `CLASS` is a method-only device's class.
// `IDENTITY` is a further identity a merged description brought (`acpi:\_SB_.TPM` joining
// `table:TPM2#0`), and `SMBIOS` is the SMBIOS record an IPMI row agrees with (`smbios:38#n`).
pub const MATCH_ID_HID: u8 = 1;
pub const MATCH_ID_CID: u8 = 2;
pub const MATCH_ID_COMPATIBLE: u8 = 3;
pub const MATCH_ID_TABLE: u8 = 4;
pub const MATCH_ID_CLASS: u8 = 5;
pub const MATCH_ID_IDENTITY: u8 = 6;
pub const MATCH_ID_SMBIOS: u8 = 7;

// The bounds of a platform row. The identity is a bounded name; the rest are the counts a firmware
// description of one device needs, and a description past them is refused whole rather than cut.
pub const PLATFORM_NAME_LEN: usize = 64;
pub const MATCH_ID_TEXT_LEN: usize = 46;
pub const MAX_MATCH_IDS: usize = 8;
pub const MAX_PLATFORM_MMIO: usize = 6;
pub const MAX_PLATFORM_LINES: usize = 4;
pub const MAX_PLATFORM_CONNECTIONS: usize = 4;

// One match id: its kind and up to `MATCH_ID_TEXT_LEN` bytes of text.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MatchId {
	pub kind: u8,
	pub len: u8,
	pub text: [u8; MATCH_ID_TEXT_LEN],
}

impl Default for MatchId {
	fn default() -> Self {
		MatchId { kind: 0, len: 0, text: [0; MATCH_ID_TEXT_LEN] }
	}
}

impl MatchId {
	// A match id from `text`, or None past the bound.
	pub fn new(kind: u8, text: &[u8]) -> Option<Self> {
		if text.is_empty() || text.len() > MATCH_ID_TEXT_LEN {
			return None;
		}
		let mut id = MatchId { kind, len: text.len() as u8, text: [0; MATCH_ID_TEXT_LEN] };
		id.text[..text.len()].copy_from_slice(text);
		Some(id)
	}

	pub fn text(&self) -> &[u8] {
		&self.text[..(self.len as usize).min(MATCH_ID_TEXT_LEN)]
	}
}

// One MMIO range of a platform row, in physical addresses.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MmioResource {
	pub base: u64,
	pub len: u64,
}

// How a wired line asserts, and which controller the kernel reaches it through.
pub const LINE_TRIGGER_EDGE: u8 = 1;
pub const LINE_TRIGGER_LEVEL: u8 = 2;
pub const LINE_POLARITY_HIGH: u8 = 1;
pub const LINE_POLARITY_LOW: u8 = 2;
// An I/O APIC's Global System Interrupt (x86_64), a GIC shared peripheral interrupt by INTID (aarch64),
// an APLIC wired source by number (riscv64).
pub const LINE_CONTROLLER_IOAPIC: u8 = 1;
pub const LINE_CONTROLLER_GIC: u8 = 2;
pub const LINE_CONTROLLER_APLIC: u8 = 3;

// One wired interrupt line of a platform row.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct WiredLine {
	pub number: u32,
	pub trigger: u8,
	pub polarity: u8,
	pub controller: u8,
	pub _pad: u8,
}

// A CONNECTION: a resource on ANOTHER device - a GPIO line of a GPIO controller, an address on an I2C
// or SPI bus - which the device's driver reaches through that controller's driver, never directly.
pub const CONNECTION_GPIO_LINE: u8 = 1;
pub const CONNECTION_I2C: u8 = 2;
pub const CONNECTION_SPI: u8 = 3;
// An I2C connection's `trigger` bit: the address is a ten-bit one.
pub const CONNECTION_I2C_TEN_BIT: u8 = 1;

// One connection: its kind, the controller it is on - that controller's ROW INDEX, or `u32::MAX` while the
// description naming it has not been joined to a row - the line or the bus address, the trigger and
// polarity a GPIO interrupt line uses (an ACPI line's `LINE_TRIGGER_*` and `LINE_POLARITY_*`, zero for a
// line read for its level alone; a tree line's interrupt-type cell in `trigger`), for an I2C address
// `CONNECTION_I2C_TEN_BIT` in `trigger`, and `extra`: for a device-tree GPIO line the controller's phandle,
// which is what the join finds its row by, and for an ACPI I2C address the bus speed.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Connection {
	pub kind: u8,
	pub trigger: u8,
	pub polarity: u8,
	pub _pad: u8,
	pub controller: u32,
	pub value: u32,
	pub extra: u32,
}

// `PlatformPart::flags`.
// The row names a DMA stream id (`dma_stream`), which a claim attaches like a PCI endpoint where this
// kernel's IOMMU driver serves the stream's controller and refuses by name where it does not.
pub const PLATFORM_FLAG_DMA_STREAM: u8 = 1 << 0;
// The row's property block lists references the kernel did not resolve - a clock that is not a
// `fixed-clock`, a reset, a regulator, a pinctrl state - so a driver refuses rather than guesses.
pub const PLATFORM_FLAG_UNRESOLVED: u8 = 1 << 1;
// The row is the KERNEL CONSOLE'S UART, which the kernel drives from its first line: claiming it takes the
// console from the kernel - its ports leave the reserved set for the claim's range and its output leaves
// through the claim's `RESOURCE_KIND_CONSOLE_TAP` - and the release gives it back.
pub const PLATFORM_FLAG_CONSOLE: u8 = 1 << 2;

// A PLATFORM ROW'S OWN DESCRIPTION, appended to `DeviceInfo`. All zeros for a PCI row.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlatformPart {
	pub kind: u8,
	pub source: u8,
	pub state: u8,
	pub flags: u8,
	pub match_count: u8,
	pub mmio_count: u8,
	pub line_count: u8,
	pub connection_count: u8,
	pub dma_stream: u32,
	// The length of the row's property block, which `SYS_DEVICE_PROPERTIES` copies to its claimant.
	pub properties_len: u32,
	pub identity: [u8; PLATFORM_NAME_LEN],
	pub match_ids: [MatchId; MAX_MATCH_IDS],
	pub mmio: [MmioResource; MAX_PLATFORM_MMIO],
	pub lines: [WiredLine; MAX_PLATFORM_LINES],
	pub connections: [Connection; MAX_PLATFORM_CONNECTIONS],
}

impl Default for PlatformPart {
	fn default() -> Self {
		PlatformPart { kind: ROW_KIND_PCI, source: 0, state: 0, flags: 0, match_count: 0, mmio_count: 0, line_count: 0, connection_count: 0, dma_stream: 0, properties_len: 0, identity: [0; PLATFORM_NAME_LEN], match_ids: [MatchId::default(); MAX_MATCH_IDS], mmio: [MmioResource::default(); MAX_PLATFORM_MMIO], lines: [WiredLine::default(); MAX_PLATFORM_LINES], connections: [Connection::default(); MAX_PLATFORM_CONNECTIONS] }
	}
}

impl PlatformPart {
	// The identity as bytes, without the NUL padding.
	pub fn identity(&self) -> &[u8] {
		let end = self.identity.iter().position(|byte| *byte == 0).unwrap_or(PLATFORM_NAME_LEN);
		&self.identity[..end]
	}

	pub fn match_ids(&self) -> &[MatchId] {
		&self.match_ids[..(self.match_count as usize).min(MAX_MATCH_IDS)]
	}

	pub fn mmio(&self) -> &[MmioResource] {
		&self.mmio[..(self.mmio_count as usize).min(MAX_PLATFORM_MMIO)]
	}

	pub fn lines(&self) -> &[WiredLine] {
		&self.lines[..(self.line_count as usize).min(MAX_PLATFORM_LINES)]
	}

	pub fn connections(&self) -> &[Connection] {
		&self.connections[..(self.connection_count as usize).min(MAX_PLATFORM_CONNECTIONS)]
	}
}

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
	// THE LENGTH OF THE DEVICE-SPECIFIC CONFIGURATION, and how its ABSENCE is said (KERN-ARCH-014).
	//
	// That structure is optional in the virtio specification, and it used to be reported as offset
	// zero when a device had none - which is also a legal offset for one that does. A driver could
	// not tell the two apart, so "no device config" and "device config at the start of the window"
	// read identically. A length of zero is the absence, and cannot be mistaken for a structure.
	pub device_len: u32,
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
	// vendor-defined number, which is what the driver-binding rules exist to stop: standard class/subclass/
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
	// EXPLICIT, for the same reason `_pad0` is - and this is the SAME DEFECT, recreated at the other
	// end of the same struct by the change that added the three fields above.
	//
	// `prog_if` ends at offset 42 and the alignment is 8, so 42..47 belonged to nothing; `write_user`
	// copies `size_of::<T>()` bytes and `sys_device_info` builds the value on the kernel stack, so
	// six bytes of whatever the stack held there went to userspace.
	//
	// What makes this worth more than six bytes is that the layout assertion DID fire on that change
	// - on the SIZE, which went 40 to 48 - and the belief it corrected was about where the fields
	// would fit, not about what the tail became. "Size, alignment and every field offset are what
	// they were" and "every byte of this struct belongs to a named field" are different properties,
	// and the first was satisfied by a change that broke the second. `no_repr_c_struct_has_implicit_padding`
	// is the second one, asserted over the whole crate rather than over this struct.
	//
	// Not `repr(packed)`: that would make `bar_len` unaligned and trade a disclosure for a soundness
	// problem.
	//
	// WHICH TRANSPORT THIS FUNCTION SPEAKS, so a rule can say "a virtio-pci function whose virtio
	// type is 1" instead of "device type 1".
	//
	// `device_type` conflates two number spaces: for a virtio function it is the virtio
	// specification's own device type, and for the xHCI controller it is `DEVICE_TYPE_XHCI` - a
	// LiberSystem constant invented for the table, standing in for the PCI class triple the scan
	// had already resolved. A rule matching on it alone cannot tell "virtio type 1" from "whatever
	// else this system decides to number 1 next", and it made every driver selected by a
	// vendor-defined number.
	pub transport: u8,
	// ONE BYTE NOW: `transport` took one of the two, and `vendor` must land on an even offset.
	pub _pad1: [u8; 1],
	// THE PCI IDENTITY OF THE PART, resolved by the same scan and retained for `lspci` alone.
	//
	// Not an identity a rule may match on ITS OWN - a vendor number says who made a device, not what
	// it is - but the only way to write a quirk for a particular part, which is what they are for.
	pub vendor: u16,
	pub product: u16,
	// WHETHER THE FUNCTION IS STILL ON THE BUS, which a boot-only inventory never had to say.
	//
	// A ROW OUTLIVES ITS DEVICE, and that is deliberate: an index is what a claim, a binding and
	// every message in flight are addressed by, so removing the row would renumber every device
	// after it. What changes when a device is unplugged is this byte - and without it, a listing
	// shows a disk somebody pulled out as though it were still there, which is the one thing an
	// operator asking "what is in this machine" must not be told.
	//
	// IT COSTS NO LAYOUT CHANGE: it takes the first of the four tail padding bytes, so every field
	// offset, the size and the alignment are what they were.
	pub on_bus: u8,
	// `product` ends at 52 and the alignment is 8. Named rather than left implicit, for the reason
	// `_pad0` is: this struct is copied to userspace with `size_of::<T>()` from a value built on the
	// kernel stack, and Rust does not promise that padding in an otherwise initialised value is
	// initialised.
	//
	// HOW MANY OF `ports` THE ROW CARRIES, which took the first of the three tail bytes.
	pub port_count: u8,
	pub _pad2: [u8; 2],
	// THE ROW'S PORT RESOURCES, in the order `SYS_DEVICE_RESOURCE_ACQUIRE` indexes them. Empty on every
	// row of aarch64 and riscv64, which have no port space.
	pub ports: [PortResource; MAX_PORT_RESOURCES],
	// A PLATFORM ROW'S DESCRIPTION, appended: its kind, identity, match ids and resources. All zeros for
	// a PCI row, whose identity is the fields above.
	pub platform: PlatformPart,
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
	/// THE MEMORY TYPE THE KERNEL MAPPED THIS SURFACE WITH, which nothing said before.
	///
	/// A CACHE POLICY THAT IS NOT STATED CANNOT BE CHECKED, and this one is a real choice with real
	/// consequences: a linear aperture on a discrete card wants WRITE-COMBINING, where a read costs
	/// an uncached round trip and a write is buffered, and ordinary RAM wants WRITE-BACK. Every
	/// machine this system runs on is the second - in QEMU the surface IS ordinary RAM - and the
	/// mapping said so by carrying no memory type at all, which is indistinguishable from not having
	/// decided.
	///
	/// A CONSUMER USES IT TO DECIDE WHETHER TO READ. Compositing into a write-combining surface by
	/// reading it back is the one access pattern that is orders of magnitude slower than it looks,
	/// and a consumer that cannot ask has to assume the worse case or be wrong.
	pub memory_type: u32,
}

/// The memory types `Framebuffer::memory_type` names. WRITE-BACK is what every target this system
/// builds for maps a boot surface with today; the other two exist so that the day one does not, the
/// answer is a value here rather than a second mapping path nobody can see from userspace.
pub const FRAMEBUFFER_WRITE_BACK: u32 = 0;
pub const FRAMEBUFFER_WRITE_COMBINING: u32 = 1;
pub const FRAMEBUFFER_UNCACHED: u32 = 2;

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
pub const OBJECT_TYPE_CLAIM: u64 = 14;
pub const OBJECT_TYPE_PORT_RANGE: u64 = 15;
pub const OBJECT_TYPE_CONSOLE_TAP: u64 = 16;
pub const OBJECT_TYPE_REGISTERS: u64 = 17;
// A latency request: held, it bounds every core's idle states - see `SYS_LATENCY_REQUEST`.
pub const OBJECT_TYPE_LATENCY_REQUEST: u64 = 18;

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

// One module's lifecycle arrays, as `process_lifecycle` writes them into the caller's buffer: the
// already-biased addresses and entry counts of its `.init_array` and `.fini_array`, and whether this
// entry is the process's OWN image rather than one of its providers.
//
// WHY THE MAIN IMAGE IS MARKED RATHER THAN ORDERED. It is loaded FIRST and must be constructed LAST:
// it is the consumer of everything else, so every provider's constructor has to have run before its
// own does. Marking it says that in the record instead of asking every reader to remember that the
// first entry is special - and a reader that ignored the flag would run the consumer first, which is
// the one order that is wrong.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ModuleLifecycle {
	pub init_array: u64,
	pub init_count: u64,
	pub fini_array: u64,
	pub fini_count: u64,
	pub is_main_image: u64,
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
// Loader scratch whose life ended at the hand-off (usable to the kernel).
pub const MEMMAP_BOOTLOADER_RECLAIMABLE: u32 = 8;
// MEMORY-MAPPED I/O the firmware reports - its device windows - kept apart from reserved memory so a firmware
// mapping of it is uncached where reserved memory is write-back.
pub const MEMMAP_MMIO: u32 = 9;

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

// HOW EACH CORE RESTS: `SYS_CPU_IDLE_INFO(index, buf, len)` writes core `index`'s record into the caller's
// buffer and answers the core count - ERR_INVALID past the end. A free read, like `SYS_IRQ_INFO`: what a
// core did while it had nothing to run is a number, or "the processor rests when there is nothing to do"
// is only a claim.
//
// `idle_ns` is the time the core spent halted, `halts` how many times it halted, and each wake is counted
// under what ended it: its timer, an IPI, the housekeeping bound (the idle boot processor's timer, when
// polled housekeeping was what set it), or a device's interrupt - in total, and per interrupt identity in
// `sources`, first come first kept. The identity is the one the architecture dispatches under: the IDT
// vector on x86_64 (a legacy IRQ n is `0x20 + n`), the INTID on aarch64, the interrupt-file identity on
// riscv64. Wakes per second are two of these readings and the time between them.
pub const SYS_CPU_IDLE_INFO: u64 = 95;
// THE PROPERTY BLOCK OF A CLAIMED PLATFORM ROW - `SYS_DEVICE_PROPERTIES(handle, buf, len)` - for the claim
// handle, or the `DeviceMemory` or declared `Registers` minted from a claim that is still the device's current
// binding, carrying `RIGHT_READ`: the device-tree node's properties and its child nodes' (bounded), with every
// `fixed-clock` reference resolved to its frequency and every other reference listed unresolved, in the record
// format `DEVICE_PROPERTY_*` describes; for a static table's row, the table itself as one VALUE record named by
// its signature - a WDAT's instructions, which its driver runs. Answers the block's length, copying what fits;
// ERR_UNSUPPORTED for a row that has none. The driver reads it through the register window or the declared
// registers it was given - the claim stays its manager's.
pub const SYS_DEVICE_PROPERTIES: u64 = 96;
// THE CONSOLE TAP'S READ - `SYS_CONSOLE_TAP_READ(tap, buf, len, dropped)` - for a tap handle carrying
// `RIGHT_READ`: moves at most `len` bytes of the kernel console's output out of its transmit ring into `buf`,
// in order, answers how many, and writes to `dropped` (a u64, or 0 for none) how many bytes the ring's bound
// dropped since the last read, which the driver reports on the wire in their place. The release of the claim
// the tap was minted from revokes it - ERR_BAD_HANDLE from then on - and a tap whose claim no longer holds the
// UART reads nothing, ERR_ACCESS_DENIED.
pub const SYS_CONSOLE_TAP_READ: u64 = 97;
// DEVELOPMENT-BUILD KERNEL REQUESTS - `SYS_DEV_CONSOLE(privilege, request)`, for a holder of the
// `ConsoleInputSource` privilege, which the development agent holds. A kernel built for any other image has
// no such call and answers ERR_BAD_SYSCALL. `DEV_CONSOLE_HOLD_AND_FLOOD` holds every console tap's reads and
// writes kernel lines until the transmit ring has dropped bytes at its bound, answering how many lines it
// wrote; `DEV_CONSOLE_PANIC` panics the kernel. Together they are the handoff gate's proof that the
// terminal-path writer puts a panic on the wire while a driver holds the console and does not drain it.
// `DEV_CONSOLE_KILL_HOLDER` kills the process that has the console UART's ports mapped - its driver - the
// way DeviceManager's SIG_KILL does, answering 0, or ERR_INVALID when no process holds them.
// `DEV_CONSOLE_KILL_DRIVER`, with a PCI vendor in bits 16..32 of the request and a device in bits 32..48, kills the
// process that has the BAR of that claimed function mapped - its driver - the same way, or answers ERR_INVALID.
pub const SYS_DEV_CONSOLE: u64 = 98;
pub const DEV_CONSOLE_HOLD_AND_FLOOD: u64 = 1;
pub const DEV_CONSOLE_PANIC: u64 = 2;
pub const DEV_CONSOLE_KILL_HOLDER: u64 = 3;
pub const DEV_CONSOLE_KILL_DRIVER: u64 = 4;
// ONE DECLARED REGISTER - `SYS_DEVICE_REGISTER_READ(registers, index)` answers its value (as a non-negative i64),
// `SYS_DEVICE_REGISTER_WRITE(registers, index, value)` writes it and answers 0 - for a `Registers` handle carrying
// `RIGHT_READ` or `RIGHT_WRITE`. The access is at exactly the register's declared width, and a write changes only the
// bits of its declared mask. ERR_INVALID for an index the row does not declare; ERR_ACCESS_DENIED once the claim the
// capability was minted from is not the device's current binding.
pub const SYS_DEVICE_REGISTER_READ: u64 = 99;
pub const SYS_DEVICE_REGISTER_WRITE: u64 = 100;

// THE ACPI SERVICE'S CALLS, every one behind a `FirmwareInterpreter` privilege handle in `a0`. The service
// DESCRIBES; the kernel decides what is minted, under its policy.
//
// `SYS_FIRMWARE_TABLE(privilege, selector, buf, len)` - `selector` a table signature's four bytes in its low 32
// bits (little-endian, "DSDT" is 0x54445344) and the instance in its high 32 - copies the table into `buf` when
// `len` holds it and answers its whole length either way (so a `len` of 0 asks the size); the DSDT and the FACS
// come through the FADT, and a table whose checksum fails is not answered. ERR_INVALID for no such table,
// ERR_UNSUPPORTED on a machine with no ACPI.
pub const SYS_FIRMWARE_TABLE: u64 = 101;
// `SYS_FIRMWARE_MAP(privilege, request)` maps a SystemMemory operation region: `request` a `FirmwareMapRequest`.
// Answers a `DeviceMemory` handle (`RIGHT_READ | RIGHT_WRITE | RIGHT_MAP`) - mapped write-back for ACPI NVS,
// ACPI-reclaimable and firmware-reserved memory, uncached for MMIO - or ERR_ACCESS_DENIED where the policy refuses
// the range, saying why on the console. A BAR admitted as the companion's makes its function firmware-held.
pub const SYS_FIRMWARE_MAP: u64 = 102;
// `SYS_FIRMWARE_MEDIATED(privilege, operation, a, b)`: the accesses the kernel performs itself - the SMI command
// port (`a` the value; the FADT's ACPI-disable value refused), a CMOS NVRAM byte from 0x0E up (the clock's
// registers and the century byte stay the kernel's), the PM timer, and the global lock's release bit.
pub const SYS_FIRMWARE_MEDIATED: u64 = 103;
pub const FIRMWARE_SMI_COMMAND: u64 = 1;
pub const FIRMWARE_CMOS_READ: u64 = 2;
pub const FIRMWARE_CMOS_WRITE: u64 = 3;
pub const FIRMWARE_PM_TIMER: u64 = 4;
pub const FIRMWARE_GLOBAL_LOCK_RELEASE: u64 = 5;
// `SYS_FIRMWARE_PCI(privilege, address, access, value)`: one PCI configuration access the kernel performs.
// `address` is `segment << 32 | bus << 16 | device << 8 | function`; `access` is `offset | width << 16 |
// write << 24` (width 1, 2 or 4, naturally aligned). A read of any function answers the value; a write is refused
// below 0x40, inside an MSI or MSI-X capability, to a function a driver holds and to a register the kernel's
// chipset rows own, and answers 0.
pub const SYS_FIRMWARE_PCI: u64 = 104;
pub const FIRMWARE_PCI_WRITE: u64 = 1 << 24;
// `SYS_FIRMWARE_REPORT(privilege, buf, len)`: one report in `platform::report`'s encoding - a namespace device or
// reservation, a withdrawal, a companion, a host bridge's `_OSC` answer, or "namespace loaded". Answers the row the
// report is about (device, companion) or 0; ERR_INVALID for a malformed report, ERR_ACCESS_DENIED for one the
// kernel refuses (said on the console), ERR_UNSUPPORTED when no instance is attached.
pub const SYS_FIRMWARE_REPORT: u64 = 105;
// `SYS_FIRMWARE_EVENTS(privilege, channel)`: attach the caller as the running instance, its events delivered on
// `channel` (the kernel holds a copy and sends on it). Answers the instance's number, from 1. Every general-purpose
// event the previous instance had enabled was disabled when it ended. The messages: `[FIRMWARE_EVENT_GPE][u16]` an
// event latched and masked, `[FIRMWARE_EVENT_STORM][u16]` one left disabled for the rest of the boot, and
// `[FIRMWARE_EVENT_GLOBAL_LOCK]` the firmware released the global lock the service waits on.
pub const SYS_FIRMWARE_EVENTS: u64 = 106;
pub const FIRMWARE_EVENT_GPE: u8 = 1;
pub const FIRMWARE_EVENT_STORM: u8 = 2;
pub const FIRMWARE_EVENT_GLOBAL_LOCK: u8 = 3;
// `SYS_FIRMWARE_GPE(privilege, operation, gpe)`: the service's requests on one general-purpose event.
// `GPE_COUNT` answers how many events the FADT's blocks number (`gpe` ignored).
pub const SYS_FIRMWARE_GPE: u64 = 107;
pub const GPE_ENABLE: u64 = 1;
pub const GPE_DISABLE: u64 = 2;
pub const GPE_WAKE_SET: u64 = 3;
pub const GPE_WAKE_CLEAR: u64 = 4;
pub const GPE_ACKNOWLEDGE: u64 = 5;
pub const GPE_REARM: u64 = 6;
pub const GPE_COUNT: u64 = 7;
// `SYS_DEVICE_NODE(index, buf, len)`: what the firmware's namespace attached to row `index` - a PCI function's
// companion node, a namespace child's parent function, and a GPIO or serial-bus controller's `_AEI` lines and
// field lines and addresses - as a `FirmwareNode`. ERR_INVALID for no such row; a row with nothing attached
// answers a node with no flags.
pub const SYS_DEVICE_NODE: u64 = 108;

// ------------------------------------------------------------------ sleep

// `SYS_DOMAIN_FREEZE(domain, on)`: park every thread of the Domain's subtree at its next return to user mode, on a
// FROZEN flag of its own - `SIG_CONT` does not clear it and the thaw does not clear `SIG_STOP`'s - or release them.
// Requires MANAGE on the Domain, the right that already lets its holder kill the subtree. A freeze answers only when
// no thread of the subtree is executing user code, and `ERR_TIMED_OUT` when that is not so within the kernel's bound
// (the flags stay set, for the caller to thaw); a process created in a frozen subtree starts frozen.
pub const SYS_DOMAIN_FREEZE: u64 = 109;
// `SYS_SYSTEM_SLEEP(root domain, state, timed wake, report)`: the kernel's sleep entry - requires MANAGE on the
// root Domain, as `SYS_SYSTEM_POWER` does. `timed wake` is a deadline on the boot-time clock in nanoseconds, zero
// for none; the call answers when the machine is awake again, the `SleepReport` written to `report`. Refused:
// `ERR_UNSUPPORTED` for a state this machine does not offer or nothing registered, `ERR_INTERRUPTED` for a wake event
// already pending, `ERR_INVALID` for a timed wake already past or beyond what the state's alarm reaches.
pub const SYS_SYSTEM_SLEEP: u64 = 110;
// `SYS_FIRMWARE_SLEEP_TYPE(privilege, state, typ a, typ b)`: THE ACPI SERVICE REGISTERS a sleep state's `\_Sx`
// pair, which the kernel writes into PM1a and PM1b control with SLP_EN - `SLEEP_STATE_RAM`, `SLEEP_STATE_DISK` or
// `SLEEP_STATE_SOFT_OFF`. Requires the FirmwareInterpreter privilege. A registered value outlives the service: it is
// firmware data. `ERR_UNSUPPORTED` when this build refuses the registration (the development switch).
pub const SYS_FIRMWARE_SLEEP_TYPE: u64 = 111;
// `SYS_CLOCK_BOOT_NS()`: nanoseconds since boot, SLEEP INCLUDED - read-only, with no timers on it. The monotonic
// clock excludes every sleep, so a deadline armed before one has the rest of its time after it; this is the clock
// a sleep's length and a wall clock are counted on.
pub const SYS_CLOCK_BOOT_NS: u64 = 112;
// `SYS_INTERRUPT_WAKE(interrupt, on)`: A DRIVER MARKS ITS INTERRUPT AS A WAKE SOURCE in a `SUSPEND` step that armed
// wake, and unmarks it at the resume - WRITE on the Interrupt, which must still own its binding. A suspend to idle
// leaves a marked interrupt unmasked, and the first one to fire while the machine sleeps wakes it as a device's wake.
// The mark goes with the binding. `ERR_RESOURCE_EXHAUSTED` when the wake set is full.
pub const SYS_INTERRUPT_WAKE: u64 = 113;
// `SYS_SLEEP_STATES()`: WHAT THIS MACHINE'S SLEEP ENTRY WOULD TAKE, and the fixed buttons it has - read-only, for any
// caller: bit `1 << SLEEP_STATE_x` for each state the entry takes (suspend to RAM once `\_S3` is registered and the
// firmware has its waking vector), and `SLEEP_FIXED_POWER_BUTTON` / `SLEEP_FIXED_SLEEP_BUTTON` for the buttons PM1
// carries rather than a control-method device.
pub const SYS_SLEEP_STATES: u64 = 114;
// `SYS_CLOCK_BASE(clock source, unix seconds)`: A DRIVER HANDS THE KERNEL THE WALL CLOCK on a machine where the kernel
// reads no RTC of its own - the ACPI Time and Alarm Device's `_GRT` - under the ClockSource privilege DeviceManager hands
// that driver at bind. `SYS_CLOCK_RTC` then answers it counted forward on the boot-time clock, and 0 before it; after
// a suspend to RAM, the base handed again at the resume is what the sleep's length is taken from.
pub const SYS_CLOCK_BASE: u64 = 115;
// ------------------------------------------------------------------ hibernation
//
// A SNAPSHOT IN THE KERNEL, WRITTEN BY USERSPACE. `SYS_SYSTEM_SLEEP(root, SLEEP_STATE_DISK, 0, report)` copies every page
// in use into free memory and RETURNS TWICE: once in the running machine, `WAKE_SNAPSHOT` - the copy held for the image
// component to read and write out - and once in the machine restored from that image, `WAKE_RESTORED`. The copy is
// refused, named, when free memory cannot hold it. Every call below requires the Hibernation privilege: its holder reads
// every byte memory held and can replace all of memory, which is why the image component alone holds it.
//
// `SYS_SNAPSHOT_INFO(privilege, info)`: the snapshot held - `SnapshotInfo`. `ERR_INVALID` when there is none.
pub const SYS_SNAPSHOT_INFO: u64 = 116;
// `SYS_SNAPSHOT_READ(privilege, first, count, buffer)`: pages [first, first + count) of the snapshot, `count` at most
// `SNAPSHOT_BATCH`: their physical addresses (`count` u64s), then their bytes (`count` pages), into `buffer`.
pub const SYS_SNAPSHOT_READ: u64 = 117;
// `SYS_SNAPSHOT_RELEASE(privilege)`: the snapshot's copies given back - after the image is written, or when it is not.
pub const SYS_SNAPSHOT_RELEASE: u64 = 118;
// `SYS_RESTORE_BEGIN(privilege, frames, count, context)`: A RESTORE of `count` pages to the physical addresses the
// `count` u64s at `frames` name, with the resume context the image carried (`SNAPSHOT_CONTEXT` bytes at `context`).
// Refused: `ERR_INVALID` for an address that is not RAM of this machine, one named twice, or a context this kernel did
// not write; `ERR_RESOURCE_EXHAUSTED` when free memory cannot hold the pages outside the frames they go to.
pub const SYS_RESTORE_BEGIN: u64 = 119;
// `SYS_RESTORE_WRITE(privilege, first, count, pages)`: the bytes of pages [first, first + count), `count` at most
// `SNAPSHOT_BATCH`, each held in a frame no page of the image goes to.
pub const SYS_RESTORE_WRITE: u64 = 120;
// `SYS_RESTORE_COMMIT(privilege, abandon)`: THE WHOLE-MEMORY REPLACEMENT - every other core stopped, every page copied
// to its frame from a trampoline in frames the image does not use, and a jump into the image's kernel along its resume
// path. Never returns when it happens; `ERR_INVALID` for a restore with pages not yet written. With `abandon` set, the
// restore is dropped and its memory given back instead.
pub const SYS_RESTORE_COMMIT: u64 = 121;
// `SYS_SYSTEM_FINGERPRINT(out)`: THE DIGESTS AN IMAGE IS CHECKED AGAINST, for any caller - `SystemFingerprint`: the
// system image this kernel runs (its own code and the packages it booted with) and the hardware (the memory map's
// classes, the cores, every PCI function).
pub const SYS_SYSTEM_FINGERPRINT: u64 = 122;

// PROCESSOR POWER - the kernel holds what runs at the scheduler's rate or needs ring 0 (the idle governor and the entry
// into an idle state, the performance governor and the register writes that carry out its choice, idle injection and
// the latency bound); ProcessorPowerService, the one holder of the `ProcessorPower` privilege every call here needs,
// installs what the firmware describes. Every register a table names is checked at install and the table refused whole
// for one refusal - `ERR_ACCESS_DENIED` for a register the kernel will not take (RAM, the reserved set, a grant, a
// claim, a device's BAR), `ERR_INVALID` for a table `procpower` refuses (a model-specific register, the platform
// channel, too many states, out of order) - and a refused table leaves the one it would have replaced standing.
//
// `SYS_PROCESSOR_IDLE_TABLE(privilege, cpu, states, count)`: core `cpu`'s idle states, `count` `ProcessorIdleState`s at
// `states`, shallowest first; `count` zero uninstalls, leaving the halt.
pub const SYS_PROCESSOR_IDLE_TABLE: u64 = 123;
// `SYS_PROCESSOR_PERF_TABLE(privilege, cpu, table)`: core `cpu`'s performance table, a `ProcessorPerfTable` at `table`;
// zero uninstalls. The window becomes the whole table.
pub const SYS_PROCESSOR_PERF_TABLE: u64 = 124;
// `SYS_PROCESSOR_PERF_WINDOW(privilege, cpu, cap, floor)`: the fastest level and the slowest the governor may choose,
// obeyed at once.
pub const SYS_PROCESSOR_PERF_WINDOW: u64 = 125;
// `SYS_PROCESSOR_IDLE_INJECT(privilege, cpu, permille)`: the share of core `cpu`'s time the scheduler injects as idle,
// at most `MAX_INJECT_PERMILLE`.
pub const SYS_PROCESSOR_IDLE_INJECT: u64 = 126;
// `SYS_LATENCY_REQUEST(privilege, bound_us)`: A LATENCY REQUEST, under the `IdleLatency` privilege - a handle to a
// kernel object that holds every core's idle governor to states whose exit latency is at most `bound_us` for as long as
// a handle to it lives. `ERR_RESOURCE_EXHAUSTED` past four per process or sixty-four in the system.
pub const SYS_LATENCY_REQUEST: u64 = 127;
// `SYS_PROCESSOR_PERF_PREFERENCE(privilege, cpu, value)`: CPPC's energy-performance preference, 0 (performance) to 255
// (energy), written to core `cpu`'s preference register. `ERR_UNSUPPORTED` where its table names none.
pub const SYS_PROCESSOR_PERF_PREFERENCE: u64 = 128;
pub const MAX_INJECT_PERMILLE: u64 = 500;

// A REGISTER a processor table names, as ACPI's Generic Address Structure gives it: the space (0 memory, 1 I/O, 0x0A the
// platform channel, 0x7F functional fixed hardware), the width in bits and the address.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorRegister {
	pub space: u8,
	pub bits: u8,
	pub _pad: [u8; 6],
	pub address: u64,
}

// How an idle state is entered.
pub const IDLE_ENTRY_HALT: u32 = 0;
pub const IDLE_ENTRY_MWAIT: u32 = 1;
pub const IDLE_ENTRY_REGISTER: u32 = 2;
pub const IDLE_ENTRY_PSCI: u32 = 3;
pub const IDLE_ENTRY_SBI: u32 = 4;
// What an idle state costs beyond its latency.
pub const IDLE_LOSES_CONTEXT: u32 = 1 << 0;
pub const IDLE_STOPS_TIMER: u32 = 1 << 1;
pub const IDLE_BUS_MASTER_ARBITRATION: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorIdleState {
	pub entry: u32,
	pub flags: u32,
	// MWAIT's hint, PSCI's power state, the SBI's suspend type.
	pub parameter: u32,
	pub exit_latency_us: u32,
	pub target_residency_us: u32,
	pub _pad: u32,
	pub register: ProcessorRegister,
}

pub const PROCESSOR_MAX_IDLE_STATES: usize = 8;
pub const PROCESSOR_MAX_PERF_STATES: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorPerfState {
	pub core_mhz: u32,
	pub power_mw: u32,
	pub latency_us: u32,
	pub control: u32,
	pub status: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessorThrottleState {
	pub percent: u32,
	pub latency_us: u32,
	pub control: u32,
}

// How a performance table is controlled.
pub const PERF_TABLE_STATES: u32 = 1;
pub const PERF_TABLE_CPPC: u32 = 2;

// A CORE'S PERFORMANCE TABLE: `_PSS` states with `_PCT`'s control and status registers, or CPPC's desired register (and
// its minimum and maximum where it has them) with its levels; `_PSD`'s domain where `coordination` is not zero; and the
// throttling states with `_PTC`'s control register where `throttle_count` is not zero.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessorPerfTable {
	pub control_kind: u32,
	pub state_count: u32,
	// `_PCT`'s control register, or CPPC's desired performance.
	pub control: ProcessorRegister,
	// `_PCT`'s status register, or CPPC's minimum performance (space 0, address 0 for none).
	pub status: ProcessorRegister,
	// CPPC's maximum performance (address 0 for none).
	pub maximum: ProcessorRegister,
	// CPPC's energy-performance preference (address 0 for none).
	pub preference: ProcessorRegister,
	pub highest: u32,
	pub nominal: u32,
	pub lowest: u32,
	pub domain: u32,
	// `_PSD`'s coordination type (0xFC, 0xFD, 0xFE), or zero for none.
	pub coordination: u32,
	pub processors: u32,
	pub throttle_count: u32,
	pub _pad: u32,
	pub throttle_control: ProcessorRegister,
	pub states: [ProcessorPerfState; PROCESSOR_MAX_PERF_STATES],
	pub throttle: [ProcessorThrottleState; PROCESSOR_MAX_PERF_STATES],
}

impl Default for ProcessorPerfTable {
	fn default() -> Self {
		ProcessorPerfTable { control_kind: 0, state_count: 0, control: ProcessorRegister::default(), status: ProcessorRegister::default(), maximum: ProcessorRegister::default(), preference: ProcessorRegister::default(), highest: 0, nominal: 0, lowest: 0, domain: 0, coordination: 0, processors: 0, throttle_count: 0, _pad: 0, throttle_control: ProcessorRegister::default(), states: [ProcessorPerfState::default(); PROCESSOR_MAX_PERF_STATES], throttle: [ProcessorThrottleState::default(); PROCESSOR_MAX_PERF_STATES] }
	}
}
// The most pages one read or write moves, and the resume context's size.
pub const SNAPSHOT_BATCH: u64 = 256;
pub const SNAPSHOT_CONTEXT: usize = 64;

// What `SYS_SNAPSHOT_INFO` answers.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotInfo {
	pub pages: u64,
	pub context: [u8; SNAPSHOT_CONTEXT],
	pub system: [u8; 32],
	pub hardware: [u8; 32],
}

impl Default for SnapshotInfo {
	fn default() -> Self {
		SnapshotInfo { pages: 0, context: [0; SNAPSHOT_CONTEXT], system: [0; 32], hardware: [0; 32] }
	}
}

// What `SYS_SYSTEM_FINGERPRINT` answers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemFingerprint {
	pub system: [u8; 32],
	pub hardware: [u8; 32],
}

pub const SLEEP_FIXED_POWER_BUTTON: u64 = 1 << 8;
pub const SLEEP_FIXED_SLEEP_BUTTON: u64 = 1 << 9;

// The sleep states, as `liber:process@1/sleep-state` numbers them, and soft-off's registration.
pub const SLEEP_STATE_IDLE: u64 = 1;
pub const SLEEP_STATE_RAM: u64 = 2;
pub const SLEEP_STATE_DISK: u64 = 3;
// THE MACHINE OFF WITH ITS IMAGE WRITTEN: the registered `\_S4` pair, or soft-off where there is none. Never returns
// when it happens.
pub const SLEEP_STATE_DISK_ENTER: u64 = 4;
pub const SLEEP_STATE_SOFT_OFF: u64 = 5;

// What woke the machine, as `liber:process@1/wake-reason` numbers it.
pub const WAKE_UNKNOWN: u32 = 0;
pub const WAKE_TIMER: u32 = 1;
pub const WAKE_POWER_BUTTON: u32 = 2;
pub const WAKE_SLEEP_BUTTON: u32 = 3;
pub const WAKE_DEVICE: u32 = 4;
pub const WAKE_RTC: u32 = 5;
pub const WAKE_PLATFORM: u32 = 6;
// The two returns of a hibernation's snapshot: taken, in the running machine; and restored from its image.
pub const WAKE_SNAPSHOT: u32 = 7;
pub const WAKE_RESTORED: u32 = 8;

// The most cores a sleep report carries.
pub const SLEEP_REPORT_CORES: usize = 64;

// One core's wakeups while parked in a suspend to idle, by cause.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CorePark {
	pub cpu: u32,
	pub timer: u32,
	pub ipi: u32,
	pub device: u32,
}

// What `SYS_SYSTEM_SLEEP` writes at the resume.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SleepReport {
	pub wake: u32,
	// The device's interrupt identity, for `WAKE_DEVICE`.
	pub detail: u32,
	// The sleep's length on the boot-time clock.
	pub slept_ns: u64,
	pub core_count: u32,
	pub _pad: u32,
	pub cores: [CorePark; SLEEP_REPORT_CORES],
}

impl Default for SleepReport {
	fn default() -> Self {
		SleepReport { wake: WAKE_UNKNOWN, detail: 0, slept_ns: 0, core_count: 0, _pad: 0, cores: [CorePark::default(); SLEEP_REPORT_CORES] }
	}
}

// What `SYS_FIRMWARE_MAP` reads: the range, the node whose `OperationRegion` it is (its row identity, `acpi:` and
// the absolute path), and the PCI function that node is the companion of (`FIRMWARE_NO_FUNCTION` for none).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareMapRequest {
	pub base: u64,
	pub len: u64,
	// `segment << 32 | bus << 16 | device << 8 | function`, as `SYS_FIRMWARE_PCI`'s address.
	pub companion: u64,
	pub node_len: u32,
	pub _pad: u32,
	pub node: [u8; PLATFORM_NAME_LEN],
}
pub const FIRMWARE_NO_FUNCTION: u64 = u64::MAX;

impl Default for FirmwareMapRequest {
	fn default() -> Self {
		FirmwareMapRequest { base: 0, len: 0, companion: FIRMWARE_NO_FUNCTION, node_len: 0, _pad: 0, node: [0; PLATFORM_NAME_LEN] }
	}
}

// The most lines or addresses a `FirmwareNode` carries in each list.
pub const FIRMWARE_NODE_LISTED: usize = 32;
pub const FIRMWARE_NODE_COMPANION: u8 = 1 << 0;
pub const FIRMWARE_NODE_PARENT: u8 = 1 << 1;
pub const FIRMWARE_NODE_FIRMWARE_HELD: u8 = 1 << 2;
// A namespace row that is itself a GPIO or serial-bus controller, with the lines and addresses the ACPI service holds
// through it: `path` is the row's identity and the lists are filled, as a companion's are.
pub const FIRMWARE_NODE_LISTS: u8 = 1 << 3;

// What `SYS_DEVICE_NODE` answers.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareNode {
	pub flags: u8,
	pub path_len: u8,
	pub aei_count: u8,
	pub field_line_count: u8,
	pub field_address_count: u8,
	pub parent_bus: u8,
	pub parent_dev: u8,
	pub parent_func: u8,
	pub parent_segment: u16,
	pub _pad: [u8; 6],
	// The companion node's identity (`acpi:` and its path).
	pub path: [u8; PLATFORM_NAME_LEN],
	pub aei: [u32; FIRMWARE_NODE_LISTED],
	pub field_lines: [u32; FIRMWARE_NODE_LISTED],
	pub field_addresses: [u32; FIRMWARE_NODE_LISTED],
}

impl Default for FirmwareNode {
	fn default() -> Self {
		FirmwareNode { flags: 0, path_len: 0, aei_count: 0, field_line_count: 0, field_address_count: 0, parent_bus: 0, parent_dev: 0, parent_func: 0, parent_segment: 0, _pad: [0; 6], path: [0; PLATFORM_NAME_LEN], aei: [0; FIRMWARE_NODE_LISTED], field_lines: [0; FIRMWARE_NODE_LISTED], field_addresses: [0; FIRMWARE_NODE_LISTED] }
	}
}

impl FirmwareNode {
	pub fn path(&self) -> &[u8] {
		&self.path[..(self.path_len as usize).min(PLATFORM_NAME_LEN)]
	}
	pub fn aei(&self) -> &[u32] {
		&self.aei[..(self.aei_count as usize).min(FIRMWARE_NODE_LISTED)]
	}
	pub fn field_lines(&self) -> &[u32] {
		&self.field_lines[..(self.field_line_count as usize).min(FIRMWARE_NODE_LISTED)]
	}
	pub fn field_addresses(&self) -> &[u32] {
		&self.field_addresses[..(self.field_address_count as usize).min(FIRMWARE_NODE_LISTED)]
	}
}

// The records of a property block, each `[kind u8][depth u8][name_len u16][value_len u32][name][value]`
// with the value padded to four bytes: a NODE opens a child at `depth` (the device's own node is depth 0
// and is not a record), a PROPERTY belongs to the last node opened at its depth, a CLOCK is a resolved
// `clocks` entry (value: the frequency in hertz as a little-endian u64) and an UNRESOLVED names a
// reference property the kernel could not turn into a value.
pub const DEVICE_PROPERTY_NODE: u8 = 1;
pub const DEVICE_PROPERTY_VALUE: u8 = 2;
pub const DEVICE_PROPERTY_CLOCK: u8 = 3;
pub const DEVICE_PROPERTY_UNRESOLVED: u8 = 4;
// The most a property block holds; a node whose block would exceed it is published with the block cut at
// a record boundary and `PLATFORM_FLAG_UNRESOLVED` set.
pub const MAX_DEVICE_PROPERTIES: usize = 4096;

// How many device identities one core's record keeps.
pub const CPU_IDLE_SOURCES: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CpuIdleSource {
	pub source: u32,
	pub _pad: u32,
	pub count: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuIdleInfo {
	pub cpu: u32,
	pub source_count: u32,
	pub idle_ns: u64,
	pub halts: u64,
	pub wakes_timer: u64,
	pub wakes_ipi: u64,
	pub wakes_housekeeping: u64,
	pub wakes_device: u64,
	pub sources: [CpuIdleSource; CPU_IDLE_SOURCES],
	// THE PROCESSOR'S POWER: each idle state installed - the halt alone where none is - with its entries and residency;
	// the performance level now, the time spent at each, the window; the idle injected; the live latency requests.
	pub state_count: u32,
	pub perf_levels: u32,
	pub states: [CpuIdleStateInfo; PROCESSOR_MAX_IDLE_STATES],
	pub perf_level: u32,
	pub window_cap: u32,
	pub window_floor: u32,
	pub inject_permille: u32,
	pub injected_ns: u64,
	pub latency_requests: u32,
	// The smallest live request's bound, `u32::MAX` for none.
	pub latency_bound_us: u32,
	pub level_ns: [u64; CPU_PERF_LEVELS],
	// THE CORE'S INTERRUPT-CONTROLLER ID - its APIC id, MPIDR affinity or hart id - which names it to firmware: the ACPI
	// service matches a processor's `_UID` through the MADT to it.
	pub hardware_id: u64,
}

impl Default for CpuIdleInfo {
	fn default() -> Self {
		CpuIdleInfo { cpu: 0, source_count: 0, idle_ns: 0, halts: 0, wakes_timer: 0, wakes_ipi: 0, wakes_housekeeping: 0, wakes_device: 0, sources: [CpuIdleSource::default(); CPU_IDLE_SOURCES], state_count: 0, perf_levels: 0, states: [CpuIdleStateInfo::default(); PROCESSOR_MAX_IDLE_STATES], perf_level: 0, window_cap: 0, window_floor: 0, inject_permille: 0, injected_ns: 0, latency_requests: 0, latency_bound_us: u32::MAX, level_ns: [0; CPU_PERF_LEVELS], hardware_id: 0 }
	}
}

// The levels whose time `CpuIdleInfo::level_ns` keeps: the performance states and the throttling states past T0.
pub const CPU_PERF_LEVELS: usize = 2 * PROCESSOR_MAX_PERF_STATES;

// Why an installed idle state is not entered on this core: 0 it is.
pub const IDLE_ENTERABLE: u32 = 0;
pub const IDLE_NO_MWAIT: u32 = 1;
pub const IDLE_COUNTER_MAY_STOP: u32 = 2;
pub const IDLE_TIMER_STOPS: u32 = 3;
pub const IDLE_CONTEXT_LOST: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CpuIdleStateInfo {
	pub entry: u32,
	pub unenterable: u32,
	pub exit_latency_us: u32,
	pub target_residency_us: u32,
	pub entries: u64,
	pub residency_ns: u64,
}

// Vector windows reported in IrqInfo::kind.
pub const IRQ_KIND_FIXED: u32 = 0;
pub const IRQ_KIND_MSI: u32 = 1;
// IrqInfo::device when no device owns the vector.
pub const IRQ_NO_DEVICE: u32 = u32::MAX;

// WHICH BINDING OF WHICH DEVICE. `SYS_DEVICE_CLAIM` copies one of these out; every capability the
// kernel derives from that claim is stamped with it, and `SYS_DEVICE_RELEASE` takes the whole thing
// back. The generation is what makes "everything from the PREVIOUS claim" a set that arithmetic can
// name rather than bookkeeping: a release naming a generation that is no longer current is refused
// instead of being applied to whoever holds the device now.
//
// It is a `u64` and it does not wrap. A slot that runs out of generations is retired for the life of
// the boot rather than wrapped onto a number a dead handle still names - the same rule handle slots
// already follow.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ClaimKey {
	pub device_index: u32,
	pub _pad: u32,
	pub generation: u64,
}

// What `SYS_DEVICE_CLAIM` copies out: the whole result of one acquisition.
//
// ONE OPERATION OR NONE OF IT. The acquisition installs two handles and copies a key, and each of
// those can fail - a partial success leaves a claim alive that its would-be owner never learned the
// name of, which is a device nothing can release and nothing can rebind. So everything the caller
// gets arrives in one struct through one copy, and a copy that fails takes the whole acquisition
// with it: both handles closed, the claim released, the caller left where it started.
//
// The two handles are why this is not just a `ClaimKey`. `memory` is the device's MMIO capability -
// what travels on to the driver - and `claim` is what STAYS with the manager.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClaimGrant {
	pub key: ClaimKey,
	pub memory: u64,
	pub claim: u64,
	// THE KERNEL'S STAMP: the entry the claim was validated against, byte for byte what the caller
	// named, and the DMA policy that entry declares - one of the `DMA_POLICY_*` codes. Declared
	// fields, never reserved padding.
	pub entry: [u8; ENTRY_NAME_LEN],
	pub policy: u32,
	pub _pad: u32,
}

impl Default for ClaimGrant {
	fn default() -> Self {
		ClaimGrant { key: ClaimKey::default(), memory: 0, claim: 0, entry: [0; ENTRY_NAME_LEN], policy: 0, _pad: 0 }
	}
}

// What `SYS_DEVICE_CLAIM_INFO` answers with: which binding this claim handle names, what state the
// device is in, and - once a release has finished - how it ended.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ClaimInfo {
	pub key: ClaimKey,
	// One of the CLAIM_STATE_* codes below.
	pub state: u32,
	// 1 once the release has reached a terminal state and `state` will not change again, 0 while the
	// claim is live. This is the bit the claim handle's readiness is defined by, so a manager parked
	// in `wait_any` learns the device is back without polling.
	pub settled: u32,
	// The same stamp `ClaimGrant` carries, read back through the handle: the entry this binding was
	// validated against and its declared policy. What a manager that did not make the binding
	// reads to learn which entry holds the device.
	pub entry: [u8; ENTRY_NAME_LEN],
	pub policy: u32,
	pub _pad: u32,
}

impl Default for ClaimInfo {
	fn default() -> Self {
		ClaimInfo { key: ClaimKey::default(), state: 0, settled: 0, entry: [0; ENTRY_NAME_LEN], policy: 0, _pad: 0 }
	}
}

// The NUL-padded 64-byte form of an entry name, for the claim request. A name longer than the field
// is not an entry the manifest could have declared, and the kernel refuses the request.
pub fn entry_name_field(name: &[u8]) -> Option<[u8; ENTRY_NAME_LEN]> {
	if name.is_empty() || name.len() > ENTRY_NAME_LEN {
		return None;
	}
	let mut field = [0u8; ENTRY_NAME_LEN];
	field[..name.len()].copy_from_slice(name);
	Some(field)
}

// The name inside a 64-byte field: the bytes before the first NUL, or the whole field.
pub fn entry_name_of(field: &[u8; ENTRY_NAME_LEN]) -> &[u8] {
	let end = field.iter().position(|byte| *byte == 0).unwrap_or(ENTRY_NAME_LEN);
	&field[..end]
}

// What `SYS_DEVICE_CLAIM_SNAPSHOT` answers with, for a manager that holds no claim handle.
//
// `release_deadline` is 0 unless the state is `Releasing`. It is the CLAIM'S OWN deadline, minted at
// the release from a constant of the kernel's - not one handed in, because the party that would hand
// one in is exactly the party that may have just died.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DeviceClaimSnapshot {
	// One of the CLAIM_STATE_* codes.
	pub state: u32,
	pub _pad0: u32,
	// The generation of the current claim, or of the last one that ended.
	pub generation: u64,
	// Absolute tick by which a teardown under way must confirm, or 0.
	pub release_deadline: u64,
	// WHAT THIS CLAIM STILL HOLDS, so a manager that did not make the binding can reconstruct the
	// charge rather than start it at zero.
	//
	// M0165's M5 asks for the device-specific holdings - MMIO windows, IRQ vectors, IOMMU grants -
	// to be reconstructable from this snapshot, and it carried only the state, the generation and the
	// deadline. DeviceManager's own `granted_resources` is a count of RESOURCE frames sent during the
	// CURRENT bind, which is zero for a reconstructed node and says nothing about a claim somebody
	// else made. These are counted from the kernel's own records at the moment of the read.
	pub mmio_windows: u32,
	pub irq_vectors: u32,
	pub iommu_grants: u32,
	// AND HOW MANY OF THOSE GRANTS ARE QUARANTINED, which is the half a restart baseline turns on.
	//
	// `iommu_grants` counts live and quarantined mappings TOGETHER, deliberately: a quarantined one
	// is charged exactly like a live one because nobody knows the device has stopped resolving it.
	// But a manager reconstructing a binding needs to know WHICH it is holding - a claim whose
	// grants are all live can be released and reused, and one holding a quarantined mapping cannot
	// be, ever, for the life of the boot. One number for both cannot say that, so the second is
	// carried here rather than left to be inferred.
	//
	// This was the reserved padding word, so the struct's size and every offset before it are
	// unchanged.
	pub iommu_quarantined: u32,
	// AND HOW MANY FAULTS THIS BINDING RAISED, which is the fourth counter P02M0153's M4 names
	// beside the mapping, IOVA and endpoint ones - and the one this struct did not carry.
	//
	// A controller-wide total cannot answer "did this restart come back to its baseline" for ONE
	// device: a driver that crashes, is rebound and faults again adds to the same number as every
	// device beside it. Counted against the DOMAIN, which is the binding, so a retained domain still
	// answers for the binding that left it behind. `u64`, because a flooding endpoint may increment
	// a counter for ever and a count that wrapped would read as a clean binding.
	pub iommu_faults: u64,
}

// The four states a device-table slot can be in. THE SAME FOUR everywhere - two lists is how a state
// ends up meaning different things in two places.
//
// `Free` - nothing holds it and it may be claimed.
// `Claimed` - exactly one holder.
// `Releasing` - a teardown is under way; a new claim does not begin until it finishes.
// `Quarantined` - the teardown could not be CONFIRMED, so no frame or vector it held goes back into
// circulation and the device is not claimable again for the life of the boot.
pub const CLAIM_STATE_FREE: u32 = 0;
pub const CLAIM_STATE_CLAIMED: u32 = 1;
pub const CLAIM_STATE_RELEASING: u32 = 2;
pub const CLAIM_STATE_QUARANTINED: u32 = 3;

// The fourth argument of `SYS_CHANNEL_SEND_ATTENUATED`, read out of the caller's memory because the
// syscall ABI has no fifth register to put it in.
//
// `rights` is a MASK, not a demand: the receiver gets the intersection of it and what the capability
// actually holds, so naming a right the capability does not have is not an error.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CapTransfer {
	pub handle: u64,
	pub rights: u32,
	pub _pad: u32,
}

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

// A blocking wait gave up because the caller has a CAUGHT interrupt pending (SYS_SIGNAL_CATCH):
// not a failure of the wait, and not the deadline - the caller asked to handle Ctrl+C itself, and
// this is the kernel handing control back so it can.
//
// It exists because without it that promise cannot be kept. A caught SIG_INT sets a flag the
// process polls; a process parked in `SYS_WAIT`/`SYS_WAIT_ANY` polls nothing, and the wait's
// condition loop re-blocks on every wake that leaves its objects unready. So the flag was set,
// the thread was woken, and it went straight back to sleep having never returned to the code that
// would have read it - Ctrl+C on an idle interactive program did nothing whatsoever.
//
// The pending flag is NOT consumed here: `SYS_SIGNAL_TAKE` is what clears it, so a caller that
// treats this as an ordinary wakeup and polls `interrupted()` sees exactly one interrupt.
pub const ERR_INTERRUPTED: i64 = -14;

// SOMEBODY ELSE HAS THIS DEVICE, or is in the middle of giving it back.
//
// Its own code rather than the `ERR_INVALID` this used to collapse into, because a caller that
// cannot tell "somebody else has it" from "you passed nonsense" cannot retry correctly: the first
// is worth waiting on and the second never will be. A device that is `Quarantined` answers
// `ERR_UNSUPPORTED` instead - that one is not a wait, it is the rest of the boot.
pub const ERR_ALREADY_CLAIMED: i64 = -15;

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
		// archive with a large enough entry table drove quadratic work.
		//
		// THIS IS THE READER'S OWN CEILING AND IT IS NOT THE WRITER'S. `bootstrap::MAX_ENTRIES` is
		// 64 and bounds a bootstrap set, which is a handful of boot programs; this is 4096 and
		// bounds what a reader will look at, which is a system package of 146 today. They are
		// deliberately different sizes because they bound different sets, and unifying them would
		// be a regression in one direction or the other.
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
