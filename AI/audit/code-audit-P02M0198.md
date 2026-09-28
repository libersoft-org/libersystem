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
