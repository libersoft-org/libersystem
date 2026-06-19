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
- Result: a new userspace crate `src/user/system_manager` builds a freestanding `no_std`/`no_main` ELF (its own `x86_64-unknown-none` target dir, `relocation-model=static` so it links non-PIE as `ET_EXEC` with no relocations - the kernel's loader applies none). Its `_start` aligns the stack and calls `__sysmgr_main(bootstrap)` (the bootstrap channel handle arrives in `rdi`, the `enter()` argument), which issues `SYS_CHANNEL_SEND` with `"SystemManager: online"` then `SYS_USER_EXIT`. The kernel's `build.rs` assembles the program into a tiny `PKGARCH1` archive (16-byte header: magic + count + reserved; 32-byte entries: 24-byte NUL-padded name + u32 offset + u32 size; then the concatenated blobs) written to `boot/.build/init.pkg`; if the userspace ELF is absent it writes an empty package and warns, so a bare `cargo build` / rust-analyzer still succeeds. `mkimage.sh` copies `init.pkg` into both the ISO and the disk image, and `limine.conf.in` declares it as a `module_path`. New kernel modules: `pkg.rs` (`Package::parse`/`lookup`, validating ranges), `elf.rs` (`load_into` - validates an LE x86-64 ELF64, maps each `PT_LOAD` segment page-by-page at its `p_vaddr` through the target address space, copying file bytes via the HHDM and zeroing the `.bss` tail), and `loader.rs` (`spawn_elf_process` - `AddressSpace::create`, `elf::load_into`, map a 4-page ring-3 stack just below the 2 GiB line, `Process::new`, hand the program its bootstrap capability, and queue a trampoline thread that drops to ring 3 at the entry point). `Process` now owns the leaf data frames backing the user image + stack (`adopt_frames` + a `Drop` that frees them), since `AddressSpace::drop` reclaims only the page-table structure, not the frames its entries point at. `main.rs` adds a `ModuleRequest`, locates the module by the `init.pkg` path suffix, and a shared `spawn_system_manager()` helper used by both the boot demo (`userspace: SystemManager reported in over IPC: "SystemManager: online"`) and the `init_package_starts_system_manager` test. 31 tests green; SystemManager loads from the init package and runs in ring 3 as the first userspace process. Gotcha confirmed: `x86_64-unknown-none` defaults to PIE (the kernel ELF is `ET_DYN`); forcing `relocation-model=static` on the userspace crate yields a fixed-base `ET_EXEC` with zero relocations, which keeps the kernel loader trivial.

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
- Result: the ramdisk is a second Limine module `boot/volume.pkg` - a `PKGARCH1` archive (same format as the init package) assembled by the kernel's `build.rs` from every file under `src/volume/` (`hello.txt`, `motd.txt`); `mkimage.sh` stages it next to `init.pkg` and `limine.conf.in` declares the extra `module_path`. The kernel locates it by the `volume.pkg` path suffix, copies its bytes into a fresh `MemoryObject` (the ramdisk) through the HHDM, and hands that object plus two channels to a new `src/user/storage` crate. That crate builds two ring-3 binaries from a shared `runtime.rs` (the `_start` stub, the syscall wrapper, the panic handler, a bounds-checked `PKGARCH1` parser, and a `VolumePath` that parses `vol://<volume>/<path>` into its canonical `(volume, path)` pair): `storage_manager` maps the ramdisk, then serves open requests until the client side closes - each request is `[rights u32][vol:// URI]`, and it answers `[status u32][size u64]`, resolving the URI, looking the path up in the archive, refusing anything beyond read+map on the read-only volume, copying the file's bytes into a freshly created `MemoryObject`, attenuating that handle to exactly the requested rights (plus `TRANSFER`), and handing it across; `storage_client` opens `vol://system/hello.txt`, maps the returned shared buffer, and reports the bytes back. The whole exchange is zero-copy: the file content crosses as a shared `MemoryObject` capability, never as channel bytes. To make a ring-3 process able to read such a buffer, `sys_memory_map` now maps into the caller's own user (lower-half) address space with the `USER` bit when the call comes from ring 3 (a separate user mmap window), keeping the ring-0 kernel-window behaviour unchanged; the two cooperating processes spin on `WOULD_BLOCK` with `SYS_YIELD`, so this also exercises the yield-safe syscall path. The kernel only brokers the three initial capabilities (ramdisk, service-server, service-client) with object-level sends before `run_until_idle`; the open, the resolve, the rights check, and the read all happen in userspace. New test `storage_serves_volume_file_to_client` asserts the bytes the client read equal the file straight from the volume archive; the boot demo prints `storage: client read "Hello from the LiberSystem ramdisk!" from vol://system/hello.txt via StorageManager`. 33 tests green.

## M17 - Simple CLI + basic System Graph
- [x] A minimal CLI component over serial (read a line, run a command, print a typed result)
- [x] `object_info_get` introspection plumbing
- [x] A basic System Graph: enumerate live Domains -> processes -> handles/channels
- [x] CLI commands: list a volume, print a file, dump the System Graph
- Done when: a command typed into the CLI round-trips to a service and the CLI can print the System Graph from live state.
- Result: the serial UART gained a non-blocking `read_byte` and a spinning `read_byte_blocking` (polling LSR bit 0), and `cli::run_interactive` reads a line at a time, echoing keystrokes and handling backspace, until `exit`. The shell understands `help`, `ls <vol://volume>`, `cat <vol://vol/path>`, and `graph`. `ls` parses the `vol://` prefix, fetches the volume's `PKGARCH1` archive, and prints each file name and size; `cat` round-trips to the real `StorageManager` (the same userspace service from M16) and prints the returned bytes. The introspection path is a new syscall `SYS_OBJECT_INFO_GET` (22): given a handle it writes a `#[repr(C)] ObjectInfo { koid, object_type, rights, generation }` into a caller buffer, where `object_type` is a stable ABI code (`ObjectType::code`, Domain=0 .. DmaBuffer=10) decoupled from the in-memory enum order, and an unknown handle returns the bad-handle error. The System Graph (`graph.rs`) walks the live tree from `sched::root_domain()`: for each Domain it records the quota usage and its live processes (each process is enumerated through a new `HandleTable::entries`, which snapshots every live capability as a `HandleInfo { koid, object_type, rights, badge, generation }`), then recurses into child Domains; `render` prints it as an indented tree (memory `used/limit` with `inf` for unlimited, then `process koid=N (M handles)` and one `handle koid=K Type rights=0x.. badge=B` line per capability). For the kernel to drive the userspace storage service as its *own* client without deadlocking the cooperative scheduler (a persistent server busy-yielding would never let `run_until_idle` drain the ready queue), `storage_read` sends the open request followed by an empty-message QUIT sentinel up front; the `StorageManager` serve loop now treats a zero-length message as "stop", so it drains its pre-queued inbox in a single pass and exits, after which the kernel reads the reply (kept alive by the shared-buffer capability still sitting in the client endpoint's inbox) and copies the file out through the HHDM. Default boot now ends by printing `boot OK` and entering the interactive `liber>` prompt (was: halt immediately); piped/automated flows still key on the `boot OK` line, `just test` never runs `boot_main`, and typing `exit` prints `halting` and idle-spins. Verified on both serial and the framebuffer screenshot - the scripted demo shows `help`, `ls vol://system` (hello.txt 36 bytes, motd.txt 117 bytes), `cat vol://system/hello.txt` ("Hello from the LiberSystem ramdisk!"), and `graph` (root Domain with two sample processes carrying a Channel, a MemoryObject, and an Event handle, each with its koid/type/rights/badge) - and three new tests bring the suite to 36 green: `object_info_get_reports_object` (the syscall reports the right koid/type/rights and rejects a bogus handle), `system_graph_reflects_live_state` (a standalone Domain's graph mirrors its one process and two handles, then shows zero processes after the process drops), and `cli_reads_file_through_storage_service` (the `cat` path's bytes equal the file straight from the volume archive). This completes phase 0.

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

Two kernel features the concept lists (kernel components: "Event/wait primitives",
the scheduler) are intentionally met by a cooperative workaround in phase 0 and
finished in phase 1: the blocking `wait` primitive (phase 0 uses non-blocking IPC
+ `SYS_YIELD` polling) and preemptive scheduling (phase 0 is cooperative). This is
consistent with the concept's "scheduler ... running on a single core for now"
framing - see phase 1, M18-M19.

# Phase 1 - First usable userspace

Phase 1 goal (from CONCEPT "Roadmap -> Phase 1"): a first usable userspace on top
of the phase-0 microkernel. The kernel gains the blocking `wait` primitive and
preemption; userspace gains the driver-enabling kernel calls; a ServiceManager
brings up the core services (Process, Storage, Log, Device, Config) in dependency
order per the concept's boot flow; virtio drivers (blk, net, console) replace the
phase-0 ramdisk/serial stand-ins; an IDL/WIT toolchain generates the typed API
bindings; a minimal WASI host runs the first Wasm component with only the
capabilities it is granted; and a powerbox file picker hands a component a file
handle. Target deployment is appliance/edge in a VM (virtio on QEMU/KVM). Per the
concept, phases 0-2 are a real near-term goal for one person or a small team.

Ordering note: the concept lists "IDL/WIT toolchain" first, but its own procedure
says to "decide after a real trial, not in advance" - so the first services
(M21-M24) speak hand-written protocols (as phase-0 StorageManager already does),
the IDL/WIT toolchain (M25) is then trialled on those real interfaces, and the
later services (M26-M27) adopt the generated bindings. The two kernel items below
(M18-M19) were moved here from phase 0, where they were met by a cooperative
workaround.

Everything stays SMP-aware and capability-first from the start (no ambient
authority - the HARD RULE holds from the MVP: a component gets only explicitly
passed capabilities).

System Graph note: phase 0 already has the basic graph (Domains -> processes ->
handles), and it will automatically pick up the new services/drivers as ordinary
processes + handles the moment they run. The SEMANTIC enrichment (labeling nodes
as Service/Driver/Device, drawing device<->driver edges and dependencies, showing
crash/restart state) plus the JSON/CBOR/CLI representations, tracing, and counters
are the concept's "full System Graph" = phase 2 observability, not phase 1.

## M18 - Blocking `wait` primitive
- [x] `Blocked` thread state + a wait registry (readiness: a Channel readable or peer-closed, an `Event` signaled, a `Timer` expired)
- [x] `SYS_WAIT`: block the calling thread until the object behind a handle is ready, with an optional deadline (absolute monotonic ticks; 0 = none)
- [x] Wake-on-signal: `channel_send` and a closing endpoint wake receivers; `event_signal` wakes waiters; a `Timer`/deadline wakes via the scheduler's deadline check
- [x] Replace the cooperative `WOULD_BLOCK` + `SYS_YIELD` poll loops: the StorageManager serve loop now blocks in `wait` (done). The CLI serial read still busy-spins - a real wait there needs a UART RX interrupt, deferred to a later step.
- Done when: a server thread sleeps in `wait` at ~0% CPU until a message arrives then runs; a deadline wakes a waiter on timeout; the M10 IPC round-trip is re-measured with real blocking (still within the single-digit-us budget).
- Concept: IPC model ("the only place that blocks is `wait`", `call() = send + wait + receive`, backpressure via `wait`), Syscall model (`wait`).
- Result: the scheduler gained a `Blocked` thread state and a global wait registry (`WAITERS: SpinLock<Vec<Waiter{thread, koid, deadline}>>`). `sched::block_on(koid, deadline)` sets the caller Blocked, parks it in the registry (the Arc keeps it alive off every run queue), and reschedules with a new `Disposition::Block` that saves the thread's stack without requeueing or zombie-ing it; the thread resumes from exactly that point when woken (the resume path restores the interrupt flag the same way the yield path does). `sched::wake_object(koid)` drains matching waiters back onto the run queue; `check_deadlines()` does the same for waiters whose deadline has passed. The waitable objects wake their registry entries directly: `Channel::send` wakes the peer endpoint (now readable), `Channel`'s `Drop` wakes the peer (so a blocked receiver observes peer-close), and `Event::signal` wakes its own waiters; a `Timer`/timeout wakes through the deadline check. `SYS_WAIT` (23) re-checks readiness in a condition-variable loop (so an early or spurious wake just re-blocks) and returns 0 when the object is ready or `ERR_TIMED_OUT` (-11) at the deadline; waiting on a `Timer` caps the block at the timer's own deadline. `run_until_idle` drives timed waits: when the run queue drains while threads are blocked with a deadline, it spins to the nearest deadline and wakes them. The StorageManager's serve loop now blocks in `wait` instead of yielding (validating `SYS_WAIT` from ring 3); the M16 kernel-as-client path is unchanged (its pre-queued QUIT sentinel makes the manager exit before it would block, and a closing client wakes it via peer-close). Two tests bring the suite to 38 green: `blocking_wait_wakes_on_message` (a server blocks in `wait`, a client's send wakes it and it recv's) and `blocking_wait_times_out_on_deadline` (a wait on an unsignaled event returns `ERR_TIMED_OUT` once the deadline passes). Simplifications, all deferred: `wait` takes a single object (not a set - no `wait_many` yet); "writable" readiness / waking senders blocked on a full queue is not wired (send-on-full still yields); and the wait/wake is correct for the cooperative, BSP-driven scheduler - APs deliberately do not touch the registry, so cross-core wake-during-block hardening and per-core preemptive deadline wakeups pair with M19 (preemption). A nasty flaky bug was fixed during bring-up: having the AP idle loop call `check_deadlines()` let an AP steal a BSP-blocked waiter onto its own run queue, so `run_until_idle` returned before the waiter ran - APs now leave the registry alone.

## M19 - Preemptive scheduling
- [x] Timer-driven preemption: the LAPIC tick can deschedule the running thread (a time slice / quantum)
- [x] Interrupt-safe scheduler state (the run queues are safe to touch from the timer ISR; spinlocks become interrupt-aware)
- [x] Full register-state save/restore on a preemptive switch (not just callee-saved, unlike the cooperative path)
- [x] Fair round-robin under preemption, still per-CPU
- Done when: a CPU-bound thread that never yields is preempted and other threads on the same core keep running; the whole test suite stays green with preemption enabled.
- Concept: phase 0 was cooperative "running on a single core for now"; preemption is the scheduler evolution ("start simple, evolve").
- Result: the LAPIC periodic timer (100 Hz) now preempts. The foundation is an interrupt-safe `SpinLock` (sync.rs): `lock` reads the interrupt flag and disables interrupts before acquiring, and the guard restores the prior state on drop, so a lock holder can never be preempted and an interrupt handler can never deadlock against a lock it interrupted (nested locks restore correctly - only the outermost re-enables). The timer gets a dedicated preemptive IDT stub (`interrupts::timer`) instead of the generic count-and-dispatch path: it bumps the tick counter, signals EOI *before* any switch (so the LAPIC keeps delivering while the thread is descheduled), and - only when it interrupted ring-0 thread code (`frame.code_segment & 3 == 0`) - calls `sched::on_timer_preempt`, which rotates to the next ready thread on the same core via `reschedule(Disposition::Requeue)` (a one-tick / 10 ms quantum; a no-op when the core is idle or the thread is alone, so a sole thread keeps running). The preemptive switch reuses the cooperative `switch_context`: the interrupted thread's caller-saved registers and `iret` frame are saved on its own kernel stack by the `x86-interrupt` prologue and `switch_context` saves the callee-saved set, so the full register state is preserved (the kernel is built `-sse,+soft-float` - verified zero XMM instructions in the binary - so there is no FPU/SSE state to save); resuming the thread re-enters the ISR tail and `iretq`s back to exactly where it was. `reschedule` now disables interrupts across the whole switch (so the timer cannot fire between dropping the run-queue lock and completing `switch_context`) and restores the captured flag on every resume/return path; a thread preempted by the timer captured `resume_if = false` and stays masked through the ISR tail, after which `iretq` restores its real flag, while a cooperative yielder is restored to enabled. New threads enable interrupts in `thread_bootstrap` (they return into the trampoline rather than back through `reschedule`, so they enable interrupts themselves to match a resumed thread). Preemption is gated behind `PREEMPTION_ENABLED`, set at the end of `sched::init()`, so the timer can count ticks during early boot - before per-CPU state and the scheduler are ready - without the preempt path touching either. Ring-3 preemption is deferred: `TSS.RSP0` is per-core (not per-thread), so a ring-3 interrupt lands on the shared per-core stack and switching from there would not travel with the thread; userspace stays cooperative (it blocks in `wait`, and syscalls run masked via FMASK) until per-thread RSP0 lands with the real drivers (M20+). New test `preemption_preempts_a_cpu_bound_thread` spawns a never-yielding CPU-bound kernel thread plus a cohabiting thread on the same core; only timer-driven preemption lets the cohabitant run and release the hog, so the test would hang without preemption - it passes, bringing the suite to 39 green (verified stable over 20 consecutive runs), fmt clean, boot OK.

## M20 - Kernel additions: driver + spawn syscalls, queue/DMA accounting
- [ ] `interrupt_bind`: hand a device IRQ to a userspace driver (delivered as an `Event`/Channel signal)
- [x] `device_memory_map`: map an MMIO region into a driver's address space (capability-gated)
- [x] `dma_buffer_create`: allocate a DMA-safe buffer and its handle
- [x] These three syscalls materialize the `Interrupt` / `DeviceMemory` / `DmaBuffer` kernel objects (the `ObjectType` variants have existed since M4; phase 1 implements the objects behind them)
- [ ] `process_create` / `thread_create` / `thread_start` exposed to userspace (capability-gated): a userspace spawner builds an empty process + address space, loads an image into it via the existing `memory_object_create` / `memory_map` syscalls, then creates and starts its thread. Phase 0 spawned ELFs only from kernel code (`loader::spawn_elf_process`); ServiceManager/ProcessService (M21/M27) need this to start services from userspace.
- [x] `random_get` (kernel CSPRNG) and `object_property_set` (name / limit / ...)
- [x] Extend resource accounting to `ipc_queue_bytes` (a queued message is charged to the SENDER's Domain until the receiver takes it - the anti-DoS / backpressure rule, with `send` returning `WOULD_BLOCK` when the receiver's queue is full) and `dma_bytes` (pinned DMA memory). Phase 0 enforces only memory/handles/threads; the concept adds queues + DMA "with IPC and drivers".
- [ ] Kernel-side driver-crash cleanup: on a driver fault, detach its IRQ, disable its DMA, remove its capabilities, free its memory, and send an event to ServiceManager
- Done when: a userspace process binds a (test) interrupt, maps an MMIO page, creates a DMA buffer, and spawns a second process from userspace; queue + DMA accounting is enforced (a full queue returns `WOULD_BLOCK`, a DMA over-cap fails cleanly); a forced driver crash is cleaned up by the kernel (IRQ detached, DMA disabled, caps removed) with an event delivered.
- Concept: Syscall model (interrupt_bind / device_memory_map / dma_buffer_create / process_create / thread_create / thread_start / object_property_set / random_get), Resource accounting ("queues and DMA will be added with IPC and drivers"; `ipc_queue_bytes`, `dma_bytes`; the in-transit message is charged to the sender), Drivers ("Driver crash" - the kernel only safely cleans up and sends an event).

## M21 - ServiceManager and the boot chain
- [ ] ServiceManager (basic): start/stop services, dependency ordering, service-state tracking
- [ ] The boot chain per the concept: SystemManager -> ServiceManager -> DeviceManager + LogService + StorageManager, then the CLI as an ordinary component
- [ ] Move the CLI to a userspace shell component (phase 0's CLI is kernel-embedded in `cli.rs`): it talks to the services over IPC and is started as an ordinary component at the end of the boot chain
- [ ] SystemManager recovery: on a crash, start a recovery SystemManager / an emergency shell / safely restart userspace / reboot / panic as the last resort
- Done when: SystemManager starts ServiceManager, which brings up the core services in dependency order, the shell runs as a userspace component, and a deliberately crashed SystemManager triggers the minimal recovery path.
- Note: a full restart policy + heartbeat/watchdog is phase 2 (see "Out of scope for phase 1").
- Concept: Boot flow, SystemManager + "Recovery on a SystemManager crash", ServiceManager.

## M22 - LogService (structured logging)
- [ ] `LogRecord { ts, severity, source, fields }` as the canonical object (structured data, not lines of text - the journald model, not syslog)
- [ ] A LogService that ingests records over IPC and answers structured queries
- [ ] Representations of the same records: human CLI, JSON, CBOR
- Done when: services emit typed `LogRecord`s to LogService and a query returns structured results renderable as text / JSON / CBOR.
- Concept: System API model (Logs row, "the object is canonical"), Examples of services (LogService).

## M23 - DeviceManager + virtio transport
- [ ] DeviceManager: device detection, mapping devices -> drivers, assigning each driver exactly the device capabilities it needs, device-state tracking, reacting to a driver-crash event
- [ ] The shared virtio transport (virtio-mmio / PCI discovery, virtqueues) used by all the drivers
- Done when: DeviceManager enumerates the QEMU virtio devices and launches the matching driver for each, handing it only its device's capabilities.
- Concept: DeviceManager, Drivers ("MVP: only virtio on QEMU/KVM").

## M24 - virtio drivers (headless): blk, net, console
- [ ] `driver.virtio-blk` (block storage)
- [ ] `driver.virtio-net` (the network *driver* only; the network stack is phase 2)
- [ ] `driver.virtio-console` (serial console / log over virtio)
- Done when: each driver runs as an isolated userspace process, drives its virtio device over virtqueues, and survives a driver-crash/restart cycle via DeviceManager + ServiceManager.
- Concept: Drivers (virtio-blk / virtio-net / virtio-console; drivers are isolated userspace services); the net *stack* is explicitly phase 2.

## M25 - IDL/WIT toolchain and generators
- [ ] Write 5-6 REAL interfaces (not hello-world): `Storage.Volume`, `Process`, `Log`, a Channel with handle passing, an `EventStream` with backpressure (+ `Transfer` across volumes)
- [ ] Generators from the IDL: the binary IPC layout, a Rust client, a CLI formatter, JSON and CBOR schemas, generated documentation, compatibility tests (and, optionally, a C ABI binding)
- [ ] The generated client provides the synchronous-looking `call(req) -> resp` (internally `send` + M18 `wait` + `receive`, with a correlation id and a reply-handle) on top of the non-blocking Channel; one-way `EventStream`s are read via `wait` (no polling) - the request/response and event-stream conventions from the IPC model
- [ ] Find where WIT chafes (handle passing, zero-copy buffers, streams, ABI stability), then decide: WIT-as-IDL vs WIT types + our own binary backend vs our own IDL
- Done when: at least one real service speaks over generated bindings, the same call renders as binary / CLI / JSON, generated docs + compatibility tests exist, and the "WIT vs own" decision is recorded from practice (not from the armchair).
- Concept: IDL language (the full generator list: binary layout, CBOR/JSON schema, Rust client, optional C ABI binding, CLI formatter, documentation, compatibility tests), IPC model (`call() = send + wait + receive`, request/response via correlation id + reply-handle, event streams via `wait`), "Relationship to WIT", "Decide after a real trial, not in advance".

## M26 - StorageService over virtio-blk
- [ ] Evolve the phase-0 ramdisk StorageManager into a StorageService backed by `driver.virtio-blk`
- [ ] `vol://` volumes over a real block device (the storage model: a path belongs to exactly one volume; if the volume is gone, the operation fails)
- [ ] The path is a typed object - `VolumePath { volume: VolumeId, path: RelativePath }`, where `RelativePath` is a list of validated segments (not a string), so `..`/`/` path traversal has nowhere to arise; the URI is just a representation, authority is the capability
- [ ] The `Storage.Volume` interface from the IDL (Open / Stat / Watch), zero-copy reads via shared buffers
- Done when: a client opens and reads a file on a `vol://` volume backed by a virtio-blk device through the typed `Storage.Volume` API.
- Note: a persistent native filesystem (CoW / checksums / snapshots) is phase 2; phase 1 may use a simple read-mostly on-disk layout.
- Note: phase 1 covers `vol://` only; the broader namespace resolvers (`user://`, `appdata://`, `cache://`, `runtime://`, per-process namespace composition) and detailed `storage://` disk/partition/volume admin + cross-volume `Transfer` are deferred (the concept's storage ergonomics are "a later phase - the direction is fixed, not the API").
- Concept: Storage model (volumes, `vol://`, "a path is an object, a URI is a representation", typed `VolumePath`/`RelativePath`), core services (Storage); the persistent native FS is phase 2.

## M27 - Core services: Process, Device, Config
- [ ] ProcessService: process lifecycle (create / start / exit / info) as a typed service over the kernel syscalls
- [ ] DeviceService: typed device enumeration / info on top of DeviceManager
- [ ] ConfigService: a typed `ConfigNode` tree with an IDL schema (no textual `/etc` parsing; text is only an editable representation)
- Done when: the Process / Device / Config services answer typed queries over IPC, renderable as CLI / JSON / CBOR. Together with LogService (M22) and StorageService (M26) this completes the phase-1 "Process, Storage, Log, Device, Config" set.
- Concept: Examples of services, System API model (Configuration row), core services list.

## M28 - Minimal WASI host: the first Wasm component
- [ ] A WASI host runtime process that maps `wasi:*` imports onto our typed services over IPC channels (e.g. `wasi:filesystem` -> StorageService)
- [ ] A WASI "world" = the set of capabilities a component receives at startup (no ambient authority)
- [ ] Run the first real Wasm component end-to-end
- Done when: a Wasm component runs under the host, performs a capability-gated operation (e.g. reads a file it was granted) via a WASI import mapped to a native service, and has no access it was not explicitly given.
- Note: the full Component Model + WASI preview 2 + an SDK + AOT compilation is phase 2; phase 1 is the minimal host + first component.
- Concept: Application model ("WASI as one of several hosts on top of a stable native ABI", "How it fits into the system"), roadmap ("minimal WASI host: running the first Wasm component").

## M29 - Prototype file picker (powerbox)
- [ ] A file-picker service that returns a file *handle* (capability), granted by the user's act of picking - not ambient filesystem access
- [ ] A Wasm component obtains file access only through the picker (the powerbox pattern)
- Done when: a component with no filesystem capability gains access to exactly one user-picked file via the picker, and to nothing else.
- Concept: Security model ("a file picker returning a file handle"), roadmap ("a prototype file picker (powerbox)"), the HARD RULE (no ambient authority).

## Definition of done (phase 1)
Phase 1 is done when the kernel preempts and offers a real blocking `wait`,
userspace virtio drivers (blk / net / console) run isolated under DeviceManager +
ServiceManager, the core services (Process, Storage, Log, Device, Config) answer
typed queries generated from the IDL/WIT toolchain, a minimal WASI host runs the
first Wasm component with only the capabilities it was granted, and a powerbox
file picker hands a component a file handle - all in a VM over virtio on QEMU/KVM,
testable under `cargo test` / QEMU.

## Out of scope for phase 1 (= phase 2, the appliance/edge platform)
A full network stack over virtio-net (the priority of phase 2 - on the edge,
networking is the core); observability and remote admin (the full System Graph
with services/drivers/devices/dependencies labeled, crash/restart state, tracing,
counters, and JSON/CBOR/CLI representations everywhere); security hardening (a
strict app sandbox, permission manifests, a threat model) and the PermissionManager
policy service; the ResourceManager policy service (the kernel already enforces
accounting from phase 0; the policy layer is later); ServiceManager with a full
restart policy + watchdog; the full Component Model + WASI preview 2 + a Rust/C/Go
SDK; a package/app format with installation + AOT compilation; a simple persistent
native filesystem. Also not phase 1: the `virtio-gpu` / `virtio-input` drivers
(headless phase 1 only - they belong to the desktop, phase 5) and any POSIX-like /
relibc compatibility layer (phase 3, server). Wall-clock time (a `TimeService`
computing `UTC = clock_get + offset`) is also deferred - it needs an RTC driver or
NTP, neither available in headless phase 1 (the kernel's monotonic `clock_get` is
enough for phase-1 timeouts, deadlines, and `LogRecord` timestamps). Phases 3-6
(server / real hardware / desktop / AI) are vision, contingent on a community
forming.
