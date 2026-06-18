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

## M0 - Skeleton (bring-up)
- [x] Boot via Limine
- [x] Serial output
- [x] Panic handler to serial
- [x] `cargo run` -> QEMU headless
- [x] GDT/IDT + basic exception handlers
- [x] `cargo test` -> QEMU + debug-exit
- [x] GDB attach + single-stepping
- Done when: "hello from the kernel" on serial, single-stepping in GDB, green `cargo test`.
- Result: `kmain` (the linker-script ENTRY) brings up serial (COM1 16550, `arch::serial`) then the GDT+IDT - `arch::gdt` is a hand-rolled null/code/data GDT plus a TSS with an IST double-fault stack, `arch::idt` fills all 32 exception vectors. `serial_println!` drives a `SerialWriter`; `panic.rs` prints the panic to serial (and exits QEMU on the test path). A `#[test_case]` harness runs under QEMU and reports through the `isa-debug-exit` port (`arch::exit_qemu`). The breakpoint handler (`int3`) prints and RETURNS (recoverable) while the rest halt; tests `trivial_assertion` + `breakpoint_exception_returns` are green, and GDB attaches over the QEMU stub (`just debug` / `just gdb`) for single-stepping.

## M1 - Memory foundation
- [x] Frame allocator (from the Limine memory map)
- [x] Paging / `AddressSpace` at the kernel level (map/unmap)
- [x] Kernel heap (enables `alloc` - Box/Vec)
- [x] First spinlock (shared frame allocator), written correctly from the start
- Done when: alloc/map/free works, `alloc` collections run in a test.
- Result: `mem::frame` is a physical frame allocator whose free list is threaded through the free frames themselves (via the HHDM), seeded from the Limine memory map's USABLE regions. `arch::paging` walks and creates the 4-level page tables on the active CR3 (`map_page`/`unmap_page`, flags PRESENT/WRITABLE/USER, no NX bit). `mem::heap` is a linked-list first-fit `#[global_allocator]` over a 1 MiB mapped window, which turns on `alloc` (Box/Vec). The first `SpinLock` (`sync.rs`, a TTAS lock) guards the shared frame allocator. Tests `frame_alloc_distinct`, `paging_map_unmap`, `heap_box_vec` are green; the boot log also prints the free-frame count and a Vec sum demo.

## M2 - Time and interrupts
- [x] Local APIC
- [x] Timer tick
- [x] Interrupt dispatch
- [x] Handler registration
- Done when: a periodic timer interrupt is counted, a handler can be registered.
- Result: `arch::apic` brings up the Local APIC in xAPIC/MMIO mode (its MMIO page is mapped explicitly NO_CACHE, since Limine's HHDM does not map it), masks the legacy 8259 PICs, and starts a periodic timer calibrated against PIT channel 2 (100 Hz) that bumps a `TICKS` counter. `arch::interrupts` dispatches IRQ vectors 32..47 through a lock-free handler table (fn pointers stored as atomics, so dispatch is safe in interrupt context) and issues EOI; `register(vector, fn)` installs a handler. Tests `timer_ticks_advance` (the tick counter advances with interrupts enabled) and `handler_registration_dispatch` (a software `int 0x2f` reaches a registered handler) are green.

## M3 - SMP
- [x] Wake AP cores (Limine SMP)
- [x] Per-CPU data
- [x] Per-CPU init
- Done when: all cores reach a known idle point and report in.
- Result: `smp::init` wakes the application processors via Limine's MP response (each AP jumps to `ap_entry` on its own Limine-provided stack), assigns per-core ids, and spins until every core has reported in. `arch::percpu` parks a per-CPU struct through the GS-base MSR; the GDT carries a per-core TSS (each core `ltr`s its own, since a busy TSS cannot be loaded twice) while the IDT is shared, and each AP programs its own LAPIC and syscall MSRs. Reporting-in happens before the online count is bumped (under a lock) so the BSP's resumed serial output never interleaves with the AP lines. Test `smp_all_cores_online` asserts `online_count() == cpu_count()` (4 cores under QEMU); the APs then park in the scheduler idle loop.

## M4 - Object and capability core
- [x] Generic kernel-object base (lifetime, refcount)
- [x] Handle table
- [x] Capability rights
- [x] Lookup with rights enforcement
- Done when: create/lookup/close handle with rights checks, kernel-side unit tests.
- Result: every kernel object embeds an `ObjectHeader` (a unique koid plus a generation counter for O(1) revocation) and implements `KernelObject` (`object/mod.rs`). `object/rights.rs` is a 12-bit `Rights` newtype bitset (READ/WRITE/MAP/SEND/RECEIVE/DUPLICATE/TRANSFER/REVOKE/.../MANAGE/WAIT) with subset checks. `object/handle.rs` holds the per-table machinery: a `Capability` (an Arc to the object + rights + badge + a generation snapshot) and a `HandleTable` of opaque `Handle`s (generation<<32 | index) with `insert` / `lookup` (rights-checked) / `lookup_typed` (type-sealed) / `duplicate` (attenuate-only) / `close` (recycles the slot and bumps its generation so the old handle value dies). Six tests cover create/lookup/close, rights enforcement, attenuating duplication, O(1) revocation, type sealing, and Arc-refcount lifetime. (Pure data-structure work - no arch or boot wiring; the table is owned by the Process from M11.)

## M5 - Threads, address spaces, scheduler
- [x] `Thread` object
- [x] `AddressSpace` object
- [x] Context switch
- [x] Run queue
- [x] Scheduler (start simple, evolve)
- Done when: multiple kernel threads multiplex on a core (and across cores).
- Result: `object/thread.rs` (a `Thread` owning its 16 KiB kernel stack + the saved RSP), `object/address_space.rs` (an `AddressSpace` wrapping a CR3; in M5 all kernel threads share the one kernel space), and `arch::context::switch_context` (hand-written `global_asm!` that saves/restores the callee-saved registers and swaps stacks, with a `thread_trampoline`/`thread_bootstrap` that calls the entry then `sched::exit`). `sched.rs` is a cooperative round-robin with PER-CPU run queues (SMP-correct from the start, no migration yet): `spawn`/`spawn_on`, `yield_now`, `exit`, `run_until_idle`. The switch is leak-safe - the outgoing thread is moved into a zombie/requeue slot before the stack swap, so a retiring thread's stack frees cleanly in the context switched to. Tests `thread_object_basics`, `scheduler_multiplexes_threads` (interleaved yielding threads), `scheduler_runs_across_cores`.

## M6 - Syscall ABI
- [x] Syscall entry/exit (`syscall` instruction, register convention)
- [x] Dispatch table
- [x] Minimal syscall set (handle ops, object create, address-space ops)
- Done when: a syscall round-trips there and back.
- Result: `arch::syscall` programs the EFER/STAR/LSTAR/FMASK MSRs and a `global_asm!` `syscall_entry` (register convention: rax = number, rdi/rsi/rdx/r10 = args; FMASK clears IF so the handler runs un-preemptible). In M6 `syscall` is issued from ring 0 by `invoke()` (the ring-3 transition lands in M8); entry returns via `push r11; popfq; jmp rcx` to stay in ring 0. `syscall.rs` is the portable dispatch table with a minimal set (debug noop/write, clock_get, memory_object create/map/unmap, handle duplicate/close) using the Linux-style error convention (success = the value, error = a small negative in [-4095,-1], `sys_is_err`). Object/handle/mapping calls operate on the current thread's handle table. Tests `syscall_roundtrip_stateless` and `syscall_object_and_handle_ops` (create -> map -> write/read-back -> attenuated duplicate -> unmap -> close) are green.

## M7 - IPC
- [x] `Channel` object
- [x] Async send/recv ("message + handle to shared memory")
- [x] Capability transfer over a channel
- [x] `Event` and `Timer` objects
- Done when: two threads exchange a message and a capability over a channel.
- Result: `object/channel.rs` is a connected endpoint pair - each end owns its inbox and holds the peer as a `Weak` so the two ends never form a refcount cycle - with non-blocking `send` (into the peer's bounded queue -> Full/PeerClosed) and `recv` (Empty/PeerClosed); a `Message` carries a byte payload + transferred `Capability`s + a sender badge. Capability transfer over the wire (`SYS_CHANNEL_SEND`/`RECV`) validates the handle (needs TRANSFER), moves the capability into the message, and consumes the source handle only on success (so a failed send drops nothing). `object/event.rs` (a signalable latch) and `object/timer.rs` (one-shot against the LAPIC tick) are pollable objects. A spawned thread is seeded its endpoint as a bootstrap handle (`spawn_with_object`). Tests: `channel_message_and_capability_transfer` (two threads move a marked MemoryObject and read it back through the granted handle), `channel_endpoint_semantics`, `event_timer_objects`, `event_timer_syscalls`. A true blocking `wait` is deferred (consumers poll + `yield_now`).

## M8 - Userspace (ring 3)
- [x] Transition to ring 3
- [x] Load a minimal user thread
- [x] Syscall from userspace
- [x] IPC to a kernel service
- Done when: the first userspace program runs and makes a capability-gated syscall + channel IPC.
- Result: the GDT gains user code/data segments (the slots mandated by SYSRET) plus a per-core RSP0 stack; `arch::usermode::enter`/`exit_to_kernel` are a setjmp/longjmp pair - `enter` parks the kernel RSP in `gs:[KERNEL_RSP]`, builds an `iretq` frame, and drops to ring 3 with the bootstrap handle in rdi. A single `LSTAR` entry serves both the ring-0 self-call (distinguished by the sign bit of the saved RCX) and the ring-3 `syscall` path (stash user rsp/rip/rflags via GS, switch to the kernel stack, `sysretq` back). Paging now propagates the USER bit down every level; the M8 user pages live in the low half of the shared kernel CR3 (per-process CR3 is M11). An embedded position-independent ring-3 program makes a capability-gated `SYS_CHANNEL_SEND` "OK" plus a debug-write, then `SYS_USER_EXIT`; user pointers are range-validated at the boundary. Test `userspace_runs_and_ipcs` asserts the ring-3 program's channel message comes back. (Diagnosed and fixed a latent M3 bug here: the per-CPU MSR const was FS_BASE, not GS_BASE.)

## M9 - Resource accounting
- [x] Per-`Domain` accounting of kernel resources (memory, handles)
- [x] Quota enforcement
- Done when: a domain at its quota cap fails cleanly, not by crashing.
- Result: `object/domain.rs` adds a `Domain` with a `ResourceAccount` of three counters (memory bytes, handle count, thread count), each an atomic check-and-add (`try_charge` is a single CAS, so it is SMP-correct). Charging happens AT THE BOUNDARY of the create paths and is enforced - an over-cap create returns a typed `ERR_RESOURCE_EXHAUSTED` rather than crashing - and cleanup is Drop-based and exactly balanced: `HandleTable::drop` refunds open handles, `MemoryObject::drop` frees frames + refunds memory, `Thread::drop` refunds the slot, so even a crashed thread leaves the account at zero. Memory is charged at object create (a mapping consumes VA, not RAM, so it is not double-counted). Test `domain_quota_enforced_cleanly`: a Domain capped at 2 pages / 4 handles / 4 threads refuses the over-cap create cleanly, and once the thread is reaped every counter is back to zero. (No Process object yet - accounting is reached through the running thread; the handle table is still the M6 Thread stand-in.)

## M10 - IPC latency benchmark (phase 0 gate)
- [x] Measure local `call()` round-trip latency (send + wait + receive)
- [x] Confirm zero-copy for large data (a handle to a shared buffer does not copy the payload)
- [x] Record the numbers so regressions stay visible
- Done when: the IPC round-trip is measured (target: single-digit microseconds) and zero-copy is empirically confirmed - the concept's gate before higher layers are built on IPC.
- Result: raw channel round-trip ~0.76 us, full syscall round-trip ~5.1 us (both within the single-digit-us budget); zero-copy confirmed (a 1 MiB buffer transferred with a 3-byte message, far-end marker read back through the moved capability). TSC-based timing (arch::tsc), boot prints the numbers, `ipc_round_trip_and_zero_copy` test asserts it deterministically.

## M11 - Process and per-process address space
- [x] `Process` object (owns its AddressSpace, handle table, threads; bound to a Domain)
- [x] Per-process `AddressSpace` with its own page tables (switch CR3 on context switch)
- [x] Move the handle table from the Thread (M6 stand-in) onto the Process
- [x] Threads belong to a Process; `process_create` / `thread_create` wired to it
- Done when: two processes run with isolated address spaces, a thread switch reloads CR3, and handle tables are per-process - green under `cargo test`.
- Result: `Process` owns the AddressSpace + handle table + Domain; `Thread` holds an `Arc<Process>` and delegates `address_space()`/`handles()`/`domain()` to it (so the syscall layer is unchanged). `AddressSpace::create` builds private page tables (empty user half, kernel higher half shared by copying PML4 entries 256..512); the scheduler reloads CR3 on every switch and restores the kernel space when a core goes idle. Each kernel thread runs in its own single-thread process. `process_isolation_and_per_process_tables` test: two processes map the same VA to different frames and each reader thread sees only its own (distinct CR3s), and a handle installed in one process is invisible to the other.

## M12 - Fault isolation and crashed-process cleanup
- [x] A userspace page fault / GPF terminates only the faulting `Process`, not the kernel
- [x] Process teardown: close all handles, free frames + address space, refund Domain accounting
- [x] `fault_info_get`: record and expose basic fault info for the killed process
- [x] Other cores and the scheduler keep running across the kill
- Done when: a userspace process that dereferences a bad pointer is killed and fully cleaned up, the kernel survives, and a test asserts the Domain's accounting returns to zero.
- Result: the page-fault and #GP handlers check the saved code selector's CPL; a ring-3 fault records a `FaultInfo` on the running `Process` and longjmps back into the kernel thread that entered ring 3 (reusing `usermode::exit_to_kernel`, the same one-way return as a clean `SYS_USER_EXIT`), so the kernel and every other core keep running. A ring-0 fault still halts loudly. The kernel thread resumes after its `enter` call and unwinds normally; dropping the thread tears the process down, and Drop-based refunds return the Domain's memory/handle/thread quotas (`HandleTable::drop` refunds open handles, `MemoryObject::drop` frees frames + refunds memory). New `fault.rs` (`FaultInfo`, `terminate_user`); `SYS_FAULT_INFO_GET` copies the recorded fault to a user buffer. `enter` now restores the caller's interrupt flag after the excursion (the `ret`-based longjmp does not, unlike `iretq`). `fault_isolation_kills_only_process` test: a ring-3 thread in a bounded Domain writes to an unmapped address; the kernel records the page fault (addr `0xdead000`), terminates the process, survives, and the Domain's memory/handle/thread accounting all return to zero.


## M13 - Domain hierarchy and lifecycle
- [x] Domain tree (parent/child); processes hang under a Domain node
- [x] `domain_create` / `domain_kill` syscalls
- [x] Bulk termination: killing a Domain tears down the whole subtree and cleans up
- [x] Hierarchical limits: a process may exceed neither its own limit nor its Domain's aggregate
- Done when: killing a parent Domain terminates all descendant processes and frees their resources; a test verifies the subtree dies and accounting returns to zero.
- Result: `Domain` now forms a tree - a `Weak` parent link, strong `Arc` children, and `Weak` back-references to the `Process`es that hang under each node (pruned on registration). The per-resource charge methods (`try_charge_memory`/`charge_handle`/`try_charge_thread`/...) charge the node's own `ResourceAccount` and then walk up to every ancestor via `Weak::upgrade`; a `try_*` that fails at an ancestor rolls back the levels it already charged, so a process can exceed neither its own limit nor any enclosing Domain's aggregate. `Domain::kill` does a BFS subtree teardown: it sets a `killed` flag, terminates each live process at the node (collected outside the lock), then recurses into the children. `Process::terminate` flips its own `killed` flag and eagerly runs `HandleTable::close_all` (refunding handles + dropping `MemoryObject` Arcs so frames/memory come back immediately); the threads themselves exit cooperatively - `sched::yield_now` is a kill point that calls `sched::exit()` when it observes its process was killed while descheduled. `SYS_DOMAIN_CREATE` (19) makes a bounded child under the caller's Domain and returns a handle with `Rights::ALL`; `SYS_DOMAIN_KILL` (20) looks up a `Domain` handle requiring `Rights::MANAGE` and kills its subtree. Two new tests: `domain_hierarchy_limits_aggregate` (a child with unlimited local quota still stops at the parent's 8 KiB aggregate, via both the direct API and the `SYS_MEMORY_OBJECT_CREATE` syscall path, and accounting returns to zero after teardown) and `domain_kill_frees_subtree` (two parked ring-0 processes under a child Domain plus a killer thread; after the kill the child's and parent's memory/handle/thread accounting are all zero). Gotcha fixed along the way: `yield_now` must drop its `Arc<Thread>` temporary before calling `exit()` - holding a `Drop`-bearing clone across the no-return `exit()` longjmp leaks the reference and pins the thread slot (the same class of bug as M12's `terminate_user`). 30 tests green.

## M14 - Init package and the first userspace process
- [x] Load a ramdisk / init package as a Limine module
- [x] A minimal ELF loader: map a userspace program from the package into a new Process
- [x] Start `SystemManager` as the first userspace process from the init package
- [x] SystemManager reports in over IPC (the first real userspace service)
- Done when: the kernel loads the init package from a Limine module and runs SystemManager in ring 3 as the first process, which sends its first IPC message.
- Result: a new userspace crate `src/user/system_manager` builds a freestanding `no_std`/`no_main` ELF (its own `x86_64-unknown-none` target dir, `relocation-model=static` so it links non-PIE as `ET_EXEC` with no relocations - the kernel's loader applies none). Its `_start` aligns the stack and calls `__sysmgr_main(bootstrap)` (the bootstrap channel handle arrives in `rdi`, the `enter()` argument), which issues `SYS_CHANNEL_SEND` with `"SystemManager: online"` then `SYS_USER_EXIT`. The kernel's `build.rs` assembles the program into a tiny `LIBERPK1` archive (16-byte header: magic + count + reserved; 32-byte entries: 24-byte NUL-padded name + u32 offset + u32 size; then the concatenated blobs) written to `boot/.build/init.pkg`; if the userspace ELF is absent it writes an empty package and warns, so a bare `cargo build` / rust-analyzer still succeeds. `mkimage.sh` copies `init.pkg` into both the ISO and the disk image, and `limine.conf.in` declares it as a `module_path`. New kernel modules: `pkg.rs` (`Package::parse`/`lookup`, validating ranges), `elf.rs` (`load_into` - validates an LE x86-64 ELF64, maps each `PT_LOAD` segment page-by-page at its `p_vaddr` through the target address space, copying file bytes via the HHDM and zeroing the `.bss` tail), and `loader.rs` (`spawn_elf_process` - `AddressSpace::create`, `elf::load_into`, map a 4-page ring-3 stack just below the 2 GiB line, `Process::new`, hand the program its bootstrap capability, and queue a trampoline thread that drops to ring 3 at the entry point). `Process` now owns the leaf data frames backing the user image + stack (`adopt_frames` + a `Drop` that frees them), since `AddressSpace::drop` reclaims only the page-table structure, not the frames its entries point at. `main.rs` adds a `ModuleRequest`, locates the module by the `init.pkg` path suffix, and a shared `spawn_system_manager()` helper used by both the boot demo (`userspace: SystemManager reported in over IPC: "SystemManager: online"`) and the `init_package_starts_system_manager` test. 31 tests green; SystemManager loads from the init package and runs in ring 3 as the first userspace process. Gotcha confirmed: `x86_64-unknown-none` defaults to PIE (the kernel ELF is `ET_DYN`); forcing `relocation-model=static` on the userspace crate yields a fixed-base `ET_EXEC` with zero relocations, which keeps the kernel loader trivial.

## M15 - Framebuffer text console
- [x] Limine framebuffer request + init
- [x] A bitmap-font text console rendering to the framebuffer
- [x] Mirror the kernel log (and/or a console service) to the framebuffer alongside serial
- Done when: the boot log appears on the QEMU graphical framebuffer, not only on serial.
- Result: a `FramebufferRequest` gets a linear RGB video mode from Limine; `init_framebuffer()` (run in kmain right after GDT/IDT, before the test/boot split) hands its geometry + colour masks to a new portable `console` module. The console renders an embedded public-domain 8x8 bitmap font (dhepper/font8x8, basic latin, shipped as the 1 KiB binary asset `font8x8.bin` and pulled in with `include_bytes!`) at 2x scale, packing each pixel per the mode's red/green/blue mask shift+size, with a cursor, tab/CR/LF handling, and scroll-by-`ptr::copy` when the cursor passes the last row. The log is mirrored, not redirected: `serial_print!`/`serial_println!` now call a crate-root `_print(args)` that writes to the serial port (always, unchanged) and then to `console::write_fmt` (best-effort under a `try_lock`, so a panic mid-print can never deadlock the logger and serial always wins). The console is a no-op if the bootloader provided no framebuffer. Verified by screendumping the headless QEMU framebuffer (1280x800) over the monitor socket: the entire boot log - from `M0: hello from the kernel` through the SystemManager IPC line to `boot OK, halting` - is legible on screen, having scrolled correctly. 31 tests still green; the on-screen mirror needs no test (it is pure output, validated by the screenshot).

## M16 - StorageManager over a ramdisk + vol:// access
- [x] A ramdisk-backed `Volume` (read-only is enough for now)
- [x] `StorageManager` userspace service: `open(path, rights) -> handle`
- [x] `vol://` resolution: a `VolumePath` object + a userspace resolver (the object is canonical, the URI is a representation)
- [x] Read a file's bytes zero-copy (metadata + a handle to a shared buffer)
- Done when: a userspace client opens a path on a `vol://` volume via StorageManager and reads its contents over a channel.
- Result: the ramdisk is a second Limine module `boot/volume.pkg` - a `LIBERPK1` archive (same format as the init package) assembled by the kernel's `build.rs` from every file under `src/volume/` (`hello.txt`, `motd.txt`); `mkimage.sh` stages it next to `init.pkg` and `limine.conf.in` declares the extra `module_path`. The kernel locates it by the `volume.pkg` path suffix, copies its bytes into a fresh `MemoryObject` (the ramdisk) through the HHDM, and hands that object plus two channels to a new `src/user/storage` crate. That crate builds two ring-3 binaries from a shared `runtime.rs` (the `_start` stub, the syscall wrapper, the panic handler, a bounds-checked `LIBERPK1` parser, and a `VolumePath` that parses `vol://<volume>/<path>` into its canonical `(volume, path)` pair): `storage_manager` maps the ramdisk, then serves open requests until the client side closes - each request is `[rights u32][vol:// URI]`, and it answers `[status u32][size u64]`, resolving the URI, looking the path up in the archive, refusing anything beyond read+map on the read-only volume, copying the file's bytes into a freshly created `MemoryObject`, attenuating that handle to exactly the requested rights (plus `TRANSFER`), and handing it across; `storage_client` opens `vol://system/hello.txt`, maps the returned shared buffer, and reports the bytes back. The whole exchange is zero-copy: the file content crosses as a shared `MemoryObject` capability, never as channel bytes. To make a ring-3 process able to read such a buffer, `sys_memory_map` now maps into the caller's own user (lower-half) address space with the `USER` bit when the call comes from ring 3 (a separate user mmap window), keeping the ring-0 kernel-window behaviour unchanged; the two cooperating processes spin on `WOULD_BLOCK` with `SYS_YIELD`, so this also exercises the yield-safe syscall path. The kernel only brokers the three initial capabilities (ramdisk, service-server, service-client) with object-level sends before `run_until_idle`; the open, the resolve, the rights check, and the read all happen in userspace. New test `storage_serves_volume_file_to_client` asserts the bytes the client read equal the file straight from the volume archive; the boot demo prints `storage: client read "Hello from the LiberSystem ramdisk!" from vol://system/hello.txt via StorageManager`. 33 tests green.

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
