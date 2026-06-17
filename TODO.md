# TODO

Phase 0 plan: kernel bring-up MVP. We proceed step by step; every milestone has
a testable "done when" (via the QEMU test harness).

Phase 0 goal: a capability microkernel that boots SMP-aware, creates core objects,
schedules threads, does Channel IPC with capability passing, handles
interrupts/timer, enforces basic resource accounting, isolates processes in their
own address spaces, kills and cleans up a faulting process, and brings up a
minimal userspace (init package -> SystemManager -> StorageManager over a ramdisk
-> a simple CLI + System Graph) - all testable in QEMU. No real drivers (beyond
serial/timer/framebuffer), persistent FS, networking, IDL toolchain, or WASM host
yet (that is Phase 1).

Everything is written SMP-aware from the start, so locks are not retrofitted later.

## M-1 - Prepare
- [x] product + version + website + github in single source of truth file, loaded from anywhere else
- [x] Rust, .sh code formaters
- [x] Update .gitignore based on real directories we have
- [x] Add README.md, INSTALL.md
- [x] Add commit.sh + other scripts
- [x] Bootable ISO CD image
- [x] Bootable HDD/SD image

## M0 - Skeleton (bring-up)
- [x] Boot via Limine
- [x] Serial output
- [x] Panic handler to serial
- [x] `cargo run` -> QEMU headless
- [x] GDT/IDT + basic exception handlers
- [x] `cargo test` -> QEMU + debug-exit
- [x] GDB attach + single-stepping
- Done when: "hello from the kernel" on serial, single-stepping in GDB, green `cargo test`.

## M1 - Memory foundation
- [x] Frame allocator (from the Limine memory map)
- [x] Paging / `AddressSpace` at the kernel level (map/unmap)
- [x] Kernel heap (enables `alloc` - Box/Vec)
- [x] First spinlock (shared frame allocator), written correctly from the start
- Done when: alloc/map/free works, `alloc` collections run in a test.

## M2 - Time and interrupts
- [x] Local APIC
- [x] Timer tick
- [x] Interrupt dispatch
- [x] Handler registration
- Done when: a periodic timer interrupt is counted, a handler can be registered.

## M3 - SMP
- [x] Wake AP cores (Limine SMP)
- [x] Per-CPU data
- [x] Per-CPU init
- Done when: all cores reach a known idle point and report in.

## M4 - Object and capability core
- [x] Generic kernel-object base (lifetime, refcount)
- [x] Handle table
- [x] Capability rights
- [x] Lookup with rights enforcement
- Done when: create/lookup/close handle with rights checks, kernel-side unit tests.

## M5 - Threads, address spaces, scheduler
- [x] `Thread` object
- [x] `AddressSpace` object
- [x] Context switch
- [x] Run queue
- [x] Scheduler (start simple, evolve)
- Done when: multiple kernel threads multiplex on a core (and across cores).

## M6 - Syscall ABI
- [x] Syscall entry/exit (`syscall` instruction, register convention)
- [x] Dispatch table
- [x] Minimal syscall set (handle ops, object create, address-space ops)
- Done when: a syscall round-trips there and back.

## M7 - IPC
- [x] `Channel` object
- [x] Async send/recv ("message + handle to shared memory")
- [x] Capability transfer over a channel
- [x] `Event` and `Timer` objects
- Done when: two threads exchange a message and a capability over a channel.

## M8 - Userspace (ring 3)
- [x] Transition to ring 3
- [x] Load a minimal user thread
- [x] Syscall from userspace
- [x] IPC to a kernel service
- Done when: the first userspace program runs and makes a capability-gated syscall + channel IPC.

## M9 - Resource accounting
- [x] Per-`Domain` accounting of kernel resources (memory, handles)
- [x] Quota enforcement
- Done when: a domain at its quota cap fails cleanly, not by crashing.

## M10 - IPC latency benchmark (phase 0 gate)
- [x] Measure local `call()` round-trip latency (send + wait + receive)
- [x] Confirm zero-copy for large data (a handle to a shared buffer does not copy the payload)
- [x] Record the numbers so regressions stay visible
- Done when: the IPC round-trip is measured (target: single-digit microseconds) and zero-copy is empirically confirmed - the concept's gate before higher layers are built on IPC.
- Result: raw channel round-trip ~0.76 us, full syscall round-trip ~5.1 us (both within the single-digit-us budget); zero-copy confirmed (a 1 MiB buffer transferred with a 3-byte message, far-end marker read back through the moved capability). TSC-based timing (arch::tsc), boot prints the numbers, `ipc_round_trip_and_zero_copy` test asserts it deterministically.

## M11 - Process and per-process address space
- [ ] `Process` object (owns its AddressSpace, handle table, threads; bound to a Domain)
- [ ] Per-process `AddressSpace` with its own page tables (switch CR3 on context switch)
- [ ] Move the handle table from the Thread (M6 stand-in) onto the Process
- [ ] Threads belong to a Process; `process_create` / `thread_create` wired to it
- Done when: two processes run with isolated address spaces, a thread switch reloads CR3, and handle tables are per-process - green under `cargo test`.

## M12 - Fault isolation and crashed-process cleanup
- [ ] A userspace page fault / GPF terminates only the faulting `Process`, not the kernel
- [ ] Process teardown: close all handles, free frames + address space, refund Domain accounting
- [ ] `fault_info_get`: record and expose basic fault info for the killed process
- [ ] Other cores and the scheduler keep running across the kill
- Done when: a userspace process that dereferences a bad pointer is killed and fully cleaned up, the kernel survives, and a test asserts the Domain's accounting returns to zero.

## M13 - Domain hierarchy and lifecycle
- [ ] Domain tree (parent/child); processes hang under a Domain node
- [ ] `domain_create` / `domain_kill` syscalls
- [ ] Bulk termination: killing a Domain tears down the whole subtree and cleans up
- [ ] Hierarchical limits: a process may exceed neither its own limit nor its Domain's aggregate
- Done when: killing a parent Domain terminates all descendant processes and frees their resources; a test verifies the subtree dies and accounting returns to zero.

## M14 - Init package and the first userspace process
- [ ] Load a ramdisk / init package as a Limine module
- [ ] A minimal ELF loader: map a userspace program from the package into a new Process
- [ ] Start `SystemManager` as the first userspace process from the init package
- [ ] SystemManager reports in over IPC (the first real userspace service)
- Done when: the kernel loads the init package from a Limine module and runs SystemManager in ring 3 as the first process, which sends its first IPC message.

## M15 - Framebuffer text console
- [ ] Limine framebuffer request + init
- [ ] A bitmap-font text console rendering to the framebuffer
- [ ] Mirror the kernel log (and/or a console service) to the framebuffer alongside serial
- Done when: the boot log appears on the QEMU graphical framebuffer, not only on serial.

## M16 - StorageManager over a ramdisk + vol:// access
- [ ] A ramdisk-backed `Volume` (read-only is enough for now)
- [ ] `StorageManager` userspace service: `open(path, rights) -> handle`
- [ ] `vol://` resolution: a `VolumePath` object + a userspace resolver (the object is canonical, the URI is a representation)
- [ ] Read a file's bytes zero-copy (metadata + a handle to a shared buffer)
- Done when: a userspace client opens a path on a `vol://` volume via StorageManager and reads its contents over a channel.

## M17 - Simple CLI + basic System Graph
- [ ] A minimal CLI component over serial (read a line, run a command, print a typed result)
- [ ] `object_info_get` introspection plumbing
- [ ] A basic System Graph: enumerate live Domains -> processes -> handles/channels
- [ ] CLI commands: list a volume, print a file, dump the System Graph
- Done when: a command typed into the CLI round-trips to a service and the CLI can print the System Graph from live state.

## Definition of done (phase 0)
The kernel core is done (M0-M9): it boots on multiple cores, creates core objects,
schedules threads, a syscall runs, a message and a capability pass over a channel,
and a quota overrun fails in a controlled way. On top of that the userspace
bring-up is done (M10-M17): the IPC round-trip is measured, processes are isolated
in their own address spaces, a faulting userspace process is killed and cleaned up
while the kernel survives, Domains form a tree that can be bulk-killed, the kernel
loads an init package and starts SystemManager as the first userspace process, a
StorageManager serves files from a ramdisk over `vol://`, a simple CLI drives it
and can print a basic System Graph, and framebuffer text works. All under
`cargo test` / QEMU.

## Out of scope for phase 0 (against scope creep)
The IDL/WIT toolchain and binding generators; the full core services
(Process/Storage/Log/Device/Config as complete services); virtio drivers
(blk/net/console); a minimal WASI host and Wasm components; a persistent native
filesystem; networking; a strict app sandbox / permission manifests; more
architectures than x86_64; real hardware; crypto/attestation/verified boot. That
is phase 1+.
