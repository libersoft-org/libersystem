# TODO

## Phase 0

Phase 0 establishes a testable, SMP-aware capability microkernel MVP: memory,
interrupts, scheduling, objects and capabilities, syscall and Channel IPC,
resource accounting, isolated process lifecycle, minimal userspace, ramdisk
storage, a CLI and framebuffer console. Every milestone has a QEMU-tested gate.

- [x] [P00M0001 - Skeleton (bring-up)](P00M0001.md)
- [x] [P00M0002 - Memory foundation](P00M0002.md)
- [x] [P00M0003 - Time and interrupts](P00M0003.md)
- [x] [P00M0004 - SMP](P00M0004.md)
- [x] [P00M0005 - Object and capability core](P00M0005.md)
- [x] [P00M0006 - Threads, address spaces, scheduler](P00M0006.md)
- [x] [P00M0007 - Syscall ABI](P00M0007.md)
- [x] [P00M0008 - IPC](P00M0008.md)
- [x] [P00M0009 - Userspace (ring 3)](P00M0009.md)
- [x] [P00M0010 - Resource accounting](P00M0010.md)
- [x] [P00M0011 - IPC latency benchmark (phase 0 gate)](P00M0011.md)
- [x] [P00M0012 - Process and per-process address space](P00M0012.md)
- [x] [P00M0013 - Fault isolation and crashed-process cleanup](P00M0013.md)
- [x] [P00M0014 - Domain hierarchy and lifecycle](P00M0014.md)
- [x] [P00M0015 - Init package and the first userspace process](P00M0015.md)
- [x] [P00M0016 - Framebuffer text console](P00M0016.md)
- [x] [P00M0017 - StorageService over a ramdisk + `vol://` access](P00M0017.md)
- [x] [P00M0018 - Simple CLI + basic System Graph](P00M0018.md)

### Definition of done (phase 0)

Done when the SMP kernel provides the core object, capability, scheduling, IPC,
accounting and isolation paths; boots SystemManager and ramdisk-backed storage;
and exposes the CLI, basic System Graph and framebuffer console under QEMU tests.

## Phase 1 - First usable userspace

Phase 1 builds the first usable userspace: blocking wait and preemption, isolated
virtio drivers, dependency-ordered core services, generated typed APIs, a minimal
WASI component host and a powerbox file picker. Early services provide real
protocols for the P01M0008 IDL trial; later services adopt the generated bindings.
Authority remains capability-first and explicitly delegated.

- [x] [P01M0001 - Blocking `wait` primitive](P01M0001.md)
- [x] [P01M0002 - Preemptive scheduling](P01M0002.md)
- [x] [P01M0003 - Kernel additions: driver + spawn syscalls, queue/DMA accounting](P01M0003.md)
- [x] [P01M0004 - ServiceManager and the boot chain](P01M0004.md)
- [x] [P01M0005 - LogService (structured logging)](P01M0005.md)
- [x] [P01M0006 - DeviceManager + virtio transport](P01M0006.md)
- [x] [P01M0007 - virtio drivers (headless): blk, net, console](P01M0007.md)
- [x] [P01M0008 - IDL/WIT toolchain and generators](P01M0008.md)
- [x] [P01M0009 - StorageService over virtio-blk](P01M0009.md)
- [x] [P01M0010 - Core services: Process, Device, Config](P01M0010.md)
- [x] [P01M0011 - Minimal WASI host: the first Wasm component](P01M0011.md)
- [x] [P01M0012 - Prototype file picker (powerbox)](P01M0012.md)

### Definition of done (phase 1)

Done when the preemptive kernel and blocking wait support isolated virtio drivers,
the supervised core services answer generated typed APIs, and a capability-scoped
Wasm component receives a file handle through the powerbox under QEMU tests.

## Phase 2 - Appliance/edge platform

Phase 2 turns the VM into an appliance/edge platform, beginning with shared event
streams and a network stack over virtio-net, then adding local I/O, policy,
observability, persistent storage and multi-architecture support.

**Version ownership rule:** `v1` remains open until the project owner explicitly
says otherwise. Completing a milestone records that its current acceptance
criteria pass; it never freezes an API, ABI, schema, file format, machine
description, product profile or release line. Coordinated breaking changes may
continue inside `v1`, and only the project owner may close it or start a
successor version. Any older roadmap wording such as "freeze" or "finalize" is
therefore interpreted only as recording the current contract, never as closing
or preventing further development of `v1`.

- [x] [P02M0001 - Event streams in the IDL toolchain (`stream<T>`)](P02M0001.md)
- [x] [P02M0002 - Interrupt-driven I/O + virtio-input keyboard](P02M0002.md)
- [x] [P02M0003 - virtio-net receive path + the link/IP layer (Ethernet, ARP, IPv4, ICMP)](P02M0003.md)
- [x] [P02M0004 - NetworkService, UDP/TCP, and the net tools as standalone programs](P02M0004.md)
- [x] [P02M0005 - TimeService: wall-clock time (RTC + NTP)](P02M0005.md)
- [x] [P02M0006 - Interactive console: line editor, history, and the console subsystem](P02M0006.md)
- [x] [P02M0007 - Pointer/mouse plumbing (virtio-input pointer + InputService)](P02M0007.md)
- [x] [P02M0008 - Observability (full System Graph, tracing, counters, CBOR)](P02M0008.md)
- [x] [P02M0009 - Security hardening: app sandbox, permission manifests, PermissionManager](P02M0009.md)
- [x] [P02M0010 - ResourceManager policy service](P02M0010.md)
- [x] [P02M0011 - ServiceManager: restart policy and watchdog](P02M0011.md)
- [x] [P02M0012 - The Liber component ABI + a Rust component SDK](P02M0012.md)
- [x] [P02M0013 - A simple persistent native filesystem](P02M0013.md)
- [x] [P02M0014 - virtio-gpu driver + runtime mode-set (the resize source for the local console)](P02M0014.md)
- [x] [P02M0015 - AudioService over virtio-sound (headless playback + capture)](P02M0015.md)
- [x] [P02M0016 - MSI-X interrupt routing (per-device vectors)](P02M0016.md)
- [x] [P02M0017 - Layered console: stream -> grid model -> renderer -> swappable display](P02M0017.md)
- [x] [P02M0018 - FAT / exFAT filesystem backend (read foreign removable media)](P02M0018.md)
- [x] [P02M0019 - LiberFS: directories and capacity scaling](P02M0019.md)
- [x] [P02M0020 - LiberFS: write semantics, metadata, and integrity](P02M0020.md)
- [x] [P02M0021 - LiberFS: block checksums (integrity)](P02M0021.md)
- [x] [P02M0022 - LiberFS: copy-on-write (toward the modern FS)](P02M0022.md)
- [x] [P02M0023 - LiberFS: 64-bit addressing, large files/volumes, and long names](P02M0023.md)
- [x] [P02M0024 - LiberFS: extents and sparse files](P02M0024.md)
- [x] [P02M0025 - LiberFS: B+tree directories and dynamic inode allocation](P02M0025.md)
- [x] [P02M0026 - LiberFS: snapshots](P02M0026.md)
- [x] [P02M0027 - LiberFS: transparent compression (optional, last)](P02M0027.md)
- [x] [P02M0028 - ISO9660 filesystem backend (read-only)](P02M0028.md)
- [x] [P02M0029 - exFAT write support (large removable media)](P02M0029.md)
- [x] [P02M0030 - UDF filesystem backend (read-only, DVD / Blu-ray)](P02M0030.md)
- [x] [P02M0031 - Thin shell: job-control / session service + commands as binaries](P02M0031.md)
- [x] [P02M0032 - USB stack (xHCI + HID + mass storage)](P02M0032.md)
- [x] [P02M0033 - Hardware inventory commands (lsblk / lspci / lscpu / ...)](P02M0033.md)
- [x] [P02M0034 - Capacity quick wins (the limits-audit small fixes)](P02M0034.md)
- [x] [P02M0035 - LiberFS sized by the disk (drop the fixed 32 MB pool)](P02M0035.md)
- [x] [P02M0036 - Kernel bounds from the runtime (retire the last magic numbers)](P02M0036.md)
- [x] [P02M0037 - Runtime-tunable policies (constants become ConfigService keys)](P02M0037.md)
- [x] [P02M0038 - GPU framebuffer realloc on resize (no resolution ceiling)](P02M0038.md)
- [x] [P02M0039 - Demand-paged user stacks](P02M0039.md)
- [x] [P02M0040 - Journal persistence (LogService on the volume)](P02M0040.md)
- [x] [P02M0041 - Streaming replies (retire the 4096 B wire ceiling)](P02M0041.md)
- [x] [P02M0042 - Contiguous DMA and full-size I/O (queues, sectors, jumbo)](P02M0042.md)
- [x] [P02M0043 - LiberFS: correctness bugs and data-loss holes](P02M0043.md)
- [x] [P02M0044 - LiberFS: allocator and free-map scaling](P02M0044.md)
- [x] [P02M0045 - LiberFS: format and modernity](P02M0045.md)
- [x] [P02M0046 - LiberFS: code quality](P02M0046.md)
- [x] [P02M0047 - LiberFS: post-audit sweep (the re-review's findings)](P02M0047.md)
- [x] [P02M0048 - LiberFS: OS- and architecture-agnostic (portability hardening)](P02M0048.md)
- [x] [P02M0049 - LiberFS: third-review fixes (GPT robustness, snap_open cost)](P02M0049.md)
- [x] [P02M0050 - LiberFS: hostile-disk robustness (sanity bounds on all on-disk values)](P02M0050.md)
- [x] [P02M0051 - LiberFS: hostile-disk robustness, second sweep (the consumers P02M0050 missed)](P02M0051.md)
- [x] [P02M0052 - LiberFS: fsck must survive what P02M0051 taught the mount to survive](P02M0052.md)
- [x] [P02M0053 - LiberFS/storage: the seed-archive loop and snapshot-name encoding](P02M0053.md)
- [x] [P02M0054 - LiberFS: dangling directory entries (report them, list around them, remove them)](P02M0054.md)
- [x] [P02M0055 - LiberFS: last nits (NUL in snapshot names, failure-count overflow)](P02M0055.md)
- [x] [P02M0056 - FAT: data-loss and correctness bugs](P02M0056.md)
- [x] [P02M0057 - FAT: hostile-media robustness (no panic, no hang on any boot sector or chain)](P02M0057.md)
- [x] [P02M0058 - FAT: allocation bounds and spec conformance](P02M0058.md)
- [x] [P02M0059 - FAT: second-pass findings (the sector-size read bug and the leftovers)](P02M0059.md)
- [x] [P02M0060 - FAT: third-pass findings (the cluster range gate and the leftovers)](P02M0060.md)
- [x] [P02M0061 - FAT: fourth-pass findings (read-side name integrity and the last layout gates)](P02M0061.md)
- [x] [P02M0062 - FAT: fifth-pass findings (write-side interop and the last mount nits)](P02M0062.md)
- [x] [P02M0063 - FAT: sixth-pass findings (forged-geometry robustness and dirty-range writes)](P02M0063.md)
- [x] [P02M0064 - FAT: seventh-pass findings (the ValidDataLength read rule)](P02M0064.md)
- [x] [P02M0065 - FAT: ninth-pass findings (FAT mirroring flags and overwrite fidelity)](P02M0065.md)
- [x] [P02M0066 - ISO9660: first-pass audit (hostile-media panics and unbounded reads)](P02M0066.md)
- [x] [P02M0067 - ISO9660: second-pass findings (the rare-legal shapes that misread)](P02M0067.md)
- [x] [P02M0068 - ISO9660: third-pass findings (the root XAR and associated files)](P02M0068.md)
- [x] [P02M0069 - UDF: first-pass audit (no hostile-media bounds at all)](P02M0069.md)
- [x] [P02M0070 - UDF: second-pass findings (the shared-buffer corruption)](P02M0070.md)
- [x] [P02M0071 - UDF: third-pass findings (the last unrefused forms)](P02M0071.md)
- [x] [P02M0072 - LiberFS: revisit under the fs-track discipline](P02M0072.md)
- [x] [P02M0073 - LiberFS: second-pass findings (the raw-length gate gap)](P02M0073.md)
- [x] [P02M0074 - Architecture sweep: memory protection, the display path, and the plumbing debt](P02M0074.md)
- [x] [P02M0075 - Kernel wake path: per-object wait queues, cross-core wake IPIs, and the serial RX interrupt](P02M0075.md)
- [x] [P02M0076 - Ring-3 preemption: per-thread kernel entry stacks](P02M0076.md)
- [x] [P02M0077 - Kernel hardening: SMAP/SMEP, frame allocator bounds, and channel backpressure](P02M0077.md)
- [x] [P02M0078 - ABI versioning and boot image hygiene](P02M0078.md)
- [x] [P02M0079 - Service lifecycle: a data-driven manifest, a typed grant sender, and honest failure reports](P02M0079.md)
- [x] [P02M0080 - Runtime elasticity: NetworkService capacity and device hotplug](P02M0080.md)
- [x] [P02M0081 - Filesystem stack unification and performance](P02M0081.md)
- [x] [P02M0082 - Shell, tools, and renderer cleanup](P02M0082.md)
- [x] [P02M0083 - Wasm: indirect calls and tables](P02M0083.md)
- [x] [P02M0084 - Own UEFI-only bootloader (x86_64 / aarch64 / riscv64)](P02M0084.md)
- [x] [P02M0085 - Architecture abstraction layer (the HAL boundary)](P02M0085.md)
- [x] [P02M0086 - aarch64 (ARM64) kernel + loader port](P02M0086.md)
- [x] [P02M0087 - riscv64 (RISC-V) kernel + loader port](P02M0087.md)
- [x] [P02M0088 - Multi-arch track follow-ups (the P02M0085-P02M0087 loose ends)](P02M0088.md)
- [x] [P02M0089 - Pre-phase-3 hardening (finish before the server platform)](P02M0089.md)
- [x] [P02M0090 - LSIDL package imports, modular generation, and language hardening](P02M0090.md)
- [x] [P02M0091 - Application graphics, raw input, and PCM audio (the app-platform layer)](P02M0091.md)
- [x] [P02M0092 - Image viewer (the first graphical application)](P02M0092.md)
- [x] [P02M0093 - Shared system libraries (dynamic linking)](P02M0093.md)
- [x] [P02M0094 - Audio player (streaming decoders over AudioService)](P02M0094.md)
- [x] [P02M0095 - Native executable artifacts (`.lsexe`)](P02M0095.md)
- [x] [P02M0096 - Image conversion tool (`imgconv`)](P02M0096.md)
- [x] [P02M0097 - Userspace source and system-volume layout cleanup](P02M0097.md)
- [x] [P02M0098 - A device claim is a count, so two drivers can own one device](P02M0098.md)
- [ ] [P02M0099 - Universal standards-based driver set](P02M0099.md)
- [x] [P02M0100 - LiberCommander (`lico`, `licoedit`, `licoview`)](P02M0100.md)
- [x] [P02M0101 - Additional system utilities](P02M0101.md)
- [x] [P02M0102 - Capability-native pipes and redirection](P02M0102.md)
- [ ] [P02M0103 - The graphics platform desktop and mobile applications are built on](P02M0103.md)
- [x] [P02M0104 - Incremental development and persistent QEMU test loop](P02M0104.md)
- [ ] [P02M0105 - Locales: language, region, time zone, keyboard and formats](P02M0105.md)
- [ ] [P02M0106 - Identity: accounts, authentication and per-user authority](P02M0106.md)
- [x] [P02M0107 - Build layering: compile, then package](P02M0107.md)
- [x] [P02M0108 - The loader reads the filesystem: retire the bootstrap archive](P02M0108.md)
- [x] [P02M0109 - LiberMemFS: a writable filesystem that lives in memory](P02M0109.md)
- [x] [P02M0110 - The build interface: scripts with flags, and a Justfile that stops being one](P02M0110.md)
- [x] [P02M0111 - Streams as pending operations, and a harness that can move the clock](P02M0111.md)
- [x] [P02M0112 - Gates that stopped checking](P02M0112.md)
- [x] [P02M0113 - LiberFS: what a mount may not do, and what a checksum does not prove](P02M0113.md)
- [x] [P02M0114 - LiberFS: what a mount may still lose, and what a parser may not quietly repair](P02M0114.md)
- [x] [P02M0115 - The kernel's lowest layers do not yet keep the promises the upper ones make](P02M0115.md)
- [x] [P02M0116 - What P02M0114 left in the disk probe, and three gaps it opened itself](P02M0116.md)
- [x] [P02M0117 - A wait set the kernel keeps](P02M0117.md)
- [x] [P02M0118 - Testing that scales with the change, not with the tree](P02M0118.md)
- [x] [P02M0119 - A fault in a user copy must be an error, not a dead kernel](P02M0119.md)
- [x] [P02M0120 - A free must never lose the memory it frees](P02M0120.md)
- [x] [P02M0121 - A receive that takes a message must be able to deliver it](P02M0121.md)
- [x] [P02M0122 - What authorises a write to somebody's disk](P02M0122.md)
- [x] [P02M0123 - What a mount allocates, and what a write may assume about what it found](P02M0123.md)
- [x] [P02M0124 - What the UDF backend claims to read, and what it actually does](P02M0124.md)
- [x] [P02M0125 - What the FAT backend writes, and what it says it wrote](P02M0125.md)
- [x] [P02M0126 - What the ISO9660 backend accepts, and the threat model it states](P02M0126.md)
- [x] [P02M0127 - The ABI's own guarantee, which is the one thing it cannot get wrong](P02M0127.md)
- [x] [P02M0128 - The terminal's state machine, and the one safe function that is not safe](P02M0128.md)
- [x] [P02M0129 - What the loader assumes about the machine it is running on](P02M0129.md)
- [x] [P02M0130 - What the component SDK is, and what its milestone says it is](P02M0130.md)
- [x] [P02M0131 - What LiberMemFS's capacity actually bounds](P02M0131.md)
- [x] [P02M0132 - The gap between what fsck can see and what a writable mount will accept](P02M0132.md)
- [x] [P02M0133 - The parts are right; the transactions between them are not](P02M0133.md)
- [x] [P02M0134 - The Wasm engine runs modules it has never validated](P02M0134.md)
- [ ] [P02M0135 - Foreign graphics-stack prerequisites](P02M0135.md)
- [ ] [P02M0136 - Text foundation: shaping, fallback and layout](P02M0136.md)
- [x] [P02M0137 - Remove redundant dynamic-report work from warm builds](P02M0137.md)
- [x] [P02M0138 - PermissionManager changes must select the tests that exercise them](P02M0138.md)
- [x] [P02M0139 - Reuse the PermissionManager test fixture without sharing test state](P02M0139.md)
- [x] [P02M0140 - Retire `just`: one way to run things, and it is a script with flags](P02M0140.md)
- [x] [P02M0141 - Wiring the system can re-run, and authority it can prove](P02M0141.md)
- [x] [P02M0142 - One boot tail for three architectures, and a HAL surface that means what it says](P02M0142.md)
- [x] [P02M0143 - Five holes an outside audit found that are worth closing, and the ones that are not](P02M0143.md)
- [x] [P02M0144 - The build says nothing it cannot back up](P02M0144.md)
- [x] [P02M0145 - The development profile does not boot, and nothing was going to tell us](P02M0145.md)
- [x] [P02M0146 - The scenarios run on one architecture, and the runner says they run on three](P02M0146.md)
- [x] [P02M0147 - A full-screen program is launched without the terminal it is about to take over](P02M0147.md)
- [x] [P02M0148 - `shutdown` tears the system down and then does not stop the machine](P02M0148.md)
- [x] [P02M0149 - A provider was rebuilt, its consumer was not, and only the packager noticed](P02M0149.md)
- [x] [P02M0150 - A digest beside a payload is not a boot trust chain](P02M0150.md)
- [ ] [P02M0151 - The architecture boundary contains no panic-shaped compatibility contract](P02M0151.md)
- [ ] [P02M0152 - Memory has a topology, not one global distance](P02M0152.md)
- [x] [P02M0153 - Virtio-IOMMU confines the first x86_64 QEMU endpoints](P02M0153.md)
- [x] [P02M0154 - The capability transfer rules have a machine-checked bounded model](P02M0154.md)
- [x] [P02M0155 - The loader searches every disk in the machine for a file that is not there](P02M0155.md)
- [x] [P02M0156 - A check that skips what it cannot read has already answered yes](P02M0156.md)
- [ ] [P02M0157 - The firmware described something the tables cannot hold, and the kernel used it anyway](P02M0157.md)
- [ ] [P02M0158 - The boot report names the same device twice and the same component two ways](P02M0158.md)
- [x] [P02M0159 - Every boot exercises the degraded DMA path and none exercises the isolated one](P02M0159.md)
- [x] [P02M0160 - One artifact has two required shapes and one name, so the last build wins](P02M0160.md)
- [x] [P02M0161 - Every driver family invents its own bring-up wire](P02M0161.md)
- [x] [P02M0162 - A bind that fails part-way leaves the parts that succeeded](P02M0162.md)
- [x] [P02M0163 - Discovery holds two kinds of device and the binding unit is an index into that table](P02M0163.md)
- [x] [P02M0164 - One NIC, one disk, one screen: a second of anything has nowhere to go](P02M0164.md)
- [x] [P02M0165 - A driver that stops answering looks exactly like one that is busy](P02M0165.md)
- [x] [P02M0166 - Everything that went wrong is reported as `failed`](P02M0166.md)
- [ ] [P02M0167 - The selector answers "everything" to every question, so nobody asks it](P02M0167.md) - revised twice on 2026-08-27 after external review: M0 added, every item given a contract

### Definition of done (phase 2)

Done when the capability-scoped appliance provides networking, wall-clock time,
interactive console and audio, policy and observability services, supervised
components, writable persistent storage and the complete filesystem set. The
kernel and UEFI loader must pass the QEMU suite on x86_64, aarch64 and riscv64.

Phase 2 additionally requires P02M0141, which is where the system stops being one
a crash can only be recovered from by rebooting and stops handing its broadest
capability to keyboard drivers: a bootstrap that can be re-run, boot authority
checked against what was declared, an owner for every long-lived branch, and a
fault matrix that shows the recovery paths work. It does not freeze or release
`v1`. Server, general real-hardware, desktop and AI directions remain
vision in the [Concept roadmap](../CONCEPT_EN.md#roadmap), not active product
milestones.
