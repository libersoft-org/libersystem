IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0191 (2026-09-27T21:23:49Z):

Status: IN PROGRESS (parts a and b first, in the owner's agreed order; c and d follow P02M0196a). This record is updated as the work proceeds; the final state is at its end.

### Progress - P02M0191a and P02M0191b (written while the first full build ran)

THE ABI (`src/abi/src/lib.rs`, additive, `ABI_VERSION` stays 1): `SYS_DEVICE_RESOURCE_ACQUIRE = 91` (claim, kind, index; kind `RESOURCE_KIND_PORT_RANGE = 1`, the call P02M0196a gives its other kinds), `SYS_PORT_RANGE_MAP = 92`, `SYS_PORT_RANGE_UNMAP = 93` (1 confirmed / 0 unconfirmed), `SYS_PORT_RANGE_FIRMWARE = 94` (privilege, base, len - the one address-taking mint); `OBJECT_TYPE_PORT_RANGE = 15`; `PortResource { base, len, source, index, _pad }` with `PORT_SOURCE_IO_BAR / DERIVED / PLATFORM`; `DeviceInfo` gains `port_count` (the first tail byte) and `ports: [PortResource; MAX_PORT_RESOURCES = 8]` (56 -> 120 bytes). `abi` tests: the syscall and object-type snapshots and both layouts updated - 28 pass.

THE OBJECT AND THE GRANT TABLE (`src/kernel/object/port_range/{mod.rs,grants.rs}`, portable):
- `grants`: one table under one lock - the FIXED reserved part (DMA controllers 0x00..0x1F with the alias at 0x10 and 0xC0..0xDF, PICs, PIT and 0x61, CMOS 0x70..0x71, 0xCF8 and 0xCFA..0xCFF (not 0xCF9), fw_cfg 0x510..0x51B; the test build adds 0xF4..0xF7), the FIRMWARE part (FADT blocks, recorded before the scan), run-time INSTALLS (per kernel item, counted; refused while granted or retired; the same item re-installs its own range), and the live grant SPANS (owner = the range's koid; an unconfirmed end rewrites the span as RETIRED in place, so retiring can never fail for memory). `grant` checks and inserts in one step; `recordable` is the record-time check; COM1 is installed by the kernel console item before the boot scan (`reserve_firmware_ports`), which is the run-time rule's first production caller; `uninstall` is test-only until the COM1 handoff (P02M0191c) calls it. `TERMINAL` (0x64, 0xCF9, 0x600/0x604/0xB004) is the tests' statement of the terminal category: never reserved.
- `PortRange`: base, len, optional claim key, state Idle / Mapped(Weak<Process>) / Ended. `mint` allocates the object first, then takes the grant (koid as owner). `map_into` (bitmap allocated fallibly first, list slot reserved, `begin_extend` held, no round - grants are pulled), `unmap_from` (caller's own; round; unconfirmed -> Ended + retired), `revoke` (for the claim's release; ends the grant, freed when confirmed, retired when not), `give_back` for a terminating process (`Process::unmap_objects` -> `give_back_all`). `take_back` = clear bits as one change, release the process lock, `arch::ioports::reload_if_running`, then ONE `tlb::shootdown()` round; claim-derived ranges count on the claim with `device::mmio_mapping_installed/torn_down`, so a release waits for a teardown in flight (`settled_mmio`) and an unconfirmed round quarantines it exactly as an MMIO flush does.
- `IoPorts` (per process, in `Process`): a lazily allocated 8 KiB bitmap of `AtomicU64` words in TSS polarity, `words` (how many leading words a copy needs), the seqlock generation (odd while changing; changes under the process's `change` lock with interrupts masked, allocating nothing), and the list of mapped ranges. Freed in `Process::drop`.

x86_64 (`src/kernel/arch/x86_64/{gdt,percpu,ioports,idt/mod,pci/mod}.rs`, `arch/common/pci/mod.rs`):
- Every core's TSS carries the 8 KiB bitmap and the trailing 0xFF byte directly after it (const-asserted), the limit covers both, the area is filled with ones at install (an AP's area arrives zeroed) and the map base starts past the limit (`IOMAP_REFUSE`).
- The per-CPU `IoCore` record: bitmap and map-base addresses, the running process (set by `switch_in`, cleared by `switch_to_idle`), the loaded koid and generation, and how many words a previous copy dirtied. `load` copies consistently with one generation (bounded, 16 attempts; past it: refuse-all, nothing recorded), restores to all ones every word a previous copy wrote past the new extent, and uses the refuse-all map base for a process with no range (copying nothing).
- Hooks: `sched::reschedule_as` (thread switch -> `switch_in`; the three idle switches -> `switch_to_idle`), `tlb::service_pending` (when a round is pending, `ioports::service()` before the flush and the acknowledgement - it reads only the core's record and masks interrupts itself), the ring-3 #GP handler (`pull_on_gp`: behind -> copy and retry; current -> the fault stands).
- Sources: `common::pci::io_bars` (endpoints only; a function with no I/O BAR is never written; sized with I/O decode off inside one serialised access; base 0 or past 0xFFFF not recorded), `set_io_decode`; `ioports::function_ports` (I/O BARs + derivation rows) and `firmware_blocks` (FADT PM1a/b event and control, PM2, PM timer, GPE0/1, SMI command). The derivation table ships EMPTY; the test build carries two rows over q35's ICH9 LPC bridge (PMBASE 0x40 & 0xFF80, ACPI_EN 0x44 bit 7, agreeing with the FADT's PM1a event block): TCO (+0x60, 32) and PM1 (+0x00, 4).
- aarch64/riscv64: `arch::ioports` stubs - unsupported, nothing recorded, nothing loaded.

DEVICE ROWS AND CLAIMS (`src/kernel/device.rs`): `DeviceEntry` gains `port_count` / `ports`; `init` reserves the firmware part first, then after the rows exist records each row's candidates through `grants::recordable` (refused with a line); hot-plug `arrive` records too; `claim` enables I/O decode for a row with an I/O BAR resource and `release_claim` disables it (bus-master order); `revoke_effects_of` gains the PortRange arm; `port_resource(index, which)`; test-only `add_synthetic_device_with_ports`.

SYSCALLS (`src/kernel/syscall/mod.rs`): `sys_device_resource_acquire` (MANAGE on the claim, settled refused, index into the row, handle reserved, mint re-checks, `register_derived`, handle with `MAP | TRANSFER`), `sys_port_range_map` / `_unmap`, `sys_port_range_firmware` (`PrivilegeKind::FirmwareInterpreter`, new; nothing mints it at boot yet - P02M0196b's ACPI service is its holder); `sys_device_info` fills the ports.

USERSPACE: `driver_protocol::ResourceKind::PortRange = 7` and `MAX_PORT_RANGES`; `driver-binding::MAX_BIND_RESOURCES = 6 + MAX_PORT_RANGES`; DeviceManager mints every port resource the claimed row carries and passes each as a `PortRange` resource frame (a refused mint ends the attempt, named); `drivers::common::Resources` keeps them in row order; `rt::{device_resource_acquire, port_range_map, port_range_unmap}` and, on x86_64, `rt::port::{inb, inw, inl, outb, outw, outl}`.

THE TEST MACHINE: `qemu-run.sh` gives the x86_64 test machine `-chardev null,id=uart2 -device isa-serial,iobase=0x2f8,irq=3,chardev=uart2`.

TESTS WRITTEN (`src/kernel/object/port_range/tests.rs`, ids `kernel.object.port_range.*`): the run-time install rules; every reserved port refused at the mint (fixed at both ends of each entry, 0x10, the exit port, the FADT blocks, COM1 through the firmware path and a claim, a one-port overlap, a second grant, past 0xFFFF, no ports, the wrong privilege) and the terminal ports and the DMA page registers mintable; on x86_64 through ring-3 probes: a claimed range's loopback round trip on 0x2F8 and a port beside it ending that probe alone, the claim unaffected and its release freeing the ports; a release under a thread looping on another core (Free, next access faults); the same with core 1's acknowledgement held back by `tlb::hold_back_for_test` (Quarantined, ports retired); the same with core 2 holding core 1's scheduler lock (`sched::hold_run_queue_lock`; still confirms); after a kill the next process on the core faults; two processes with different ranges alternating on one core, each faulting on the other's; a sibling thread already running reaching a range mapped after it started (#GP pull); the scan's SMBus I/O BAR and the TCO/PM1 derivation rows; a synthetic I/O BAR minted with the claim and revoked by the release; and the switch-cost measurement. On the other two ports, one test that every call answers unsupported and no row carries a port.

BASELINE MEASURED BEFORE ANY OF THIS (debug test kernel, KVM, 4 vCPUs): 667 ns per thread switch (five passes: 667, 667, 654, 683, 668) - `kernel.kernel.baseline_thread_switch_ping_pong`, a temporary test run once and removed.

### Verification of P02M0191a/b (commands and results)

PASSED:
- `cd src/abi && cargo test`: 28 passed (the syscall and object-type snapshots, the `DeviceInfo` layout at 120 bytes with `port_count` at 53 and `ports` at 56, `PortResource` at 8 bytes).
- Host: `driver-protocol` 75 passed (the closed-set test now names `PortRange = 7` and probes 8), `driver-binding` 85 passed, the drivers library 378 passed.
- Kernel: `cargo build` and `TEST=1 TEST_TAGS="" cargo build --tests` in `src/kernel` - both clean (deny-warnings); one rustc SIGILL and one SIGSEGV on the way, each gone on the retry (the known crashes on this machine).
- `./build.sh --arch x86_64`: ok (920 s - the `DeviceInfo` change rebuilt every userspace crate). `python3 src/tools/foreign-audit-link.py --check` after the `rt` change: the recorded pass-2 inventory reproduces on all three targets.
- x86_64 kernel tests: all twelve `kernel.object.port_range.*` in one selection - 12 passed (runs x86_64-20260927T221803Z and, after the switch-path tightening below, x86_64-20260927T222131Z). The ring-3 GP faults the probes are meant to take are in the log (`fault: ring-3 general protection fault ... terminating process`); the quarantine case logs `tlb: shootdown 13 timed out with 2/3 acknowledgements` and `ports: 0x03e8..0x03ef are retired for this boot`; the boot logs the test derivation row's refusal `device: 00:1f.0 port range 0x0600..0x0603 is not recorded - Reserved(Firmware)`.
- A full boot with the change: `LIBER_DEVELOPMENT=1 ./image.sh --format iso`, then `./check.sh --gate qemu-gamepad-tool` (passes) and a `./lab.sh boot` whose serial log shows every driver bound as before (`driver.ahci: online (00:0b.0, ...)`; the q35 AHCI at 00:1f.2 gives up exactly as before on its ATAPI-only ports) and no "could not be given a port range" line.
- The cost (`docs/PERF.md`, "The port permission bitmap on every thread switch"): before 667 ns per switch; after 733 with no range, 1,119 with one side holding eight ports at 0x2F8. The first cut measured 780 / 1,191; the switch path was then tightened - the per-CPU record reached through a GS-relative load of a new `PerCpu::self_addr` instead of an RDMSR, the process's id kept in `IoPorts` beside its generation (set by `Process::new`) instead of read through the object header, and the one-line helpers `#[inline(always)]`, because the kernel ships unoptimised (`build.sh` runs plain `cargo build` for it).

NOT DONE / NOT RUN:
- Source (a) of the mint (platform rows) - P02M0196a publishes them; the milestone does not close before it is wired.
- P02M0191c (the COM1 handoff) and P02M0191d's handoff tests and `serial-handoff` gate - after P02M0196a, with P02M0099's 16550 item, in the agreed order.
- aarch64 and riscv64 builds and their one portable test (`a_machine_with_no_port_space_mints_nothing`) - at the end of the job with the other cross-architecture work.

ADDENDUM (2026-09-28, recorded during P02M0196a): source (a)'s static half is wired. P02M0196a's platform rows
carry port resources (the kernel's declarations, `SPCR`, `DBG2`, `WDAT`), checked against the reserved set and
the live grants at publication (`device::publish_locked`, claimable rows only) and again at the mint
(`sys_device_resource_acquire`, unchanged), and DeviceManager's existing loop hands them to the driver. Verified
on x86_64 by `kernel.platform_rows.a_platform_row_is_claimed_with_its_resources_and_a_release_takes_every_one_back`
(COM2's range minted from a platform row claim, revoked by the release) and by
`kernel.platform_rows.a_kernel_held_device_an_overlap_and_memory_or_a_bar_are_refused` (a claimable row over
COM1's ports refused at publication). The `_CRS` half waits for the ACPI service (P02M0196b).

### P02M0191c and P02M0191d's handoff parts, with P02M0099's 16550 item - started 2026-09-28T14:16:57Z
Begun after P02M0196a (the platform claim and claim-scoped wired lines) and P02M0190, in the owner's agreed order.
The record of what was built and verified follows below when it is.

#### What was implemented - P02M0191c, P02M0191d's handoff parts, and P02M0099's 16550 item
- THE UART AS AN INSTANCE (`src/kernel/arch/x86_64/serial.rs`): `Uart { base, irq, console, inner: SpinLock<Inner> }`
  with the transmit ring, the OWNER (`Kernel`, `Driver(gen)`, `Sleep(gen)` - test build only until P02M0197b's
  sleep entry calls it - and `Terminal`), the bound's dropped count, the claim's tap and the sleep window. `COM1`
  is the console; the test build adds `COM2` over the test machine's second UART, whose every register access
  is recorded (a ring of the last 8192, `record_from`). Every register access goes through `read`/`write`, which
  refuse AND COUNT (`stray`) an access by `Path::Kernel` while a driver holds the port; the owner they check is
  an atomic mirror every owner change writes (`set_owner`), not what the caller believes. Every path checks the
  owner first: the timer and idle drains, the receive poll (`read_byte` answers `None` while a driver holds the
  UART, so no COM1 byte reaches the console channel), the lossless path's full-ring drain (drops and counts
  instead), `drain_sync`, `tx_pending`.
- THE HANDOFF IS THE CLAIM: `kernel:com1` is published CLAIMABLE with `PLATFORM_FLAG_CONSOLE` (kernel-held in
  the test build, whose wire is the suite's); `device::claim` calls `arch::serial::console_hand_over` before
  the claim commits - the owner flips under the ring's lock first, then the kernel's IRQ 4 handler is
  unregistered (`interrupts::unregister`), and a line names the row and generation. The claim's port range is
  minted through `PortRange::mint_from_install` / `grants::grant_from_install`, which MOVES the console item's
  install into the grant in one step under the table's lock, and `grants::end` moves it back, confirmed or not.
- THE TAP (`src/kernel/object/console_tap.rs`, `ObjectType::ConsoleTap`, `abi::RESOURCE_KIND_CONSOLE_TAP`,
  `SYS_CONSOLE_TAP_READ` = 97): a derived object of the console claim, minted by DeviceManager from the claim
  (`device_resource_acquire(.., RESOURCE_KIND_CONSOLE_TAP, 0)`) and passed as the new `ConsoleTap` resource
  frame; reads move bytes out of the ring in order and report what the bound dropped; it is pending when the
  ring goes from empty to holding bytes, signalled from contexts that hold no other lock (the timer tick, the
  idle loop, the debug-write syscall - `deliver_tap_signal`), because waking its reader takes the scheduler's
  locks and a kernel line may be printed under any lock. The release revokes it.
- REACQUISITION (`Uart::hand_back`, from `device::finish_release` on both of its exits, so a quarantined release
  returns the UART too): owner KERNEL under the lock, DLAB cleared, the receiver read out (at most 64 bytes)
  BEFORE the boot sequence resets the FIFO, the boot initialisation, the IRQ 4 handler re-registered and
  re-routed, the bytes fed to the console channel, and one line - with the stray count in the development and
  test builds - plus the bound's drops when there were any.
- THE TERMINAL-PATH WRITER (`flush_sync` -> `Uart::terminal`): the ring's lock with a bounded wait
  (`SpinLock::get_unlocked` past it), the whole boot initialisation, the backlog by polling, the dropped count
  line, owner TERMINAL (never left; later lines go to the wire synchronously, lock-free). Entered before the
  first line of the panic handler (both builds), the double fault, every ring-0 fatal exception (#GP, #PF,
  the generic vectors), and before reset, power-off and the test exit. `drain_sync` is the non-terminal
  drain the suite and the perf drain use; the perf drain waits for whole lines (`write_whole`, `make_room`,
  which yields to the driver while one holds the UART).
- THE SLEEP ENTRY RULE (`sleep_begin`, `sleep_wake(lost_settings)`, `sleep_end`), implemented to the item's
  words and exercised by the suite; `#[cfg(test)]` until P02M0197b's kernel sleep entry is its production
  caller (the item gives the wiring to whichever lands second - P02M0197b).
- The HAL contract (`arch/mod.rs`) names the new serial surface; aarch64 and riscv64 answer every handoff call
  with a refusal and keep their synchronous writers.
- DEVELOPMENT-BUILD KERNEL REQUESTS: the kernel's `build.rs` emits `cfg(liber_development)` for the
  development build; `SYS_DEV_CONSOLE` (98) exists only there, gated by the `ConsoleInputSource` privilege:
  `DEV_CONSOLE_HOLD_AND_FLOOD` (every tap's reads held, kernel lines written until the ring drops),
  `DEV_CONSOLE_PANIC`, and `DEV_CONSOLE_KILL_HOLDER` (SIG_KILL to the process holding the console's port range
  - the gate's "driver killed"; nothing else in this tree can kill a driver from the host). The development
  agent serves them as `OP_KERNEL_CONSOLE` (0x2c); `lab dev-kernel-console hold-and-flood|panic|kill-holder`.
- THE 16550 DRIVER (P02M0099's item): `drivers::uart` - the register engine over a `Registers` trait (program,
  quiet, the transmit interrupt, FIFO-load transmit, receive with the line-status errors counted), the line
  description (`Line::new(clock, baud)`), the bounded held input (512, newest dropped and counted) and the
  dropped-output marker, all host-tested (7 tests) - and `uart16550` (manifest: platform rule `hid = PNP0501`,
  `dma = "none"`, one `console-bytes` provider): maps the port range, programs the UART whole, drains the tap
  before anything else, publishes `ConsoleBytes` under `driver_protocol::provider::KERNEL_CONSOLE_NAME`
  (`org.libersystem.console`) through `serial_port::Session`, serves IRQ 4 (every received byte kept until a
  consumer is listening), empties the tap before every console-stream write, waits for the transmitter's
  interrupt rather than spinning when the FIFO is full, and on a planned stop leaves every interrupt enable off.
  On the ports it refuses its bind in words: no port space.
- CONSOLESERVICE: a `CONSOLEBYTES` catalogue role (`console-bytes`, optional) and `SerialWire`, following the
  publication named `KERNEL_CONSOLE_NAME` with `driver_protocol::console`'s host-tested decisions (the
  development agent's): while attached, the mirror goes out as console-stream writes with each newline as
  CR LF (as the kernel's write path puts it) and an `again` keeps the backlog; received chunks enter
  `handle_keys` as serial input; when the provider goes, the gap marker is written through `SYS_DEBUG_WRITE`
  and both directions fall back to the kernel's paths, whose console channel it never stopped reading.
- THE KERNEL TESTS (`src/kernel/object/port_range/tests/handoff.rs`, six, on `COM2` with a console row of the
  suite's own appended without placement - `device::synthetic_console_row` - so other suites' rows over the
  same UART never merge into it): the tap in order with the bound's drops counted and nothing on the UART;
  the ports reserved while the kernel drives the UART, the claim's range the one mint admitted, back in the set
  after the release (firmware path included); a ring-3 script probe holding the UART with DLAB set and bytes in
  its receiver while every kernel path runs - the record still, the stray count zero, one deliberate stray
  access counted and refused - and the release reading the bytes out with DLAB cleared before the FIFO reset,
  in the record's order, then the queued backlog drained; a quarantined release returning the UART and the row
  refusing a new claim; the sleep rule on both owners with an S3 wake re-running the boot initialisation
  before the resumed line (receive interrupt on for the kernel, off while lent); and the terminal writer after
  a probe broke the UART every way, then on a fresh instance past its bound - backlog, count, marker. The
  probe program is new (`program_port_script_bytes`).
- THE GATE `serial-handoff` (`src/tools/check-serial-handoff.sh`, registered in `check.sh`, the verify-model
  catalog as a guest-booting gate and `release-required.toml`): a private development instance; the handoff
  line; `lab sh` through the driver; the driver killed - the reacquisition with a zero count, DeviceManager's
  restart, the next handoff, ConsoleService attached again, `lab sh`; the binding disabled - reacquisition,
  `lab sh` through the kernel; enabled - the next handoff, `lab sh`; the flood with `lab sh` still answering;
  the panic - the dropped count, then `*** KERNEL PANIC ***` and its message.
- `lab` learned to type `#` (P02M0190's gate needed it) - unrelated here.

#### Verification - P02M0191c/d and the 16550 driver (2026-09-28)
- `./check.sh --gate serial-handoff`: PASS (exit 0, 319 s) - boot handoff line and the driver online; `lab sh`
  through the driver; the driver killed (`dev.sh kernel-console kill-holder`): "COM1 is the kernel's again - 0
  kernel access(es)", DeviceManager's restart, the next handoff, ConsoleService attached again, `lab sh`; the
  binding disabled with `lsdev --disable kernel:com1`: the reacquisition, `lab sh` through the kernel's own path;
  enabled: the next handoff, `lab sh` through the driver; the flood (135 kernel lines while the taps' reads were
  held) with `lab sh` still answering; the panic: "console: N byte(s) of kernel output were dropped at the
  ring's bound before this point" and then "*** KERNEL PANIC ***" and its message. Every reacquisition line
  counted 0. After that pass the gate's `all_counts_zero` was rewritten for the source-hygiene gate (no pipe into
  `grep -q` under pipefail) - the same check, not re-run since.
- THE GATE FOUND A DRIVER DEFECT, fixed and then passed: the driver acknowledged its edge-triggered IRQ 4 after
  draining the receiver, so a byte arriving in between left the UART's output high with no further edge -
  reproduced on a private instance ("echo fir" of `lab sh echo first`, then no input for the rest of the boot;
  single keystrokes and bursts through the console socket worked once the timing missed the window); the
  driver now acknowledges first. Two earlier runs failed on the gate's own plumbing (the development image
  build needed `foreign-audit-link.py --check` after the `rt` change, and `dev-*` requests go through
  `./dev.sh`, which gained the `kernel-console` verb).
- Kernel, x86_64: `TEST_SELECTION=<the six handoff tests, every port_range, platform_rows and idle test,
  kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick,
  kernel.dma_policy.the_kernel_registry_is_the_manifest_migration_table> ./test.sh --arch x86_64`: 31 passed.
  The first run failed on its own expectation (a released claim's tap answers ERR_BAD_HANDLE - the release
  revokes the capability - not ERR_ACCESS_DENIED); test and ABI text corrected.
- WATCHED FAILING: `Uart::read_byte` without its owner check (a receive poll left running) makes
  `while_a_probe_holds_the_console_uart_no_kernel_path_touches_it_and_its_release_reads_out_before_the_reset`
  fail - "and none tried", the stray count 1; restored. The access path's owner is an atomic mirror written by
  every owner change, so a path that believes it owns the UART cannot hide from the count.
- Also verified: the production, development (`LIBER_DEVELOPMENT=1`) and test kernels compile; `./build.sh
  --arch x86_64` ok; host suites `drivers --lib` 402 (the engine's 7 among them), driver protocol 76, binding
  89, abi 28, system-manifest 27; rustfmt clean on every changed file; `shfmt -d` clean on `dev.sh` and the
  gate; `./check.sh --gate source-hygiene` clean (after the fix above), `--gate arch-surface` ok, `./gen.sh
  --check` ok; `python3 src/tools/foreign-audit-link.py --check` reproduces after the `rt` change.
- REGRESSION: `./check.sh --gate qemu-tpm-tool` passes again (445 s) with the handoff live - its serial log shows
  the handoff, the driver online and ConsoleService attached, so its keyboard-driven scenario and serial
  oracles ran over the driver's wire.
- NOT RUN: the aarch64 and riscv64 builds (the portable surface answering "unsupported"), at the end of the job;
  `src/tools/verify-model`'s suite, which cannot load the model while the stale port test binaries name the
  replaced clock test (pre-existing; the end-of-job port rebuild clears it), so the new gate's catalog entry is
  not checked by it yet.

#### Open after this record
- Source (a)'s `_CRS` ranges (P02M0196b); the cross-builds; the sleep entry's production wiring (P02M0197b);
  the 16550's `RESUME` reprogramming with P02M0197's suspend and resume exchange.

## The console at a planned end, and the gate run again (2026-10-01)

- Found by P02M0197's gate and recorded in its audit: a power-off or reset a process asked for, while `uart16550`
  held COM1, lost the lines the driver had read from the tap and not yet put out (up to 1024 bytes). `sys_system_power`
  now calls `arch::serial::settle_driver` first: the tap signalled and the core yielded until the ring has been empty
  for 100 ms, at most 1 s (empty on the ports). The terminal-path writer itself, and the panic and forced-deadline
  paths, are unchanged. Kernel test `kernel.object.port_range.handoff.a_planned_end_waits_for_the_driver_to_take_the_ring_and_no_longer_than_its_bound`
  (watched failing with the wait removed).
- And `uart16550` keeps its consumer across a sleep it takes itself (the session was reset after every step, ending
  ConsoleService's input stream at each wake) - P02M0197's audit.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate serial-handoff` -> PASS twice on 2026-10-01, the second with the settle in.
- (2026-10-01) the three cross-builds pass; on the device-tree ports the port-range calls answer `ERR_UNSUPPORTED`
  (`arch::ioports::supported()` false - read in the code, not run). Status: COMPLETE.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0191 (2026-10-08T03:26:17Z):

Continuation review: read the complete milestone plan and the preserved implementation record, including earlier incomplete verification and later completion claims. Reviewed the dependency and verification conventions in docs/TESTING.md. No previous audit text was changed.
Review in progress: completion labels are being checked against the current code and available verification artifacts; no new passing test results are claimed at this point.

Continuation result (2026-10-08): reviewed the actual PortRange mapping/revocation implementation, its grant/reservation table, x86 TSS generation copy and #GP pull, claim derivation, and the serial handoff/sleep hooks. The previously recorded platform and namespace sources are present; the current code includes the sleep entry's console loan and wake reinitialization. No missing port-authority behavior was identified.

Mandatory adjacent cleanup: source-hygiene found `src/kernel/arch/common/console_uart.rs` beside `console_uart/tests.rs`, violating the repository's required test-module layout. Moved the former byte-for-byte to `src/kernel/arch/common/console_uart/mod.rs`; Rust's existing `mod console_uart` and `mod tests` declarations resolve unchanged. This changes no UART behavior. The original path in historical notes is preserved.

Fresh verification: `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` PASS (560 tests, including UART/console engine cases). Kernel and cross-target compilation after the move are reserved for the coordinator's final build; the kernel handoff/port tests and hardware serial gates have not been rerun in this continuation. Existing completion status is retained for the implemented mechanism; the file-layout-only edit's final validation is still pending below.

Follow-up verification: `./check.sh --gate source-hygiene` after the UART move passed the module-layout check but failed overall on existing early-reader pipelines in `check-bluetooth-le-audio.sh`, `check-bluetooth-transfer.sh`, and `check-brightness-acpi.sh`. This is a failed check, not completion evidence for global hygiene; the root implementer is coordinating the final check. No additional UART behavior changed.

Coordinated final checks at the frozen continuation source (2026-10-08), run by the root implementer: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (487 s; `.build/logs/end-of-job/continuation-build-all-final.log`) on x86_64, aarch64 and riscv64. `./check.sh --gate source-hygiene --gate verify-model --gate verify-model-tests` also PASS (205 s; `.build/logs/end-of-job/continuation-static-final.log`): source hygiene, model consistency, and all 157 model tests. This supersedes the earlier global-hygiene failures; the narrow diagnostic-script fixes are recorded by the root implementer. Runtime gates are reported separately below.

Continuation final state for this milestone: the only owned source change is the byte-for-byte common UART module relocation required by source hygiene. The three final builds resolved it successfully, and post-fix source hygiene passed. The earlier port-range and UART handoff runtime evidence is preserved; no additional serial-handoff guest run was performed solely for the relocation. The plan remains COMPLETE.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

AUDITOR'S REVIEW ON P02M0191 (2026-10-15T04:38:16Z):

**Rating: 9/10.** Port I/O is a capability exactly as specified. I found no meaningful defect in the kernel mechanism. The one gap is a verification step that the gate item still names and that was never added.
- **The object.** A `PortRange` has no directional rights, is mapped into one process at most, and grants nothing until it is mapped.
- **Enforcement.** Every core's TSS carries the 8 KiB bitmap and its trailing `0xFF` byte, under one limit; the map base sits past the limit for a process with no range. Changes are serialised with an odd/even generation and allocate nothing. Copies take no lock and retry a bounded number of times, refusing everything past the bound. The switch, the shootdown service step and the `#GP` pull read only the per-core record, never `current_thread`. A revocation clears the bits, releases the process lock, recopies locally and then runs one shootdown round. An unconfirmed round retires the ports for the boot and ends the claim `Quarantined`.
- **The reserved set.** Its fixed part is exactly the specified list: `0xCF9` left out, the DMA page registers mintable, the exit port test-only. The FADT blocks are recorded before the boot scan in both builds. Run-time installs are counted per item, and COM1 moves into and out of the claim's grant in one step under the table lock.
- **The sources.** Exclusivity is checked at record and again at mint. I/O BARs are sized with decode off and refused past `0xFFFF`; the claim sets I/O decode and the release clears it. The derivation table holds only P02M0200's TCO row. The `FirmwareInterpreter` mint is the only call that takes an address.
- **Other architectures and DeviceManager.** Every call answers `ERR_UNSUPPORTED` on aarch64 and riscv64. DeviceManager mints every port range a row carries, and the console tap.
- **The COM1 handoff.** The owner state is KERNEL, DRIVER, SLEEP or TERMINAL. Every access goes through one counted access path. The owner flips before IRQ 4 is unregistered. The reacquisition reads the receiver out with DLAB cleared before the FIFO reset. The terminal-path writer is entered before the first line of every fatal path. The sleep entry lends the UART and gives it back with the driver's settings.

## Findings

1. **Low, Defect - The `serial-handoff` gate never gained the "ACROSS A SLEEP" step its item requires, and its header still says there is no sleep entry to drive.**
   - **What the item asks.** The gate item (`docs/todo/P02M0191.md:432-435`) requires: right after the first `lab sh` round trip, a suspend to idle while the driver holds COM1, then `sleep: entered` and `sleep: resumed` in the log and `lab sh` answering through the driver after the resume. The purpose is "so the kill that follows reports its zero count over a hold that included a sleep". The item says the step is "carried by whichever of this part and P02M0197b lands second".
   - **What the gate has.** P02M0197b landed second, and its kernel sleep entry is now the production caller of `serial::sleep_begin`/`sleep_wake`/`sleep_end` (`/data/yellow/libersystem/src/kernel/sleep/mod.rs:251`, `src/kernel/arch/x86_64/sleep.rs:385`, `:452`). Yet `/data/yellow/libersystem/src/tools/check-serial-handoff.sh` has no sleep step. Its header still reads "ACROSS A SLEEP is P02M0197b's to add to this gate: there is no sleep entry to drive yet" (:28). P02M0197's own `sleep` gate checks the timing and a typed line after a suspend to idle, but no reacquisition count.
   - **Why it matters.** The zero-stray-access count over a hold that included a sleep is the gate-level proof that the lend-and-return path leaves no ordinary kernel path touching COM1 while the driver holds it. At present it is shown only on the second UART, by the kernel test `the_sleep_entry_follows_the_owner_it_finds_and_a_wake_from_s3_programs_the_uart_before_its_first_line` (`src/kernel/object/port_range/tests/handoff.rs:247-306`, which asserts `stray() == 0`).
   - **Fix.** Add the step and correct the header. It is small: `sleepctl` with a timed suspend to idle between steps 2 and 3, then the existing kill and its zero-count check.

## Verification performed

- **Static review only.** Nothing was built, run or tested for this review.
- **Read:**
  - `src/kernel/arch/x86_64/ioports.rs` whole: `load`, `switch_in`, `service`, `pull_on_gp`, the FADT blocks, the derivation table and its fixture test;
  - `src/kernel/object/port_range/mod.rs`: `IoPorts` change/copy protocol, `mint`, `mint_from_install`, `map_into`, `unmap_from`, `revoke`, `take_back`, `give_back_all`;
  - `src/kernel/object/port_range/grants.rs`: the fixed set, `recordable`, `grant`, `grant_from_install`, `end`, `install`, `uninstall`;
  - the TSS layout and limit in `gdt.rs`;
  - `io_bars` and `set_io_decode` in `arch/common/pci/mod.rs`, and the claim/release decode calls in `device.rs`;
  - the port-range and console-tap syscalls in `syscall/mod.rs`;
  - DeviceManager's resource loop;
  - the x86_64 console owner code in `serial.rs`: `write_byte`, `write_bytes`, `hand_over`, `tap_read`, `hand_back`, `terminal`, the sleep entry, `settle_driver`;
  - the fatal paths: the panic handler, the double fault, ring-0 `#GP` and `user_fault_or_halt`;
  - the list of port-range and handoff kernel tests;
  - the gate `check-serial-handoff.sh`;
  - the `PERF.md` switch-cost section;
  - the second `isa-serial` in `qemu-run.sh`.
- **Independent cross-check.** A separate read-only pass covered the lock order and lifetime arguments. I re-checked its two candidates at the code and do not report them, because each needs an extremely unlikely coincidence:
  - In `write_bytes` (`serial.rs:437-479`), the owner is checked and the ring pushed under two separate lock acquisitions. A terminal-path entry on another core in exactly that gap would strand a mirror chunk in a ring nobody drains. The panic text itself is unaffected.
  - In `map_into` (`port_range/mod.rs:289`, `:305-307`), `try_reserve(1)` does not reserve one slot per concurrent caller. Two simultaneous maps in one process at a capacity boundary, with an allocation failure at that moment, would make the second `push` infallible-allocate, so its `ALLOC-OK` note overstates the guarantee.

IMPLEMENTER'S RESPONSE ON P02M0191 (2026-10-17T01:44:22Z):

**Finding 1 - the `serial-handoff` gate never gained its "ACROSS A SLEEP" step, and its header says there is no sleep entry (Low): ACCEPTED.**
- `src/tools/check-serial-handoff.sh`, step 2:
  - suspends to idle for 3 s through `./dev.sh launch sleepctl suspend idle 3`;
  - awaits `sleep: entered`, `resumed`, and the transaction ending "slept and woke" with every count zero;
  - then requires a round trip on the port.
- The stale header line is removed.
- `src/harness/scenarios/serial-handoff-port.toml` gains the same steps (1b) for the ports.
- The milestone records it under "Corrections of 2026-10-15".

**Verification:**
- `serial-handoff` passed on x86_64 with the sleep step: entered, resumed, the transaction slept and woke with every count zero, and a round trip after it.
- - `serial-handoff --arch aarch64` passed (450 s) and `--arch riscv64` passed (505 s), the new sleep steps included: on both, the suspend to idle resumed on its timed wake after 2998 ms, and the transaction ended slept and woke.

AUDITOR'S RE-AUDIT ON P02M0191 (2026-10-18T17:47:21Z):

**Rating: 7/10.** The accepted finding is fixed:
- **The sleep step.** Step 2 of the x86_64 gate now suspends to idle through `sleepctl` while `uart16550` holds COM1. It requires `sleep: entered`, `sleep: resumed` and "slept and woke", then a round trip.
- **The count across the sleep.** The kill that follows reports its count over a hold that included the sleep. The counter is reset only in `hand_back`, and the retained log shows "0 kernel access(es)".
- **The header and the ports.** The stale header line is gone, and the ports' scenario has the same steps.
- **Unchanged.** The port-authority and handoff mechanisms.

What remains: the gate's round-trip check cannot fail on a command the shell refused. The retained log of the latest x86_64 PASS shows exactly that for the step after the disable and the step after the enable.

## Findings

1. **Medium, Defect - The handoff gate's round trips accept a refused command, and in the latest x86_64 PASS the round trip through the kernel's own path and the one after the enable both failed.**
   - **What the plan requires.** After the disable, "`lab sh` answers through the kernel's own path; the binding enabled again through the same verb and the next handoff line awaited" (`/data/yellow/libersystem/docs/todo/P02M0191.md:424-426`). Earlier, "`lab sh` round-trips through the driver" (:421).
   - **What the code does.**
     - `round_trip` passes if the word appears anywhere in `lab sh`'s output (`/data/yellow/libersystem/src/tools/check-serial-handoff.sh:161-166`, used at :221 and :230).
     - `lab sh` drops the echoed line only when it contains the whole command (`/data/yellow/libersystem/src/harness/lab.py:3455-3456`). The shell's error message quotes the word back.
     - What the latest x86 run's retained log (`/data/yellow/libersystem/.build/logs/serial-handoff/dev-serial.log`) shows:
       - after the disable, the shell received "cho handoff-through-the-kernel" and answered "unknown command: cho handoff-through-the-kernel" (:585-586);
       - after the enable, it answered "unknown command: eecho handoff-after-the-enable" (:591-592);
       - so the first typed byte arrived after the line's carriage return, and the leftover then broke the next command;
       - the run nevertheless went on through the flood and the panic (:729-733), so both round trips were accepted.
     - The port scenario's `serial-sh` steps are weaker still. They search a reply that includes the echoed command (`lab.py:3043`, `/data/yellow/libersystem/src/harness/scenario.py:829-833`), so `serial-handoff-port.toml:132-136` and :167-171 cannot fail on a refused command either. The port logs show those commands did run.
   - **Why it matters.** The PASS recorded under "RESULTS (2026-10-16/17)" (`P02M0191.md:454`) does not prove two required gate steps. On x86_64, input typed after the kernel took COM1 back arrived out of order.
   - **Likely cause (not provable statically).** The kernel's own COM1 receive path:
     - the idle-hook pump (`/data/yellow/libersystem/src/kernel/main.rs:524-526`) and the IRQ 4 handler (:1370-1373) both do `read_byte()` then `feed_serial()`;
     - `read_byte` masks interrupts only while it holds the UART lock (`/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:547-553`, `/data/yellow/libersystem/src/kernel/sync.rs:38-43`);
     - IRQ 4 is routed to the BSP, the same core that runs the pump (`serial.rs:673`);
     - the 14-byte receive trigger (FCR `0xC7`, `serial.rs:329`) can leave a burst's first byte to the pump, so an IRQ taken between the two calls queues the rest of the burst ahead of it.

     That code predates this milestone; the gate is where the failure should have been caught.
   - **Minimal fix.**
     - Make `round_trip` require the word as a whole output line and fail on `unknown command`.
     - In the port `serial-sh` steps, search only the output after the echoed line.
     - Rerun the gate. If the kernel-path step then fails as this log suggests, make both loops read and enqueue each byte inside one interrupt-masked section.

## Verification performed

- **Documents read.** `docs/todo/P02M0191.md`, the gate item (415-436) and the corrections (448-456); the audit review (222-260) and the implementer's response (261-274).
- **Code read in the current tree.**
  - `src/tools/check-serial-handoff.sh` and `src/harness/scenarios/serial-handoff-port.toml`, both in full.
  - `src/kernel/arch/x86_64/serial.rs`: the access path, `read_byte`, `hand_over`, `hand_back`, `sleep_begin`/`sleep_wake`/`sleep_end`, `settle_driver`, and where the stray counter is reset.
  - `src/kernel/console_input.rs`.
  - `main.rs`: the pump, the shell loop and the wired-line handler.
  - `SpinLock` in `src/kernel/sync.rs`.
  - `lab.py`: `cmd_sh`, the broker's RUN loop and `serial_sh`.
  - `scenario.py`: the `serial-sh` check.
  - The diffs of commits `58fb476c` and `7ee3e9bf`.
- **Retained results read (read-only).** `.build/logs/serial-handoff/` (`dev-serial.log`, `sleepctl.log`, `up.log`, `down.log`), `serial-handoff-aarch64/serial.log` and `serial-handoff-riscv64/serial.log`.
- **Claims checked.**
  - Step 2 suspends to idle for 3 s through `./dev.sh launch sleepctl suspend idle 3`: confirmed (:190-200).
  - The gate awaits entered, resumed and "slept and woke" with every count zero: confirmed. No reacquisition line exists yet at that point, so the zero count is actually proved by the kill that follows (log :566).
  - A round trip after the sleep: confirmed (:200; log :559-560).
  - Stale header removed: confirmed (:14-17).
  - Port scenario gains step 1b: confirmed (`serial-handoff-port.toml:32-64`).
  - x86_64 PASS with the sleep step: the sleep is confirmed in the retained log (2905 ms). The same run's kernel-path and post-enable round trips were refusals (finding 1).
  - aarch64 in 450 s and riscv64 in 505 s, with the 2998 ms timed wake: the sleep lines are confirmed in both port logs (aarch64 :529-554, riscv64 :588-611). The durations cannot be checked statically.
- **Not done.** This was a static review only: nothing was built or run.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON P02M0191 (2026-10-21T12:09:39Z):

**Finding 1 - the handoff gate's round trips accept a refused command, and in the latest x86_64 PASS the round trip through the kernel's own path and the one after the enable both failed (Medium): ACCEPTED.**
- `src/tools/check-serial-handoff.sh`: a round trip passes only when the word comes back as its own whole line and the
  shell did not answer "unknown command". The word anywhere in the output passed a command the shell refused, whose
  error quotes it back.
- `src/harness/scenario.py`: the `serial-sh` step reads only what the command printed after its echo, and refuses an
  "unknown command" answer the same way.
- Watched failing, on the outputs the review quoted: the old `round_trip` passes "unknown command: cho handoff-7",
  "eecho handoff-7" followed by its refusal, and an output that is only the command's echo; the new one refuses all
  three and passes the answered round trip. The old and new `serial-sh` predicates were put to the same four outputs,
  with the same result.
- `src/kernel/main.rs`: one interleaving that reorders received bytes, found by reading, is closed. The serial pump's
  two sites - `serial_console_pump` and `console_shell_loop` - read a byte and queued it as two steps with interrupts
  enabled between them, while the console line's interrupt handler (`wired_line_interrupt`) does the same pair on the
  same core: one taken between them puts the rest of a burst ahead of the byte just read. `take_serial_byte` reads and
  queues as one step with interrupts masked, and both sites use it.
- THE GARBLED ROUND TRIP IS REPRODUCED, and its most likely cause is the interleaving above - an inference: the
  reproduction forced the window by leaving the receive interrupt off. An ordinary run reaches the window
  rarely: the pump has to win a burst's first byte, which it does only when the burst lands while its own read holds the
  line with interrupts masked - which is why four ordinary runs, one of them with a spin widening the wrong gap (after
  the read, when the line's handler has already taken the burst), never garbled. A reproduction, never kept, made the
  pump win: when the kernel took COM1 back from the disabled binding it left the UART's receive interrupt off, so the
  next burst waited in the FIFO and the pump's polling read took its first byte, and then the receive interrupt was
  turned on again with the rest of the burst still waiting.
  - With the byte queued as the code did before - after interrupts were open again - the shell received "cho
    handoff-through-the-kernel", answered "unknown command: cho handoff-through-the-kernel", and the "e" arrived after
    the line, where it began the next command - the reviewed run's refusals - and the gate failed on it (186 s).
  - With the byte queued the tree's way - read and queued under one mask - the same timing passed (serial-handoff PASS,
    195 s, "handoff-through-the-kernel" answered as its own line).
  - A first form of the reproduction waited for the burst with interrupts masked; it held the boot processor long enough
    that the disable's own reply never came back, and both of its runs were discarded.
- `src/kernel/console_input.rs`: a second ordering defect on the same path, found while reading it for the first, is
  closed. `drain` - which the receive path calls for every byte it queues, and the idle hook calls to empty what a full
  console left waiting - read the byte at the front, sent it, and only then took it off the front; two at once (the idle
  hook's pass and the line's handler on one core, or any two callers on two) both sent the same byte, and the second
  take removed one nobody had sent. Deliveries now hold one lock across the whole pass (`DELIVERING`). The kernel test
  `kernel.console.two_deliveries_at_once_hand_the_console_every_byte_once_and_in_order` queues a numbered stream on one
  core while a second core delivers in a loop, and requires the console to receive exactly what was queued: 40000 bytes
  received once each and in order, beside 1468502 delivery passes on the second core; with the ring as it was, the
  console received 76915 bytes for the 40000 queued, different from the second byte on.

**Verification:**
- The predicates, old and new, on the review's outputs (above).
- The reproduction, both ways (above).
- `serial-handoff` PASS on the final tree (198 s), with the masking and the single delivery in.

**Found by the regression run (2026-10-21):** `serial-handoff` PASS (264 s). `tickless-idle`'s riscv64 typed-input check
predates this milestone's handoff: it counts the interrupt the kernel announced for the console (63), which the handoff
to the console driver takes off its line, and reads 0 - recorded under P02M0198 and reported. With both console-input
changes (this review's and that of 2026-10-19) reverted, the ports' idle-sleep clock check fails exactly as before, so
that failure is not this path.

**Milestone status (verified after these changes):** COMPLETE. Every item is done and the finding is fixed: the gate
refuses a refused command, the garbled round trip's cause is reproduced and closed, and a second way for input to be
reordered on the same path is closed with it (above).

**Full regression on the final tree:** A release rehearsal, `./verify.sh --release --in-place --jobs 2` over the tree
(2026-10-20/21), ran 183 of its 373 steps in 22 hours and was stopped there by the owner's decision; every step it
failed was run again alone, and the 190 it never reached were then run - so every release step has a result. An
independent check of every finding of these responses against the code then found gaps, which are fixed and recorded
above, and the fixed tree was verified one step at a time: the builds of all three targets; the drivers' 638 host tests,
`service-logic`'s 965, `ucsi`'s 16 and `conformance2d`; the Bluetooth reconnect proof; `source-hygiene`,
`graphics-profile`, `opentype-profile`, `guest-verdict`, `milestone-index` and `dynamic-report` (regenerated);
`brightness-acpi`, `usb-legacy`, `typec-ucsi`, `bluetooth-le` and `driver-mutations`; and the x86_64 kernel suite, 585
passed. The other suites on the tree before those fixes, which touched no kernel code but one x86_64 test: aarch64 546
passed; riscv64 passed but `spool_service_sends_submitted_jobs_exactly_once`, which fails there alone (a printer's
answer past its 200-tick bound; P02M0186, outside this review). The 148 host suites PASS (`host-tests`). Failed in the
rehearsal only for the run's order, concurrency or load and passed alone: `perf-anchor`, `sleep`, `verify-evidence`,
`signed-boot`'s pairing, `staged-consistency`, `run-verdict`, `capability-trace` and five arch profiles; and
`provider-media-order`, `development-gate` and the x86_64 suite, which failed because the gates that boot through
`dev.sh` leave the shared x86_64 staging in the development profile and nothing restores it - a harness gap reported to
the owner. Not caused by this review - its one kernel change, and the 2026-10-19 one beside it, reverted change nothing
- and reported to the owner: `qemu-pcie-hotplug` and `concurrent-selection` (since 2026-09-29), `signed-boot`'s
re-signing cases (the signer has required `--dma-mode` since 2026-09-10), `arch-profile-*-no-dt-absent-1` (since
2026-10-12), the IOMMU hostile and transition phases on both ports, the ports' idle-sleep clock check, `tickless-idle`
on riscv64, `typec-tcpci-riscv64` (timing) and `development-lifecycle`'s protocol fuzz.
