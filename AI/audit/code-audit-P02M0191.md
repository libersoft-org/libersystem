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
