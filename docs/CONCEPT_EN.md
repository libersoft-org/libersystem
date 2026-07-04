# LiberSystem - design of a modern OS

## Contents

### 1. Introduction and principles

- [Project direction](#project-direction)
- [Why this OS instead of Linux](#why-this-os-instead-of-linux)
- [Language policy](#language-policy)
- [System API model](#system-api-model)

### 2. Kernel

- [Kernel design](#kernel-design)
- [Kernel components](#kernel-components)
- [Memory model](#memory-model)
- [Kernel object model](#kernel-object-model)
- [Capability model](#capability-model)
- [IPC model](#ipc-model)
- [Syscall model](#syscall-model)
- [Resource accounting](#resource-accounting)

### 3. System services and boot

- [Boot flow](#boot-flow)
- [SystemManager](#systemmanager)
- [ServiceManager](#servicemanager)
- [DeviceManager](#devicemanager)
- [PermissionManager](#permissionmanager)
- [ResourceManager](#resourcemanager)
- [Drivers](#drivers)
- [System Graph](#system-graph)

### 4. Storage and data

- [Storage model](#storage-model)
- [Native filesystem](#native-filesystem)

### 5. Security and updates

- [Security model: current decisions](#security-model-current-decisions)
- [Immutable system and update model](#immutable-system-and-update-model)

### 6. Interfaces and application model

- [Application model: native ABI + WebAssembly/WASI host](#application-model-native-abi--webassemblywasi-host)
- [IDL language](#idl-language)
- [Compatibility and POSIX-like layer (deferred)](#compatibility-and-posix-like-layer-deferred)

### 7. Roadmap and conclusion

- [MVP design](#mvp-design)
- [Roadmap](#roadmap)
- [License](#license)
- [Open questions](#open-questions)
- [Recommended next step](#recommended-next-step)

---

## 1. Introduction and principles
### Project direction

The goal is to design a new, modern operating system from scratch.

It is not:

- a Linux distribution
- a Unix-like system
- a Linux-compatible OS

It does not inherit historical models:

- no POSIX as the primary kernel API
- no `/proc`, `/sys`, `/dev`
- no mount points
- no global root filesystem
- no "everything is a file" (it does not blend processes, devices, sockets, or settings into untyped byte streams and textual pseudo-files)

Instead it builds on a typed object / capability model - every resource has a clear type and an explicit interface.
This is a deliberate design philosophy - doing things in a more modern, more object-oriented, and better-typed way.

The main pillars that set the OS apart:

- **Capability-based security**
- **A modern application host** on top of a native typed ABI
- **Memory safety (thanks to Rust)**

#### Decision

- Do not build on the Linux kernel.
- Do not copy the Linux directory-structure model.
- Do not use `/proc`, `/sys`, `/dev` as the primary system API.
- Do not use mount points.
- Do not use a global root filesystem as the basic storage model.
- Do not use the "root can do anything" model as the primary security model.
- Do not use `ioctl`-style chaos as the main device API.
- Design the OS as a modern capability-based system.

#### Mental model

```text
Kernel = a small, safe arbiter
System services = the functions of the OS
Drivers = isolated, restartable services
Applications = sandboxable components
Storage = explicit volumes, not mount points
System API = a typed object/capability API
```

#### Layering principle (stackable, replaceable layers)

The system is designed as a **stack of layers where each layer is replaceable as long as it honors its contract.** This is not a rule for one specific choice - it is a universal design principle of the whole OS.

```text
Each layer talks to its neighbor through a stable, typed contract (IPC/IDL).
A layer's implementation is replaceable; the contract stays.
No layer may be "hardwired" into the others such that it cannot be replaced.
```

Consequences:

- **The dependency is on the contract, not on a specific implementation.** The layer above it should not know *who* serves it, only *what interface* it gets.
- **Risky or immature technologies can be isolated behind an adapter.** When such a technology changes or is replaced, only that one layer's adapter is rewritten, not the system.
- **Multiple implementations of the same contract can coexist** (e.g. several filesystem backends behind a single Volume API, several application hosts on top of the same native IPC).

This principle maps concretely onto the application model (see *Application model*: WASI is just one of several hosts on top of a stable native ABI), storage (several FS backends behind the Volume API), and the API layer (several representations on top of one typed model).

---

### Why this OS instead of Linux

The short, honest answer: because it offers a model that, due to 30 years of backward compatibility, can no longer be retrofitted into Linux. The main reasons (ordered by strength):

1. **Capability-based security as the foundation.** No "root can do anything", no ambient authority. The principle of least privilege is structural - whole classes of bugs disappear (privilege escalation, confused deputy).
2. **WebAssembly/WASI as the native application model.** Portable, sandboxed, language-neutral applications; a single artifact runs on x86/ARM/RISC-V. Security and portability "for free".
3. **Memory safety from the ground up (Rust).** Eliminates most of the CVE classes that plague kernels written in C (memory-safety bugs).

Supporting reasons:

- **Microkernel reliability** - a driver/service crash does not bring the system down, it just restarts.
- **A clean, typed, inspectable API** - one API with four representations (binary/CBOR/JSON/CLI), no scraping text from `/proc`, no `ioctl` chaos. Great for tooling and automation.
- **An explicit storage model** - no silent writes to the wrong disk, no surprises from mount points.
- **No legacy debt** - modern choices without dragging decades of compatibility along.
- **Open source** - maximum freedom of use and forking.

#### Who the system is for in the current phase

At present the system is **aimed at developers, early adopters, and edge/security and research deployments**, not as a replacement for Linux in the short term:

- This is an intent, not a weakness - it represents the **entry point of a long-term direction**, not the final state.
- The early target group are those who are drawn to the capability model, WASI, and a clean architecture, and who will start building an ecosystem of applications, tools, and drivers.
- The driving force of the direction is not "modernity and the absence of legacy" in itself, but **capability security, the WASI application model, and memory safety**.

The system's further direction (server -> real hardware -> desktop -> AI platform) and the moment of opening to a wider audience are described in the *Roadmap*.

#### Deployment targets: appliance/edge -> server -> desktop

Besides layering *in time* (who), the project also has a clear order of *deployment targets* (where). The targets are ordered by a single principle: **from maximum self-control to minimum**. The more the system controls its own hardware and software, the smaller the external ecosystem it needs - and dependence on an external ecosystem tends to be the most common cause of failure for new operating systems.

| Target | Order | Who controls the hardware | Who writes the running software | Required external ecosystem |
|---|---|---|---|---|
| **Appliance / edge / embedded** | **1st (current)** | the project (a single board / VM profile) | the project (native / WASI) | minimal |
| **Server** | 2nd | partly the project | partly external (services, DB) | medium (POSIX compat.) |
| **Desktop** | 3rd | anyone (unbounded HW) | the whole world (GUI apps) | extensive (where most OSes fail) |

Key rules of this ordering:

- **Each target is a superset of the previous one.** From embedded to server you mainly add networking (and, later, POSIX compatibility for foreign software); from server to desktop you add the GUI/compositor, the full input and audio stacks (basic console input and a headless audio service already arrive earlier), and a wider range of drivers. These are not three independent from-scratch starts, but building in layers (see *Layering principle*).
- **Each target is valuable on its own, not merely a stepping stone.** Even the appliance/edge target is a full product in its own right (a secure edge node), so the system delivers real value from the very first phase - not only at the end of the journey. Server and desktop build on this foundation as full-fledged extensions the system is heading toward.

### Language policy

The OS is written primarily in Rust.

#### Decision

```text
Safe Rust practically everywhere.
Unsafe Rust only where it is truly necessary.
Assembler only where it is unavoidable.
Do not use C/C++ for new core OS code.
Allow C/C++ only exceptionally for adopted libraries, temporary ports, or large external stacks.
```

#### Where safe Rust is

- most kernel logic,
- scheduler logic,
- IPC,
- capabilities,
- handle tables,
- object lifecycle,
- resource accounting,
- SystemManager,
- ServiceManager,
- DeviceManager,
- StorageService,
- Log/EventService,
- System Graph,
- CLI tools,
- the native filesystem,
- most userspace drivers.

#### Where `unsafe Rust` is needed

- page tables,
- MMIO,
- DMA,
- IOMMU,
- raw physical memory,
- interrupt setup,
- CPU registers,
- the kernel/userspace transition,
- low-level arch-specific operations.

#### Where assembler is

- boot glue,
- context switch,
- syscall entry/exit,
- interrupt entry/exit,
- possibly very low-level CPU operations.

#### Rule for unsafe

```text
Unsafe is a quarantine for contact with hardware.
Unsafe must not be the everyday programming style.
Unsafe must be small, auditable, and wrapped in a safe API.
```

---

### System API model

The system does not use `/proc`, `/sys`, `/dev` or textual pseudo-files as its main API.

#### Decision

There is a single canonical typed API.

On top of it there are various representations:

| Representation | Purpose |
|---|---|
| Binary / IDL | fast communication between system parts |
| CBOR | compact structured data including byte strings |
| JSON | scripts, debugging, remote administration |
| Human CLI | human-readable output |

#### Important rule

```text
There are not 4 different APIs.
There is one typed API and 4 representations of the same data.
```

CLI, JSON, CBOR, and binary output are just different views of the same object model.

#### Principle: the object is canonical (holds across the whole system)

This is not just a rule about the system API - it is a **system-wide principle** that holds everywhere the system names, transfers, or stores structured information:

> The canonical form is always a **typed object** defined in IDL.
> Text / URI / JSON / CBOR / CLI / ... are its **representations**.
> Identity and authority live in the **capability**, not in the name.

For paths the principle is spelled out in the *Storage model* section ("A path is an object, a URI is just a representation"); here it is generalized to the rest of the system.

##### Where the principle applies

| Area | Canonical object (proposal) | Representations | Authority / note |
|---|---|---|---|
| **Configuration** (`ConfigService`) | a typed `ConfigNode` tree with a schema in IDL | JSON / CBOR / CLI / binary | no parsing of textual `/etc` files; text is only an editable representation |
| **Logs** (`LogService`) | `LogRecord { ts, severity, source, fields }` | human text, JSON, CBOR, binary | logs are queryable structured data, not lines of text (the journald model, not syslog) |
| **Service / driver identity** | `ServiceId` / `ComponentId` | string `driver.usb`, JSON | authority is always a **handle/capability to a Channel**, not "find a service by name" |
| **Errors / states** | a typed `Error` (variant) | numeric ABI code, human string, JSON | no errno-style bare ints or ad-hoc text messages |
| **Network addresses / endpoints** (`NetworkService`) | `Endpoint` / `SocketAddr` / `{ ip, port }` | `192.168.0.1:80`, URL text | parsing a string is a source of holes; the type is safe (analogous to `VolumePath`) |
| **Wall-clock time** (the *Syscall model* section) | `Timestamp` / `Instant` | ISO-8601, epoch, human format | monotonic time is an int (ns), calendar time is an object |
| **Package manifest / permissions** (the *Security model* section) | a typed `Manifest` / `PermissionSet` | text for manual editing, JSON | the model is an object, not a YAML/JSON file as the "source of truth" |
| **System Graph** (the *System Graph* section) | a graph of typed references to objects | tree/image, JSON, CBOR, CLI | nodes = references to Process / Service / Driver / Device / Volume |

##### Where, by contrast, NOT to force the principle

- **Bulk binary data** - file contents, a DMA / shared-memory payload, a video frame, a network packet. These are deliberately opaque bytes; the principle is about structured *identifiers and metadata*, not about wrapping every buffer in a schema.
- **Performance hot paths** - they may have only a single canonical binary form and not generate the other representations. A representation is an *option*, not an obligation for every value.

#### Examples of services

```text
ProcessService
StorageService
DeviceService
NetworkService
GraphicsService
AudioService
ConfigService
LogService
SystemGraphService
```

#### What we do not want

```text
cat /proc/meminfo
cat /sys/class/net/...
open("/dev/input/event0")
ioctl(fd, MAGIC, ptr)
```

#### What we do want

```text
MemoryService.GetInfo()
DeviceService.List()
InputService.Subscribe(...)
Storage.Volume.Open(...)
```

And alongside it:

```text
command
command --json
command --cbor
command --binary
```

---

## 2. Kernel

### Kernel design

The kernel is a small capability-based message core.

The kernel is not "the whole operating system". The kernel is only the security, scheduling, and isolation foundation.

#### What the kernel knows

- who is running,
- what memory it has,
- whom it may communicate with,
- which capabilities it holds,
- which hardware it has access to,
- what needs to be cleaned up on a crash.

#### What the kernel does not know

- what a file is in the user sense,
- what a volume alias is,
- what a window is,
- what an audio stream is,
- what a network connection is,
- what an application is in the product sense,
- what a package manager is,
- what a system update is.

---

### Kernel components

**In the kernel:**

Only what must be privileged, absolutely trusted, or what directly enforces isolation belongs in the kernel.

| Area | In the kernel | Reason |
|---|---:|---|
| Boot takeover | yes | the kernel has to start up |
| CPU management | yes | control of CPUs, modes, cores |
| Scheduler | yes | decides what runs |
| Thread low-level model | yes | the basis of running code |
| Process low-level model | yes | an isolated run container |
| Address spaces | yes | memory isolation |
| Physical RAM | yes | ownership and page allocation |
| Virtual memory | yes | page tables, mapping, protection |
| Memory protection | yes | read/write/execute, guard pages |
| IPC / message passing | yes | the basis of service communication |
| Capabilities / handles | yes | the security model |
| Kernel object model | yes | the system's basic primitives |
| Interrupt routing | yes | safe IRQ delivery |
| Timers | yes | scheduler, timeouts, sleep |
| IOMMU / DMA protection | yes | a device must not write anywhere |
| MMIO mapping | yes, in a controlled way | a driver gets only its own device's registers |
| Device access control | yes | enforcement of HW rights |
| Shared memory primitives | yes | high-performance data sharing |
| Event/wait primitives | yes | waiting without polling |
| Fault detection | yes | page fault, illegal instruction, crash |
| Resource cleanup | yes | reclaiming memory, IRQ, DMA, capabilities |
| Resource accounting primitives | yes | limits on RAM, handles, IPC queues, DMA |
| Starting the first userspace process | yes | launching the SystemManager |
| Early logging / panic | yes | emergency diagnostics |
| Recovery foundation | minimally | if the SystemManager crashes |

#### The shortest definition of the kernel

```text
Kernel = memory + scheduling + IPC + capabilities + safe hardware access + cleanup.
```

#### SMP / multicore: a design constraint from the start

Multicore is not a feature you "bolt on later". This does not mean the MVP must immediately be tuned for performance - but **the data structures, locks, and IPC are designed SMP-aware from the first version**, even if SMP is not yet optimized (it can boot and run on a single core for now).

```text
RULE:
The kernel is designed SMP-aware from Phase 0.
SMP need not be optimized in the MVP (running on a single core is fine),
but no data structure or invariant may assume single-core.
```

Why it must be there from the start, not added later:

- **SMP leaks into the whole locking and IPC model.** Per-CPU run queues, how handles are passed between cores, how resources are accounted across CPUs, how the handle table and capability operations are synchronized - all of this decides whether the kernel is SMP-ready. These choices cannot be made "later" without rewriting the foundations.
- **Retrofitting SMP into a single-core kernel is expensive and painful.** A kernel designed for one core typically has one big lock; breaking it up into fine-grained locking later is exactly the path Linux paid for over years (the Big Kernel Lock and its gradual removal). We avoid that by designing lock granularity correctly right away.
- **The capability and Domain model must account for it.** Passing, duplicating, and revoking a handle and accounting into a `Domain` can happen from several cores at once. When it is SMP-aware from the start, it is just a matter of the right lock/atomics; when it is not, it is a later rewrite of security-critical code.

For the MVP, running on a single core is enough, but **the design must be multicore-ready from Phase 0**:

- The actual SMP scheduling, per-CPU run queue, and load balancing are tuned later.
- The design foundation (lock granularity, SMP-aware data structures) must stand right away, though.

---

**Not in the kernel:**

| Area | Where it lives |
|---|---|
| filesystems | a filesystem driver service |
| StorageService / volumes | a userspace service |
| USB stack | `driver.usb` |
| NVMe/SATA/AHCI | driver services |
| GPU driver | `driver.gpu` + `GraphicsService` |
| audio stack | `AudioService` |
| network stack | `NetworkService` |
| Wi-Fi/Bluetooth | driver services |
| graphics compositor | `GraphicsService` |
| input routing | `InputService` |
| config system | `ConfigService` |
| main logging system | `LogService` |
| package manager | a later `PackageManager` |
| update system | a later `UpdateService` |
| driver restart | `ServiceManager` + `DeviceManager` |
| app sandbox policy | a higher layer / later |
| JSON/CBOR/human CLI rendering | the API/CLI layer |
| `system://`, `vol://` resolver | userspace services |
| `/proc`, `/sys`, `/dev` | do not do at all |

---

### Memory model

The kernel manages:

```text
physical RAM
virtual memory
page tables
shared buffers
DMA buffers
mapping protection
```

The kernel does not manage as the main owner:

```text
VRAM
GPU textures/surfaces
filesystem cache
application heap
```

Split:

```text
Kernel Memory Manager:
  RAM, mapping, isolation, DMA safety

GPU/GraphicsService:
  VRAM, textures, surfaces, framebuffers

StorageService:
  file cache, block cache

Applications/runtime:
  heap allocator
```

---

### Kernel object model

The kernel should know a small set of objects.

Proposed kernel objects:

| Object | Meaning |
|---|---|
| `Domain` | a hierarchical container of processes (a group for limits, recovery, and bulk termination) |
| `Process` | an isolated running container |
| `Thread` | a concrete running thread |
| `AddressSpace` | a process's virtual memory |
| `MemoryObject` | a chunk of RAM or shared memory |
| `Channel` | a communication channel |
| `Event` | wait/wake/signal |
| `Timer` | a timer |
| `Interrupt` | an interrupt handed to a driver |
| `DeviceMemory` | an allowed MMIO region |
| `DmaBuffer` | DMA-safe memory |
| `Capability` | a right to an object |
| `Handle` | a process's holding of a capability |

#### Hierarchy: Domain

Processes are not flat - they form a tree under nodes of type `Domain` (similar to Zircon *Job*). A `Domain` is a group node on which the following hang:

- **resource limits** for the whole subgroup (memory, handle/thread counts, IPC queues, DMA) - see Resource accounting,
- **bulk termination**: killing a `Domain` terminates the whole subtree (a process and its descendants) and the kernel cleans up all their handles,
- **recovery and isolation**: critical subsystems (e.g. all drivers) can run in their own `Domain`, so they can be restarted as a whole.

The tree typically looks like:

```text
root Domain
├── SystemManager
│   ├── ServiceManager
│   │   ├── LogService
│   │   └── StorageService
│   └── DeviceManager Domain
│       ├── driver.virtio-blk
│       └── driver.virtio-net
└── Apps Domain
    ├── app A (WASM component)
    └── app B (WASM component)
```

This model still needs to be worked out into a detailed specification, but the direction is decided.

---

### Capability model

The kernel does not use the primary model:

```text
root / user / group
```

Instead:

```text
capability = an unforgeable right to an object
handle = a concrete capability in a process's table
```

#### Rules

- A capability cannot be guessed.
- A capability cannot be fabricated from a number.
- A capability can be obtained only by being passed.
- A capability can be restricted by rights.
- A capability can be passed over IPC.
- On a process crash the kernel erases its handle table.
- The process thereby loses access to all objects.

#### Example of rights

```text
read
write
execute
map
send
receive
duplicate
transfer
revoke
```

#### Example handle table

The `driver.nvme` process may have:

```text
handle 1 -> Channel to DeviceManager
handle 2 -> PCI device capability
handle 3 -> MMIO region BAR0
handle 4 -> Interrupt 42
handle 5 -> DMA domain
handle 6 -> LogService channel
```

On a driver crash the kernel automatically removes all handles and the related permissions.

#### How it should work correctly (detailed model)

**A capability = (a reference to a kernel object + a set of rights + a badge), held exclusively in the kernel.** Userspace never holds a raw capability; it holds only a *handle* - an opaque index into its handle table. This is the same principle as a file descriptor, but for all of the system's objects.

```text
Capability {
  object:  ref to a kernel object (Process, Channel, MemoryObject, …)
  rights:  a bitset of allowed operations
  badge:   an optional immutable label set at creation time
}
Handle = an index into the per-process handle table -> Capability
```

**Rights.** An operation on an object is allowed only when the handle carries it. Rights cannot be "invented" out of a handle.

```text
read         write        execute
map          send         receive
duplicate    transfer     revoke
get_info     manage       wait
```

**Attenuation (narrowing rights).** From a capability you can derive a *weaker* one, never a stronger one. `handle_duplicate` can create a copy with only a subset of the original rights. This naturally enforces the principle of least privilege when passing capabilities.

```text
handle(read|write|duplicate)  --duplicate(read)-->  handle(read)
```

**Badging (distinguishing clients on a shared channel).** A server can assign multiple clients to one Channel, each with a different immutable `badge`. The kernel attaches the badge to the message, so the server reliably knows "who it is from" without any chance of forgery. It serves as the basis for identities and per-client policies.

**Passing (transfer).** A capability is obtained *only* by being passed over a Channel (`handle_transfer`). It cannot be guessed, fabricated from a number, or "found" in a global namespace. Passing can be:

- *move* (the sender loses the handle), or
- *copy* with attenuation (the sender keeps it; the recipient gets a weaker one).

**Revocation.** Removing a right has two levels:

- *closing a handle* (`handle_close`) - local, only the given process loses access,
- *revoking the object* - the kernel invalidates *all* handles to the object (e.g. the StorageService revokes access to a volume that is disappearing). Implemented via a generation counter / revocation on the object, so revocation is O(1) and cannot be bypassed.

**Sealing / typing.** A capability is always bound to the object's type; you cannot send a "MemoryObject where a Channel is expected". Type checking is done by the kernel, not the client.

**Lifecycle and cleanup.** Objects are reference-counted; they live as long as a handle to them exists (or the kernel/a message in transit holds them). On a process crash the kernel walks its handle table, closes each handle (decrements the refcount), and the bound resources (IRQ, DMA, MMIO) are thereby freed too. This is the entire "security cleanup" - it does not require cooperation from the crashed process.

**What this buys us.**

- No ambient authority - a process has exactly what it was given, nothing more.
- *Confused deputy* and *privilege escalation via "root"* disappear - there is no global "root can do anything".
- Security is a structural property of the graph of passed capabilities, not a set of checks scattered through the code.

---

### IPC model

The basic kernel object for communication is the `Channel`.

#### Small messages

Used for ordinary control:

```text
Storage.Open(path, rights)
Device.GetInfo()
Process.Start()
Log.Write()
```

#### Large data

Large data is not sent directly in an IPC message.

Instead:

```text
message = metadata + a handle to a SharedBuffer / DmaBuffer
```

This is important for:

- storage,
- networking,
- audio,
- video,
- GPU,
- high-performance IPC.

#### Communication model (decided)

The backbone is **asynchronous and non-blocking at the kernel level**, with a convenient synchronous-looking client API on top of it. This is exactly what Zircon does and it has proven itself.

```text
Kernel primitives: channel_send / channel_receive are NON-blocking.
Waiting:           the only place that blocks is `wait`
                   (waits for a channel's readability/writability, an event, a timer).
Client API:        on top of that, IDL generates "call(req) -> resp",
                   which internally does send + wait + receive.
```

Reasons:

- A non-blocking core = no holding of kernel locks during waiting, better scaling and resilience.
- The server itself controls when and how many messages it accepts (natural **backpressure**).
- Async streams and request/response can both be built on the same primitive.

**Request/response** is a convention on top of a Channel: a message carries a `correlation id`, and the reply is sent back on a reply-handle. A synchronous `call()` is just `send` + `wait` + `receive` wrapped in generated code.

**Event streams** = a one-way Channel where the server publishes and the client reads via `wait` (no polling).

**Backpressure**: each Channel has a bounded queue (accounted into the sender's resource accounting). A full queue -> `send` returns `WOULD_BLOCK`, and the sender waits via `wait`. A message is never silently dropped.

**Timeouts**: handled by `wait` with a deadline (via a `Timer`), not a separate mechanism in every call.

##### Default wire format: decode-cheap binary (decided)

The default wire format for IPC messages is a **cheap-to-decode binary in the style of FIDL / Cap'n Proto**, not CBOR or JSON. This is a deliberate decision: **a microkernel lives and dies by the cost of IPC**, and every hop (de)serializes a typed record, so the default format must be cheap to read.

```text
RULE:
Default IPC wire = a decode-cheap binary layout (fixed offsets / in-place reads, FIDL/Cap'n Proto style).
CBOR/JSON are NOT the default transport format - they are optional representations
for scripts, debugging, and remote administration (see System API model).
```

Reasons:

- **Decoding without parsing.** Fixed offsets / in-place reads mean the recipient touches a field directly in the buffer without allocation and without a full parser - orders of magnitude cheaper than CBOR/JSON.
- **Low latency on the hot path.** Storage, graphics, and other hot paths cannot afford the cost of a structured parser on every `call()`.
- **This does not violate "the object is canonical".** The canonical form remains the typed object from IDL; the decode-cheap binary is its **default representation on the wire**, and CBOR/JSON/CLI are the other (optional) representations (see *System API model*). For hot paths the other representations are simply not generated.

A distinction to avoid confusion: this is about the **format of control-plane messages**. Large data is not sent in the message body anyway, but as a `handle to a SharedBuffer / DmaBuffer` (zero-copy, see above) - the default wire format does not concern it.

##### Still to be refined

- the exact binary message layout (the *style* is decided - decode-cheap; the concrete bytes remain, to be resolved by IDL/WIT),
- queue priorities and fair-scheduling of messages,
- the concrete default queue sizes.

##### Verify by measurement as early as possible

A microkernel lives and dies by IPC performance - historically a number of systems died on it. Therefore: **as soon as Channel IPC is running (Phase 0), measure right away, do not estimate.**

- **Round-trip latency of a local `call()`** (send + wait + receive) - **measured as soon as the Channel runs (Phase 0), and it is a gate, not a nice-to-have**: until the round-trip fits the target budget (on the order of single-digit µs), higher layers are not built on the IPC. Measure continuously, not only at the end.
- **Zero-copy for large data** - empirically confirm that "metadata + a handle to a shared/DMA buffer" really does not copy the payload. This is correct in the design, but it must be verified, not assumed.
- **The cost of typed serialization at the boundary** - where "the object is canonical" is cheap, and where for hot paths (storage, graphics) only a single binary form pays off.

The point of measuring is to **confirm or refute** where a given design is good and where it must be done differently - before higher layers are built on it.

---

### Syscall model

The kernel should have few syscalls.

Proposed minimal set:

| Syscall | Meaning |
|---|---|
| `process_create` | create a process |
| `process_exit` | terminate a process |
| `thread_create` | create a thread |
| `thread_start` | start a thread |
| `channel_create` | create a channel |
| `channel_send` | send a message |
| `channel_receive` | receive a message |
| `wait` | wait for an event/channel/timer |
| `memory_object_create` | create a memory object |
| `memory_map` | map memory |
| `memory_unmap` | unmap memory |
| `handle_duplicate` | copy a handle with restricted rights |
| `handle_transfer` | pass a handle |
| `handle_close` | close a handle |
| `timer_create` | create a timer |
| `interrupt_bind` | hand an interrupt to a driver |
| `device_memory_map` | allow an MMIO region |
| `dma_buffer_create` | create a DMA-safe buffer |
| `fault_info_get` | information about a process crash |
| `domain_create` | create a `Domain` (a group of processes) |
| `domain_kill` | terminate a `Domain` along with its whole subtree |
| `object_info_get` | introspect an object (typed) |
| `object_property_set` | set an object property (name, limit…) |
| `event_signal` | set/clear a signal on an `Event` |
| `clock_get` | read the kernel's monotonic time (running since boot) |
| `random_get` | cryptographic randomness from the kernel |

A kernel syscall should not be:

```text
file_open
file_read
socket_create
window_draw
audio_play
device_list
```

Those are functions of services, not of the kernel.

#### Realistic expectation of scope

"Few syscalls" is a principle, not a hard number. The set above is the *core*; realistically it will grow toward ~50-100 calls (introspection, debugging, `Domain` management, object properties, time, randomness). More important than the count is:

- **a stable, versioned ABI** - the syscall interface must not change incompatibly; new calls are added, old ones do not disappear,
- **narrow semantics** - each syscall does one thing over a kernel object,
- **no "service" operations in the kernel** - everything else goes through IPC to services.

For comparison: Zircon targets ~100 syscalls and is still a "small" kernel.

#### Time: what `clock_get` returns

`clock_get` returns the **kernel's monotonic time** - a counter tied to a hardware timer that starts (essentially from zero) at boot, only increases, and never goes backward. **It is not a calendar date and time**, but a flowing baseline for:

- timeouts and deadlines (`wait`),
- measuring durations ("how much elapsed"),
- the scheduler.

The unit is fixed in the ABI (nanoseconds); the caller computes seconds/milliseconds itself. Because the scheduler and timeouts depend on monotonic time, it is **non-settable by design** - if it could be "rewound", all time-dependent logic would break.

**Wall-clock time (the real date and time, UTC) is not kernel state but the policy** of a userspace service (`TimeService`, possibly part of `ConfigService`):

- TimeService holds the offset and computes `UTC = clock_get (monotonic) + offset` (+ time zone, DST… a purely userspace matter).
- The offset is obtained from the RTC (via a driver) or from NTP.
- **Setting the real time = a capability-gated operation of the service**, not a syscall - only the holder of a handle to TimeService with the `write` right may do it (an NTP client, an RTC driver, an admin tool). No global "root sets the time".
- A fast read of UTC can go through a read-only shared mapping of the offset (vDSO-style), so it is not an IPC on every query.
- Wall-clock time is passed in the API as a typed `Timestamp` object (canonical); ISO-8601, epoch, and the human format are just its representations (see *System API model*).

---

### Resource accounting

The kernel should count and enforce basic resources from the start.

Not as a full Linux cgroups model, but more simply.

Each process has a resource account:

```text
memory_used
handle_count
thread_count
ipc_queue_bytes
dma_bytes
```

The kernel must be able to refuse:

```text
another DMA buffer
another thread
another handle
another IPC message when the queue is full
another memory object when the limit is exceeded
```

Policy can later be handled by `ResourceManager`, but enforcement must be in the kernel.

#### How it should work correctly

**Both a process and a `Domain` have an account.** Limits compose hierarchically: a process may not exceed its own limit nor the aggregate limit of its `Domain`. This makes it possible to budget a whole group (e.g. "all applications together at most N MB").

**Who pays for a message "in transit".** A clear rule against DoS: **the memory of a message in the queue is accounted to the sender until the recipient takes it over.** A full recipient queue -> `send` returns `WOULD_BLOCK` (the message is not dropped; the sender waits). This way a sender cannot flood a recipient nor "for free" allocate memory in someone else's account.

**Enforcement at the boundary, not in the middle.** The limit check is an atomic part of the operation that creates the resource (`*_create`, `memory_map`, `channel_send`). Either the operation succeeds and the resource is counted, or it fails - never "half allocated".

**The real resource is accounted, not an abstraction.** At minimum:

```text
memory_used     physical pages held by the process (incl. shared ones by share)
handle_count    the number of handles in the table
thread_count    the number of threads
ipc_queue_bytes memory of messages waiting in queues (on the sender's side)
dma_bytes       pinned DMA memory
```

**Failure is a first-class state, not a panic.** On exceeding a limit the kernel returns a typed error (`RESOURCE_EXHAUSTED`), giving services a chance to react (slow down, free a cache) instead of crashing.

**Cleanup returns resources immediately.** The demise of a process/`Domain` tears down the refcounts -> memory, handles, DMA, and queue slots are freed without cooperation from the crashed component.

For the MVP it is enough to count and enforce `memory_used`, `handle_count`, `thread_count`; queues and DMA will be added with IPC and drivers.

---

## 3. System services and boot

These layers are not yet specified in detail, but the basic responsibilities are clear.

### Boot flow

The proposed system startup:

```text
1. The bootloader loads the kernel + the init package.
2. The kernel initializes the CPU, memory, interrupts, the timer.
3. The kernel creates the first AddressSpace.
4. The kernel starts the first userspace process: SystemManager.
5. SystemManager starts ServiceManager.
6. ServiceManager starts DeviceManager, LogService, StorageService.
7. DeviceManager starts launching drivers.
8. StorageService makes the first volume available.
9. The CLI or GUI starts as an ordinary component.
```

#### Bootloader choice (decided)

**We do not write our own bootloader** - it is weeks of work with no added value. For the MVP a ready-made, modern bootloader is used:

- **Limine** as the primary choice (clean, built directly for new/hobby OSes, supports x86-64 and ARM64, passes the memory map, framebuffer, modules).
- **direct UEFI** as an alternative, if more control is needed.

Our own boot code is limited to the necessary *boot glue* (taking over control from the bootloader, transitioning into our own environment). The bootloader choice does not affect the kernel architecture - it is a replaceable entry gate.

#### The first practical goal

```text
kernel
SystemManager
LogService
StorageService over a ramdisk
CLI shell
```

---

### SystemManager

- the first userspace process,
- starting the basic system services,
- recovery on a crash of some critical parts,
- handing control to higher services.

#### Recovery on a SystemManager crash

If it crashes, the kernel should be able to perform minimal recovery behavior:

```text
1. start a recovery SystemManager,
2. start an emergency shell,
3. safely restart userspace,
4. reboot,
5. panic, if there is no other option.
```

This is the only exception where the kernel should have a minimal rescue mechanism beyond pure mechanism.

### ServiceManager

- starting services,
- stopping services,
- restart policy,
- dependency management,
- heartbeat/watchdog,
- tracking service state.

### DeviceManager

- device detection,
- mapping devices to drivers,
- assigning device capabilities to drivers,
- device state,
- reacting to a driver crash.

### PermissionManager

- the policy for assigning capabilities,
- later a detailed app sandbox.

The detailed security policy and its phasing (what holds from the MVP, what is deferred) is in the *Security model: current decisions* section.

### ResourceManager

- the policy for resource limits,
- quotas,
- possibly later CPU/GPU/network/storage budgets.

---

### Drivers

Drivers are outside the kernel as isolated services.

#### MVP: only virtio on QEMU/KVM

So the project does not freeze on drivers for real (and buggy) hardware, **the first target is exclusively virtio on QEMU/KVM.** Virtio is clean, well documented, and enough for a full-fledged system in a VM:

```text
driver.virtio-blk      # block storage
driver.virtio-net      # network
driver.virtio-console  # serial console / log
driver.virtio-gpu      # framebuffer / 2D, later acceleration
driver.virtio-input    # keyboard / mouse
```

Real HW (USB, NVMe, AHCI, GPU, Wi-Fi, audio) is added **gradually and as needed** - when someone wants to deploy it on a specific machine. We deliberately do not write our own GPU/Wi-Fi stack any time soon (it is the most common graveyard of new OSes).

#### Target drivers (later)

```text
driver.usb
driver.nvme
driver.gpu
driver.audio
driver.network
driver.fs.liberfs
driver.fs.fat
driver.fs.iso9660
driver.fs.udf
```

#### Driver crash

If, for example, `driver.usb` crashes:

```text
1. The driver faults.
2. The kernel stops the driver process.
3. The kernel removes its capabilities.
4. The kernel detaches its IRQ.
5. The kernel disables its DMA access.
6. The kernel frees its memory.
7. The kernel sends an event to ServiceManager.
8. DeviceManager marks the device as offline/restarting.
9. ServiceManager restarts the driver according to policy.
10. The driver re-initializes the device.
```

The kernel does not decide whether USB should be restarted. The kernel only safely cleans up the damage and sends an event.

The restart policy belongs in `ServiceManager` / `DeviceManager`.

---

### System Graph

The System Graph is an approved concept.

In line with the principle *the object is canonical* (the *System API model* section), the System Graph is **a graph of typed references to objects** - the nodes are Process / Service / Driver / Device / Volume, the edges are channels and dependencies. The visual tree is just one representation; the graph is equally queryable as JSON / CBOR / CLI.

#### MVP System Graph

Shows:

- what is running,
- which services exist,
- which drivers control which devices,
- which component has which capabilities,
- what the dependencies are,
- what has crashed,
- what has restarted.

#### Later extension: Flow Graph

Later consider a visualization of data flows:

```text
node = application/service/driver/device/volume
edge = communication or data flow
edge width = capacity
fill = current utilization
color/state = OK / warning / overload / error
```

Example:

```text
VideoPlayer -> VideoDecoder
  420 MB/s of 500 MB/s

VideoDecoder -> GPU
  480 MB/s of 500 MB/s

StorageService -> NVMe driver
  80 MB/s of 3500 MB/s
```

The goal is to see bottlenecks, queues, latency, and the load of the "pipes" between components.

The Flow Graph is deferred to a later phase. The System Graph as a basic overview should be there from the start.

---

## 4. Storage and data

### Storage model

The storage model is one of the fundamental differences from Linux. The main intent is simple: **every path belongs unambiguously to one volume and must never silently touch a different disk.**

#### Main principle

```text
A path always belongs to exactly one volume.
If the volume is not available, the operation fails (it does not write elsewhere).
The naming of data is separate from physical location.
```

#### Why not mount points and a global root tree

- **Design.** Mounting devices and filesystems into one shared tree is a long-outdated model - mixing physical devices and their filesystems into a common directory structure is a mess that a modern system should not create.
- **Security.** In a model where filesystems are "mounted" into a shared global tree (`/mnt/...`, `/media/...`):
  - After a volume is detached, an empty directory remains and a write silently ends up on a *different* (or the system) disk.
  - The same path can mean different devices over time, depending on what happens to be attached.
  - Mixing devices and filesystems into one tree blurs the line of "where the data physically is".

That is why the OS uses explicit volumes and volume-relative paths: storage identity is part of the path, so "accidentally on the wrong disk" is structurally impossible.

#### Separation of layers

We distinguish:

```text
Disk       = a physical device
Partition  = a region on a disk
Volume     = a filesystem / data space
Path       = a path inside a concrete volume
```

#### Storage/admin namespace

Administration of disks, partitions, and volumes:

```text
storage://disk/nvme0
storage://disk/nvme0/partition/1
storage://partition/gpt/<id>
storage://volume/<uuid>
```

`storage://` is not an ordinary path to user data. It is an administration namespace.

#### Data namespace

The canonical address of data is always tied to the **unambiguous identity of the volume (UUID)**, not to a human name:

```text
vol://<volume-uuid>/path/to/file
```

Example:

```text
vol://7a1f91c2-4d10-4a2a-a57e-f21c00112233/Documents/book.pdf
```

`vol://<uuid>` is the **escape / scripting form** - always unambiguous, never leading to the wrong disk. For everyday work an application does not work with the UUID directly, but with **the capabilities it was handed** (typically from the file picker), which resolve to a concrete volume (see below).

A petname (a human label) **never appears** in a canonical path - because it need not be unique, it has no `vol://`/URI form, and nothing can be addressed through it (see *Human-friendly naming*).

#### Human-friendly naming (identity layers)

A UUID is unambiguous, but as the primary UX it is unusable - nobody wants to type `vol://7a1f91c2-…/…`. Naming is therefore layered, and each layer handles just one thing:

| Layer | What it is | Property | Trust / use |
|---|---|---|---|
| **UUID** | a volume's persistent identity (in metadata) | globally unique, immutable | the source of truth for resolution, the only thing in `vol://` |
| **Petname** | a human label for a volume (`backup-ssd`) | **need not be unique**, display only | never addressed or resolved through, has no URI form |
| **Self-label** | a name written into the volume at format time (`Samsung-T7`) | just a hint | untrusted, informational only |

Key rules:

- **Resolution always goes via UUID/capability, never via a petname.** A petname is a purely display label - nothing can be opened or addressed through it.
- **A petname need not be unique - and that is intentional.** When a user attaches a USB disk with the same petname as another disk, nothing happens and the system does not "throw" anything to the console: a petname never determines where something is touched. No renaming, no conflict to resolve. Two disks with the same petname are simply distinguished in the UI by additional data (self-label, UUID prefix, capacity, connection).
- **A self-label is never trusted.** It is just an informational hint written into the volume; it has no effect on resolution.
- **An application does not see a global list of disks.** At startup it gets a per-process namespace (the Plan 9 / Fuchsia model): a mapping of logical names to *concrete* volume capabilities. What it did not get a capability for, it cannot even name. These names are unique within the namespace because its owner manages them. **Note: this restriction applies to applications, not to the user** - the user, through a trusted file manager / shell, sees and manages all disks (see *Ergonomics*, the user vs. application roles).
- **The UUID is normally not shown to the user.** In the CLI/admin, the petname + self-label + a short UUID prefix are shown for disambiguation, e.g. `backup-ssd (Samsung-T7, 7a1f…2233)`. The full UUID only in `storage://` and `vol://`.
- **`vol://<uuid>` is the only "escape" form of addressing by bare identity** - for scripts and recovery, not for everyday typing.

For user-driven access to files there is a **file picker / powerbox**: the user selects a file in a trusted system dialog and the application gets a capability (handle) to exactly that place - without naming the volume at all. (Details in the security model section.)

#### A path is an object, a URI is just a representation

Schemes like `vol://` or `storage://` look like URLs, but **the canonical form of a path is not a text string - it is a typed object.** A URI is just one of its representations, exactly in the spirit of the rule from the *System API model* section ("one typed API, several representations").

With a "path", three different things are usually merged into a single string, and they need to be distinguished:

| Layer | What it is | Form |
|---|---|---|
| **Authority** | what the resource is actually opened with | **a capability / handle** (an unforgeable reference to an object) |
| **Canonical value** | what is passed in the API | **a typed object** (a record/variant from IDL) |
| **Representation** | how the value is displayed/written | URI text, JSON, CBOR, binary |

Canonical types (proposal):

```text
VolumeId     = { uuid: Uuid }                              // unambiguous identity
Segment      = a non-empty name without "/", ".", and ".."
RelativePath = [Segment]                                   // a list of segments, not a string
VolumePath   = { volume: VolumeId, path: RelativePath }
DeviceRef    = variant { Disk(id) | Partition(id) | Volume(VolumeId) }  // storage://
```

The same value has several representations:

```text
object:  VolumePath { volume: { uuid: 7a1f… }, path: ["Documents", "book.pdf"] }
URI:     vol://7a1f…/Documents/book.pdf
JSON:    {"volume":{"uuid":"7a1f…"},"path":["Documents","book.pdf"]}
```

What this buys us:

- **Authority is not in the name.** A string (in any representation) opens nothing by itself; opening goes through a capability to a namespace that the process already holds. This eliminates *confused deputy* and "resolution against a global root".
- **Resistance to path traversal at the type level.** `RelativePath` is a list of validated segments, not a string - `..`/`/`-injection has nowhere to arise (the classic source of holes with string paths).
- **The URI remains a convenient text serialization** for the shell, config, and log. It is a full-fledged representation (it has the grammar `scheme://authority/path`), it is just not the *model* - the model is the object.

Rule: **the object is canonical; URI/JSON/CBOR/binary are its representations; authority is always in the capability.**

#### Logical namespaces

The following logical namespaces have been agreed:

```text
system://
vol://
storage://
```

Meaning:

| Namespace | Meaning |
|---|---|
| `system://` | system files / the OS base |
| `vol://` | an explicit volume by UUID |
| `storage://` | administration of disks/partitions/volumes |

Important: these namespaces are not mount points. They are logical resolvers over the storage and capability model.

#### Ergonomics: working across volumes and UX naming

Explicit volumes solve security (a wrong disk is never silently touched), but they must not turn ordinary operations into suffering. The target vision of ergonomics:

**Operations across volumes (move/copy from disk A to disk B).**
They are coordinated by **StorageService**, not by the application manually. The application holds a capability to the source and the target (typically from the file picker) and calls a single typed operation:

```text
StorageService.Transfer(src: FileCapability, dst: DirCapability, mode: Move | Copy)
```

- The service holds both volume capabilities and performs the transfer as **a single trackable operation** (progress, cancellation, resumption) - no "the application takes it over byte by byte".
- A move *within* one volume is an atomic rename; a move *between* volumes is copy + verify + delete, because they are physically two devices. This boundary is explicit, not hidden.
- The application never needs a global view of the disks - two concrete capabilities are enough for it.

**A unified "home" across multiple devices (a replacement for symlinks/overlay).**
Instead of silently merging disks into one tree (the classic overlay, where it is unknown where the data physically is), we solve "one logical home" through **explicit composition at the namespace level**:

```text
The home view is not one disk, but a typed view composed of explicitly added volumes.
Each item in it knows which volume it physically resides on.
The composition of the view is owned by the user/service, not by the chance of what is attached.
```

This preserves the convenience ("I have one Home"), but **the information about where the data really lives is never lost** - the opposite of the overlay/mount model.

**Backup and sync without a global view of the disks.**
A backup is not done by anyone who "sees all the disks" (that is exactly the ambient authority we are getting rid of). It is done by **a service with explicitly passed capabilities** to the source and target volumes:

```text
BackupService gets capabilities to the source volumes + the target volume.
It sees exactly what was passed to it - nothing more.
Snapshot/checksum/incremental sync are properties of the FS backend (see Native FS).
```

**The user's mental model of "where my files are".**

**The capability and namespace model:**

- restricts applications (each gets only what we pass it)
- does not restrict the user (who has full control)

**A "system application" is not a special privileged class.** A file manager, a shell, or a disk manager are not "a different kind" of software from third-party applications - they get capabilities through **exactly the same mechanism**. They differ only in *which capabilities they got*, not in *what they are*.

- no uid 0
- no "system" exception
- no ambient right tied to the binary's origin

- **Broad control over disks is held by the application to which the system/user granted it.** Typically a file manager or a shell - it gets broad storage capabilities (all disks, arbitrary volumes, browsing and creating structure), and through them the user has full control. A "trusted tool" here means exactly and only "it got a broad capability", not a built-in privileged status.
- **A third-party application can get the same capabilities too.** A user's own file manager gets *exactly the same* as the built-in one - the mechanism is one and the same. And vice versa: a built-in app with a narrow capability has no more rights than anyone else.
- **Most applications get only narrow capabilities.** They do not see a global list of disks; they get only what the user passed them (typically a single file/directory via the file picker). This is not a restriction of the user - it is protection of the user *from applications*.

How it holds together (no "root"):

- A privilege is **not a property of a process** ("I am a system application"), but **a property of the held capability**.
- A concrete tool holds broad access because it was **explicitly granted** to it (at install time or by the user in a session) - auditably and revocably, not as ambient authority.
- The user then *delegates narrow slices* of that broad authority onward (one file, one directory) via the picker.

**The home view is a default convenience, not a cage.**

- For the **everyday ordinary flow** (and for a non-expert), the home view + the file picker is a convenient default: the user does not have to deal with volumes or UUIDs, just "Documents", "Downloads".
- **But an advanced user is not locked into the home view** - via the file manager/shell they reach `storage://`, a concrete `vol://`, other disks, and compose their own structure. The home view is just one (default) view, not the boundary of what the user may do.
- When there are multiple devices with the same petname, the UI distinguishes them by additional data (self-label, capacity, connection, a short UUID prefix).

In summary: **the application is restricted, not the user.** The model stays strict toward applications (authority in the capability, identity in the UUID), but the user, through trusted tools, has full Windows-like control over their disks. Ergonomics for a non-expert come from the home view + the picker as a default, not as a cage. The detailed design of these operations belongs to a later phase - here the direction is fixed, not the API.

---

### Native filesystem

The storage model is decided, but the native filesystem is not yet designed in detail.

#### Supported compatible FS

- FAT12/16/32,
- exFAT,
- ISO9660,
- UDF.

These filesystems are backends behind the unified Volume API; further ones can be added behind the same API as the need arises.

#### Native FS - LiberFS

Possible features:

- copy-on-write,
- checksums,
- snapshots,
- encryption,
- compression,
- atomic writes,
- typed metadata,
- rollback.

For the MVP a simpler FS or a ramdisk/init package is enough.

---

## 5. Security and updates

### Security model: current decisions

Capabilities are a firm foundation.

But a strict application sandbox and detailed permission manifests are not mandatory for the first MVP.

#### For the MVP

The line is clear: **no ambient authority, starting from the MVP.** What is deferred is the *granularity* of policy and manifests - not isolation itself.

```text
HARD RULE (holds from the MVP):
a component/service gets ONLY explicitly passed capabilities, nothing more.
No global access to the FS, devices, or other services "by default".
```

- **We get this practically for free from the WASI/capability model** - a Wasm component has no ambient authority by design, so there is no reason to soften it in the MVP.
- The reason for strictness from the start: if code got used to ambient authority, a later sandbox retrofit is painful (that is exactly why Android/iOS do it right away). A capability model without an enforced "nothing extra" is largely just a different syntax.

```text
DEFERRED to later (not for the MVP):
- detailed granularity of permissions and permission manifests,
- fine-grained portals (mic/cam/screenshot), network policies,
- full audit and policy management.
```

#### Later

- a strict app sandbox,
- detailed permission manifests (a typed `PermissionSet` / `Manifest` object, not a text file - see *System API model*),
- a file picker returning a file handle,
- network permissions,
- mic/camera/screenshot portals,
- a detailed capability audit.

---

### Immutable system and update model

An immutable signed system, A/B updates, rollback, and verified boot are considered the right modern direction, but not a mandatory blocker for the MVP.

#### Deferred to a later phase / for consideration

- immutable `system://`,
- a signed system image,
- A/B updates,
- rollback,
- verified boot,
- a package trust chain,
- encrypted user volumes.

For the first version the system can be simpler.

---

## 6. Interfaces and application model

### Application model: native ABI + WebAssembly/WASI host

**The default and stable application contract is the native typed capability IPC/ABI** - the same ABI over which the kernel, drivers, and core services speak:

- We are building it anyway, so it is the **default for applications too**, not just for system parts.
- We do not build applications on native ELF + POSIX (deliberately not), but we also do not make an exception of them with their own independent contract.

**On top of this native ABI, the WebAssembly Component Model + WASI is the first and recommended application host:**

- We write applications preferentially as Wasm components - a bold but deliberate modern decision: from Wasm/WASI come sandboxing, portability, and language neutrality practically for free.
- The key point, though, is that in line with the *Layering principle*, **WASI is just one of several hosts on top of the stable native contract, not the *definition* of the system** - the system does not *rest* on it and does not depend on its maturity (elaborated below in *WASI as one of several hosts*).
- Why this way (and not "Wasm is the whole application model"): by making the native ABI the default for applications too, **the dependence on still-immature parts of WASI (GUI, async, threading) drops**. What WASI cannot yet do cleanly can in the meantime be handled directly through the native ABI, without waiting for a foreign spec to stabilize.

#### Why WASI/components

- **Capability-based by design.** WASI (preview 2) has no ambient authority - a component gets only the imports/capabilities we pass it. This maps 1:1 onto the kernel capability model.
- **Sandbox by default.** Wasm linear memory is isolated; the application is shielded even beyond process isolation (defense in depth).
- **Language neutrality.** Rust, C, C++, Go, and others compile to Wasm. The developer does not choose the language by the OS.
- **Portability.** A single binary artifact runs on x86-64, ARM64, and RISC-V. This significantly mitigates the "ecosystem from scratch" problem.
- **Unification with the IDL.** WIT (the component interface) can be directly our IDL (see the IDL section).

#### How it fits into the system

```text
Native (Rust) processes:  kernel, drivers, core services (Storage/Net/…).
WASM components:          APPLICATIONS (and gradually higher services too).
WASI host:                a runtime process that maps WASI imports
                          onto our typed service API over IPC channels.
```

- **A WASI "world" = a set of capabilities** that a component gets at startup (a filesystem handle from the file picker, a socket from NetworkService, …).
- **WASI imports are implemented by our services.** E.g. `wasi:filesystem` we call over a Channel to StorageService, `wasi:sockets` to NetworkService.
- **Performance:** components can be interpreted/JITed (Wasmtime/Cranelift) for portability, or **AOT-compiled at install time** for speed.

#### WASI as one of several hosts on top of a stable native ABI

This is a key architectural decision that addresses the main risk of betting on WASI (both the Component Model and WASI are young and evolving):

```text
Stable base = the native typed capability IPC/ABI (our own, from IDL).
WASI host   = a layer ON TOP of this base that maps wasi:* imports
              onto our service API. One of several possible hosts, not the only one.
```

Why exactly this way:

- **The system is not bound to a moving spec.** When WASI/the Component Model changes, the **WASI host adapter** is rewritten, not the kernel, the services, or their contracts. The risk is isolated to a single replaceable layer.
- **Multiple application models can coexist.** On top of the same native ABI, alongside the WASI host, a later native app ABI, a POSIX-like shim (see *Compatibility*), or another runtime can arise - without rewriting the system.
- **The advantages of WASI remain where they are useful** (sandboxing, portability, language neutrality for applications), but we do not pay for them by having the whole OS depend on a foreign, not-yet-settled standard.
- **It fits the *Layering principle*** (the Introduction and principles section): the dependency is on the contract, not on a concrete host implementation.

In other words: **the native ABI is "plan A" and "plan B" at the same time**:

- we are building it anyway (the kernel, drivers, and core services speak over it)
- the system does not *rest* on WASI
- the system rests on its own IPC (WASI is its first and recommended application consumer).

#### Honest trade-offs

- Wasm has overhead compared to purely native code (mitigable with AOT).
- The Component Model is young and evolving.
- Performance-extreme or low-latency tasks (GPU, drivers) stay native - Wasm is the layer of *applications*, not of the whole system.
- **The default contract is the native typed ABI** (we build it anyway for the kernel, drivers, and services) and it is available to applications too. **A Wasm component is the first and recommended path for applications** on top of this ABI; special or performance-sensitive applications can go directly through the native ABI.

---

### IDL language

Still to be designed.

The goal is a formal description of the system APIs.

Example:

```text
interface Storage.Volume {
  Open(path: RelativePath, rights: Rights) -> FileHandle
  Stat(path: RelativePath) -> FileInfo
  Watch(path: RelativePath) -> EventStream<FileEvent>
}
```

From the IDL the following should be generated:

- the binary IPC layout,
- a CBOR schema,
- a JSON schema,
- a Rust client,
- possibly a C ABI binding,
- a CLI formatter,
- documentation,
- compatibility tests.

This is crucial so that the API does not degenerate into chaos.

#### Relationship to WIT (WebAssembly Interface Types)

Because the application model builds on WebAssembly components (see *Application model*), a strong candidate is to **adopt WIT as the IDL** instead of inventing our own language - or to keep our own IDL and generate from/to WIT. Advantages of WIT:

- it already handles types, interfaces, worlds, and versioning,
- it has tooling (`wit-bindgen`) generating bindings into multiple languages,
- it naturally fits the capability model (imports = capabilities).

Our own binary IPC layout, the CBOR/JSON representations, and the CLI formatter can then be *backends* on top of the WIT description. The decision between "our own IDL" and "WIT as the IDL" is open, but the direction is to unify the IDL with WIT, not to maintain two parallel systems.

#### Decide after a real trial, not in advance

WIT was not designed as a low-level system IPC IDL - and things like passing kernel capabilities, zero-copy shared buffers, DMA handles, or async streams with backpressure may not map onto it cleanly (which is why Fuchsia deliberately built its own FIDL). Therefore **we do not make the definitive choice of "WIT vs. something else ready-made vs. our own IDL" now from the armchair.**

```text
PROCEDURE:
1. Write 5-6 REAL interfaces in WIT, not hello-world:
   Storage.Volume, Process, Log,
   Channel with handle passing,
   EventStream with backpressure (+ e.g. Transfer across volumes).
2. Find out where it chafes (handle passing, zero-copy, streams, ABI stability).
3. Only then decide, based on that experience.
```

A likely compromise (to be verified, not dogma): **WIT as the source of types and interfaces, our own binary layout + handle table as the backend.** But it will be confirmed by practice on real interfaces, not in advance.

**Decided (after the M25 trial):** writing these interfaces in WIT exposed five sticking points - capability/handle passing (WIT's `resource` own/borrow does not express a wire-level capability transfer with rights and badges, the reason Fuchsia built FIDL), zero-copy shared buffers (WIT copies `list<u8>`), streams with backpressure (WIT `stream` is the unstable async component-model proposal, ours are `wait`-drained bounded Channels), wire ABI stability (WIT's binary form is the WASM canonical ABI, not a stable Channel layout), and tooling (`wit-bindgen` targets WASM components, not our Channel IPC). So the system IDL is **our own**, with WIT-inspired types (records, enums, variants, results, lists, tuples, options) plus first-class `handle`, `buffer`, and `stream`, and our own binary backend and generators; WIT *types* may still be emitted as one backend for the Wasm component boundary. This confirms the likely compromise above from practice, not from the armchair.

---

### Compatibility and POSIX-like layer (deferred)

The system is primarily **our own** (a typed capability API + WASI). POSIX is **not** a goal of the core. Still, we do not throw compatibility away - we just handle it correctly and later.

#### Principle

POSIX-like compatibility is an **optional userspace layer**, not part of the kernel:

- A translation layer (libc + syscall emulation) that maps POSIX calls onto our native services.
- No POSIX primitive gets into the kernel.

#### Possible levels (from the simplest)

```text
1. WASI -> POSIX shim:        a POSIX-like API for Wasm components.
2. relibc-style libc:         a native libc on top of our services
                              (the Redox relibc model) for porting programs.
3. Linux-syscall emulation:   running unmodified Linux binaries
                              (the Fuchsia Starnix / WSL1 model) - the most demanding,
                              the latest phase.
```

#### Order: WASI first, POSIX-like later

First we properly build the native and WASI path. The compatibility layer comes once there is something and a reason to port - as a convenience for developers, not as a crutch that dilutes the native model.

> Note on wording: yes, it is "our own compatibility layer" - specifically a **userspace translation layer** that converts the POSIX/Linux interface onto our services. The point is not to turn the OS into Linux, but to be able to *run* existing software on it when that makes sense.

---

## 7. Roadmap and conclusion

### MVP design

The first practical version of the OS should be able to:

```text
boot in QEMU
serial log
framebuffer text output
physical memory manager
virtual memory
heap allocator
userspace address space
thread
scheduler
channel IPC
handle table
basic capabilities
start SystemManager
send the first message over IPC
catch a page fault of a userspace process
clean up a crashed process
ramdisk/init package
StorageService over a ramdisk
vol:// access
a simple CLI
a basic System Graph
```

**Drivers in the MVP:** virtio only (see the Drivers section). **The application ABI is decided** - WebAssembly components + WASI (see *Application model*); once the core IPC and services are running, a near-term goal is a **minimal WASI host that runs the first component**. The MVP itself, however, rests on native Rust services; the Wasm host comes right in the following phase (see *Roadmap*).

Deliberately not addressed in the MVP:

```text
GPU acceleration
USB
networking
NVMe
a full filesystem
GUI
package manager
a strict app sandbox
immutable update
verified boot
Flow Graph metrics
```

---

### Roadmap

The roadmap is milestone-based, not time-based (deliberately without dates):

- We manage scope through phases, and each phase should be a *usable* intermediate state.
- The order of the phases follows the deployment targets appliance/edge -> server -> desktop (see *Why this OS instead of Linux*).
- Deployment on real hardware comes after the server phase; the AI platform as the final evolution on top of the desktop.

**How to read the phase horizon.** Phases 0-2 target appliance/edge and represent a **real, near-term goal** for one person or a small team (a bootable capability microkernel + the first WASI component + virtio + networking). They should be understood as a *complete* project, not as a stepping stone to something bigger - even the appliance/edge platform alone is a finished, meaningful product.

**Phases 3-6 are not a plan but a vision - and they hold only on the assumption that a community forms around the project.** Phase 3 (server), Phase 4 (real hardware), Phase 5 (a full-fledged desktop), and Phase 6 (the AI platform) represent hundreds of person-years. They are therefore deliberately phrased as a *direction* in which the system **can** grow thanks to its architecture as more contributors arrive.

**What will attract a community and what will not**:
- NO - modernity and the absence of legacy
- YES - capability-based security built into the foundation of the system (no ambient authority, no "root can do anything") - a structural guarantee that, due to 30 years of backward compatibility, can be added to Linux only with great difficulty. The other pillars (the WASI application model, memory safety) support it, but it is precisely this single property that is the reason anyone would join the project at all and start building an ecosystem.

#### Phase 0 - Bring-up (kernel MVP)

```text
boot in QEMU (Limine), serial log, framebuffer text
physical/virtual memory, heap, address spaces
thread, scheduler (SMP-aware design, running on a single core for now), Channel IPC, handle table, capabilities, Domain
start SystemManager, the first IPC message
catching a page fault, cleanup of a crashed process
ramdisk/init package, StorageService over a ramdisk, vol:// access
a simple CLI, a basic System Graph
```

#### Phase 1 - First usable userspace

```text
IDL/WIT toolchain and generators
core services: Process, Storage, Log, Device, Config
virtio drivers (headless): blk, net, console
minimal WASI host: running the first Wasm component
a prototype file picker (powerbox)
```

#### Phase 2 - Appliance/edge platform

```text
a network stack over virtio-net (a priority - on the edge, networking is the core)
an interactive console: keyboard input + a userspace line editor (command history, cursor movement, in-line editing, ANSI key sequences for arrows) - the kernel console stays a dumb byte sink, the line editor lives in the shell
simple pointer/mouse plumbing over virtio-input (text-cell pointer + button events for TUI apps such as a file manager); no mouse stack or touch yet (those are the desktop phase)
observability: full System Graph, JSON/CBOR/CLI representations, tracing, counters (the JSON/CBOR forms are network-friendly; exposing and administering it over the network is phase 3)
security hardening: app sandbox, permission manifests, threat model
ServiceManager with restart policy and watchdog
full Component Model + WASI preview 2, an SDK for Rust/C/Go
package/app format, installation, AOT compilation
a simple persistent native filesystem
```

#### Phase 3 - Server platform

```text
user accounts / identities (multi-user management, remote access) - userspace identity over capabilities, not kernel uid/gid
remote admin: the System Graph / logs / counters exposed and administered over the network, authenticated against the identity model (phase 2 keeps observability local + network-friendly representations)
localization (locale, language, time zone, formatting) - relevant already in the CLI and in logs
a wider network stack and server-class workloads
immutable signed system, A/B updates, rollback, verified boot
encrypted user volumes
a native modern FS (CoW, checksums, snapshots, compression)
first first-party services: a simple static-file web server and similar small services - dogfooding the network stack + storage + service model on our own / WASI layer (no POSIX needed)
a minimal headless AudioService over virtio-sound (playback, optionally capture) - so audio works from the console too (e.g. for a headless voice assistant); the full desktop audio stack stays Phase 5
a CLI package manager (search / install / update / remove of first-party packages, built on the Phase 2 package/app format) - the end-user app store stays Phase 5
```

#### Phase 4 - Real hardware and foreign-software compatibility

```text
a POSIX-like compatibility layer (relibc-style) - for foreign server software
the driver binding model in practice: DeviceManager pairs real devices -> drivers
selective real-HW drivers per deployment (NVMe, NIC, storage, buses)
support for specific servers and SBCs (single-board computers)
ARM64 / RISC-V boards alongside x86-64
power management per deployment (ACPI, idle/suspend)
the transition from virtio/VM to bare metal
```

#### Phase 5 - Desktop platform

```text
GUI/compositor (virtio-gpu and real GPUs), input: keyboard/mouse/touch
a window manager, a desktop shell, and a complete user environment
the full audio stack (mixing, per-app routing, recording, real-HW drivers) - building on the headless Phase 3 AudioService
portals: mic/cam/screenshot, screen sharing, file selection
a package manager / app store for end users (GUI) - on top of the Phase 3 CLI package manager
user profiles and desktop settings, accessibility (screen readers, etc.)
notifications, clipboard, drag-and-drop, multi-monitor support
accelerated graphics and multimedia
optional Linux binary emulation (Starnix-style) - running existing applications
Flow Graph metrics
```

**Only in this phase does "friendliness for ordinary users" become a real goal.** It is the culmination of the developer-first -> ecosystem -> broad friendliness trajectory (see *Why this OS instead of Linux*): the ordinary user arrives *only* at a mature desktop with an ecosystem and applications, not at a bare kernel. Until then, the ordinary user is not the target group by which early design decisions are made.

#### Phase 6 - AI platform

An alternative to the classic desktop:

- the primary interface is not direct control of applications, but an AI that carries out the user's intents on their behalf.
- it builds on the desktop (Phase 5), because it is a **virtual agent (a 3D avatar)** - a visual and voice agent that displays alongside itself content relevant to the conversation (text, video, audio, images).
- it needs the complete graphics, audio, and multimedia foundation from the desktop phase
- the capability model and the typed API (*the object is canonical*) make the system a safe, machine-controllable substrate for such an agent
- beyond the local system, the agent also connects to external tools and services via a standard protocol (**MCP - Model Context Protocol**), but each such connector is just another capability-restricted component, so connecting to the outside does not extend its permissions beyond what the user granted.

```text
an embodied virtual agent (a 3D avatar) + voice and text input/output as the primary interface
presentation of found content alongside the agent: text, video, audio, images (multimedia from Phase 5)
the AI interface as the primary mode of interaction - the user formulates an intent, does not control applications directly
the AI agent carries out the user's requests through the typed system API and applications
a capability-restricted agent: it acts only within the granted permissions (auditably, revocably)
orchestration of applications and services by the AI layer over the typed object API
connection to external tools, data, and services via MCP (Model Context Protocol) - a unified protocol through which the agent calls remote tools and APIs
each MCP connector runs as a separate capability-restricted component (sandbox, auditably, revocably)
portals and confirmation of sensitive actions - the AI must not exceed the granted capabilities
auditing of the AI's actions via the System Graph and the capability model
the classic desktop remains available as an alternative interface
```

---

### License

The project is **open source under the Unlicense** (release into the public domain).

- **Maximum freedom:** anyone may use, modify, distribute, commercialize, and even close a derivative work, without conditions and without having to attribute authorship.
- **No copyleft, no attribution** - deliberately the lowest possible barrier to adoption and forking.
- **Contributions** are accepted under the Unlicense; it is advisable to add a DCO/note that the contributor agrees to it.
- **Third parties:** our own code is Unlicense, but adopted components carry their own (permissive) licenses - e.g. Wasmtime (Apache-2.0), Limine (BSD). That is fine; they just need to be tracked.

---

### Open questions

Some of the original questions are now decided (see above).

```text
DECIDED:
- sync vs async IPC ....... async core + sync-looking API (IPC section)
- bootloader .............. Limine (or UEFI) (Boot flow section)
- application model ....... the native typed ABI is the default for applications too,
                           WASI is the first and recommended host on top of it (Application model section)
- IPC wire format ......... default decode-cheap binary (FIDL/Cap'n Proto style),
                           not CBOR/JSON (IPC model section)
- layering principle ...... replaceable layers via stable contracts (Introduction and principles section)
- capability model ........ detailed design (Capability model section)
- kernel object model ..... + Domain hierarchy (Kernel object model section)
- SMP / multicore ......... SMP-aware design from Phase 0, optimization later (Kernel components section)
- paths/naming ............ the object is canonical, the URI is a representation, the petname is just a label (Storage model section)
- object = canonical (everywhere) .. text/URI/JSON are representations, authority in the capability (System API model section)
- ambient authority ....... none from the MVP on, only granularity is deferred (Security model section)
- license ................. Unlicense (License section)
```

Still open:

1. The exact binary IPC/message layout (the *style* is decided - decode-cheap FIDL/Cap'n Proto; only the concrete bytes are open).
2. The event stream model in detail.
3. Distinguishing Process vs Component vs Service vs Driver vs App in practice.
4. ResourceManager policy (limits, quotas).
5. Native filesystem (format, features).
6. GUI/compositor/input model.
7. Audio/video/network stack in detail.
8. The exact form of the System Graph and later Flow metrics.
9. Verified boot / update model (immutable, A/B, rollback).
10. Power management (ACPI, suspend/resume, idle states) - necessary for laptops.
11. Testing strategy (unit, integration on QEMU, syscall fuzzing, property tests of capabilities).
12. An explicit threat model (whom we defend against: a malicious app, a compromised driver, …).
13. Observability (counters, tracing spans, profiling across services).
14. Behavior under memory pressure (reclaim, OOM via Domain limits).

Note: items 10-14 do not need a detailed design yet - they are here deliberately as a "do not forget", not as a task for the MVP. Most of them will be decided by practice in Phase 0-1.

---

### Recommended next step

Phases 0 and 1 are complete (the kernel MVP and the first usable userspace - see the *Roadmap*). What is built and running:

```text
1. Kernel: SMP-aware, preemptive, with a blocking `wait`; objects, capabilities, Domains, Channel IPC, fault isolation + crashed-process cleanup.
2. Boot chain: SystemManager -> ServiceManager -> DeviceManager + the core services, with recovery.
3. The IDL toolchain (our own, LSIDL) and its generators: binary codec, Rust client/server, JSON + CLI renderers, docs, compatibility tests.
4. Core services over generated bindings: Log, Storage (over a real virtio-blk device), Process, Device, Config.
5. virtio drivers (blk, net, console) isolated under DeviceManager; a minimal WASI host running the first Wasm component; a powerbox file picker handing out a single file capability.
```

The recommended next step is therefore **Phase 2 (the appliance/edge platform)**. Its priority is a network stack over virtio-net - on the edge, networking is the core - followed by the rest of the phase (full System Graph + observability, security hardening + PermissionManager, the ResourceManager policy, ServiceManager restart/watchdog, the full Component Model + WASI preview 2 + an SDK, a package/AOT path, and a simple persistent native filesystem); see the *Roadmap*.
