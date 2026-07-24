# TODO

## Phase 0

Phase 0 establishes a testable, SMP-aware capability microkernel MVP: memory,
interrupts, scheduling, objects and capabilities, syscall and Channel IPC,
resource accounting, isolated process lifecycle, minimal userspace, ramdisk
storage, a CLI and framebuffer console. Every milestone has a QEMU-tested gate.

- [x] [M0 - Skeleton (bring-up)](M0000.md)
- [x] [M1 - Memory foundation](M0001.md)
- [x] [M2 - Time and interrupts](M0002.md)
- [x] [M3 - SMP](M0003.md)
- [x] [M4 - Object and capability core](M0004.md)
- [x] [M5 - Threads, address spaces, scheduler](M0005.md)
- [x] [M6 - Syscall ABI](M0006.md)
- [x] [M7 - IPC](M0007.md)
- [x] [M8 - Userspace (ring 3)](M0008.md)
- [x] [M9 - Resource accounting](M0009.md)
- [x] [M10 - IPC latency benchmark (phase 0 gate)](M0010.md)
- [x] [M11 - Process and per-process address space](M0011.md)
- [x] [M12 - Fault isolation and crashed-process cleanup](M0012.md)
- [x] [M13 - Domain hierarchy and lifecycle](M0013.md)
- [x] [M14 - Init package and the first userspace process](M0014.md)
- [x] [M15 - Framebuffer text console](M0015.md)
- [x] [M16 - StorageService over a ramdisk + vol:// access](M0016.md)
- [x] [M17 - Simple CLI + basic System Graph](M0017.md)

### Definition of done (phase 0)

Done when the SMP kernel provides the core object, capability, scheduling, IPC,
accounting and isolation paths; boots SystemManager and ramdisk-backed storage;
and exposes the CLI, basic System Graph and framebuffer console under QEMU tests.

### Out of scope for phase 0 (against scope creep)

Deferred are full services and drivers, IDL-generated APIs, Wasm, persistent
storage, networking, security hardening, other architectures and real hardware.
Blocking wait and preemption arrive in M18-M19; phase 0 uses cooperative polling
and scheduling.

## Phase 1 - First usable userspace

Phase 1 builds the first usable userspace: blocking wait and preemption, isolated
virtio drivers, dependency-ordered core services, generated typed APIs, a minimal
WASI component host and a powerbox file picker. Early services provide real
protocols for the M25 IDL trial; later services adopt the generated bindings.
Authority remains capability-first and explicitly delegated.

- [x] [M18 - Blocking `wait` primitive](M0018.md)
- [x] [M19 - Preemptive scheduling](M0019.md)
- [x] [M20 - Kernel additions: driver + spawn syscalls, queue/DMA accounting](M0020.md)
- [x] [M21 - ServiceManager and the boot chain](M0021.md)
- [x] [M22 - LogService (structured logging)](M0022.md)
- [x] [M23 - DeviceManager + virtio transport](M0023.md)
- [x] [M24 - virtio drivers (headless): blk, net, console](M0024.md)
- [x] [M25 - IDL/WIT toolchain and generators](M0025.md)
- [x] [M26 - StorageService over virtio-blk](M0026.md)
- [x] [M27 - Core services: Process, Device, Config](M0027.md)
- [x] [M28 - Minimal WASI host: the first Wasm component](M0028.md)
- [x] [M29 - Prototype file picker (powerbox)](M0029.md)

### Definition of done (phase 1)

Done when the preemptive kernel and blocking wait support isolated virtio drivers,
the supervised core services answer generated typed APIs, and a capability-scoped
Wasm component receives a file handle through the powerbox under QEMU tests.

### Out of scope for phase 1 (= phase 2, the appliance/edge platform)

Deferred are networking, wall-clock time, rich observability, policy services,
restart supervision, full Component Model support, persistent storage and local
graphics/input. Server compatibility, package installation, real hardware and
desktop work remain beyond phase 2.

## Phase 2 - Appliance/edge platform

Phase 2 turns the VM into an appliance/edge platform, beginning with shared event
streams and a network stack over virtio-net, then adding local I/O, policy,
observability, persistent storage and multi-architecture support.

- [x] [M30 - Event streams in the IDL toolchain (the M25-deferred `stream<T>`)](M0030.md)
- [x] [M31 - Interrupt-driven I/O + virtio-input keyboard (the driver-framework gap for RX devices)](M0031.md)
- [x] [M32 - virtio-net receive path + the link/IP layer (Ethernet, ARP, IPv4, ICMP)](M0032.md)
- [x] [M33 - NetworkService, UDP/TCP, and the net tools as standalone programs](M0033.md)
- [x] [M34 - TimeService: wall-clock time (RTC + NTP)](M0034.md)
- [ ] [M35 - Interactive console: line editor, history, and cursor](M0035.md)
- [x] [M36 - Pointer/mouse plumbing (virtio-input pointer + InputService)](M0036.md)
- [x] [M37 - Observability (full System Graph, tracing, counters, CBOR)](M0037.md)
- [x] [M38 - Security hardening: app sandbox, permission manifests, PermissionManager](M0038.md)
- [x] [M39 - ResourceManager policy service](M0039.md)
- [x] [M40 - ServiceManager: restart policy and watchdog](M0040.md)
- [x] [M41 - Full Component Model + WASI preview 2 + an SDK](M0041.md)
- [x] [M43 - A simple persistent native filesystem](M0043.md)
- [x] [M44 - virtio-gpu driver + runtime mode-set (the resize source for the local console)](M0044.md)
- [x] [M45 - AudioService over virtio-sound (headless playback + capture)](M0045.md)
- [x] [M46 - MSI-X interrupt routing (per-device vectors)](M0046.md)
- [x] [M47 - Layered console: stream -> grid model -> renderer -> swappable display](M0047.md)
- [x] [M48 - FAT / exFAT filesystem backend (read foreign removable media)](M0048.md)
- [x] [M49 - LiberFS: directories and capacity scaling](M0049.md)
- [x] [M50 - LiberFS: write semantics, metadata, and integrity](M0050.md)
- [x] [M51 - LiberFS: block checksums (integrity)](M0051.md)
- [x] [M52 - LiberFS: copy-on-write (toward the modern FS)](M0052.md)

### LiberFS modernization track (M53-M57)

M53-M57 remove the early LiberFS capacity limits with 64-bit addressing, extents,
sparse files, B+tree directories, dynamic inodes and snapshots while preserving
CoW and checksums. Compression is the final optional layer; authorization remains
capability-owned, and deduplication and encryption stay outside the filesystem.

- [x] [M53 - LiberFS: 64-bit addressing, large files/volumes, and long names](M0053.md)
- [x] [M54 - LiberFS: extents and sparse files](M0054.md)
- [x] [M55 - LiberFS: B+tree directories and dynamic inode allocation](M0055.md)
- [x] [M56 - LiberFS: snapshots](M0056.md)
- [x] [M57 - LiberFS: transparent compression (optional, last)](M0057.md)

### Foreign-FS interop track (M58-M60)

M58-M60 add ISO9660, exFAT write support and UDF behind the shared
`Storage.Volume` and `BlockDevice` interfaces. The no_std backends are checked
with image fixtures and live QEMU reads.

- [x] [M58 - ISO9660 filesystem backend (read-only)](M0058.md)
- [x] [M59 - exFAT write support (large removable media)](M0059.md)
- [x] [M60 - UDF filesystem backend (read-only, DVD / Blu-ray)](M0060.md)
- [x] [M61 - Thin shell: job-control / session service + commands as binaries](M0061.md)
- [x] [M62 - USB stack (xHCI + HID + mass storage)](M0062.md)
- [x] [M63 - Hardware inventory commands (lsblk / lspci / lscpu / ...)](M0063.md)
- [x] [M64 - Capacity quick wins (the limits-audit small fixes)](M0064.md)
- [x] [M65 - LiberFS sized by the disk (drop the fixed 32 MB pool)](M0065.md)
- [x] [M66 - Kernel bounds from the runtime (retire the last magic numbers)](M0066.md)
- [x] [M67 - Runtime-tunable policies (constants become ConfigService keys)](M0067.md)
- [x] [M68 - GPU framebuffer realloc on resize (no resolution ceiling)](M0068.md)
- [x] [M69 - Demand-paged user stacks](M0069.md)
- [x] [M70 - Journal persistence (LogService on the volume)](M0070.md)
- [x] [M71 - Streaming replies (retire the 4096 B wire ceiling)](M0071.md)
- [x] [M72 - Contiguous DMA and full-size I/O (queues, sectors, jumbo)](M0072.md)

### LiberFS audit track (M73-M85)

M73-M85 harden LiberFS after a full implementation audit: durability and
data-loss fixes, scalable allocation, format cleanup, hostile-media bounds and
fsck recovery. Pre-release format changes remain allowed; backward compatibility
is not yet a constraint.

- [x] [M73 - LiberFS: correctness bugs and data-loss holes](M0073.md)
- [x] [M74 - LiberFS: allocator and free-map scaling](M0074.md)
- [x] [M75 - LiberFS: format and modernity](M0075.md)
- [x] [M76 - LiberFS: code quality](M0076.md)
- [x] [M77 - LiberFS: post-audit sweep (the re-review's findings)](M0077.md)
- [x] [M78 - LiberFS: OS- and architecture-agnostic (portability hardening)](M0078.md)
- [x] [M79 - LiberFS: third-review fixes (GPT robustness, snap_open cost)](M0079.md)
- [x] [M80 - LiberFS: hostile-disk robustness (sanity bounds on all on-disk values)](M0080.md)
- [x] [M81 - LiberFS: hostile-disk robustness, second sweep (the consumers M80 missed)](M0081.md)
- [x] [M82 - LiberFS: fsck must survive what M81 taught the mount to survive](M0082.md)
- [x] [M83 - LiberFS/storage: the seed-archive loop and snapshot-name encoding](M0083.md)
- [x] [M84 - LiberFS: dangling directory entries (report them, list around them, remove them)](M0084.md)
- [x] [M85 - LiberFS: last nits (NUL in snapshot names, failure-count overflow)](M0085.md)

### FAT audit track (M86-M95)

M86-M95 apply the same audit discipline to FAT/exFAT: repair write-side data-loss
paths, reject hostile media without panic or hangs, bound allocation and enforce
the relevant on-disk specifications.

- [x] [M86 - FAT: data-loss and correctness bugs](M0086.md)
- [x] [M87 - FAT: hostile-media robustness (no panic, no hang on any boot sector or chain)](M0087.md)
- [x] [M88 - FAT: allocation bounds and spec conformance](M0088.md)
- [x] [M89 - FAT: second-pass findings (the sector-size read bug and the leftovers)](M0089.md)
- [x] [M90 - FAT: third-pass findings (the cluster range gate and the leftovers)](M0090.md)
- [x] [M91 - FAT: fourth-pass findings (read-side name integrity and the last layout gates)](M0091.md)
- [x] [M92 - FAT: fifth-pass findings (write-side interop and the last mount nits)](M0092.md)
- [x] [M93 - FAT: sixth-pass findings (forged-geometry robustness and dirty-range writes)](M0093.md)
- [x] [M94 - FAT: seventh-pass findings (the ValidDataLength read rule)](M0094.md)
- [x] [M95 - FAT: ninth-pass findings (FAT mirroring flags and overwrite fidelity)](M0095.md)
- [x] [M96 - ISO9660: first-pass audit (hostile-media panics and unbounded reads)](M0096.md)
- [x] [M97 - ISO9660: second-pass findings (the rare-legal shapes that misread)](M0097.md)
- [x] [M98 - ISO9660: third-pass findings (the root XAR and associated files)](M0098.md)
- [x] [M99 - UDF: first-pass audit (no hostile-media bounds at all)](M0099.md)
- [x] [M100 - UDF: second-pass findings (the shared-buffer corruption)](M0100.md)
- [x] [M101 - UDF: third-pass findings (the last unrefused forms)](M0101.md)
- [x] [M102 - LiberFS: revisit under the fs-track discipline](M0102.md)
- [x] [M103 - LiberFS: second-pass findings (the raw-length gate gap)](M0103.md)
- [x] [M104 - Architecture sweep: memory protection, the display path, and the plumbing debt](M0104.md)
- [x] [M105 - Kernel wake path: per-object wait queues, cross-core wake IPIs, and the serial RX interrupt](M0105.md)
- [x] [M106 - Ring-3 preemption: per-thread kernel entry stacks](M0106.md)
- [x] [M107 - Kernel hardening: SMAP/SMEP, frame allocator bounds, and channel backpressure](M0107.md)
- [x] [M108 - ABI versioning and boot image hygiene](M0108.md)
- [x] [M109 - Service lifecycle: a data-driven manifest, a typed grant sender, and honest failure reports](M0109.md)
- [x] [M110 - Runtime elasticity: NetworkService capacity and device hotplug](M0110.md)
- [x] [M111 - Filesystem stack unification and performance](M0111.md)
- [x] [M112 - Shell, tools, and renderer cleanup](M0112.md)
- [x] [M113 - Wasm: indirect calls and tables](M0113.md)
- [x] [M114 - Own UEFI-only bootloader (x86_64 / aarch64 / riscv64)](M0114.md)

### Multi-architecture port track (M115-M117)

M115-M117 extract the architecture boundary, then port the kernel, drivers and
UEFI loader to aarch64 and riscv64. All three architectures must pass under QEMU;
board-specific real-hardware support remains later work.

- [x] [M115 - Architecture abstraction layer (the HAL boundary)](M0115.md)
- [x] [M116 - aarch64 (ARM64) kernel + loader port](M0116.md)
- [x] [M117 - riscv64 (RISC-V) kernel + loader port](M0117.md)
- [x] [M118 - Multi-arch track follow-ups (the M115-M117 loose ends)](M0118.md)
- [x] [M119 - Pre-phase-3 hardening (finish before the server platform)](M0119.md)
- [ ] [M120 - LSIDL package imports, modular generation, and language hardening](M0120.md)
- [x] [M121 - Application graphics, raw input, and PCM audio (the app-platform layer)](M0121.md)
- [x] [M122 - Image viewer (the first graphical application)](M0122.md)
- [x] [M123 - Shared system libraries (dynamic linking)](M0123.md)
- [ ] [M124 - Audio player (streaming decoders over AudioService)](M0124.md)
- [x] [M125 - Native executable artifacts (`.lsexe`)](M0125.md)
- [x] [M126 - Image conversion tool (`imgconv`)](M0126.md)
- [ ] [M127 - Userspace source and system-volume layout cleanup](M0127.md)
- [ ] [M128 - Declarative driver binding and lifecycle core](M0128.md)
- [ ] [M129 - Universal standards-based driver set](M0129.md)
- [ ] [M130 - LiberCommander (`lico`, `licoedit`, `licoview`)](M0130.md)
- [ ] [M131 - Additional system utilities](M0131.md)
- [ ] [M132 - Capability-native pipes and redirection](M0132.md)
- [ ] [M133 - Future-ready 3D graphics foundation + software-rendered scene](M0133.md)

### Definition of done (phase 2)

Done when the capability-scoped appliance provides networking, wall-clock time,
interactive console and audio, policy and observability services, supervised
components, writable persistent storage and the complete filesystem set. The
kernel and UEFI loader must pass the QEMU suite on x86_64, aarch64 and riscv64.

### Out of scope for phase 2 (= phase 3, the server platform)

Deferred are POSIX compatibility, identities and remote administration,
localization, wider networking and server workloads, multi-queue devices, signed
A/B updates, encrypted volumes, advanced multi-device LiberFS, package management
and AOT compilation. Real hardware and the desktop stack remain later phases.
