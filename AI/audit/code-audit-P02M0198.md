IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0198 (2026-09-28T00:47:08Z):

Status: IN PROGRESS - P02M0198a (idle without a tick) first, in the owner's agreed order (after P02M0195a,
before P02M0196a); parts b to e follow P02M0196b/c in that order. This record is updated as the work proceeds;
the final state is at its end.

## P02M0198a - what was implemented (2026-09-28)

THE CLOCK IS COMPUTED, NOT ADVANCED.
- New host crate `src/tickclock` (`Clock`): `anchor(counter, hz)` once the counter's frequency is known,
  `ticks(counter)` = (counter - offset - anchor) / cycles-per-tick kept non-decreasing by one atomic maximum,
  `nanos(counter)` = counter less the same offset, `counter_at(tick)` for compare values (None past the counter
  or while suspended), the SUSPENDED state (`suspend(counter)`: every reading answers the monotonic value at
  suspend and none raises the maximum past it) and `rebase(at_suspend, at_resume)` (offset and maximum set
  together, leaves the suspended state) - one arithmetic for a counter that kept running and one that restarted.
  9 host tests (`cargo test --manifest-path src/tickclock/Cargo.toml`).
- `arch::common::time::CLOCK` is the one clock; each backend anchors it where its counter frequency becomes
  known (x86 `init_tsc`, aarch64 CNTFRQ, riscv64 timebase). `apic::ticks()` on all three reads it (a test
  build still adds the harness skew); `SYS_CLOCK_MONO_NS` = `CLOCK.nanos(counter)`. `TICK_HZ`,
  `TICKS_PER_SECOND` and every deadline stay ticks: no userspace change.

A BUSY CORE KEEPS ITS TICK; AN IDLE ONE PROGRAMS A ONE-SHOT.
- New HAL entries `apic::timer_one_shot(Option<tick>) -> bool` and `apic::timer_periodic()`: x86 TSC-deadline
  MSR where CPUID offers it, else a one-shot LAPIC count (re-armed in steps by the halt loop when the count
  clamps); aarch64 CNTP one-shot; riscv64 `stimecmp` with Sstc, else `sbi_set_timer`. A clock that is not
  anchored programs nothing and the periodic tick stays (the halt it used to be).
- New module `src/kernel/idle/mod.rs`: `halt(until, ready)` is EVERY halt the kernel makes - masks, publishes
  HALTING on the BSP, runs the last check under the mask, computes the wake (its own bound; on the BSP the
  earliest deadline of ANY kind - `sched::earliest_deadline`, progress and `WAIT_PERIODIC` alike - the
  HOUSEKEEPING_BOUND of 10 ticks while polled housekeeping exists, one tick for held console input or a
  console UART with no receive line; on any core one tick for a non-empty x86 transmit ring), publishes it,
  programs the one-shot, halts in the masked form (`arch::idle_halt`: x86 `sti; hlt` entered masked, aarch64
  `wfi` masked then `msr daifclr` - reversed from before -, riscv64 masked `wfi`), restores the periodic tick
  and accounts the halt.
- Halts converted: `run_until_idle_until`'s deadline wait, `cpu_idle_loop` (which now also reaps before it
  halts - an idle core with no tick held a dead thread and its Domain charge until its next reschedule, found
  by a process test), the console loop's settled halt, `drive_slice`, the boot supervisor's wait, and every
  kernel test's wait (`tests.rs`, `test_suites/{boot,kernel,services}.rs`).
- Wakes that replace the tick: `idle::deadline_armed` (a deadline pushed on another core earlier than the wake
  the BSP published sends it a wake IPI; called from `block_on_flagged`/`block_on_any` after the push);
  `idle::wake_bsp` (a shell that starts listening - `console_input::attach` - and the resident SystemManager's
  terminal transition via `idle::watch_process`/`process_ended` from `Process::mark_exited`/`terminate`);
  `enqueue_on` wakes a remote core (`idle::wake_core`), so `start_thread_on` and every placement come with the
  IPI; `enqueue_on_unwoken`/`spawn_on_unwoken` keep the remote-spawn control.
- Polled housekeeping registers itself: hot-plug ports routed nowhere (`HOUSEKEEPING_HOT_PLUG`), PCI error
  reporters (`HOUSEKEEPING_PCI_ERRORS`), the IOMMU fault queue (`HOUSEKEEPING_IOMMU_FAULTS`).
- Proof that the timer fires: the aarch64 and riscv64 prologues count timer INTERRUPTS (`timer_interrupts()`),
  since a computed tick advances with none; `check-qemu-arch-profiles.sh` reads the new line.

SERIAL RECEIVE ON EVERY PORT.
- New `fdt::Fdt::console_interrupt()` (the stdout-path node's `interrupts` against its own or the nearest
  ancestor's `interrupt-parent`, or its `interrupts-extended`; the controller's `#interrupt-cells` read by
  phandle; refused when short or unresolvable) with 7 host tests whose expected values were decoded from the
  five QEMU fixtures by an independent parser first.
- New HAL entry `serial::arm_rx_interrupt(handler) -> Result<identity, reason>`: x86 moves COM1's IRQ 4
  arming out of `kmain` into it (vector 36); aarch64 arms the PL011's SPI from the tree through
  `gic::enable_spi` (UARTIMSC RX|RT; `read_byte` clears RT on an empty FIFO); riscv64 arms the 16550's APLIC
  source to `WIRED_EID` (IER bit 0). Each refuses in words a console node that is not the UART it writes to,
  a controller it does not drive, or a full wired table.
- `main.rs`: ONE handler for the kernel's own wired lines, `wired_line_interrupt` (drains the UART, then reads
  every hot-plug slot), passed to both the console arming (`arm_console_interrupt`, in the boot tail on all
  three ports, printing `console: typed input raises interrupt N` or `... is polled - <why>` and then capping
  the idle BSP at one tick via `idle::console_polled`) and every slot arming - so riscv64's shared `WIRED_EID`
  is answered for both whichever registration came last.
- THE RELEASE when a userspace driver takes the port is the handoff items' (COM1 through its claim, the ports'
  UARTs through their drivers'), none of which exists in the tree yet; the handler reaches the UART only through
  `read_byte`, which is where such a handoff leaves the port alone. Not implemented here: nothing can take
  the port yet, so there is nothing to release.

PER-CORE IDLE ACCOUNTING.
- ABI: `SYS_CPU_IDLE_INFO` = 95, `CpuIdleInfo` (cpu, idle_ns, halts, wakes by timer / IPI / housekeeping bound /
  device, and up to `CPU_IDLE_SOURCES` = 8 device identities with counts; layout asserted 184/8), a free read
  answering one record per core like `SYS_IRQ_INFO`; `rt::cpu_idle_info`. Each port's interrupt paths record
  the cause (`idle::interrupt(Cause::{Timer,Ipi,Device(identity)})`, the identity being the IDT vector on
  x86_64, the INTID on aarch64, the IMSIC identity on riscv64).
- System graph: IDL records `core-wake-source` and `core-idle`, `graph.cores` (regenerated, `--accept-breaking`
  - pre-release wire change), filled by SystemGraphService (`idle_rows`, span `cpu.idle`) and printed by the
  shell's `graph`.

TEST HOOKS (test build only): `idle::in_the_next_window(cpu, hook)` / `no_window()` run a hook between a
halt's last check and the halt; `idle::bsp_asleep_until()`; x86 `serial::pace(bool)` makes a drain push one
FIFO load, as a UART at its baud rate does (QEMU's takes everything at once).

HARNESS AND GATES.
- `qemu-run.sh`: `TSC_DEADLINE=on|off` (x86_64), so the LAPIC-count one-shot can be run under KVM, whose
  in-kernel local APIC always offers the deadline mode.
- New gate `tickless-idle` (`src/tools/check-tickless-idle.py`, all three targets one at a time; registered in
  `check.sh`, the verification model's gate list and guest-booting list, and `release-required.toml`): builds
  and boots the development build cold with the serial line on a socket it holds; types three bursts on the
  UART of the idle machine and times each echo (bound 1 s); reads the per-core records through `graph` before
  and after and requires at least one wake per burst on the identity the kernel printed; measures each idle
  core's wakes per second over 10 s; plugs a `virtio-serial-pci` into `hotplug0` over QMP and waits for the
  kernel's arrival line - after the typing, so riscv64's shared identity counts the UART's wakes first.
- `release-required.toml` gains `host.tickclock` and `gate.tickless-idle`.

## P02M0198a - verification so far (2026-09-28, x86_64 and the host)

PASSED:
- `cargo test --manifest-path src/tickclock/Cargo.toml`: 9 passed (the computed tick, the non-decreasing
  maximum, compare values, the suspended state and the rebase for a counter that kept running and one that
  restarted, two sleeps in one offset).
- `cargo test` in `src/fdt`: 115 passed, 7 of them new for `console_interrupt` (aarch64 GICv2/v3/v3+ITS: SPI 1
  level through the root's GIC; riscv64 AIA: source 10 level through the APLIC the node names; riscv64 PLIC:
  one cell; a node's own `interrupt-parent` over the root's; `interrupts-extended` winning; no line / no parent
  / an unknown phandle / a controller with no `#interrupt-cells` / a short specifier all refused).
- x86_64 kernel tests, `TEST_SELECTION` of part a's eleven: the four new `kernel.idle.*` tests, the remote-spawn
  control, the clock across cores, the periodic wait, the bounded drain and wait, and both recovery-ladder
  tests - 11 passed with the TSC-deadline one-shot (boot line "idle: an idle core's timer is a one-shot - the
  TSC deadline"), and 11 passed again under `TSC_DEADLINE=off` (boot line "... a local APIC count, re-armed in
  steps"). Measured on the first: a timeout armed on core 1 for tick 43 while the BSP slept toward 641
  expired at 43; a thread placed in the window at tick 39 (one-shot at 339) ran at 39; a one-shot for tick 36
  expiring in the window ended the halt at once (the sleeper ran at 37); 314 bytes left the ring 10 ticks after
  their core went idle, paced one FIFO load a drain.
- MUTATIONS, each run against its test and each caught: `deadline_armed` sending no IPI - "the timeout armed
  for tick 44 expired at 642: the boot processor slept on toward the wake it had published"; interrupts
  enabled and disabled again between the window and the halt - "the thread made runnable in the window at
  tick 34 ran at tick 334" and "the halt entered after its one-shot expired slept on until core 1 woke it";
  the one-tick cap for a non-empty transmit ring removed - "a burst of 314 bytes stayed in the ring for 100
  ticks after its core went idle". The source was restored after each (checked by grep).
- x86_64 kernel tests `--tags scheduler,smp,interrupt,apic,console,process`: 175 passed before the run reached
  the modem-service test and made no further progress (see FOUND ON THE WAY).
- Gate `tickless-idle` on x86_64 (`python3 tools/check-tickless-idle.py x86_64`): PASS - 3 bursts, 45 keys
  typed one at a time on the UART, each echoed within 10 ms (median 5 ms); the per-core records credited 28
  wakes to the UART's interrupt (vector 36) beyond what typing `graph` alone does (1); a `virtio-serial-pci`
  plugged over QMP into `hotplug0` was seen 74 ms later; each core's wakes over 11 idle seconds: cpu0
  94.2/s, cpu1 0.0/s, cpu2 0.0/s, cpu3 0.0/s. THE FIRST CRITERION WAS REWRITTEN AFTER ITS FIRST RUN: a burst
  written whole lands in the UART's FIFO as one interrupt, and the development image's boot processor is
  woken 50 to 95 times a second by userspace housekeeping (DeviceManager's driver heartbeats among it), so
  whether that one interrupt found it halted was a coin toss - measured: 3 bursts, 1 wake. Typed a key at a
  time, each key is its own interrupt, and the reads before and after subtract a control read that measures
  what typing `graph` itself contributes.
- `source-hygiene` (clean), `arch-surface` (64 files, no placeholder), rustfmt on every touched crate,
  `shfmt -d` on the touched scripts, `foreign-audit-link.py --check` (the recorded inventory reproduces; its
  first attempt failed once in `build-foreign-static.sh` and passed on the retry).

FOUND ON THE WAY:
- `permission_manager_enforces_static_and_dynamic_probe_policy` failed on the vocabulary P02M0192 extended
  (`input-gamepad`); fixed and recorded as an addendum in that milestone's record.
- THE MODEM-SERVICE TEST MAKES NO PROGRESS WHEN IT RUNS AFTER THE MEDIA-IMPORT TEST, alone it passes in 25 s.
  Reproduced with the pair alone. Sampled through an HMP monitor while stuck: cpu0 busy in a user process's
  `sys_wait_any`/`sys_channel_recv_caps`, cpus 1-3 halted. With the idle one-shot disabled - every halt
  keeping the periodic tick, as before this part - it still stalls, so it is not a lost wake. Five services
  (media-import, spool, modem, camera, MIDI) answered a CLOSED serve or admin root with `return` and waited on
  it again at once - a closed channel stays readable, so each spun a core for ever once its root was gone;
  fixed (a closed root is closed and dropped from the wait set; the connections it minted are still
  served). The pair still stalls after that fix, so it is not the only cause; the tree before this part
  (4afb8288) is being built to run the same pair.
- The gate's first form built with `STRIP=none` as the cold scenario runner does; over a tree whose consumers
  recorded the stripped libraries that build refuses the mix ("abiprobe admits icdprobe at ..."), so the gate
  builds the development profile with the ordinary strip.

NOT YET RUN (the end of the job): aarch64 and riscv64 - neither compiled nor run; the whole kernel suite on all
three; the gate on aarch64 and riscv64; the idle wakeups per second BEFORE this part for `docs/PERF.md`;
`verify-model check` (it cannot load the model until the two ports' test kernels are rebuilt: their built
suites still name the replaced clock test).

## P02M0198b, c and d - what was implemented (2026-10-01)

THE KERNEL (`src/kernel/processor/mod.rs`, `src/kernel/arch/x86_64/processor.rs`, `src/procpower`):

- `procpower`, a crate a host drives: the install checks (`check_register`, `check_idle_table`,
  `perf::check_table`), whether a state is enterable on this CPU (`enterable`: MWAIT offered, the invariant TSC past
  the first state, a timer that keeps running, no context loss), the idle governor (`governor::Predictor`,
  `governor::choose` - the deepest enterable state whose target residency the predicted idle time covers and whose exit
  latency the bound allows), the performance governor (`perf::next_level`: busy past 80 % of its period the window's
  fastest level, idle past 70 % one level slower, never faster than the table's transition latency or 10 ms), the
  table's levels (`perf::Table::levels`, `setting`: the performance states, CPPC spread over at most 32 levels, then the
  throttling states past T0 at the slowest performance state), the register counts (`holdings`), the latency requests
  (`latency::Requests`: four per process, 64 in all), the memory admission (`admission`, with the development build's
  fixture exception) and the ABI's records read (`records`, shared by the kernel and the service). CPPC's
  energy-performance preference register is carried and counted with the table.
- Syscalls 123 to 128 (`src/abi`): the idle table, the performance table, the window, the injection, the latency
  request (a `LatencyRequest` object, released with its last handle) and CPPC's preference
  (`SYS_PROCESSOR_PERF_PREFERENCE`, `ERR_UNSUPPORTED` where the table names no preference register); the
  `ProcessorPower` and `IdleLatency` privileges, minted by the boot chain and kept by ServiceManager.
- Every register a table names is taken before the old table's are given back, counted per table, ports into the
  reserved set and memory admitted and mapped uncached (`firmware::admit_processor_memory`); a refused table leaves the
  old one standing.
- The idle path: with no table the CPU's own - MWAIT's C1 hint where CPUID offers MONITOR/MWAIT (read once), the halt
  otherwise - counted as the record's one state; with a table the governor's choice, entered by halt, MWAIT or a
  register read; a register read that returns by itself with the waking interrupt pending under the mask takes it at
  once instead of halting past it (`after_register_entry`, the LAPIC's IRR).
- The performance governor at the scheduler's own points, the window obeyed at once, idle injection (at most a half)
  in the idle loop, the latency bound read on the idle path without a lock.
- After a sleep that lost power, every core's level and preference written again (`processor::resumed`, from the sleep
  epilogue).
- The free per-core record (`CpuIdleInfo`) extended with each state's entries and residency, the level, the time at
  each level, the window, the injection and the latency requests; the system graph's `core-idle` rows carry them
  (`src/idl/observability.lsidl`, `system_graph_service.rs`), and the tickless gate's row pattern reads past them.

PROCESSORPOWERSERVICE (`src/user/services/core/src/processor_power_service.rs` and its `cores.rs` and
`thermal.rs`; manifest `processor_power_service`, transparent, plan-relaunchable, its PROCPOWER, SLEEP, SYSPOWER and
SHUTDOWN roles filled by `supervisor_role` at every start):

- Every processor the ACPI service reports that is a running core: its `_LPI`/`_CST` installed (a state wanting
  bus-master arbitration left out and said), its CPPC or `_PSS`/`_PCT` table with `_PSD` and `_PTC`/`_TSS` installed,
  each level's share of the fastest computed from the table the kernel will hold (`procpower::records`).
- The window where the profile, `_PPC` and the thermal cap meet; `_TPC` obeyed above any profile; the injection where a
  limit is below the slowest level; CPPC's preference per profile - each set only when it changed.
- `Notify` 0x80, 0x81 and 0x82: the processor read again, what changed installed again, acknowledged through `_OST`.
- The power source from PowerService's state, through `power_model::canon::supply` - moved there from PowerService's
  sleep policy, which now uses it too.
- Every `thermal-zone` and `cooling-device` publication adopted through the catalogue; passive cooling by the equation
  once per `_TSP`, with the zone's driver asked to sample at `_TSP` while it is engaged; the fans from the active trips
  and, where `_FIF` leaves the fan to the operating system, the owner's curve; `_SCP` from the profile; past `_HOT`
  hibernation through `system-sleep` (`sleep-reason` gains `thermal`), past `_CRT` or where hibernation is refused the
  forced deadline through `system-power`'s `power-off-within` and then `system-shutdown`'s orderly power-off.
- `processor-power-admin` on CONTROL: the status, the profile and a fan's curve.
- THE POLICY'S DECISIONS are `service_logic::processor_policy` (host-tested): `default_profile`, `profile_window`,
  `window`, `Cooling::sample` (ACPI 6.5 11.1.5.1's equation and `Pn = Pn-1 - ΔP` held between 0 and 100 %, in
  thousandths, with no rounding), `limit_level`, `strictest`, `active_percent`, `default_curve`, `check_curve`,
  `curve_percent`, `fan_control`, `Trips::reading`, `energy_preference`, `cooling_mode`.

THE DRIVERS:

- `acpi_power`'s cooling half (`acpi_power_driver.rs`): a zone's binding publishes `thermal-zone` beside its
  `power-source`: `describe` (`_PSV`, `_CRT`, `_HOT`, `_TC1`, `_TC2`, `_TSP`, `_PSL`, every `_ACx` with `_ALx`, whether
  there is `_SCP` - asked with `acpi-node.has`), `readings` (every `_TMP` read), `sample-every` (at least 100 ms, the
  zone's own period again at zero or when the policy's connection closes) and `cooling-policy` (`_SCP`). Every reading
  is compared with `_CRT` through the over-temperature alarm the power model derives, and at the crossing the forced
  power-off is armed through the `system-power` connection DeviceManager hands a zone's binding alone.
  `drivers::acpi_power::devices` and `namespace_path` read `_PSL` and `_ALx`.
- `acpi_fan` (`acpi_fan_driver.rs`, `drivers::acpi_fan`): a `PNP0C0B` node published as `cooling-device`: `_FIF`,
  `_FPS`, `_FSL`, `_FST`, or the device power state for an ACPI 1.0 fan; it takes the sleep itself - the node in the
  state the sleep lets it enter, and at the resume `_FST` read and the level last commanded applied again, saying the
  level it found.

THE TOOLS AND GRANTS: `powerctl` (`src/user/apps/tools/src/powerctl.rs`, a shipping tool: the power sources, the
profile, each core's states with entries and residency and its window, the live latency requests, each zone and each
fan; the profile and a fan's curve set) granted `power-state` and the new `processor-power` capability
(`security.lsidl`, PermissionManager's vocabulary, the broker resolving `processor_power_service`'s CONTROL).
AudioService holds a `LatencyRequest` of 1000 us while its device plays or records (its LATENCY role, the kept
`IdleLatency` privilege).

THE FIXTURE AND THE GATES: `acpi-fixture.py --processors` adds C000 (`_PSS` through `_PCT`, `_PPC` from the pages,
`_PSD`, `_OST` into the pages, line 7's `_E07` notifying it), C001 (CPPC revision 3 with its preference register),
C002 (`_PCT` of model-specific registers), every core's `_LPI` naming the same two registers on a page of their own,
the zone's `_TC1`, `_TC2`, `_TSP`, `_PSL`, `_AL0` and `_SCP`, and the two fans (also in the sleep table);
`--processor-read` and `--raise`. `check-processor-power.sh` (`check.sh` gate `processor-power`); the fan across a sleep
in `check-sleep.sh`'s platform boot; `procprobe`, the development probe that loads every core or runs the scripted
idle pattern; `qemu-run.sh` takes `INVTSC=on`.

DECISIONS AND OPEN QUESTIONS FOR THE OWNER:

- THE DEFAULT PROFILES PER POWER SOURCE: balanced on line power (or where the source is not known), power saving on
  battery - PROPOSED, to be confirmed. Performance keeps every core at its fastest state, balanced lets the governor
  use every performance state, power saving the slower half; no profile reaches a throttling state.
- THE DEFAULT FAN CURVE where `_FIF` leaves the fan to the operating system: off below 40 C and a straight line to all
  of its range at 80 C, in steps of ten degrees - PROPOSED, to be confirmed.
- THE `_CRT` BOUND: ten seconds, as PowerService's critical battery already uses - PROPOSED, to be confirmed.
- CPPC's preference per profile: 0, 128 and 192 - never 255, which some platforms read as "lowest performance always".
- `_TPC` above zero holds the core to the slowest performance state throttled to that state: the one level order puts
  throttling only below the slowest performance state.
- The specification states the passive-cooling equation and gives no numeric example; the host suite's cases are
  worked by hand from the equation (`processor_policy/tests.rs`), which the item's "worked examples" is read as.

## P02M0198b, c and d - verification so far (2026-10-01, x86_64 and the host)

- Host suites (`cargo test --manifest-path ...`): `procpower` 23 (the governors, the install checks, the holdings, the
  latency bounds, the admission with the fixture exception, the ABI's records); `service-logic` 737, of them
  `processor_policy` 9 (the equation, the windows, the limits as levels and injection, the fans, the curves, the
  critical trips); `power-model` 51 (`canon::supply` among them); `acpi-model` 22 (the processor objects, `uid_of`,
  `core_of`); `acpi` 51 (`Madt::processors`); `drivers` 451 (`acpi_fan` 3, `acpi_power`'s device lists); `abi` 28;
  `proto` 53; `driver-protocol` 79; `system-manifest` 29; `aml` 45.
- MUTATIONS ON THE HOST, each watched failing and reverted: the equation's sign (two tests), a level's capacity compared
  strictly, `_CRT` never acted on, `_PPC` left out of the cap.
- The processor SSDT run through the interpreter and the parsers on the host (a stub DSDT with `\_SB.CPUS.C000` to
  `C002` and the BAR at 0xFE000000): `_PSS` 4 states, `_PCT` at BAR+0x8000 and +0x8004, `_PPC` read from the pages,
  `_PSD` software-all, every `_LPI` the halt and two states on BAR+0x8010 and +0x8014 (50 and 400 us), `_CPC` 255..50
  with its desired register at BAR+0x8008 and its preference at BAR+0x800C, C002's `_PCT` model-specific, `_PSL` and
  `_AL0` as references, `_FSL`, `_FST`, `_OST` and `_SCP` writing the pages.
- Kernel, x86_64: `TEST_SELECTION=<the five kernel.processor tests> ./test.sh --arch x86_64` -> PASS, 5 passed (29 s).
  EACH WATCHED FAILING against a mutation of its own, one run each: a register's count never reaching zero (`holdings`),
  a model-specific register admitted by `check_register` (the test's register made 32 bits wide, so the refusal is the
  model-specific register's and not the width's), a request's end not lifting its bound, an idle period not counted, a
  preference "written" with no register. FOUND ON THE WAY: the tests' ports were fw_cfg's (0x514, 0x515, 0x518) - the
  kernel holds 0x510..0x51B - moved to 0x0E10..0x0E20; and a mutation restored with `cp -p` kept its old mtime, so cargo
  kept the mutated `procpower` - the restore now refreshes the mtime.

## P02M0198b, c and d - faults found on the way to the gate, and fixed (2026-10-01)

- THE KERNEL PANICKED MAPPING THE FIRST REGISTER PAGE: the processor registers' virtual window was placed on the MSI-X
  window's top-level slot, and nothing reserved it at boot, so the first install's mapping walked a table another
  window owned. The window is now `0xffff_f600_0000_0000` (`arch/x86_64/processor.rs`, `REGISTER_VIRT`, past LAPIC
  f100, IOAPIC f200, MSI-X f300, ECAM f400 and the declared windows' f500), reserved at boot by
  `processor::reserve_window()` from `mem::init`, right after the kernel's own virtual-map reservation.
- A CPPC CORE STAYED MID-LEVEL AT REST: one level slower per period from 32 levels is seconds of needless performance
  after a load. `procpower::perf::next_level` now sends a core busy for less than 5 % of its period
  (`REST_PERMILLE`) straight to the window's floor; between 5 % and 30 % it still steps one level at a time.
- PROCESSORPOWERSERVICE'S PROCESSORS AND POWERSTATE ROLES TIMED OUT: a `client` role delivers a duplicate of the
  provider's kept root end, which only works for a provider serving requests on its root; the ACPI service and
  PowerService answer CONNECT on these roots and serve the connection it mints. Both roles are `factory` now
  (`services/manifest.toml`).
- THE DEVELOPMENT PROBE WAS REFUSED BY THE LAUNCHER: `procprobe` had no PermissionManager entry - it has one now,
  with no grants. Its loads were launched one per core, and launches through the development agent run one at a time,
  so the second waited for the first: `procprobe load SECONDS THREADS` now runs every thread in one launch
  (`rt::pool::Pool`).
- THIS SCHEDULER BALANCES NOTHING, AND THE GATE NOW SAYS SO: a thread starts on its starter's core and only a wake
  moves it, so which core a load lands on is not the gate's to choose. `check-processor-power.sh` launches the load
  again (at most five times) until it lands on C000 or C001 and reads the core it landed on; the profiles' checks pin
  the levels through the performance profile instead of relying on where a load ran.
- EVERY `processor-power` GRANT WAS REFUSED: PermissionManager keeps the connection the broker mints for it and mints
  each launch's grant from THAT connection (`rt::connect_or_resolve`), and ProcessorPowerService answered CONNECT on
  its root only - on an operator connection it was an unknown request and the connection was closed, so `powerctl`
  never launched. CONNECT and the heartbeat are answered on every connection now (`serve_control`), as TypeCService
  answers them, and the operator bound is 8 (PermissionManager's, a tool's, a replacement's).
- THE FIXTURE'S FAN1 DECLARED A FINE-GRAIN STEP OF 10, on which the default curve's 75 % at 71.8 C is not a level
  the fan can take; its step is 5 now, so the gate's reading is the curve's own value.
- THE BUILD: a `Vec<u64>` decoder in the observability record pulled `RawVec<u64>::grow_one` into a protocol package,
  a shared generic another provider already exports - the per-level time is a list of `core-level` records instead;
  `powerctl`'s providers are what it links (`wire` added, `process-proto` removed); and the dynamic report's tool waves
  in `lib.sh` name it.
- A RELAUNCHED PROCESSORPOWERSERVICE WAS NEVER ANSWERED BY THE ZONE'S OR THE FANS' DRIVERS: the first consumer of a
  publication takes the endpoint the driver offered, and every later one is a `CONNECT` the driver accepts into its
  `Serving` set inside `drivers::common::wait_or_answer_until` - which went on waiting on the caller's copy of the
  set, without the endpoint it had just accepted, so the relaunched instance's `describe` was never read. The wait now
  hands a newly accepted consumer back as "nothing ready" and the caller's loop builds its set again; the endpoint stays
  marked new for a loop that reports connections. The same wait serves the zone, the fans, the lid and buttons, the I2C
  HID device and the Type-C port controller.
- THE FIXTURE'S `\_SB.CPUS.C000._OST` WAS SHADOWED: QEMU's q35 DSDT declares `_OST` on every `\_SB.CPUS` processor for
  CPU hot-plug, the first declaration stands, and the SSDT's (which wrote the pages) was never run. The fixture no
  longer declares one, and the gate reads QEMU's own record of what C000's `_OST` was told (`query-acpi-ospm-status`),
  checking first that it does not already read (0x80, 0).
- THE GATE'S OWN FAULTS: `launch | grep -q` under `pipefail` failed a launch that printed after grep stopped reading
  (now `launch_says`, which keeps the output and searches it after); the per-state entry parser ran its program from a
  here-document, which took the stdin the status arrived on (now `python3 -c`).
- the host case (`perf/tests.rs:70`, at rest straight to the floor) WATCHED FAILING with the rest branch removed (22 passed, 1 failed), then 23 passed restored.

## The static gates over this goal's changes, and what they found (2026-10-01)

Run one at a time with `LIBER_DEVELOPMENT=1 ./check.sh --gate NAME` after the processor-power gate passed. Passed as
they were: `grant-vocabulary`, `supervisor-waits`, `declared-interfaces`, `provider-routing`, `single-cap-receive`,
`one-wait`, `forwarded-abi`, `test-tags`, `firmware-fixtures`, `aml-emitter`, `i2c-backend`, `ucsi-ppm`,
`ipmi-harness`, `gate-oracles`, `milestone-index`, `gate-result-logs`, `dependency-policy`, `staged-consistency`,
`build-order`, `arch-surface`. NOT THIS GOAL'S, failing as before it: `bootstrap-plan` (`font_catalogue` alone, as the
P02M0200 and P02M0201 audits record) and `driver-event-dispatch` (the `Wedged` arm's `Online` guard, from 2026-09-22).
`capability-model` (the TLA+ model check, which nothing here touches) was stopped after ten minutes.

FOUND AND FIXED:

- `kernel-allocations` - 117 infallible allocations in kernel code this goal added, most on the ACPI service's calls
  (`firmware::map`, `report`, `pci`, `node`, `loaded`, `claim_refusal`), on the processor tables' syscalls, on the
  restore's commit and in the snapshot's digests. Every one is now fallible or carries an `ALLOC-OK:` reason that is a
  boot-only or test-only path:
  - `mem::heap` gains `try_collect`, `try_to_vec` and `try_format`, beside `try_push` and `try_string`.
  - THE WORDS THE CONSOLE IS TOLD ARE WRITTEN WITH NO ALLOCATION: `firmware::Bdf`, `Text` (UTF-8 chunks, U+FFFD for the
    rest), `ListText`, `MapRefusal`, `CrsWhy`, and the refusals as enums with `Display` - `firmware::ClaimRefusal`
    (with `NoMemory`, which refuses: a claim must never land under a region) and `device::Unpublished` in place of the
    `String` `publish_namespace` and its `admit` answered.
  - Every record a report keeps is copied or reserved before anything is published, so a short heap refuses the report
    whole (`ERR_NO_MEMORY`): the identity, the properties, the targets, `reported`, `parents`, `merged`, `companions`,
    `lists`, `grants`; `withdraw` takes the merges and companions out one at a time instead of partitioning; `loaded`
    builds its list reserved; `node` copies the row's identity into a platform name's room; `msi_ranges` refuses a
    configuration write when it cannot hold the capabilities (the write may be an MSI's).
  - `arch::firmware::kernel_lines` answers a fixed array on all three targets.
  - THE PROCESSOR TABLES: the new table's room is reserved before a register is taken, and `procpower`'s `to_take`,
    `replace` (with `reserve`), `idle_registers`, `Table::registers` and `records::perf_table_of` (now
    `Result<_, Unread>`, `Unknown` or `NoMemory`, so the kernel answers `ERR_NO_MEMORY` and not `ERR_INVALID` for a
    short heap) and `latency::Requests::add` (`Full::NoMemory`) are fallible. A replacement the heap cannot hold gives
    back what it took.
  - THE SNAPSHOT'S DIGESTS ALLOCATE NOTHING: `bootproto::sha256::Sha256`, a streaming hasher (`update`, `finish`) beside
    `digest`, fed the parts' digests in turn; `arch::sleep::development_variant` fills a 64-byte buffer. The restore's
    list and directory frames are reserved before they are taken.
  - `syscall::sys_device_properties` reserves its buffer; `device::console_holder` and `bar_holder` (development
    requests) collect fallibly; the `perfbuf` ring's pushes, an `Arc` clone and the test-only sites are marked.
  Verified: `KERNEL_SRC=<tree>/src/kernel tools/check-kernel-allocations.sh` -> `kernel-allocations: clean`;
  `bootproto` 89 host tests, among them `parts_fed_in_turn_digest_as_their_concatenation` (every split of 300 bytes,
  byte by byte to 128, and the `abc` vector in two parts), WATCHED FAILING with the partial block's early return removed
  (an equivalent mutation - the redundant reset of the fill after a full block - passed, and the redundant line was
  removed); `procpower` 23; the x86_64 kernel and the test kernel compile (`cargo build`, `TEST=1 TEST_TAGS=""
  cargo build --tests`, the second after one rustc SIGILL retried).
- `development-gate` - `tpmprobe` was a DYNAMIC program of the tools crate with `development = true`, so nothing kept it
  out of a shipping image: `build-shared.sh` stages every dynamic volume row, and the manifest export is the same in both
  configurations - against the ticked "not staged in the shipping image" of P02M0190. It is now a static probe in the
  services crate behind `required-features = ["development"]`, as every other development probe is
  (`src/user/services/core/src/tpmprobe.rs`; the volume bundle, the grants after it and a bounded file read written
  there; `tpm-client`, `tpm-client-provider` and `tpm-proto` with `channel-client-impl` linked statically). And
  `admin_fixture` (not this goal's) was missing its `required-features` too - added.
- `source-hygiene` - `kernel/sleep/disk.rs` beside `disk/tests.rs`: moved to `disk/mod.rs` (a plain move; nothing else
  named the path).
- `no-fixed-provider-slots` - its extracted-production regression (`check-driver-connections.py`) no longer compiled:
  `drain_control_into` had grown the node channel (P02M0196b) and the sleep's surfacing (P02M0197a) and
  `wait_providers` had become `wait_providers_inner`. The fixture now stubs the node and the sleep the way a driver
  that takes neither sees them, extracts `wait_providers_inner` and `wait_or_answer_until`, and gains a case for this
  session's fix - a consumer accepted during a wait on the caller's set is handed back as "nothing ready" - with its
  mutation: 6 regressions passed, five mutations rejected (the new one among them).
- `component-oracles` - the census reads kernel tests' `covers` only; every driver and service this goal added is proven
  by a guest gate instead. `component-oracle-exceptions.txt` names, for each of them, the gate and what it asserts (the
  form `gamepad_fixture`'s line set): power_service, power_fixture, acpi_service, acpi_fixture, acpi_power, acpi_button,
  acpi_tad, sleep_fixture, acpi_fan, processor_power_service, hibernation_service, tpm_driver, tpm_service, i2c_hid,
  uart16550, i6300esb, tco, wdat, watchdog_service, ipmi, smbus_ich9, bmc_service, ucsi_acpi, tcpci, typec_service. The
  census still fails on components that predate this goal (bluetooth, camera, MIDI, modem, smart card and admin
  fixtures and services), which are not this goal's to describe.
- `verify-model` - `GATES` declared 159 entries and holds 160 since the processor-power row: the size is 160. Its tests
  now load the model and fail only where the aarch64 and riscv64 TEST BINARIES are older than P02M0198a - they still
  carry `the_global_clock_advances_once_per_period_however_many_cores_tick`, which that part replaced - which the
  ports' rebuild at the end of the job clears.
- `driver-protocol-note` reads the aarch64 system volume, which is older than this goal's drivers (23 notes below the
  floor of 29): the ports' rebuild at the end of the job.

## P02M0198 on aarch64 and riscv64 - the device tree's idle states (2026-10-01)

WHAT WAS IMPLEMENTED - the ports' half of the idle path, which until now halted for every state:

- `fdt::Fdt::idle_states(IdleBinding)` (`src/fdt/src/lib.rs`): one walk of `/cpus` reads every `/cpus/idle-states`
  child that is `arm,idle-state` (PSCI, `arm,psci-suspend-param`) or `riscv,idle-state` (SBI,
  `riscv,sbi-suspend-param`), not `status = "disabled"`, and carries a phandle and all four of the parameter,
  `entry-latency-us`, `exit-latency-us` and `min-residency-us` (`local-timer-stop` as a flag), and every cpu node's
  `reg` with the phandles its `cpu-idle-states` names, in its order - bounded at `MAX_TREE_IDLE_STATES` (7) and
  `MAX_TREE_IDLE_CPUS` (64). `TreeIdleStates::for_cpu(hardware id)` answers a core's states.
- `processor::install_tree_states` (`src/kernel/processor/mod.rs`), called at boot on both ports right after
  `sched::init` (`arch/aarch64/boot.rs`, `arch/riscv64/boot.rs`): every core's table is the halt and then the tree's
  states ordered by entry plus exit latency (what a latency request bounds), installed through the same
  `install_idle` a ProcessorPowerService table uses - so the governor, the counts and the records are the x86_64 ones.
  A state that loses the core's context (`arch::processor::state_loses_context`: PSCI's StateType bit, bit 30 in the
  extended StateID format and bit 16 in the original - `PSCI_FEATURES(CPU_SUSPEND)` bit 1 says which this firmware
  takes; the SBI's non-retentive bit 31) carries `IDLE_LOSES_CONTEXT`, and `local-timer-stop` `IDLE_STOPS_TIMER`; both
  are held out by `procpower::enterable` - neither port has a per-core resume path, so no such state is entered.
- `arch::processor::firmware_suspend` (`arch/aarch64/processor.rs`, `arch/riscv64/processor.rs`, new): PSCI's
  CPU_SUSPEND (`psci::cpu_suspend`, SMC64) or the SBI's HSM HART_SUSPEND with the tree's parameter, entered with
  interrupts masked - both return once an interrupt is pending, as WFI does - and the unmask after takes it. A
  firmware that refuses the call is counted (`firmware_refusals`, for the suite), said once, and the halt waits
  instead. On x86_64 the same function is the halt (a firmware entry is refused at install there).
- `map_register` answers `Result<_, &'static str>` on all three targets: the ports map no processor register (a
  table naming one comes from ACPI, which they do not read), so such a table is refused with that reason.
- THE GATE'S TREE: `fdt_edit.py idle-fixture` adds `/cpus/idle-states` with two retention states and one that loses the
  core's context, and every cpu node's `cpu-idle-states`, to the tree QEMU dumps (`-machine dumpdtb`), and
  `qemu-run.sh` hands it back with `-dtb` when a run asks `IDLE_FIXTURE=1` (`idle_dtb_args`, aarch64 and riscv64; one
  edited tree per run - another fixture's edit and this one are refused together). `check-tickless-idle.py` boots the
  ports with it and checks the boot's install lines, the per-core records over the idle sample (the retention states
  entered through the firmware, none refused, the context-losing one never) and the ports' suspend to idle.
- A KERNEL TEST ON THE PORTS (`processor/tests.rs`,
  `kernel.processor.a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request`): three tree states
  installed the way the boot installs them, the order by entry plus exit latency checked, the context-losing state
  unenterable, a long idle period entering the deep state, a 1000 us latency request keeping the core in the
  retention state, and every entry the firmware's (`firmware_refusals` unchanged). The four x86_64 cases (ports, a
  model-specific register, an idle period, CPPC's preference) are compiled for x86_64 alone - they drive port I/O.
- `cfg` CHANGES THE PORTS' BUILD NEEDED (`--deny warnings`; nothing in this codebase uses `allow(dead_code)`): the S3
  and hibernation halves the ports do not take are compiled for x86_64 (and the suite) only - `sleep::disk`'s snapshot
  side, `idle::begin_hold`/`all_others_held`/`end_hold`, `device::pci_addresses`, `iommu::resume_after_reset`,
  `declared::replay_claim_writes`, `sched::kernel_cr3`, `mem::frame::is_free`, `sync::get_unlocked`, `entropy::stir`,
  `console_tap::signal`, `pci::io_bars`/`set_io_decode`, the port-range copy's accessors, the x86_64-only suite
  helpers; `declared::suppressed` answers false on the device-tree ports (no ACPI table suppresses a row there).
  `main::announce_measurement` (the `\x1ePERF tsc_hz` anchor and the frame account's buffer) is called by every
  port's boot, as `firmware::init` now is before `device::init` on both ports.

DECISIONS:

- The tree's states are installed by the kernel itself at boot, with no service between: on a device-tree machine
  there is no interpreter, and the item's "the kernel reads them" is taken literally. A later table from a service
  replaces it, as on x86_64.
- No context-losing state is entered on the ports: a core that loses its context needs a per-core resume path (the
  warm-boot entry PSCI and the SBI jump to), which neither port has. The state is installed, shown in the records as
  unenterable and said at boot - not silently dropped.

VERIFICATION, so far: the six kernel configurations (`cargo check` for x86_64, aarch64 and riscv64, production and
test) compiled clean in the working copy before it was synced; `fdt` 123 host tests (the idle-state parser's among
them). THE PORTS' RUNS - the kernel test above and `check-tickless-idle.py aarch64 riscv64` with the fixture - are
the job's slow-architecture step at its end and are recorded when run.

## P02M0198 - the gate batches of 2026-10-01 (x86_64)

- `processor-power` -> PASS three times (the third with the ports' processor work in the tree), every line of
  P02M0198b to d's guest checks as listed in the gate's PASS line.
- `TEST_SELECTION=<the five kernel.processor tests and the seven handoff tests> ./test.sh --arch x86_64` -> PASS, 12 in
  37 s, after `./build.sh --arch x86_64` (a run before the build had refused: "the x86_64 build does not match the
  sources").
- `python3 tools/check-tickless-idle.py x86_64` -> PASS: 3 bursts of 45 keys echoed within 4 ms (median 2 ms), 36 wakes
  on the UART's interrupt, an idle cpu0 at 91.7 wakes/s (the housekeeping bound) and the others at 0.0/s, a device
  plugged in seen 77 ms later.
- `kernel-allocations`, `source-hygiene` (after its own fix in the sleep gate's `run_stick`: a `tr | grep -q` under
  `pipefail`), `development-gate`, `no-fixed-provider-slots`, `gate-oracles`, `firmware-fixtures` -> PASS.
- The fan across a sleep: the gate `sleep`'s platform boot - P02M0197's audit.

## P02M0198 on aarch64 and riscv64 - run (2026-10-01)

- `./build.sh --arch aarch64` and `--arch riscv64` -> ok (after the fixes recorded in P02M0190's and P02M0196's audits
  and one `foreign-audit-link.py --check`, which the rt changes of this goal require: "the recorded pass-2 inventory
  reproduces"); `./build.sh --arch x86_64` -> ok again after the graph key's change (904 s, every cache rebuilt).
- `TEST_SELECTION=kernel.processor.a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request
  ./test.sh --arch aarch64` -> PASS (1 passed, 153 s); `--arch riscv64` -> PASS (1 passed, 211 s). On both: "core 0's
  idle table from the device tree - 4 state(s)", the context-losing state said not entered ("it loses the core's
  context, and no per-core resume path"), the retention states entered through PSCI's CPU_SUSPEND and the SBI's
  HART_SUSPEND with no refusal, the deep state entered by a long idle period and the retention state within a 1000 us
  request.
- The firmwares' offers, said at boot: "PSCI SYSTEM_SUSPEND is not offered by this firmware" (aarch64) and "the SBI
  System Suspend extension is not offered by this firmware" (riscv64).
- `python3 tools/check-tickless-idle.py aarch64 riscv64` (2026-10-01) - aarch64 PASS on its second run (the first
  never reached a shell: the aarch64 kernel left EL0 reads of CNTVCT_EL0 trapping - P02M0189's audit): every core
  installed the tree's 4 idle states with the context-losing one held out; 3 bursts of 45 keys echoed within 21 ms
  (median 6 ms), 22 wakes on the UART's interrupt 33; the retention states entered 1311 times over the sample through
  PSCI and the context-losing state never; an idle cpu0 at 67.2 wakes/s, the others at 0.0/s over 20 s; a plugged
  device seen 45 ms later; "PSCI SYSTEM_SUSPEND is not offered by this firmware"; suspend to idle - 5007 ms between
  the kernel's lines for a 5 s wake with the counter silent, count 2 then 3 (monotonic +3475 ms, boot-time +8476 ms),
  4 cores parked with no wake but the wake.
- riscv64, on the same tree, passed every stage up to the suspend to idle in two runs whose failures were the GATE's:
  (1) `graph`'s prompt buried by a late ipv6 line, waited for 600 s - `Serial.wait_prompt` now types an empty line
  once the output has been quiet a while past a prompt the command produced, as `lab`'s boot wait does; (2) the
  shell's prompt, printed when `sleepctl` ended, landed inside a background counter line ("sleepcheck: vol://system>
  count 23 ...") and read as a skipped value - the prompt is taken out of a line before the counter's pattern is
  matched (`PROMPT_TEXT`). Its stages so far: the 4 tree states installed, 45 keys echoed within 25 ms with 26 wakes
  on the UART's interrupt 63, the retention states entered 1259 times through the SBI and the context-losing one
  never, cpu0 at 58.8 wakes/s and the others at 0.0/s, a plugged device seen 52 ms later, "the SBI System Suspend
  extension is not offered by this firmware", and the suspend to idle's own numbers in its log (5001 ms slept, count
  2 then 3 with monotonic +4688 ms and boot-time +9690 ms - the difference the 5 s sleep - and every core parked with
  no device wake). The third run is in progress.
- `powerctl` had the same generic transport residual as P02M0192's `gamepad` (power::Client and
  processor_power_admin::Client over `ChannelTransport` in the tool): `power-client` (PowerService's `sources`;
  ProcessorPowerService's `status`, `set-profile`, `set-fan-curve`) and `power-client-provider` (the four
  trampolines), a `lib/clients/power-client.lslib` row, and `powerctl`'s providers `base-proto`, `power-client`,
  `lsrt`. `./build.sh --arch x86_64` ok; the `processor-power` gate, which drives `powerctl`, is in the batch running
  now.
- riscv64's third run -> PASS: suspend to idle "the host measured 5009 ms for a 5 s wake with the counter silent,
  count 2 then 3 (monotonic +4641 ms, boot-time +9643 ms), 4 cores parked with no wake but the wake". And x86_64
  again with the gate's two fixes -> PASS (keys within 4 ms, a plugged device seen 80 ms later).
- `docs/PERF.md` gains "An idle core's wakeups once the tick stops": application cores 0.0/s on all three targets,
  the boot core 91.7 (x86_64), 67.2 (aarch64) and 58.8 (riscv64) a second - the housekeeping bound's rate. The BEFORE
  figure is the tick's own 100/s, by construction, not a run of the old kernel.
- `verify-model release-required --write`: 335 keys, seven new (`gate.processor-power` and the host suites of
  `bmc-proto`, `platform`, `procpower`, `smbios`, `tpm-proto`, `typec-proto`); the gate `verify-model` then asked for
  `acpi`, `aml`, `platform`, `procpower` and `tpm` in `lib.sh`'s VOLUME_SOURCES - each reaches the system volume and
  a change in it was invisible to the staleness check - and passes with them. `verify-model-tests` pass.
- The last host gates over the tree as it stands -> PASS: `dynamic-report` (after `--refresh dynamic-report` wrote
  the three tables), `driver-protocol-note` (red before only because the aarch64 volume was older than this goal's
  drivers), `source-hygiene`, `kernel-allocations`, `development-gate`, `dependency-policy`, `staged-consistency`,
  `arch-surface`, `verify-model`, `firmware-fixtures`, `milestone-index`. Host suites: `service-logic` 737,
  `driver-binding` 93, `procpower` 23, `drivers` 451.

Status: parts a to d implemented and run on the targets each names; OPEN for context-losing idle states (P02M0197's
per-core resume path on the ports), the owner's three-target kernel run, and the owner's confirmation of the defaults.

## The owner's profile defaults, built (2026-10-02)

The owner decided (2026-10-02): on mains the maximum performance, on battery something between (balanced), and the
person can switch.

WHAT WAS DONE:
- `service_logic::processor_policy::default_profile`: performance on line power (or where it is not known), balanced
  on battery; its host test follows.
- `power-profile` gained `automatic = 4` (`src/idl/power.lsidl`; regenerated with `--accept-breaking`, nothing being
  versioned before the first release): `set-profile(automatic)` gives the choice back to the power source's default,
  and a status never names it. ProcessorPowerService maps it to "no choice"; `powerctl profile auto` asks for it.
- THE GOVERNOR DECIDES AT A CHANGE OF THE WINDOW (`kernel/processor`): with performance the default, the gate's switch
  to balanced left C001 - idle the whole time, so with no utilisation period ending to decide at - at the fastest level
  the performance window had pinned (`cpc_desired` 255 for 20 s). `set_window` now decides the level in the new window
  at once from the busy share the core's last period measured (`last_busy_permille`, recorded by `on_tick`), through the
  same `perf::next_level` and its transition-latency rule.
- The gate `processor-power`: at boot the line-power default is performance (both cores pinned at their fastest,
  preference 0, "profile performance" in the service's online line); balanced is chosen before the at-rest and
  under-load checks; and after the profile cases `powerctl profile auto` gives the choice back - preference 0 and
  "profile: performance (the default on line power)" in `powerctl status`.

VERIFICATION:
- `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml` -> 746 passed (processor_policy 9).
- `./gen.sh --accept-breaking` -> ok (`power-profile` gained a value; the ABI checker calls an added enum value
  breaking, and nothing is versioned before the first release).
- The first run of `LIBER_DEVELOPMENT=1 ./check.sh --gate processor-power` with the new defaults -> FAIL: "at rest,
  C001's desired performance is not CPPC's lowest (cpc_desired reads 255, not 50, after 20 s)" - the idle core kept the
  level the performance window pinned; the `set_window` change above is the fix.
- `cd src/kernel && cargo build` and `TEST=1 TEST_TAGS="" cargo build --tests` -> ok.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate processor-power` -> PASS (2026-10-02): "on line power the default is
  performance: C000 at 0x10 and C001 at 255, with preference 0"; "at rest C000's control register reads 0x13 and
  C001's desired performance 50, with the balanced preference 128"; "auto gives the choice back: performance, the
  default on line power, with preference 0"; every other case as before.

## Idle states that lose the core's context, entered on aarch64 and riscv64 (2026-10-03)

WHAT WAS DONE:
- `procpower::Cpu::context_resume` is true on both device-tree ports, so the governor may choose a state that loses the
  core's context there; `firmware_suspend` enters such a state through P02M0197's per-core resume path -
  `resume::save_and_leave(suspend_leave, ..)`: the core's callee-saved and EL1/S-mode state saved into its record, then
  a power-down CPU_SUSPEND (aarch64) or a non-retentive HART_SUSPEND (riscv64). QEMU's PSCI ends the power-down
  CPU_SUSPEND as a wait - the call returns with the context kept - and OpenSBI ends the non-retentive default suspend
  at the resume address, so the hart comes back through `riscv64_resume_start` and returns `RESUMED` on its own stack
  with its IMSIC file and tick put back.
- A state that also stops the core's timer (`local-timer-stop`) is still held out: no broadcast timer, as part b says.
- `kernel.processor.a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request` changed with it: a
  1000 us request now admits the context-losing state (entered, on riscv64 through the entry), a 300 us request the
  retention state alone.

DECISIONS: the gate `tickless-idle`'s tree keeps its context-losing state `local-timer-stop`, so the gate's existing
oracle ("the one that loses the context never") still holds and tests the timer rule; the context-losing entry itself
is proven by the kernel test, whose tree has a context-losing state that keeps its timer.

VERIFICATION:
- The ports' kernel selection (16 ids each, `--tags kernel,scheduler,smp,domain,process,syscall`, after
  `env -u LIBER_DEVELOPMENT ./build.sh --arch <port>`): PASS on aarch64 (16 passed, 153 s) and riscv64 (16 passed,
  223 s) - `kernel.processor.a_trees_idle_states_are_entered_through_the_firmware_within_a_latency_request` and
  `kernel.processor.a_core_that_lost_its_context_comes_back_through_the_entry_its_boot_came_through` among them.
- `python3 tools/check-tickless-idle.py --no-build aarch64` (2026-10-03): PASS - the context-losing tree state held out
  for its timer, the retention states entered 1303 times, the wakeups as in `docs/PERF.md`; riscv64 ran PASS on
  2026-10-02 with the same kernel change.
- The gate `hibernate-ports` (PASS on both ports, 2026-10-02) holds and restarts every core through the same per-core
  path.

## `_CRT`: the orderly power-off with no deadline, and a separate threshold for the immediate one (2026-10-03)

The owner decided (2026-10-03): no ten seconds waited for the orderly power-off to end; a separate threshold for the
immediate power-off instead. The default fan curve and CPPC's preferences stand as built.

WHAT WAS DONE:
- `power_model::acpi::IMMEDIATE_MARGIN` (50, the zone's tenths of a degree: five degrees) and `immediate_trip(critical)`
  - the threshold above `_CRT` past which the machine goes off at once. ACPI defines no trip above `_CRT`; this one is
  the system's, and the two places that act on it share it.
- `service_logic::processor_policy`: `Critical::Immediate`, and `Trips::reading(temperature, critical, hot, immediate)`
  - the gravest trip first, each once per crossing, a reading past several asking for the gravest alone. Host test
  `past_the_immediate_threshold_the_machine_goes_off_at_once`; the existing trip test with the new argument.
- ProcessorPowerService: past `_CRT` the orderly power-off through `system-shutdown` and nothing armed after it ("is
  past _CRT - the machine powers off in order"); past the threshold `system-power`'s `power-off` at once ("is 5.0
  degrees past _CRT - the machine powers off at once"); `_HOT`'s refused hibernation still takes the `_CRT` sequence.
  `FORCED_BOUND_SECONDS` is gone from it. A `_TMP` reading the firmware calls unknown (`0xFFFFFFFF`) trips nothing and
  moves no cooling - before, it compared above every trip.
- The zone's driver (`acpi_power_driver`): at `_CRT`'s crossing it says so and leaves it to the policy - it arms no
  deadline any more; at the immediate threshold it powers the machine off at once through its own `system-power`
  connection, so a stopped or dead policy still stops a zone that goes on heating there. Unknown readings are ignored.
- The gate `processor-power`: case 2 past `_CRT` - the orderly power-off under way, no deadline armed, nothing at once,
  QEMU gone; case 3 with the policy stopped - past `_CRT` the driver's line and the machine running on 10 s later, past
  the threshold the driver's immediate power-off and QEMU gone within 5 s with no orderly power-off; case 4, a new boot
  with the policy running, straight past the threshold - off at once (by the policy or the driver, whichever is first:
  the first power-off ends the machine before the other speaks) with no orderly power-off.

VERIFICATION:
- `cargo test --manifest-path user/services/logic/Cargo.toml processor_policy`: 10 passed (the new
  `past_the_immediate_threshold_the_machine_goes_off_at_once` among them); `cargo test --manifest-path
  user/libs/power/model/Cargo.toml`: 51 passed.
- `./check.sh --gate processor-power` (x86_64, 2026-10-03): PASS in 614 s - "past _CRT: the orderly power-off run with
  no deadline armed, QEMU gone 2234 ms after the zone was heated past it"; "with ProcessorPowerService stopped: past _CRT
  the machine ran on; past the immediate threshold the zone's driver powered it off at once, QEMU gone 273 ms after";
  "straight past the immediate threshold with the policy running: the machine powered off at once (the zone's driver
  first), with no orderly power-off, QEMU gone 267 ms after". An earlier run passed the same cases but timed them from
  the moment the gate saw the line, after QEMU had gone (negative milliseconds); they are timed from the heating now,
  with the immediate ones held to 5 s.
- The development builds of all three (with the rt change's audit relink): RESULT ok.

BLOCKERS: none. Open in this milestone: the owner's three-target kernel run.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0198 (2026-10-08T03:26:12Z):

Continuation review (2026-10-08). Read the complete milestone plan, the previous implementation record, docs/TESTING.md and docs/ARCHITECTURES.md. Existing implementation and past results are being checked against the current tree; prior records are preserved verbatim. No new guest execution or hardware verification has passed in this continuation yet.

Current-tree review found the completed structure recorded by the previous implementer: `tickclock::Clock` supplies
the computed ABI tick and the shared suspend offset; `kernel/idle` owns masked final-check/one-shot halts and their
wakes; `kernel/processor` installs tables, accounts states and bounds the governors; `processor_power_service` and
`processor_policy` hold the confirmed profiles and thermal actions. The latest plan records the three complete
kernel suites and the alternate x86 timer run on 2026-10-06/07, later than the old audit's last open-run note. Those
are historical recorded results, not tests rerun in this continuation. No required implementation change was found
in this review; status stays COMPLETE.

Fresh targeted verification (all exited 0):
- `cargo test --offline --manifest-path src/tickclock/Cargo.toml`: 9 passed.
- `cargo test --offline --manifest-path src/procpower/Cargo.toml`: 23 passed.
- `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml processor_policy`: 10 passed.

Not rerun here: the full kernel suites, guest processor-power and tickless-idle gates, or hardware qualification.
There is no new change to the clock or kernel governors requiring another full sweep in this continuation.
