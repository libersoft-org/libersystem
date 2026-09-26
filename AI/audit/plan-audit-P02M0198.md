AUDITOR'S REVIEW OF PLAN P02M0198 (2026-09-25T22:53:15Z):

**Rating: 4/10.** The division into tickless idle, idle states, performance states and thermal policy is right, and the fixture-read oracle is well chosen. But the plan never places its governors on either side of the kernel boundary, and it changes the clock under everything without stating the contract or the duties the tick carries today.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0198.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a`, together with:

- the plans it takes from or feeds: [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:63), [P02M0181](/data/yellow/libersystem/docs/todo/P02M0181.md:6), [P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md:24), [P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:44) and [P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:39);
- the kernel's tick, clock, timer, scheduler, idle-hook and power paths, and the ABI;
- SystemManager, ServiceManager and PowerService's manifest row;
- QEMU's x86 CPU model.

Plans P02M0197 to P02M0202 exist only in the working tree (they are not yet committed), and P02M0190, P02M0196 and P02M0099 carry uncommitted edits; all were read as they stand there. The plan is a requirements list whose parts each get their own written plan. The findings concern decisions those parts cannot make on their own, because this text omits or misstates them.

1. **High - The governors, the limits and the latency bound are never placed on either side of the kernel boundary, and no interface between the two sides is named.**

   The plan specifies these parts by behaviour only:
   - [the idle-state sources](/data/yellow/libersystem/docs/todo/P02M0198.md:28);
   - [the idle governor and the latency bound](/data/yellow/libersystem/docs/todo/P02M0198.md:34);
   - [the performance governor](/data/yellow/libersystem/docs/todo/P02M0198.md:43);
   - [passive cooling](/data/yellow/libersystem/docs/todo/P02M0198.md:49) and [fan control](/data/yellow/libersystem/docs/todo/P02M0198.md:52).

   Apart from its test suite, the plan never mentions the kernel. The split is not free, because the two halves live on different sides.

   On the kernel side:
   - The idling core enters an idle state from the kernel's idle loop, today [`sti; hlt`](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:131) and [`wfi`](/data/yellow/libersystem/src/kernel/arch/aarch64/mod.rs:129).
   - MWAIT, the HWP and `IA32_PERF_CTL` registers (MSRs), PSCI CPU_SUSPEND and SBI HSM suspend are privileged.
   - Utilisation and injected idle belong to the scheduler.

   On the userspace side:
   - `_CST`, `_LPI`, `_PSS`, `_PCT`, `_PPC`, `_CPC` and the thermal zones are AML. By default they are evaluated in [a userspace ACPI service](/data/yellow/libersystem/docs/todo/P02M0196.md:46) and [handed to this milestone](/data/yellow/libersystem/docs/todo/P02M0196.md:63).
   - A zone's temperature and trip points reach PowerService as [observations, not instructions](/data/yellow/libersystem/docs/todo/P02M0181.md:108). Its subscriptions [coalesce to one update per 100 ms per source and close on overflow](/data/yellow/libersystem/docs/todo/P02M0181.md:152).
   - A `PNP0C0B` fan is a namespace device whose methods only the ACPI service can run, and [P02M0196b's device classes](/data/yellow/libersystem/docs/todo/P02M0196.md:50) do not include it.

   The verification already presupposes an answer. The fixture puts [`_PCT` and `_CPC` registers in the harness's memory region](/data/yellow/libersystem/docs/todo/P02M0198.md:69), and whichever component governs must write them:
   - if the kernel, it must map an address named by AML inside a PCI BAR that no claim gave it;
   - if a userspace service, it cannot perform the fixed-function (MSR) forms.

   Parts b, c and d all depend on this split. The kernel interface it needs is the largest new interface in the milestone: how tables and caps enter the kernel, who may set them, what object a latency request is, and how residency and utilisation leave the kernel.

   **Correct the plan by adding a placement item before parts b-d.** One workable split: the kernel holds the idle governor, state entry, fixed-function P-state writes, idle injection and the latency-request object; userspace holds table evaluation, thermal policy, fan control and profiles. The item should then define:
   - the privileged kernel interface that receives per-core idle and performance tables and caps, from AML or the device tree;
   - how residency and utilisation are reported out of the kernel;
   - the latency request as a kernel object, released when its handle closes and granted through PermissionManager;
   - the thermal policy's input: a PowerService subscription, or a direct path from the ACPI service that samples at the `_TSP` period;
   - fan actuation through the ACPI service;
   - which component writes the fixture's registers.

2. **High - Tickless idle and the counter-read monotonic clock are specified without the ABI decision and without the other duties the tick carries in this kernel.**

   The plan [reads the monotonic clock from the free-running counter](/data/yellow/libersystem/docs/todo/P02M0198.md:18) and [lets an idle core sleep until its next deadline](/data/yellow/libersystem/docs/todo/P02M0198.md:20). It names preemption as the only thing a tick is for. In this kernel the tick also carries these duties:
   - **The ABI unit.** Every deadline argument is [an absolute value on the 100 Hz tick counter, and `TICKS_PER_SECOND` is part of the ABI](/data/yellow/libersystem/src/abi/src/lib.rs:223). [TimeService](/data/yellow/libersystem/src/user/services/core/src/time_service.rs:31) and [NetworkService](/data/yellow/libersystem/src/user/services/core/src/network_service.rs:62) keep private copies of the rate. The plan does not say whether ticks stay the unit or deadlines move to nanoseconds.
   - **Deadline expiry.** Timed waits expire only in the BSP's drain loop, which relies on [the 100 Hz timer to wake it and re-check](/data/yellow/libersystem/src/kernel/sched/mod.rs:1186). [The other cores do not drive deadlines](/data/yellow/libersystem/src/kernel/sched/mod.rs:1231), and the deadline list is global.
   - **Housekeeping waits.** These are [excluded from the nearest deadline](/data/yellow/libersystem/src/kernel/sched/mod.rs:999) and woken only when something else runs the check. They include StorageService's guard ([service.rs](/data/yellow/libersystem/src/user/services/storage/src/service.rs:1502)), the console caret ([console_service.rs](/data/yellow/libersystem/src/user/services/core/src/console_service.rs:693)) and LogService's flush.
   - **The BSP's idle hook.** It [detects a lost SystemManager, polls serial input, settles PCI hot-plug and delivers platform events](/data/yellow/libersystem/src/kernel/main.rs:488). Its serial polling is ["lossless at the lower poll rate"](/data/yellow/libersystem/src/kernel/main.rs:505) of the tick's wakes, and serial receive has [no interrupt on aarch64 and riscv64](/data/yellow/libersystem/src/kernel/arch/mod.rs:35).
   - **Serial output.** The timer interrupt [drains the serial transmit ring](/data/yellow/libersystem/src/kernel/arch/x86_64/apic/mod.rs:215).

   A sleep adds one more constraint. The counter keeps running during suspend to idle and restarts after S3, so the new read path is also where [P02M0197's "does not count the sleep"](/data/yellow/libersystem/docs/todo/P02M0197.md:44) must be implemented. Neither plan refers to the other.

   Built as written, an idle BSP that sleeps until the next progress deadline would cause three failures. It stops console input on two ports. It delays hot-plug and housekeeping. It misses timeouts armed from other cores. The whole-suite gate would find these one at a time.

   **Correct item a** so that it:
   - States the ABI. For example, keep `SYS_CLOCK_GET` and every deadline in 100 Hz ticks computed from the counter, so userspace does not change; or give a migration plan.
   - Lists the tick's dependents with a replacement for each:
     - a one-shot timer programmed from the global earliest deadline;
     - housekeeping deadlines included, or a bounded longest idle;
     - reprogramming when another core arms an earlier deadline;
     - serial receive by interrupt, or a bounded poll, on aarch64 and riscv64.
   - Designs the read path together with P02M0197's sleep offset and the rebase after S3.

3. **Medium - A `_CST` C-state cannot be entered "through P02M0191".**

   The [entry item](/data/yellow/libersystem/docs/todo/P02M0198.md:31) routes the `_CST` port read through P02M0191. That milestone's `PortRange` is a per-process authority for userspace drivers, [installed in the TSS when the scheduler switches to the holder's threads](/data/yellow/libersystem/docs/todo/P02M0191.md:29). Entering an I/O-register C-state is a read the idling core performs itself, in ring 0 in the kernel idle loop, as its last act before stopping. No process can do that on a core's behalf, and the kernel reaches ports without any bitmap.

   **Correct the entry item (a small change suffices).** The kernel idle loop performs the `_CST` register access, for every address space the register may name, with addresses delivered through finding 1's interface. Remove the reference to P02M0191.

4. **Medium - The critical thermal action has no executor, no authority and no fallback.**

   [Past `_CRT`](/data/yellow/libersystem/docs/todo/P02M0198.md:55) the plan requires an orderly power-off within a bound, then an immediate one, and says neither is "a policy anyone may turn off". The existing power paths do not fit that:
   - `system-power`'s power-off [is immediate](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:441), and its interface [leaves a graceful shutdown out on purpose](/data/yellow/libersystem/src/idl/process.lsidl:185).
   - The only orderly power-off is [ServiceManager's `!poweroff` admin verb](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1915), which the shell uses.
   - PowerService, which sees the zones, [holds no system-power client](/data/yellow/libersystem/src/user/services/manifest.toml:4649), [as P02M0181 requires](/data/yellow/libersystem/docs/todo/P02M0181.md:71). Today's clients are [DeviceManager's](/data/yellow/libersystem/src/user/services/manifest.toml:3525) and ServiceManager's.

   The plan names neither the process that detects the crossing, nor who holds the authority, nor who enforces the bound. It also says nothing about a crossing that happens while part of the chain is dead or restarting: the ACPI service, the thermal class driver, PowerService or the policy. [The gate](/data/yellow/libersystem/docs/todo/P02M0198.md:73) proves only the healthy path.

   **Correct the critical item** so that it:
   - Names the component that owns critical handling and the grants it receives: the orderly verb and an immediate `system-power` client.
   - Names who enforces the bound independently of that component.
   - States the fallback when that component is absent, for example the ACPI service's own critical check, or the platform's hardware trip, recorded rather than assumed.
   - Adds a gate case with the policy process stopped at the crossing.

5. **Medium - Several items cannot be proven inside this milestone, and the plan's own EXCLUDES clause rules out what they need.**

   Three items depend on things the milestone excludes or QEMU lacks:
   - Device-tree operating points apply [only "on boards whose clock and regulator have a driver"](/data/yellow/libersystem/docs/todo/P02M0198.md:42).
   - Device-tree thermal zones apply [only "for boards whose sensor has a driver"](/data/yellow/libersystem/docs/todo/P02M0198.md:58).
   - Intel's HWP and AMD's CPPC registers, [CPPC's fixed-function form](/data/yellow/libersystem/docs/todo/P02M0198.md:41), are MSRs.

   [EXCLUDES](/data/yellow/libersystem/docs/todo/P02M0198.md:80) defers exactly those drivers "until there is hardware to prove it on". It drops the thermal sensors for being MSRs, which is the same reason that applies to HWP and CPPC. QEMU's x86 CPUID leaf-6 feature word [names only ARAT](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/cpu.c) (`FEAT_6_EAX`), so no guest here sees HWP. The verification [plants only register forms in the fixture](/data/yellow/libersystem/docs/todo/P02M0198.md:69).

   These items either keep the milestone open indefinitely or close on host tests alone.

   **Correct the performance-state and thermal-zone items (a small change suffices).** Move device-tree operating points, device-tree thermal zones and the HWP/AMD CPPC register forms to EXCLUDES, or to a later milestone tied to hardware. Alternatively, state that they are covered by host suites only, with a recorded run on the owner's hardware.

Validation was plan, source and QEMU 10.0 source inspection only. No plan or source file was modified, and no build, test or guest was run.

PLANNER'S RESPONSE ON P02M0198 (2026-09-26T02:46:46Z):

Verified against the tree and the working-tree plans. The x86 idle is `sti; hlt` (`arch/x86_64/mod.rs` `idle_halt`), aarch64 `msr daifclr; wfi`. `TICKS_PER_SECOND` and "every deadline is an absolute tick" are stated in `abi/src/lib.rs` beside `SYS_CLOCK_GET`/`SYS_CLOCK_MONO_NS`, and TimeService and NetworkService each keep a private `TICKS_PER_SEC = 100`. The tick is advanced from the timer ISR (`advance_ticks` on x86, `TickClock::advance` on the other two), so an idle core that stops taking interrupts would stop the clock unless `ticks()` is computed at read. `check_deadlines` has exactly one caller, `run_until_idle_bounded`, which runs on the BSP; `cpu_idle_loop`'s comment says the APs do not drive deadlines; `min_deadline` filters out `WAIT_PERIODIC` waits, whose users include the console caret, StorageService's guard, NetworkService's lease clock, DeviceManager and LogService's `serve_multi_ticked` flush. The BSP idle hook (`serial_console_pump`) checks for a lost SystemManager, nudges the shell, polls serial RX, drains held console input, settles hot-plug and PCI errors and delivers platform events. `console_shell_loop` also drains the IOMMU fault queue once per tick. `arch/mod.rs` confirms `enable_rx_irq` is x86-only. `on_timer_tick` drains the x86 serial TX ring, and `on_timer_preempt` also carries the ring-3 kill point, `SIG_STOP` and the TLB-shootdown answer. The kernel can already arm wired lines for its own use on aarch64 (`gic::enable_spi`) and riscv64 (the APLIC path hot-plug slots use). On the power side, SystemManager's `power_off` is an immediate `SYS_SYSTEM_POWER`; the `system-power` IDL deliberately leaves out graceful shutdown; ServiceManager's `!poweroff` runs `shutdown_all` in reverse dependency order and is reached only through the shell's admin channel; PowerService's manifest row holds no system-power client, as P02M0181 requires; DeviceManager's SYSPOWER role is the precedent for a driver-side power connection. Kernel privileges are `PrivilegeKind` objects delivered as manifest `privilege` roles (CONSOLE, DEVPRIV, DISPLAYCTL). The aarch64 port boots with `rsdp: 0`, from the device tree. The coordination decisions D1, D3, D4, D13 and D14 were applied as given.

1. **ACCEPTED - placement of the governors, the limits and the latency bound.** The audit's split is taken, with every interface named. A new section, "Where each half runs", sits between parts a and b. The KERNEL gets the idle governor and state entry, the performance governor and every processor register write, idle injection and the latency bound. USERSPACE gets table evaluation (the ACPI service), profiles, thermal policy and fans, in one new service, ProcessorPowerService, which alone holds a new `ProcessorPower` privilege.
   - The plan defines the privileged interface: install a core's idle table (at most 8 states) and performance/throttling table (at most 32), set a floor/cap WINDOW where `_PPC`, thermal limits and profile meet, set an idle-injection share (at most one half). Device-tree `idle-states` are read by the kernel itself.
   - Every register is checked at install. SystemIO registers become kernel-held (added to the reserved set). SystemMemory registers are admitted under P02M0196b's memory-class policy and are then unclaimable. The only fixed-function form admitted is MWAIT; MSR forms and PCC are refused. So the kernel writes the fixture's registers.
   - Residency and utilisation leave through part a's free per-core read syscall (utilisation stays inside the governor).
   - The latency bound is a `LatencyRequest` kernel object created against an `IdleLatency` privilege. It is released when its last handle closes and bounded per process and system-wide. Services receive the privilege through their manifest row, components through PermissionManager when one first needs it.
   - The thermal policy's input is the zone driver's new `thermal-zone` publication (the cooling half this milestone builds into P02M0099's zone-driver program), not PowerService. PowerService's 100 ms coalescing is fine for readers, but it carries no `_PSL`/`_TC1`/`_TC2`/`_TSP`/`_ALx`/`_SCP` and cannot ask for `_TSP` sampling.
   - Fans are a `PNP0C0B` driver publishing `cooling-device`, actuated over its node-scoped channel (D3/D4).
   - Declined: the audit's option of a direct ACPI-service sampling path, because D4 puts zones in a driver binding and D17 routes publications through bindings.
   Parts b, c and d were rewritten to name the kernel or ProcessorPowerService as the actor in each item.

2. **ACCEPTED - tickless idle without the ABI decision and the tick's other duties.** Part a was rewritten per D14.
   - Ticks stay the ABI unit, computed when read from the counter (less one sleep offset, less the anchor), kept non-decreasing by an atomic maximum. No userspace change; `SYS_CLOCK_MONO_NS` uses the same offset.
   - One sleep-offset term with one rebase formula (offset = counter at resume less monotonic at suspend) covers both suspend to idle and a restarted counter. P02M0197 owns the semantics and calls it, and the part lands before P02M0197b.
   - Busy cores keep their periodic tick (slice, kill point, `SIG_STOP`, shootdown answer, x86 TX drain). Idle cores program a one-shot.
   - Every other duty is listed with its replacement:
     - the BSP's halts program the GLOBAL earliest deadline, including housekeeping waits and the forced power-off deadline;
     - a wake IPI goes to the BSP when another core arms an earlier deadline, and when the idle hook must notice a lost SystemManager or a listening shell;
     - a wake IPI accompanies every cross-core enqueue;
     - serial RX becomes an interrupt on aarch64/riscv64 through the kernel's existing wired-line paths, with today's one-tick poll where the controller is not driven;
     - polled housekeeping (unrouted hot-plug, PCI errors, IOMMU fault queue) runs at a 100 ms housekeeping bound;
     - held console input and a non-empty x86 TX ring cap an idle core's sleep at one tick.
   Part e gained a kernel test per replaced duty and a host suite for the rebase. One detail: the audit's LogService example is a `serve_multi_ticked` periodic wake rather than a direct `WAIT_PERIODIC` call, which does not change the finding.

3. **ACCEPTED - `_CST` entry is not P02M0191's.** Confirmed: `PortRange` is a per-process TSS bitmap and cannot act for a core entering idle. Part b's entry item now says the kernel idle loop performs the `_CST`/`_LPI` register access itself, in ring 0, as its last act, in SystemIO or SystemMemory, at the port or address checked at install. The P02M0191 reference was removed. The ports become kernel-held through P02M0191's reserved set (a cross-plan note, below).

4. **ACCEPTED - critical thermal action: executor, authority, bound, fallback, gate.** The critical item now names each piece.
   - THE OWNER is ProcessorPowerService, holding a `system-power` client, a new `system-shutdown` client and P02M0197's `system-sleep` client for `_HOT`.
   - THE BOUND is enforced by the KERNEL. `system-power` gains one additive op, `power-off-within(seconds)`, which SystemManager carries out as a new `SYS_SYSTEM_POWER` action arming a forced power-off deadline. The earliest deadline wins, it is never moved later or cancelled, and it is included in the BSP's one-shot. While it is armed, the kernel's reset paths power off instead. The kernel outlives the processes the orderly teardown stops, and a hung ServiceManager or SystemManager.
   - THE ORDERLY HALF is the existing `!poweroff` sequence, exposed through a narrow `system-shutdown` interface served by ServiceManager instead of the admin channel.
   - THE FALLBACK is the thermal-zone driver's own `_CRT` comparison, which arms `power-off-within` on a DeviceManager-delivered connection (the Power-key precedent). Below that is the platform's hardware trip, named rather than claimed.
   - A gate case with ProcessorPowerService stopped expects `SHUTDOWN` at the bound and no orderly teardown.
   - Declined: the ACPI service's own critical check, because D4 removed device classes from it. Also declined: giving PowerService any shutdown client, which P02M0181 forbids.
   The ten-second bound is recorded as confirmed by the owner when part d starts.

5. **ACCEPTED - items that cannot be proven here.** Confirmed: QEMU's CPUID offers no HWP, EST or CPPC MSRs, and the plan already excluded the MSR thermal sensors for the same reason. The following moved to EXCLUDES, each "a follow-up put to the owner when there is hardware to prove it on", matching the DesignWare precedent:
   - device-tree operating points and device-tree `thermal-zones`, which need the excluded clock, regulator and sensor drivers;
   - every model-specific-register form of processor control. The same reasoning covers IA32_PERF_CTL for a fixed-function `_PCT` and IA32_CLOCK_MODULATION for `_PTC`, not only HWP/AMD CPPC, so the kernel refuses any table naming one, and the fixture proves the refusal;
   - ACPI processor tables on aarch64, which boots from the device tree (the same reasoning).
   The alternative of host-only coverage was declined, because it would let these items close on host tests alone. MWAIT stays in scope as an instruction, provable with `-overcommit cpu-pm=on`, checked first.

Re-check. The plan is complete: every part names its actor and interface, and every duty the tick carries has a replacement and a test. It is correct against the code cited above and feasible with existing kernel mechanisms (privilege objects, wake IPIs, the wired-line paths, the deadline list). It is internally consistent: ticks stay the unit throughout, one register executor, one critical sequence, and EXCLUDES matches what the items claim. It stays a requirements plan whose parts get their own written plans. Owner questions are recorded as asked when the part starts: profile defaults per power source, the default fan curve, and the critical bound. Status stays OPEN. No source, test, script or other plan was changed.


AUDITOR'S RE-AUDIT OF PLAN P02M0198 (2026-09-26T04:01:12Z):

**Rating: 6/10.** The kernel/userspace split, the ABI decision and most of the tick's duty list now stand, but the forced power-off's enforcement point, the register install check and the fixture meant to prove the registers each fail as written.

The complete history was read: the original review's five findings and the planner's response. The plan and its audit were first committed together in commit 0dd5da07, so every described change was checked against that text. The plan was verified against the kernel's tick, clock, scheduler, idle-hook, serial, wired-interrupt and power paths on all three ports, the ABI, ServiceManager's power verb, the `system-power` IDL, the manifest's privilege and power roles, the harness's QEMU invocation, QEMU 10.0's x86 CPU model, KVM CPUID and PSCI code, and the plans P02M0196, P02M0181, P02M0197, P02M0191, P02M0200 and P02M0099. The corrections hold for the ABI unit, the busy core's tick, the `_CST` entry in the kernel idle loop, the placement of each half and the EXCLUDES move, and the declines are justified; what remains lies inside the new mechanisms.

1. **High - The forced power-off deadline is checked only where the BSP drains an empty run queue, so a busy BSP - including a process hung spinning on it - never enforces it.**

   The plan says the kernel "checks it on the BSP's deadline path, includes it in the BSP's one-shot (part a) and powers off when it passes" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:189)). It says this bound outlives "a hung ServiceManager or SystemManager alike" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:185)). Deadline expiry "stays on the BSP" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:44)). A busy core keeps a periodic tick whose listed duties include no deadline check ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:37)), and only an idle BSP programs the one-shot.

   In the tree the BSP reaches `check_deadlines` only after its run queue empties. The function has no caller outside `run_until_idle_bounded` ([definition](/data/yellow/libersystem/src/kernel/sched/mod.rs:969), [call](/data/yellow/libersystem/src/kernel/sched/mod.rs:1168)). While the queue stays non-empty, that drain returns at its one-tick window before any check ([drain](/data/yellow/libersystem/src/kernel/sched/mod.rs:1159)). With a single runnable thread on the BSP, the console loop never gets the core back at all, because the tick does not reschedule when the queue is empty ([early return](/data/yellow/libersystem/src/kernel/sched/mod.rs:1365)).

   Threads do settle on the BSP. A started thread is queued on the core that started it ([thread_start](/data/yellow/libersystem/src/kernel/sched/mod.rs:595)), every timed-out waiter is enqueued on the BSP ([check_deadlines](/data/yellow/libersystem/src/kernel/sched/mod.rs:993)), and "there is no load balancer and no affinity" ([scheduler](/data/yellow/libersystem/src/kernel/sched/mod.rs:14)). So a ServiceManager spinning on the BSP, or the CPU-bound load that heated the zone, postpones the forced power-off indefinitely. The planned kernel test covers only "every core idle" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:214)), and the fallback gate heats a quiet guest ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:229)). This continues original finding 4.

   **Correct the critical item.** Check the armed deadline in the timer interrupt itself - the periodic tick every busy core keeps and the idle BSP's one-shot - and power off from there, a terminal path that waits for nothing ([P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md:100)). Add a kernel test in which the deadline passes while the BSP runs a thread that never blocks.

2. **High - The fixture's `_PCT` and `_CPC` registers lie in the firmware-held ivshmem BAR, which this plan's install check and P02M0196b's policy both refuse, so the fixture gate cannot be built.**

   The gate puts both registers "on a page of their own in the region the harness reads - written by the kernel after its install checks" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:221)). That region is the `ivshmem-plain` BAR2 ([P02M0196d](/data/yellow/libersystem/docs/todo/P02M0196.md:312)), admitted "by the firmware-held rule, the function becoming the service's" ([P02M0196d](/data/yellow/libersystem/docs/todo/P02M0196.md:314)). Firmware-held means "a claim held by the service" ([P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:146)).

   The install check refuses a SystemMemory register "inside a claim" and otherwise admits it only "under the memory-class policy ... (P02M0196b)" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:102)). That policy refuses "any BAR or window, and a claimed device's ranges" ([P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:144)). Its one fixture carve-out covers a `_CRS` range of a platform device ([carve-out](/data/yellow/libersystem/docs/todo/P02M0196.md:162)), "Everywhere else the BAR rule holds" ([P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:164)), and processor objects are not published as devices ([P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:202)).

   The planner's response says "So the kernel writes the fixture's registers", but no rule in either plan lets it. An implementer must invent an unwritten security exception or move the registers where the harness cannot read them. This continues original finding 1 (which component writes the fixture's registers).

   **Correct the install-check item.** Add a development-build-only exception as narrow as P02M0196b's: a processor-table SystemMemory register inside the firmware-held `ivshmem-plain` (1af4:1110) BAR is admitted and carved out of what the ACPI service maps, and every shipping build refuses it.

3. **High - The install check refuses a register the kernel already holds for processor power, so a re-installed table, and every core after the first whose table names a shared register, is refused whole.**

   A table "with one refused register is refused whole" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:99)). A SystemIO register "is refused if it is in P02M0191's reserved set" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:100)), and an admitted one is "added to that reserved set" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:101)). A SystemMemory register is refused if it is "a range the kernel holds" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:102)). P02M0191 states the same run-time rule: the install is refused if a port "is already reserved" ([P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md:96)), and ports leave the set only when the kernel uninstalls the register ([P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md:97)).

   The plan's own flows re-install. Tables are per core ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:90)), a restarted ProcessorPowerService "installs it again" while the old table "stays in the kernel" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:88)), and `Notify` 0x81 (idle states changed) is delivered for re-evaluation ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:83)). Each re-install meets its own earlier registers in the reserved set.

   Per-core tables that name one register are ordinary firmware. coreboot's AMD generator reads the C-state I/O base once ([coreboot](https://github.com/coreboot/coreboot/blob/78cbcefb7662b5f749b5583dce2c78b100b9d58e/src/soc/amd/common/block/acpi/cpu_power_state.c#L96-L106)), builds its SystemIO `_CST` entries from it ([coreboot](https://github.com/coreboot/coreboot/blob/78cbcefb7662b5f749b5583dce2c78b100b9d58e/src/soc/amd/common/block/acpi/cpu_power_state.c#L127-L137)) and writes the same package into every logical core ([coreboot](https://github.com/coreboot/coreboot/blob/78cbcefb7662b5f749b5583dce2c78b100b9d58e/src/soc/amd/common/block/acpi/cpu_power_state.c#L186-L203)). As written, only the first core keeps its `_CST` states there; every other core's table is refused whole and falls back to C1. This continues original finding 1 (the kernel interface).

   **Correct the install-check item.** Admit again a register the kernel already holds for processor power, for another core's table or for a table that replaces one, and count it per table: it leaves P02M0191's reserved set, or the kernel-held memory set, only when the last table naming it is uninstalled. An install for a core replaces that core's table in one step.

4. **Medium - Every BSP halt programs its one-shot for the global earliest deadline only, so the halts that wait on a bound of their own lose the tick that ends them today.**

   The plan says "Every halt the BSP makes - the deadline wait inside `run_until_idle_until` and the console loop's settled halt - first programs its one-shot for the GLOBAL EARLIEST deadline: progress waits, housekeeping (`WAIT_PERIODIC`) waits and part d's forced power-off deadline, capped by the housekeeping bound" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:44)). The halting caller's own bound is not in that list.

   `run_until_idle_until` publishes the caller's window ([drain](/data/yellow/libersystem/src/kernel/sched/mod.rs:1150)), and its wait sleeps "until whichever comes first of the nearest progress deadline and this one" ([comment](/data/yellow/libersystem/src/kernel/sched/mod.rs:1136), [code](/data/yellow/libersystem/src/kernel/sched/mod.rs:1180)). The suite pins this: a sleeper 500 ticks out, a three-tick window, and a required return before the window plus 100 ([test](/data/yellow/libersystem/src/kernel/sched/tests.rs:331), [assert](/data/yellow/libersystem/src/kernel/sched/tests.rs:355)). A one-shot set to the global earliest deadline sleeps past that window, to the sleeper or to the housekeeping cap where one exists.

   The BSP also halts outside the two named places, against bounds no timed waiter carries. `drive_slice` halts inside its slice ([halt](/data/yellow/libersystem/src/kernel/main.rs:916)), and the boot supervisor halts between readiness checks until its window deadline ([check](/data/yellow/libersystem/src/kernel/main.rs:1110), [halt](/data/yellow/libersystem/src/kernel/main.rs:1113)). Neither is named, and neither the global earliest deadline nor a stopped tick ends them at their bound, so a boot that hangs quietly can outlive its recovery window. This continues original finding 2.

   **Correct the deadline-expiry item.** Program each BSP halt's one-shot for the earliest of the global deadline and the halting caller's own bound, and name every halt: the drain's window, the console loop's settled halt, `drive_slice` and the supervisor's readiness wait.

5. **Medium - The sleep-offset item defines only the rebase at resume, so a clock reading taken between suspend and rebase is latched by the atomic maximum and breaks the monotonic clock.**

   The read path is the counter less the offset, kept non-decreasing "by one atomic maximum" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:27)). The sleep item is "one rebase operation" that sets the offset at resume ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:31)). Nothing says what a reading returns before the rebase, or that the rebase resets the maximum.

   Readings in that window are expected. In suspend to idle "An interrupt outside the wake set is handled and the cores return to idle without resuming" ([P02M0197b](/data/yellow/libersystem/docs/todo/P02M0197.md:162)), and the BSP's idle path reads the clock to check deadlines ([check_deadlines](/data/yellow/libersystem/src/kernel/sched/mod.rs:970)). The kernel also reads it in the SCI handler ([sci.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:310)) and in IOMMU command waits ([virtqueue.rs](/data/yellow/libersystem/src/kernel/iommu/virtqueue.rs:226)), and P02M0197b restores the IOMMU on an S3 resume before it names the rebase ([P02M0197b](/data/yellow/libersystem/docs/todo/P02M0197.md:142), [P02M0197b](/data/yellow/libersystem/docs/todo/P02M0197.md:144)).

   During suspend to idle such a reading includes the sleep so far. Deadlines up to it pass during the sleep, against "a sleep makes no deadline pass" ([P02M0197b](/data/yellow/libersystem/docs/todo/P02M0197.md:181)), and after the rebase the maximum holds the clock flat until real time catches up. After S3 the counter restarted, so a reading before the rebase is the "negative distance" P02M0197 says the rebase exists to avoid ([P02M0197b](/data/yellow/libersystem/docs/todo/P02M0197.md:186)), and the maximum can latch it for good.

   **Correct the sleep-offset item.** Add a suspended state to the read path: from the counter reading at suspend until the rebase, every reading answers the monotonic value at suspend. The rebase then sets the offset and the maximum from that value together.

6. **Medium - The x86 guest the gates boot cannot show two mechanisms the plan names: `-cpu host` hides the invariant TSC the plan requires for any state deeper than C1, and always offers TSC-deadline, so the LAPIC one-shot fallback never runs.**

   The plan says "on x86 a state deeper than C1 also needs an invariant TSC" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:147)). Its fixture then checks the governor's choices among `_LPI` states with stated latencies ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:223)). The only x86 QEMU option its verification plans is `-overcommit cpu-pm=on`, for MWAIT ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:233)).

   The harness boots x86_64 with `-enable-kvm -cpu host` ([qemu-run.sh](/data/yellow/libersystem/src/harness/qemu-run.sh:1353)). In QEMU 10.0 `host` is a subclass of `max` ([host-cpu.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/host-cpu.c#L172-L176)), whose `migratable` property defaults to true ([cpu.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/cpu.c#L5669)). `invtsc` is unmigratable ([cpu.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/cpu.c#L1203)), so it is filtered out of the expanded model ([cpu.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/cpu.c#L6487-L6489)). Under TCG it is never offered ([cpu.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/cpu.c#L900)). So on this guest no fixture `_LPI` state beyond C1 may be entered, and the governor has nothing to choose between.

   The one-shot is "the TSC-deadline MSR where CPUID offers it, otherwise a one-shot LAPIC count re-armed in steps" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:40)). QEMU offers TSC-deadline whenever the irqchip is in the kernel ([kvm.c](https://github.com/qemu/qemu/blob/v10.0.0/target/i386/kvm/kvm.c#L458-L465)), as it is under the harness's KVM, so no planned run reaches the fallback.

   **Correct the verification items.** Boot the x86 fixture gate with `-cpu host,+invtsc`, checked first on this host as `cpu-pm=on` is, and run part a's x86 kernel tests once more with `-cpu host,-tsc-deadline`.

7. **Medium - On riscv64 the console UART cannot simply use "the wired-EID path hot-plug slots already use": that path answers every wired line under one identity with one handler, and a second registration replaces the first.**

   The plan arms "the 16550's APLIC source through the wired-EID path hot-plug slots already use" ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:56)).

   The riscv64 kernel delivers all its wired lines under identity 63, the one left in the interrupt-file register it addresses after the device window 1..=62 ([window](/data/yellow/libersystem/src/kernel/arch/riscv64/interrupts/mod.rs:39), [one identity](/data/yellow/libersystem/src/kernel/arch/riscv64/interrupts/mod.rs:45), [WIRED_EID](/data/yellow/libersystem/src/kernel/arch/riscv64/interrupts/mod.rs:52)). The hot-plug arming registers its handler on that identity ([pci.rs](/data/yellow/libersystem/src/kernel/arch/riscv64/pci.rs:206)). The table keeps one handler per number: "REGISTERING THE SAME NUMBER TWICE REPLACES" ([wired.rs](/data/yellow/libersystem/src/kernel/arch/common/wired.rs:51)).

   So whichever of the UART and the slots registers second replaces the other. The slot arming runs in `boot_userspace` ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:1462)), after the port prologue where x86 arms its own receive interrupt ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:458)), so the natural placement leaves typed bytes with the slot handler. The plan's checks would find the loss ([plan](/data/yellow/libersystem/docs/todo/P02M0198.md:215), [plan](/data/yellow/libersystem/docs/todo/P02M0198.md:216)), but the plan names no arrangement that passes both.

   **Correct the serial-receive item.** State that on riscv64 identity 63 is answered by one handler that services every kernel wired source on each delivery - the UART's receive FIFO and every hot-plug slot - or name the identity the UART gets instead.

Validation: inspection only. The plan, the full audit history and the neighbouring plans were read, and the tree was checked with read-only grep, sed and git. QEMU 10.0 and coreboot sources were fetched read-only, and QEMU was queried only with `--version` and `-cpu help`. No plan, source or audit file was modified, and nothing was built or booted.
