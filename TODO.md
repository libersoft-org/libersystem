# TODO

Phase 0 plan: kernel bring-up MVP. We proceed step by step; every milestone has
a testable "done when" (via the QEMU test harness).

Phase 0 goal: a kernel that can create core objects, run multiple threads under a
scheduler across multiple cores, do IPC over channels with capability passing,
handle interrupts/timer, and enforce basic resource accounting - all testable in
QEMU. No real drivers, FS, networking, or WASM host.

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

## Definition of done (phase 0)
The kernel boots on multiple cores, creates core objects, schedules threads, a
syscall runs, a message and a capability pass over a channel, one userspace stub
uses the whole thing, and a quota overrun fails in a controlled way. All under
`cargo test` in QEMU.

## Out of scope for phase 0 (against scope creep)
Real drivers (besides serial/timer), filesystem, networking, WASM host, userspace
beyond the test stub, more architectures than x86_64, real hardware,
crypto/attestation. That is phase 1+.
