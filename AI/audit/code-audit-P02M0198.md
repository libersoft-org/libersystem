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
