# TODO

## Phase 0

Phase 0 establishes a testable, SMP-aware capability microkernel MVP: memory,
interrupts, scheduling, objects and capabilities, syscall and Channel IPC,
resource accounting, isolated process lifecycle, minimal userspace, ramdisk
storage, a CLI and framebuffer console. Every milestone has a QEMU-tested gate.

- [x] [M0000 - Skeleton (bring-up)](M0000.md)
- [x] [M0001 - Memory foundation](M0001.md)
- [x] [M0002 - Time and interrupts](M0002.md)
- [x] [M0003 - SMP](M0003.md)
- [x] [M0004 - Object and capability core](M0004.md)
- [x] [M0005 - Threads, address spaces, scheduler](M0005.md)
- [x] [M0006 - Syscall ABI](M0006.md)
- [x] [M0007 - IPC](M0007.md)
- [x] [M0008 - Userspace (ring 3)](M0008.md)
- [x] [M0009 - Resource accounting](M0009.md)
- [x] [M0010 - IPC latency benchmark (phase 0 gate)](M0010.md)
- [x] [M0011 - Process and per-process address space](M0011.md)
- [x] [M0012 - Fault isolation and crashed-process cleanup](M0012.md)
- [x] [M0013 - Domain hierarchy and lifecycle](M0013.md)
- [x] [M0014 - Init package and the first userspace process](M0014.md)
- [x] [M0015 - Framebuffer text console](M0015.md)
- [x] [M0016 - StorageService over a ramdisk + vol:// access](M0016.md)
- [x] [M0017 - Simple CLI + basic System Graph](M0017.md)

### Definition of done (phase 0)

Done when the SMP kernel provides the core object, capability, scheduling, IPC,
accounting and isolation paths; boots SystemManager and ramdisk-backed storage;
and exposes the CLI, basic System Graph and framebuffer console under QEMU tests.

### Out of scope for phase 0 (against scope creep)

Deferred are full services and drivers, IDL-generated APIs, Wasm, persistent
storage, networking, security hardening, other architectures and real hardware.
Blocking wait and preemption arrive in M0018-M0019; phase 0 uses cooperative polling
and scheduling.

## Phase 1 - First usable userspace

Phase 1 builds the first usable userspace: blocking wait and preemption, isolated
virtio drivers, dependency-ordered core services, generated typed APIs, a minimal
WASI component host and a powerbox file picker. Early services provide real
protocols for the M0025 IDL trial; later services adopt the generated bindings.
Authority remains capability-first and explicitly delegated.

- [x] [M0018 - Blocking `wait` primitive](M0018.md)
- [x] [M0019 - Preemptive scheduling](M0019.md)
- [x] [M0020 - Kernel additions: driver + spawn syscalls, queue/DMA accounting](M0020.md)
- [x] [M0021 - ServiceManager and the boot chain](M0021.md)
- [x] [M0022 - LogService (structured logging)](M0022.md)
- [x] [M0023 - DeviceManager + virtio transport](M0023.md)
- [x] [M0024 - virtio drivers (headless): blk, net, console](M0024.md)
- [x] [M0025 - IDL/WIT toolchain and generators](M0025.md)
- [x] [M0026 - StorageService over virtio-blk](M0026.md)
- [x] [M0027 - Core services: Process, Device, Config](M0027.md)
- [x] [M0028 - Minimal WASI host: the first Wasm component](M0028.md)
- [x] [M0029 - Prototype file picker (powerbox)](M0029.md)

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

- [x] [M0030 - Event streams in the IDL toolchain (the M0025-deferred `stream<T>`)](M0030.md)
- [x] [M0031 - Interrupt-driven I/O + virtio-input keyboard (the driver-framework gap for RX devices)](M0031.md)
- [x] [M0032 - virtio-net receive path + the link/IP layer (Ethernet, ARP, IPv4, ICMP)](M0032.md)
- [x] [M0033 - NetworkService, UDP/TCP, and the net tools as standalone programs](M0033.md)
- [x] [M0034 - TimeService: wall-clock time (RTC + NTP)](M0034.md)
- [ ] [M0035 - Interactive console: line editor, history, and cursor](M0035.md)
- [x] [M0036 - Pointer/mouse plumbing (virtio-input pointer + InputService)](M0036.md)
- [x] [M0037 - Observability (full System Graph, tracing, counters, CBOR)](M0037.md)
- [x] [M0038 - Security hardening: app sandbox, permission manifests, PermissionManager](M0038.md)
- [x] [M0039 - ResourceManager policy service](M0039.md)
- [x] [M0040 - ServiceManager: restart policy and watchdog](M0040.md)
- [x] [M0041 - Full Component Model + WASI preview 2 + an SDK](M0041.md)
- [x] [M0043 - A simple persistent native filesystem](M0043.md)
- [x] [M0044 - virtio-gpu driver + runtime mode-set (the resize source for the local console)](M0044.md)
- [x] [M0045 - AudioService over virtio-sound (headless playback + capture)](M0045.md)
- [x] [M0046 - MSI-X interrupt routing (per-device vectors)](M0046.md)
- [x] [M0047 - Layered console: stream -> grid model -> renderer -> swappable display](M0047.md)
- [x] [M0048 - FAT / exFAT filesystem backend (read foreign removable media)](M0048.md)
- [x] [M0049 - LiberFS: directories and capacity scaling](M0049.md)
- [x] [M0050 - LiberFS: write semantics, metadata, and integrity](M0050.md)
- [x] [M0051 - LiberFS: block checksums (integrity)](M0051.md)
- [x] [M0052 - LiberFS: copy-on-write (toward the modern FS)](M0052.md)

### LiberFS modernization track (M0053-M0057)

M0053-M0057 remove the early LiberFS capacity limits with 64-bit addressing, extents,
sparse files, B+tree directories, dynamic inodes and snapshots while preserving
CoW and checksums. Compression is the final optional layer; authorization remains
capability-owned, and deduplication and encryption stay outside the filesystem.

- [x] [M0053 - LiberFS: 64-bit addressing, large files/volumes, and long names](M0053.md)
- [x] [M0054 - LiberFS: extents and sparse files](M0054.md)
- [x] [M0055 - LiberFS: B+tree directories and dynamic inode allocation](M0055.md)
- [x] [M0056 - LiberFS: snapshots](M0056.md)
- [x] [M0057 - LiberFS: transparent compression (optional, last)](M0057.md)

### Foreign-FS interop track (M0058-M0060)

M0058-M0060 add ISO9660, exFAT write support and UDF behind the shared
`Storage.Volume` and `BlockDevice` interfaces. The no_std backends are checked
with image fixtures and live QEMU reads.

- [x] [M0058 - ISO9660 filesystem backend (read-only)](M0058.md)
- [x] [M0059 - exFAT write support (large removable media)](M0059.md)
- [x] [M0060 - UDF filesystem backend (read-only, DVD / Blu-ray)](M0060.md)
- [x] [M0061 - Thin shell: job-control / session service + commands as binaries](M0061.md)
- [x] [M0062 - USB stack (xHCI + HID + mass storage)](M0062.md)
- [x] [M0063 - Hardware inventory commands (lsblk / lspci / lscpu / ...)](M0063.md)
- [x] [M0064 - Capacity quick wins (the limits-audit small fixes)](M0064.md)
- [x] [M0065 - LiberFS sized by the disk (drop the fixed 32 MB pool)](M0065.md)
- [x] [M0066 - Kernel bounds from the runtime (retire the last magic numbers)](M0066.md)
- [x] [M0067 - Runtime-tunable policies (constants become ConfigService keys)](M0067.md)
- [x] [M0068 - GPU framebuffer realloc on resize (no resolution ceiling)](M0068.md)
- [x] [M0069 - Demand-paged user stacks](M0069.md)
- [x] [M0070 - Journal persistence (LogService on the volume)](M0070.md)
- [x] [M0071 - Streaming replies (retire the 4096 B wire ceiling)](M0071.md)
- [x] [M0072 - Contiguous DMA and full-size I/O (queues, sectors, jumbo)](M0072.md)

### LiberFS audit track (M0073-M0085)

M0073-M0085 harden LiberFS after a full implementation audit: durability and
data-loss fixes, scalable allocation, format cleanup, hostile-media bounds and
fsck recovery. Pre-release format changes remain allowed; backward compatibility
is not yet a constraint.

- [x] [M0073 - LiberFS: correctness bugs and data-loss holes](M0073.md)
- [x] [M0074 - LiberFS: allocator and free-map scaling](M0074.md)
- [x] [M0075 - LiberFS: format and modernity](M0075.md)
- [x] [M0076 - LiberFS: code quality](M0076.md)
- [x] [M0077 - LiberFS: post-audit sweep (the re-review's findings)](M0077.md)
- [x] [M0078 - LiberFS: OS- and architecture-agnostic (portability hardening)](M0078.md)
- [x] [M0079 - LiberFS: third-review fixes (GPT robustness, snap_open cost)](M0079.md)
- [x] [M0080 - LiberFS: hostile-disk robustness (sanity bounds on all on-disk values)](M0080.md)
- [x] [M0081 - LiberFS: hostile-disk robustness, second sweep (the consumers M0080 missed)](M0081.md)
- [x] [M0082 - LiberFS: fsck must survive what M0081 taught the mount to survive](M0082.md)
- [x] [M0083 - LiberFS/storage: the seed-archive loop and snapshot-name encoding](M0083.md)
- [x] [M0084 - LiberFS: dangling directory entries (report them, list around them, remove them)](M0084.md)
- [x] [M0085 - LiberFS: last nits (NUL in snapshot names, failure-count overflow)](M0085.md)

### FAT audit track (M0086-M0095)

M0086-M0095 apply the same audit discipline to FAT/exFAT: repair write-side data-loss
paths, reject hostile media without panic or hangs, bound allocation and enforce
the relevant on-disk specifications.

- [x] [M0086 - FAT: data-loss and correctness bugs](M0086.md)
- [x] [M0087 - FAT: hostile-media robustness (no panic, no hang on any boot sector or chain)](M0087.md)
- [x] [M0088 - FAT: allocation bounds and spec conformance](M0088.md)
- [x] [M0089 - FAT: second-pass findings (the sector-size read bug and the leftovers)](M0089.md)
- [x] [M0090 - FAT: third-pass findings (the cluster range gate and the leftovers)](M0090.md)
- [x] [M0091 - FAT: fourth-pass findings (read-side name integrity and the last layout gates)](M0091.md)
- [x] [M0092 - FAT: fifth-pass findings (write-side interop and the last mount nits)](M0092.md)
- [x] [M0093 - FAT: sixth-pass findings (forged-geometry robustness and dirty-range writes)](M0093.md)
- [x] [M0094 - FAT: seventh-pass findings (the ValidDataLength read rule)](M0094.md)
- [x] [M0095 - FAT: ninth-pass findings (FAT mirroring flags and overwrite fidelity)](M0095.md)
- [x] [M0096 - ISO9660: first-pass audit (hostile-media panics and unbounded reads)](M0096.md)
- [x] [M0097 - ISO9660: second-pass findings (the rare-legal shapes that misread)](M0097.md)
- [x] [M0098 - ISO9660: third-pass findings (the root XAR and associated files)](M0098.md)
- [x] [M0099 - UDF: first-pass audit (no hostile-media bounds at all)](M0099.md)
- [x] [M0100 - UDF: second-pass findings (the shared-buffer corruption)](M0100.md)
- [x] [M0101 - UDF: third-pass findings (the last unrefused forms)](M0101.md)
- [x] [M0102 - LiberFS: revisit under the fs-track discipline](M0102.md)
- [x] [M0103 - LiberFS: second-pass findings (the raw-length gate gap)](M0103.md)
- [x] [M0104 - Architecture sweep: memory protection, the display path, and the plumbing debt](M0104.md)
- [x] [M0105 - Kernel wake path: per-object wait queues, cross-core wake IPIs, and the serial RX interrupt](M0105.md)
- [x] [M0106 - Ring-3 preemption: per-thread kernel entry stacks](M0106.md)
- [x] [M0107 - Kernel hardening: SMAP/SMEP, frame allocator bounds, and channel backpressure](M0107.md)
- [x] [M0108 - ABI versioning and boot image hygiene](M0108.md)
- [x] [M0109 - Service lifecycle: a data-driven manifest, a typed grant sender, and honest failure reports](M0109.md)
- [x] [M0110 - Runtime elasticity: NetworkService capacity and device hotplug](M0110.md)
- [x] [M0111 - Filesystem stack unification and performance](M0111.md)
- [x] [M0112 - Shell, tools, and renderer cleanup](M0112.md)
- [x] [M0113 - Wasm: indirect calls and tables](M0113.md)
- [x] [M0114 - Own UEFI-only bootloader (x86_64 / aarch64 / riscv64)](M0114.md)

### Multi-architecture port track (M0115-M0117)

M0115-M0117 extract the architecture boundary, then port the kernel, drivers and
UEFI loader to aarch64 and riscv64. All three architectures must pass under QEMU;
board-specific real-hardware support remains later work.

- [x] [M0115 - Architecture abstraction layer (the HAL boundary)](M0115.md)
- [x] [M0116 - aarch64 (ARM64) kernel + loader port](M0116.md)
- [x] [M0117 - riscv64 (RISC-V) kernel + loader port](M0117.md)
- [x] [M0118 - Multi-arch track follow-ups (the M0115-M0117 loose ends)](M0118.md)
- [x] [M0119 - Pre-phase-3 hardening (finish before the server platform)](M0119.md)
- [ ] [M0120 - LSIDL package imports, modular generation, and language hardening](M0120.md)
- [x] [M0121 - Application graphics, raw input, and PCM audio (the app-platform layer)](M0121.md)
- [x] [M0122 - Image viewer (the first graphical application)](M0122.md)
- [x] [M0123 - Shared system libraries (dynamic linking)](M0123.md)
- [ ] [M0124 - Audio player (streaming decoders over AudioService)](M0124.md)
- [x] [M0125 - Native executable artifacts (`.lsexe`)](M0125.md)
- [x] [M0126 - Image conversion tool (`imgconv`)](M0126.md)
- [x] [M0127 - Userspace source and system-volume layout cleanup](M0127.md)
- [ ] [M0128 - Declarative driver binding and lifecycle core](M0128.md)
- [ ] [M0129 - Universal standards-based driver set](M0129.md)
- [ ] [M0130 - LiberCommander (`lico`, `licoedit`, `licoview`)](M0130.md)
- [ ] [M0131 - Additional system utilities](M0131.md)
- [ ] [M0132 - Capability-native pipes and redirection](M0132.md)
- [ ] [M0133 - Future-ready 3D graphics foundation + software-rendered scene](M0133.md)

### Development-loop priority

M0134 removes whole-system rebuild and cold-boot repetition from ordinary leaf-tool
development while preserving the existing full release gates. All six phases are delivered and
the loop is in daily use. Two defects it found in older code are open where that code lives,
in M0126 and M0039.

- [x] [M0134 - Incremental development and persistent QEMU test loop](M0134.md)

### Internationalization

The system has no locale layer: TimeService serves UTC with nowhere to put an offset, the
keyboard is a US table compiled into the input driver, and nothing formats a number or a date
by any convention but one. M0135 adds the policy every one of those is a consumer of.

- [ ] [M0135 - Locales: language, region, time zone, keyboard and formats](M0135.md)

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
