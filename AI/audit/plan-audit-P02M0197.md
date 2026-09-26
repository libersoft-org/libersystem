AUDITOR'S REVIEW OF PLAN P02M0197 (2026-09-25T22:53:15Z):

**Rating: 3/10.** The split into transaction, sleep states, wake policy and verification is sound. But the transaction's order, its clock rules and its kernel entry contradict how this system is built, and two of its proofs ask QEMU for behaviour it does not have.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0197.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a`, together with:

- the plans it leans on: [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:59), [P02M0181](/data/yellow/libersystem/docs/todo/P02M0181.md:6), [P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:26), [P02M0190](/data/yellow/libersystem/docs/todo/P02M0190.md:29) and [P02M0198](/data/yellow/libersystem/docs/todo/P02M0198.md:18);
- the kernel's clock, timer, scheduler, signal, SCI and power paths;
- the driver bring-up wire, DeviceManager, ServiceManager, SystemManager, ProcessService, TimeService and StorageService;
- the x86_64 harness.

Plans P02M0197 to P02M0202 exist only in the working tree (they are not yet committed), and P02M0190, P02M0196 and P02M0099 carry uncommitted edits; all were read as they stand there. The plan is a requirements list whose parts each get their own written plan before they start. The findings concern decisions this text gets wrong, leaves contradictory, or leaves to two plans that each assume the other owns them. They do not concern the expected absence of implementation.

1. **High - The suspend order freezes the processes that must flush the filesystems and quiesce the drivers, and the plan names neither the frozen set nor the orchestrator.**

   The [order](/data/yellow/libersystem/docs/todo/P02M0197.md:31) freezes "user processes", then flushes the filesystems, then quiesces the drivers. In this system all three are user processes:
   - The filesystem is [StorageService](/data/yellow/libersystem/src/user/services/storage/src/service.rs:1), a ring-3 service that reaches the disk over the virtio-blk driver's channel.
   - Every driver is an ordinary process that DeviceManager launches and speaks to over [the bring-up wire](/data/yellow/libersystem/src/user/libs/driver/protocol/src/lib.rs:1).
   - DeviceManager is [the only process that holds the bindings](/data/yellow/libersystem/src/idl/device.lsidl:144).

   There is no boundary to freeze by, either. An ordinary launch from ProcessService [passes Domain 0](/data/yellow/libersystem/src/user/services/core/src/process_service.rs:1170), which puts the program in [the caller's Domain](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:140). That is the same control-plane Domain every service runs in.

   The kernel's only existing pause is job control. `SIG_STOP` sets the stopped flag and [parks each thread at its next kernel exit](/data/yellow/libersystem/src/kernel/sched/mod.rs:1339), and [`fg` resumes a stopped job with `SIG_CONT`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:2334). "Without signalling them" implies a separate state, but the plan never says so.

   Two more points do not fit this system:
   - "A USB device before its controller" is not an order between processes here: USB class devices are [bound inside the xHCI driver's own process](/data/yellow/libersystem/src/user/drivers/core/src/classes.rs:3).
   - "SystemManager stays the one authority" works if SystemManager only holds the kernel capability. But [P02M0141 keeps driver and application policy out of SystemManager](/data/yellow/libersystem/docs/todo/P02M0141.md:311), and the service dependency order already lives in [ServiceManager](/data/yellow/libersystem/src/user/services/core/src/service_manager/lifecycle.rs:24).

   Taken literally, the freeze stops StorageService before the flush and every driver before it can be asked to suspend, so no suspend can complete. Built on `SIG_STOP`, the thaw would continue a job a person had stopped. With no orchestrator named, the part plan must decide on its own who talks to ServiceManager, DeviceManager, StorageService and the ACPI service, and in what order.

   **Correct the transaction's order item** so that it:
   - Separates the FROZEN set from the PARTICIPANTS. The frozen set is applications and sessions, found by a mechanism the plan names, since no Domain separates them today. The participants are the control-plane services, StorageService, DeviceManager with its drivers, and the ACPI service. Participants are asked to quiesce and are never frozen.
   - Names the orchestrator and each participant's role: ServiceManager announces to and orders the services, DeviceManager suspends drivers in its bind order, and the xHCI driver orders its own USB devices internally.
   - Defines the freeze as a kernel state separate from job-control stop, preserved across the thaw, and gated by a named privilege.

2. **High - The clock rules contradict the clocks this kernel has and each other, and are not ordered with P02M0198's change of clock source.**

   The [clock item](/data/yellow/libersystem/docs/todo/P02M0197.md:44) says two things:
   - The monotonic clock does not count the sleep.
   - A timer whose deadline passed during the sleep fires once on resume.

   Every Timer and every wait deadline is [an absolute value on the monotonic tick counter](/data/yellow/libersystem/src/abi/src/lib.rs:223) ([the Timer's check](/data/yellow/libersystem/src/kernel/object/timer/mod.rs:51)). If that clock excludes the sleep, no such deadline can pass during it. Only a timer on the second clock could, and the plan does not let timers use the second clock.

   The existing mechanisms also do the opposite of "does not count the sleep". The tick counter is gated on the cycle counter and [claims the whole backlog in one step](/data/yellow/libersystem/src/kernel/arch/x86_64/apic/mod.rs:244). The portable [TickClock](/data/yellow/libersystem/src/kernel/arch/common/time.rs:32) does the same on the other two ports.
   - **Suspend to idle:** the counter keeps running, so on resume the clock jumps by the whole sleep. That is the "fires once" behaviour, not the exclusion.
   - **S3:** the boot core returns [through real mode](/data/yellow/libersystem/docs/todo/P02M0197.md:57), that is through a reset, and a reset [zeroes the TSC](https://www.felixcloutier.com/x86/rdtsc). QEMU resumes an x86 guest by resetting the machine ([`pc_machine_wakeup`](https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/pc.c)). The tick gate then measures [a negative distance from the old anchor](/data/yellow/libersystem/src/kernel/arch/x86_64/apic/mod.rs:240) and stops advancing until the TSC passes its pre-sleep value. Meanwhile [`SYS_CLOCK_MONO_NS` reads the raw TSC](/data/yellow/libersystem/src/kernel/syscall/mod.rs:516) and runs backwards.

   The rest of the item is also unsettled:
   - The "second clock" needs a new syscall, and the plan gives no unit for it ([`clock()` counts ticks, `clock_ns()` nanoseconds](/data/yellow/libersystem/src/user/runtime/rt/src/lib.rs:2349)).
   - P02M0198 [moves the monotonic clock onto the free-running counter](/data/yellow/libersystem/docs/todo/P02M0198.md:18). Neither plan mentions the other's clock work.
   - Every core runs a [periodic LAPIC timer](/data/yellow/libersystem/src/kernel/arch/x86_64/apic/mod.rs:278), so [suspend to idle](/data/yellow/libersystem/docs/todo/P02M0197.md:53) still wakes each core a hundred times a second. Its power benefit therefore depends on P02M0198's tickless idle, and the plan does not say so.

   After the first S3 resume, every tick-based wait in the system would stall for as long as the machine had been up. Two part plans reading this item can pick incompatible semantics.

   **Correct the clock item** so that it:
   - Chooses the semantics for each clock. For example, the monotonic clock excludes the sleep through a kernel sleep offset applied in the read path and in the tick gate, rebased after a counter reset, while a boot-time clock includes the sleep.
   - States the second clock's unit and syscall.
   - Says which clock Timer and wait deadlines use, and either offers timers on the boot-time clock or restricts "fires once on resume" to wall-clock alarms.
   - Designs the read path once, together with P02M0198a, and states the order of the two milestones.
   - Says whether suspend to idle waits for tickless idle or stops the tick itself.

3. **High - Nothing defines the kernel's entry into a sleep state, how wake events are armed, or how `\_S5` is delivered; P02M0196c and this plan each hand these mechanics to the other.**

   This plan does S3 ["through P02M0196c's platform mechanics"](/data/yellow/libersystem/docs/todo/P02M0197.md:56). P02M0196c [hands "the wake vectors and the PM1 registers written, and the wake devices' `_PRW` and GPEs armed" to P02M0197](/data/yellow/libersystem/docs/todo/P02M0196.md:59). Neither says which side of the kernel boundary does this work.

   The facts that decide it:
   - AML runs by default in [a userspace ACPI service](/data/yellow/libersystem/docs/todo/P02M0196.md:46), so `\_S3`, `\_S5`, `_PTS`, `_WAK` and `_PRW` are evaluated only there.
   - The kernel holds PM1 and the SCI, and ["arms no general-purpose event, enters no sleep state"](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:25). P02M0200 [records the kernel keeping PM1 and GPE](/data/yellow/libersystem/docs/todo/P02M0200.md:39).
   - The only power entry is [`sys_system_power`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1915): reboot or off, on the root Domain with MANAGE.
   - SLP_EN must be the kernel's last act, after it has saved its state and written its own resume trampoline into the FACS.

   The wake sources meet the same boundary. The CMOS alarm needs RTC_EN in the PM1 enable register, which the kernel [programs for the two buttons only](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:165). A `_PRW` GPE's enable bit lives in the GPE block.

   [Soft-off from `\_S5`](/data/yellow/libersystem/docs/todo/P02M0197.md:63) needs a value only AML can produce, inside a power-off path that today [writes fixed ports](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:175). That path must keep working when the ACPI service is absent or dead.

   S3, soft-off and every ACPI wake source depend on this interface. With the two plans pointing at each other, the kernel ABI would be invented during implementation.

   **Correct the S3, soft-off and wake-source items, and the matching P02M0196c text:**
   - Define the privileged kernel operation that enters a sleep state: the sleep-type values and wake set it takes, the FACS vector, the cache flush, the SLP_TYP|SLP_EN write, and its return on resume.
   - Place the ACPI service's `_PTS` and `_SST` before that operation and `_WAK` after it.
   - Give GPE and RTC_EN enabling to one owner, with an interface.
   - Say how `\_S5` reaches the kernel when the ACPI service starts, and keep today's fixed ports, PSCI and SBI as the fallback until it has.

4. **Medium - The driver contract is placed in the wrong interface and does not fit the binding wire's semantics or its measured deadlines.**

   The [contract item](/data/yellow/libersystem/docs/todo/P02M0197.md:37) puts suspend and resume "in `liber:device@1`". That package is [device enumeration, the binding list and the provider catalogue](/data/yellow/libersystem/src/idl/device.lsidl:141). DeviceManager and a driver talk over a separate binary wire in `driver-protocol`, whose [revision every driver declares in an ELF note, checked before the claim](/data/yellow/libersystem/src/user/libs/driver/protocol/src/lib.rs:21).

   That wire's only quiesce verb is [`Stop`](/data/yellow/libersystem/src/user/libs/driver/protocol/src/lib.rs:219), answered by a `Stopped` that [ends the binding](/data/yellow/libersystem/src/user/libs/driver/protocol/src/lib.rs:225). A suspend needs the opposite: the binding and its generation survive. The supervisor also [pings every bound driver on every pass](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:3001), and the plan does not say whether a suspended driver keeps answering.

   "A driver that does not answer within its bound ABORTS" runs into a problem DeviceManager has already measured. Healthy drivers miss one-second deadlines on the emulated ports, so DeviceManager [scales its deadlines by the port's boot window](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:127).

   A contract written into the IDL package reaches no driver. Unscaled bounds would abort every suspend on aarch64 and riscv64 for nothing, and the gate [requires suspend to idle on all three](/data/yellow/libersystem/docs/todo/P02M0197.md:100).

   **Correct the contract item** so that it:
   - Specifies non-terminal SUSPEND and RESUME exchanges in the driver-protocol wire, sent only by DeviceManager.
   - Says either that a suspended driver keeps its control path live and answers `Ping`, or that DeviceManager pauses heartbeat supervision during the transaction.
   - Scales each per-driver bound the way `machine_scale` scales DeviceManager's deadlines.

5. **Medium - P02M0200's hardware watchdog would end a suspend to idle, and neither plan disarms it for a sleep.**

   P02M0200's service pets the device [only while the supervisor confirms that it and SystemManager are alive](/data/yellow/libersystem/docs/todo/P02M0200.md:26). Once armed, the watchdog [stays armed](/data/yellow/libersystem/docs/todo/P02M0200.md:30): only an orderly power-off or reboot disarms or extends it. This plan freezes processes and then [parks the cores in suspend to idle](/data/yellow/libersystem/docs/todo/P02M0197.md:53). The watchdog is a timer outside the processor, so it keeps counting throughout.

   The first suspend to idle longer than one watchdog timeout resets the machine, and the gate records a reset instead of a resume. Resume is also the one moment the watchdog should cover, so the plan must say when it is re-armed.

   **Correct the transaction's order item**, and ask P02M0200 for the matching operation and gate case:
   - Before the freeze, the watchdog service disarms the device. Where the device cannot be disarmed, it extends the timeout past the sleep's bound, as P02M0200 already does for shutdown.
   - On resume, the watchdog is re-armed first, with a timeout that also catches a resume that hangs.

6. **Medium - Two of the verification's wake triggers and oracles are not ones QEMU provides for the mechanisms the plan names.**

   The [S3 gate](/data/yellow/libersystem/docs/todo/P02M0197.md:97) wakes the guest with "a key sent with `input-send-event`". The x86_64 harness's keyboards are [`usb-kbd` behind `qemu-xhci`](/data/yellow/libersystem/src/harness/qemu-run.sh:904) and, in interactive runs, [`virtio-keyboard-pci`](/data/yellow/libersystem/src/harness/qemu-run.sh:2238). Those are the [only two keyboards this system drives](/data/yellow/libersystem/src/user/drivers/core/src/keys.rs:1).

   QEMU 10.0's source shows that neither can wake the guest:
   - The only keyboard model that requests a system wakeup is [the PS/2 one](https://github.com/qemu/qemu/blob/v10.0.0/hw/input/ps2.c) (`ps2_keyboard_event`), which this system does not drive.
   - A USB HID key reaches the xHCI only as [a port-status change](https://github.com/qemu/qemu/blob/v10.0.0/hw/usb/hcd-xhci.c) (`xhci_wakeup`).
   - [virtio-input has no wakeup call](https://github.com/qemu/qemu/blob/v10.0.0/hw/input/virtio-input-hid.c).
   - `usb-kbd` [activates its input handler at init](https://github.com/qemu/qemu/blob/v10.0.0/hw/input/hid.c), so an `input-send-event` that names no device goes to it.

   The gate therefore cannot exercise the plan's own mechanism, ["USB remote wakeup through the controller"](/data/yellow/libersystem/docs/todo/P02M0197.md:78).

   Suspend to idle ["with the same checks"](/data/yellow/libersystem/docs/todo/P02M0197.md:100) has a second gap. QEMU never leaves its running state, so there are no `SUSPEND` or `WAKEUP` events, and `system_wakeup` is [refused outside the suspended run state](https://github.com/qemu/qemu/blob/v10.0.0/system/runstate.c). The plan names neither the trigger nor the oracle for this state.

   **Correct the verification items:**
   - Mark key wake from S3 as checked first, with the outcome written down, or drop it for the harness and keep `system_wakeup` and the RTC alarm.
   - For suspend to idle, name a wake the guest arms itself, such as the RTC alarm's interrupt or a key or serial byte through the running drivers.
   - Also name a host-side oracle, for example a harness-timed interval during which the frozen program's counter stays silent on the serial log.

7. **Medium - Hibernation is carried as one checkbox, with a gate, a follow-on and policy dependants, although in this architecture it is a kernel, loader and storage design of its own.**

   The [item](/data/yellow/libersystem/docs/todo/P02M0197.md:66) asks for its own plan, yet this milestone still carries:
   - [its gate](/data/yellow/libersystem/docs/todo/P02M0197.md:102);
   - [hybrid sleep](/data/yellow/libersystem/docs/todo/P02M0197.md:72);
   - [the critical-battery policy](/data/yellow/libersystem/docs/todo/P02M0197.md:88);
   - [P02M0198's `_HOT` action](/data/yellow/libersystem/docs/todo/P02M0198.md:56).

   What hibernation would have to build on here:
   - **Storage.** The kernel has no storage path ([the bring-up block probe is gone](/data/yellow/libersystem/src/kernel/arch/aarch64/boot.rs:765)). The disk is served by a driver process and [StorageService](/data/yellow/libersystem/src/user/services/storage/src/service.rs:9), so the image must be written by processes the transaction otherwise stops. It must be restored either by the UEFI loader or by a fresh boot's userspace handing the kernel a whole-memory replacement.
   - **The key.** The TPM is reachable only through [a driver process](/data/yellow/libersystem/docs/todo/P02M0190.md:29), and P02M0190 [excludes measured boot of the loader and kernel](/data/yellow/libersystem/docs/todo/P02M0190.md:70). No PCR policy can therefore tie the key to the kernel being resumed. riscv64 [has no TPM model](/data/yellow/libersystem/docs/todo/P02M0190.md:64), and the plan gives no key source there.
   - **Space.** The volume format reserves no area: its container is [a LiberFS partition or the whole device](/data/yellow/libersystem/docs/LIBERFS.md:274).

   A design of that size keeps this milestone open after suspend to idle and S3 are done, and the constraints that decide it are recorded nowhere.

   **Correct the hibernation and hybrid-sleep items** by moving them to a milestone of their own that starts from these constraints. Keep the fallbacks here (a critical battery powers off, and so does `_HOT`), so this milestone can close on suspend to idle and S3.

8. **Low - The Time and Alarm Device's clock has no path to TimeService, and the other readers of the hardware clock bypass TimeService.**

   The [item](/data/yellow/libersystem/docs/todo/P02M0197.md:82) makes the TAD's clock "TimeService's RTC", but the TAD is evaluated in the ACPI service. TimeService takes its time only from the kernel's [`SYS_CLOCK_RTC`](/data/yellow/libersystem/src/user/services/core/src/time_service.rs:79) and [serves only its client channel](/data/yellow/libersystem/src/user/services/core/src/time_service.rs:92). So it also has no way to receive the resume event the clock item relies on.

   StorageService stamps the filesystem [from the same kernel call](/data/yellow/libersystem/src/user/services/storage/src/service.rs:1023). On aarch64 that call reads [a fixed QEMU address](/data/yellow/libersystem/src/kernel/arch/aarch64/mod.rs:309).

   **Correct the TAD item (a sentence suffices):**
   - Name the path from the TAD's owner to TimeService, and the event on which TimeService re-reads the clock.
   - Either register the TAD as the source behind the kernel's RTC read, or move the other RTC readers onto TimeService.

Validation was plan, source and harness inspection, plus QEMU 10.0 source inspection and one read-only `qemu-system-x86_64 -device ICH9-LPC,help` query. The installed descriptor for the harness's OVMF build advertises `acpi-s3`, so S3 under this firmware is not in question. No plan or source file was modified, and no build, test or guest was run.

PLANNER'S RESPONSE ON P02M0197 (2026-09-26T02:50:09Z):

Every finding was checked against the tree at the working-tree state, not taken on trust. Read: the driver wire (`src/user/libs/driver/protocol/src/lib.rs` - `STOP`/`STOPPED` ends the binding, the internal wire revision), DeviceManager (`machine_scale` at 139, `tick_heartbeats` gating the ping on `Online` at 6782, the heartbeat's mark-and-leave at 4418, one child Domain per binding at 4098), ServiceManager's `shutdown_order`/`shutdown_all` and its `SYSPOWER` role, SystemManager's control-plane Domain and SystemPower door, ProcessService's `launch` forms (Domain 0, `bounded_domain`, `limited_domain`), `sys_domain_create` (child of the caller's Domain only), `sys_process_create` (ungated within the caller's Domain), the kernel's `SIG_STOP` park in `sched/mod.rs:1339` and `sys_process_signal`, `SYS_CLOCK_GET`/`SYS_CLOCK_MONO_NS`/`SYS_CLOCK_RTC` in `syscall_dispatch`, `sys_system_power`, `advance_ticks` in `arch/x86_64/apic/mod.rs` and `TickClock` in `arch/common/time.rs`, `arch/x86_64/sci.rs` (PWRBTN|SLPBTN only), `arch/x86_64/mod.rs` `poweroff`/`reset`, the kernel `PrivilegeKind` set, `platform_event.rs` (delivered to DeviceManager), TimeService, every `clock_rtc()` caller (StorageService three times, the development agent, TimeService), the manifest entries for `time_service` (loaded from the volume) and `storage_service` (pinned), the aarch64 PL031 read, `drivers/core/src/classes.rs`, `keys.rs`, and `qemu-run.sh` at 904 and 2238; plans P02M0196, P02M0198, P02M0200, P02M0190, P02M0181 and P02M0141 around 311. External facts: `qemu-system-x86_64 -device ICH9-LPC,help` (disable_s3, disable_s4, s4_val present), `-device usb-kbd,help` and `-device qemu-xhci,help` (no wake property); the QEMU 10.0 source claims about PS/2 wakeup, xHCI port-change delivery, virtio-input and `system_wakeup` outside the suspended state are consistent with QEMU's wakeup-notifier design and were not decisive beyond "checked first", so they were not re-fetched. The coordination decisions D4, D12, D13, D14, D15 and D18 were applied as written.

1. **ACCEPTED - the freeze stops the participants; frozen set, orchestrator and freeze state named.** Confirmed: StorageService, every driver and DeviceManager are ring-3 processes; ProcessService's `launch` passes Domain 0 and so lands applications in the control-plane Domain; the only pause is `SIG_STOP`, cleared by `fg`'s `SIG_CONT`; USB class devices live inside the xHCI process; the service order lives in ServiceManager and P02M0141 keeps policy out of SystemManager. Plan changes (part a): a new item separating PARTICIPANTS (control-plane services, every StorageService instance, DeviceManager with its drivers, the ACPI service - never frozen) from the FROZEN SET (applications), with the missing boundary made concrete - ProcessService creates one applications Domain at start and puts every client launch in it, the per-launch bounded/limited Domains becoming its children through an additive parent argument on `SYS_DOMAIN_CREATE`; `start` (service launches) keeps the control-plane Domain; VT 1's shell stays an unfrozen, idle service. A new item defines `SYS_DOMAIN_FREEZE(domain, on)` gated by MANAGE on the Domain (the right that already kills the subtree), parking at the `SIG_STOP` exit point on a separate FROZEN flag, so a person's stopped job stays stopped after the thaw and a `SIG_CONT` sent during the freeze applies at the thaw; completion is confirmed within a bound. A new WHO DOES WHAT item: ServiceManager serves `system-sleep` and orchestrates; SystemManager only makes the kernel call (it alone holds root-Domain MANAGE); the policy moves to the power-state service (part c), which takes idleness from InputService's `input-activity` root as P02M0199c specifies it rather than a second activity signal, and `_HOT` stays with P02M0198d's ProcessorPowerService as a `system-sleep` requester. The order item is rewritten as six numbered steps (announce and inhibit, freeze, flush and hold writes, drivers in reverse bind order with provider consumers first and xHCI ordering its USB devices internally, the ACPI service's `_PTS`/`_SST`, enter) and their reverse on resume, with the applications thawed last.

2. **ACCEPTED - clock semantics, the second clock, the counter reset and the order with P02M0198.** Confirmed: every Timer and wait deadline is absolute on the tick counter (`abi` 223, `Timer::is_expired`); the tick gate claims a whole backlog in one step and measures a signed distance from its anchor, so a reset TSC stalls it; `SYS_CLOCK_MONO_NS` reads the raw TSC; every core runs a periodic LAPIC timer. Plan changes (part b, per D14): an ORDER line (P02M0198a lands before this part) and a rewritten clock item - the monotonic clock (ticks and `SYS_CLOCK_MONO_NS`, which P02M0198a computes when read from the counter less ONE sleep-offset term) excludes the sleep, so no Timer or wait deadline passes during a sleep; the kernel's entry CALLS P02M0198a's rebase on every resume, whose one arithmetic (the offset becomes the counter at resume less the monotonic value at suspend) covers both suspend to idle and S3's counter reset; a new read-only `SYS_CLOCK_BOOT_NS` (nanoseconds, `clock_boot_ns()` in `rt`, no timers on it) includes the sleep, measured by the counter in suspend to idle and by the RTC across S3; "fires once on resume" is restricted to wall-clock alarms (the scheduled wake); TimeService counts wall time on the boot-time clock and re-reads the RTC on the resume notice. The suspend-to-idle item now relies on P02M0198a's tickless idle rather than stopping the tick, and programs the one-shot timer from the timed wake alone, never from a monotonic deadline. A new item makes the per-core resume path one operation (the architecture's secondary-core entry, then the saved per-core state), which P02M0198b's context-losing idle states also use.

3. **ACCEPTED - kernel sleep entry, wake arming and `\_S5` delivery.** Confirmed: the SCI module arms only PWRBTN|SLPBTN and "enters no sleep state"; `sys_system_power` is the only power entry (root-Domain MANAGE); `poweroff` writes fixed ports. Plan changes (part b, per D13): a new item defining `SYS_SYSTEM_SLEEP(root Domain, state, timed wake)` under the same root-Domain MANAGE - wake-set arming and status clearing, secondary cores stopped, the kernel's own devices saved (including every PCI function's recorded configuration header, since S3 resets every device), the FACS waking vector, cache flush, SLP_TYPa/b then SLP_EN, the resume through `src/smpboot`'s trampoline with IOMMU, interrupt controllers and PCI headers restored before any driver runs, the wake reason returned, and the waking fixed-button status cleared rather than delivered as a press (QEMU's `system_wakeup` sets the power-button status). A new item: the ACPI service registers the `\_S3`/`\_S4`/`\_S5` pairs with the kernel at start through a call gated by the privilege P02M0196b gives it; the soft-off item uses the registered `\_S5` and keeps today's fixed ports, PSCI and SBI as the fallback until then. `_PTS`/`_SST` sit in step 5 before the entry and `_WAK` first on resume (part a). The wake-set item (part c) gives each source one owner: PM1 fixed bits and RTC_EN are the kernel's, wake GPEs go through P02M0196b's kernel GPE interface on `_PRW`/`_DSW` of the companion node, device interrupts are marked by their driver. The matching P02M0196c text is another plan's and is listed for the coordinator.

4. **ACCEPTED - the driver contract belongs in the driver wire, with a heartbeat rule and scaled bounds.** Confirmed: `liber:device@1` is enumeration and catalogue; the wire's only quiesce verb `STOP` ends the binding; the heartbeat is sent only to `Online` nodes; `machine_scale` exists because healthy drivers miss unscaled deadlines. Plan changes (part a): the contract item is rewritten as four non-terminal opcodes in `driver-protocol` (`SUSPEND`, `SUSPENDED`, `RESUME`, `RESUMED`) exchanged only between DeviceManager and a driver, with their payloads (target state, wake request, timed wake; outcome, refusal code, an "awake by" bound; lost-power flag; did-not-come-back leading to the existing teardown and rebind); a per-entry `suspend-deadline` beside `heartbeat-deadline`, multiplied by `machine_scale()`; the only `liber:device@1` change is an appended `suspended` binding state. A new item states the heartbeat rule: supervision pauses because the node is not `Online`, the existing gate that already covers `Stopping`, the channel is still drained and `beat.resume` restarts it. The transaction's own step bounds are scaled the same way by ServiceManager. The wire's revision number is explicitly NOT bumped (nothing is versioned before the first release), which the recommendation did not ask for but the owner's rule requires stating.

5. **ACCEPTED - the hardware watchdog across a sleep.** Confirmed against P02M0200a ("once armed, it stays armed"; only an orderly power-off disarms or extends). Plan changes, per D15: a new part-a item naming the watchdog's step in the driver exchange - suspended last, disarmed where the device allows it, otherwise its longest timeout with a last pet and an "awake by" bound that caps the sleep's timed wake below the timeout; re-armed FIRST on resume with a timeout that also catches a hung resume, WDAT's `STOPPED` flag read. The recommendation's "before the freeze" was not taken: the watchdog is kept armed through the freeze and the driver pass so they are covered too, and disarmed only as the last driver step. A device that stops counting in the requested state (WDAT `STOPPED`) returns no bound, and the watchdog service keeps petting until its binding's step, since only applications are frozen. Part d gains the gate case "suspend to idle longer than the timeout" with `-action watchdog=pause` read back by `query-status` (`running` after the sleep, `watchdog` after a resume made to hang) - the harness keeps no standing QMP listener for a `WATCHDOG` event - carried by whichever of P02M0197 and P02M0200 lands second.

6. **ACCEPTED - QEMU wake triggers and the suspend-to-idle oracle.** Confirmed in the tree that the harness keyboards are `usb-kbd` behind `qemu-xhci` and `virtio-keyboard-pci` and that no PS/2 keyboard is driven; `-device ...,help` shows no wake property on either. Plan changes (part d): the S3 case wakes by `system_wakeup` and by an RTC alarm the guest arms (which also proves RTC_EN arming, since QEMU wakes on the RTC only with it set); a key wake from S3 is marked CHECKED FIRST with the outcome to be written in the plan and not required by the gate until shown. Suspend to idle wakes by the guest-armed timed wake through `sleepctl`, and its host-side oracle is the serial log's timing: the harness-timestamped interval between `sleep: entered` and `sleep: resumed` with the counter program silent throughout, at least the armed time long, and no catch-up burst after.

7. **ACCEPTED (in part) - hibernation.** The constraints were confirmed (no kernel storage path; the TPM only through P02M0190's driver, P02M0190 excluding measured boot, no riscv64 TPM model; `docs/LIBERFS.md` reserving no area). The recommendation to move hibernation and hybrid sleep to a new milestone is DECLINED under the coordinator's decision D18, which keeps them here because the owner approved "to disk". Plan changes: hibernation becomes part e, "a part of its own", stating that suspend to idle and S3 do not depend on it and that until it exists a critical battery and `_HOT` power off (also stated in the policy item); the milestone's status line says it closes with all five parts and that parts a to d do not wait for e. Part e records the three constraints and the CHOSEN APPROACH: a kernel SNAPSHOT state that returns twice, the image written afterwards by the disk's driver, the TPM driver and an image writer while StorageService keeps holding writes; a dedicated GPT partition of a LiberSystem hibernation type as the space; restore by a fresh boot with the system volume mounted read-only until the header is checked, handing the kernel a whole-memory replacement that rejoins the S3 resume path (the loader stays out of it); the key built to fit P02M0190's TpmService as it now stands - a 32-byte key-encryption key (within its 96-byte limit) sealed to ONE PCR, the loader's PCR 4, wrapping the image key in the header, sealed and unsealed by the SAME component because TpmService refuses `other-component` - with the "not tied to the kernel" limit written down, no hibernation where TpmService cannot seal, and a passphrase fallback recorded as an owner question asked when the part starts; the refusal rules (including `policy-refused`); hybrid sleep; and its own verification. The critical-battery fallback powers off in order - `system-power`'s `power-off-within` armed first, then ServiceManager's `system-shutdown`, both P02M0198d's - never by an immediate `system-power` call, and part d gains a fixture case for it once P02M0099's ACPI battery producer exists. The hibernation gate moved from part d into part e.

8. **ACCEPTED - the Time and Alarm Device's clock path.** Confirmed: TimeService reads only `SYS_CLOCK_RTC` and serves only its client channel; StorageService stamps from the same call; the option "move the other readers onto TimeService" is infeasible for StorageService, which is pinned and mounts the volume TimeService is loaded from. Plan changes (part c): the TAD item now registers the TAD's `_GRT` BEHIND THE KERNEL'S RTC READ on a machine where the kernel drives no RTC (FADT says the CMOS RTC is absent) - the TAD's driver hands the kernel a wall-clock base through a `clock-source` resource DeviceManager hands it at bind, refreshed in its `RESUME` step, and `SYS_CLOCK_RTC` answers base plus the boot-time clock (0 before it) - so every reader keeps the one kernel read. TimeService's resume event is the sleep notice it declares in its manifest entry (part a step 1), on which it re-reads the RTC (part b). `_SRT` is recorded as unused, since nothing writes a hardware clock. The lid and control-method buttons were added as P02M0197's own drivers on P02M0196b's node-scoped channel (D4), which the TAD item already assumed.

Re-check of the whole plan: every part now names its owner for each step (ServiceManager orchestrates, SystemManager enters, ProcessService freezes, StorageService flushes, DeviceManager drives the wire, the ACPI service runs the methods and registers the sleep types, the kernel arms PM1/RTC and enters, the power-state service holds the policy); the kernel interfaces it adds are named with their privileges (`SYS_SYSTEM_SLEEP` under root-Domain MANAGE, `SYS_DOMAIN_FREEZE` and the `SYS_DOMAIN_CREATE` parent argument under Domain MANAGE, the sleep-type registration under the ACPI service's privilege, `SYS_CLOCK_BOOT_NS` ungated read-only, the `clock-source` resource); every interface change is additive and no version moves; the clock semantics are one consistent set with the order against P02M0198a stated; suspend to idle and S3 are separated from hibernation; and every gate names a trigger QEMU provides and a host-side oracle. The owner questions left open are recorded as asked when their part starts (the policy defaults; the hibernation passphrase fallback). No source, test, script or other plan was modified; only `docs/todo/P02M0197.md` was edited and this response appended.


AUDITOR'S RE-AUDIT OF PLAN P02M0197 (2026-09-26T04:01:12Z):

**Rating: 6/10.** The structure and most corrections now hold, but the frozen set still freezes the participants, the monotonic clock is undefined between the entry and the rebase, the TPM loses its state across S3, and several gates cannot fail or cannot run as written.

The complete history was read: the original review and the planner's response. The plan was checked at commit 0dd5da07, which matches the working tree. It was checked against the kernel's clock, scheduler, SCI, power, Domain and signal paths, ProcessService, ServiceManager's launch paths, SystemManager, DeviceManager, the driver wire, TimeService, LogService, the manifest and the x86_64 harness. It was also checked against P02M0196, P02M0198, P02M0199, P02M0200, P02M0190, P02M0181, P02M0141 and P02M0099, and against QEMU 10.0.0, edk2 and Linux sources. The corrections for findings 4 and 5 hold, and the partial decline of finding 7 is justified on its merits, because hibernation is now a part that parts a to d do not wait for. The corrections for findings 1, 2, 3, 6 and 8 hold in outline but leave the gaps below. Three defects are new.

1. **High - The applications Domain is keyed to ProcessService's operations, but ServiceManager loads 24 of the 34 services through those same operations, so the freeze stops participants the plan says are never frozen.**

   The plan puts ["every client launch"](/data/yellow/libersystem/docs/todo/P02M0197.md:57) into the applications Domain - `launch`, `launch_prepared`, and the bounded and limited forms. It keeps only ["`start`, ServiceManager's launch of a service"](/data/yellow/libersystem/docs/todo/P02M0197.md:61) in the control plane.

   ServiceManager does not use `start`. It [raw-spawns only the pinned set and loads every other service through ProcessService](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:545). It uses [`launch`](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:363), or [`launch-prepared-limited`](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:312) for a service with limits ([the branch](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:572)). Restarts take the same path ([here](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1500) and [here](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1574)). The manifest stages 24 of its 34 services on the volume. They include [VT 1's shell](/data/yellow/libersystem/src/user/services/manifest.toml:2285), [TimeService](/data/yellow/libersystem/src/user/services/manifest.toml:2562), [the power-state service](/data/yellow/libersystem/src/user/services/manifest.toml:2447) and [ConsoleService](/data/yellow/libersystem/src/user/services/manifest.toml:875). Only DeviceManager, the StorageService instances, LogService and [ProcessService](/data/yellow/libersystem/src/user/services/manifest.toml:1839) are pinned. `start` is actually the [`run` tool's](/data/yellow/libersystem/src/user/apps/tools/src/run.rs:49). `run` is an application, and ProcessService [spawns what it starts into its own Domain](/data/yellow/libersystem/src/user/services/core/src/process_service.rs:1120).

   As written, [step 2](/data/yellow/libersystem/docs/todo/P02M0197.md:77) has these effects:
   - It freezes VT 1's shell, which the plan says [is not frozen](/data/yellow/libersystem/docs/todo/P02M0197.md:62).
   - TimeService is still frozen when [the resume notice it must act on](/data/yellow/libersystem/docs/todo/P02M0197.md:91) is sent, because applications thaw last.
   - It freezes the power-state service that [holds the policy](/data/yellow/libersystem/docs/todo/P02M0197.md:232).
   - It freezes P02M0200's watchdog service, which is [plan-relaunchable](/data/yellow/libersystem/docs/todo/P02M0200.md:80) and so launched through ProcessService. The service therefore stops petting at step 2, not ["until its binding's step"](/data/yellow/libersystem/docs/todo/P02M0197.md:120).

   Programs started with `run` escape the freeze. And ["holds new launches until the thaw"](/data/yellow/libersystem/docs/todo/P02M0197.md:70) would also hold ServiceManager's own synchronous relaunch of a participant that dies mid-transaction. This continues original finding 1: the correction still freezes participants.

   **Correct the frozen-set item.** Key the Domain to who asks, not to the operation. ServiceManager's launches keep the control-plane Domain and are never held. They can come in on a ProcessService root that no client holds, or be marked on ServiceManager's own launcher connection. Every other client's launch, `start` included, goes into the applications Domain and is held until the thaw.

2. **Medium - The plan never says what the monotonic clock reads between the entry and the rebase. With P02M0198a's clock, any read in that window makes the rebase stall the clock after suspend to idle.**

   P02M0198a computes ticks when read and keeps readings non-decreasing ["by one atomic maximum"](/data/yellow/libersystem/docs/todo/P02M0198.md:27). Its [rebase](/data/yellow/libersystem/docs/todo/P02M0198.md:31) sets the offset from the counter at resume. It leaves the meaning of a sleep [to this plan](/data/yellow/libersystem/docs/todo/P02M0198.md:34).

   This plan rebases only [on resume](/data/yellow/libersystem/docs/todo/P02M0197.md:179). It parks the cores [through P02M0198a's tickless idle](/data/yellow/libersystem/docs/todo/P02M0197.md:159), and during the sleep an interrupt outside the wake set ["is handled and the cores return to idle"](/data/yellow/libersystem/docs/todo/P02M0197.md:162). In suspend to idle the counter keeps running, so a read in that window returns the sleep's time. That idle path reads the clock on every wake:
   - [`check_deadlines`](/data/yellow/libersystem/src/kernel/sched/mod.rs:969) reads it, and P02M0198a keeps that call in [every BSP halt](/data/yellow/libersystem/docs/todo/P02M0198.md:44).
   - So does [the idle wait loop](/data/yellow/libersystem/src/kernel/sched/mod.rs:1193).
   - So do [the SCI's storm check](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:310) and [the idle pass's platform-event check](/data/yellow/libersystem/src/kernel/platform_event.rs:121).

   The timed wake itself ends the halt in that loop before the entry's code can rebase. Any such read raises the maximum past the suspend value. After the rebase, the clock then returns that maximum until the counter catches up. That is a stall as long as the sleep, the failure original finding 2 described. So ["no negative distance that stalls the ticks"](/data/yellow/libersystem/docs/todo/P02M0197.md:186) does not hold. The same deadline check fires the deadlines that ["a sleep makes no deadline pass"](/data/yellow/libersystem/docs/todo/P02M0197.md:181) excludes.

   **Correct the clock item.** State that the monotonic clock holds its suspend value from the entry until the rebase. Every read in that window answers the suspend value. The suspend-to-idle wait runs no deadline check, and the rebase is the first act on every exit from that wait. The maximum then never passes the suspend value.

3. **Medium - The driver contract gives the TPM driver nothing to do at a sleep. Without `TPM2_Shutdown(TPM_SU_STATE)`, an S3 resets the firmware's PCRs, which breaks part e's key and P02M0190's sealed secrets.**

   The contract's `SUSPEND` duties are [stopping DMA, masking interrupts, the lowest state and wake](/data/yellow/libersystem/docs/todo/P02M0197.md:99). Step 4 names the device-specific duties of [storage, xHCI and the watchdog](/data/yellow/libersystem/docs/todo/P02M0197.md:81). The TPM driver has [no interrupt and no DMA](/data/yellow/libersystem/docs/todo/P02M0190.md:75), and P02M0190 says nothing about sleep anywhere. Yet the part covers [every driver the image ships](/data/yellow/libersystem/docs/todo/P02M0197.md:117).

   On S3 wakeup, QEMU resets the devices ([`pc_machine_wakeup` and `pc_machine_reset`](https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/pc.c#L1722-L1743)). The CRB front-end's reset handler resets the backend and starts the TPM again ([`tpm_crb_reset`](https://github.com/qemu/qemu/blob/v10.0.0/hw/tpm/tpm_crb.c#L234-L280), [registered here](https://github.com/qemu/qemu/blob/v10.0.0/hw/tpm/tpm_crb.c#L314)). On an S3 resume, edk2 sends `TPM2_Startup(TPM_SU_STATE)`. When that fails, as it does with no earlier `TPM2_Shutdown(TPM_SU_STATE)`, edk2 falls back to `Startup(CLEAR)` and extends an error separator into PCRs 0 to 7 ([Tcg2Pei.c](https://github.com/tianocore/edk2/blob/master/SecurityPkg/Tcg/Tcg2Pei/Tcg2Pei.c#L1081-L1114)). Linux sends that shutdown in its suspend path ([tpm-interface.c](https://github.com/torvalds/linux/blob/master/drivers/char/tpm/tpm-interface.c#L449)).

   After any S3 in a boot, PCR 4 no longer holds the loader's measurement. Part e seals its key-encryption key [to PCR 4's present value](/data/yellow/libersystem/docs/todo/P02M0197.md:327). The restore then gets `policy-refused` and drops the image [as "the loader changed"](/data/yellow/libersystem/docs/todo/P02M0197.md:334). Applications may [seal to any PCR](/data/yellow/libersystem/docs/todo/P02M0190.md:128) and are promised that an object opens ["only while its PCR holds its value"](/data/yellow/libersystem/docs/todo/P02M0190.md:214). Their seals to PCRs 0-7 stop opening until the next boot, and PCRs 16 and 23 lose their extends. Part d has no TPM case, and the harness adds a TPM [only when a run asks for one](/data/yellow/libersystem/src/harness/qemu-run.sh:2086), so no gate would catch this.

   **Correct the driver-contract item.** Name the TPM driver's step. It sends `TPM2_Shutdown(TPM_SU_STATE)` in its `SUSPEND` for S3. It sends no `Startup(CLEAR)` in its `RESUME` when the firmware has already started the TPM with the saved state. Add a case to part d's S3 gate with `swtpm` behind the CRB front-end:
   - a PCR 16 value extended before the sleep reads back unchanged after it;
   - a secret sealed to PCR 4 before the sleep unseals after it.

4. **Medium - The plan never says when `system-sleep`'s suspend answers, and two of its requesters are participants that the transaction waits on while the request is outstanding.**

   The requesters include [the sleep buttons](/data/yellow/libersystem/docs/todo/P02M0197.md:45). The control-method sleep button's driver asks for a suspend itself, and [DeviceManager asks for the fixed button](/data/yellow/libersystem/docs/todo/P02M0197.md:216). Both must act in [step 4](/data/yellow/libersystem/docs/todo/P02M0197.md:80): DeviceManager sends `SUSPEND` to every binding, and each binding, the button's own included, must answer or [abort the suspend](/data/yellow/libersystem/docs/todo/P02M0197.md:115).

   The only answer the plan defines is the entry's, which [returns on resume](/data/yellow/libersystem/docs/todo/P02M0197.md:87). A `suspend` that answers the same way leaves the requester blocked in its own request. DeviceManager already makes its power request that way: [a synchronous round trip with no deadline](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:5115). Built on the same pattern, every sleep that a button asks for would abort at the step bound.

   **Correct the vocabulary item.** State that `suspend` answers when ServiceManager accepts or refuses the transaction, never at the resume. Its outcome is read from the last sleep's record.

5. **Medium - The soft-off case cannot tell the registered `\_S5` from the fallback, and its fallback half runs the registered path.**

   On q35, QEMU's `\_S5` is [(0, 0)](https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/acpi-build.c#L1694-L1699), and the FADT's PM1a control block is [the PM base plus 4](https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/acpi-build.c#L177-L178). So the registered path writes the same value to 0x604 that [the fallback writes first](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:179). Either path produces the [`SHUTDOWN` the case checks](/data/yellow/libersystem/docs/todo/P02M0197.md:276), so a registration that never happened still passes.

   The plan also says that [a registered value outlives the service](/data/yellow/libersystem/docs/todo/P02M0197.md:152). "Killed first" therefore leaves the registered path in force, and the fallback never runs. The same sentence contradicts ["absent, dead or not yet started"](/data/yellow/libersystem/docs/todo/P02M0197.md:174). This continues original finding 3.

   **Correct the soft-off and verification items.**
   - The kernel says on the serial log which path powered the machine off, and each half of the case checks that line.
   - The fallback half runs with nothing registered, because the ACPI service is kept from starting, not killed after it registered.
   - The soft-off item says that a service which died after registering leaves its value in force.

6. **Medium - Four verification cases rely on QMP events that this harness never receives, as the plan itself says two items later.**

   These cases rely on events:
   - S3 uses [`SUSPEND` and `WAKEUP` as its oracle](/data/yellow/libersystem/docs/todo/P02M0197.md:254).
   - Soft-off checks [`SHUTDOWN`](/data/yellow/libersystem/docs/todo/P02M0197.md:276), and so does [the battery case](/data/yellow/libersystem/docs/todo/P02M0197.md:285).
   - Hibernation checks [`SUSPEND_DISK`](/data/yellow/libersystem/docs/todo/P02M0197.md:341).

   The watchdog case uses `query-status` because ["the harness keeps no standing QMP listener for an event"](/data/yellow/libersystem/docs/todo/P02M0197.md:279). The harness's only QMP helper [opens a connection per command](/data/yellow/libersystem/src/harness/lab.py:3336), [skips every event](/data/yellow/libersystem/src/harness/lab.py:3325) and [closes the connection](/data/yellow/libersystem/src/harness/lab.py:3353). QMP sends an event only to a client connected at that moment, as [P02M0200 records](/data/yellow/libersystem/docs/todo/P02M0200.md:204). This continues original finding 6.

   **Correct the verification items.** Use the durable state. `query-status` answers `suspended` while the guest is in S3 and `running` after the wake, and a soft-off is QEMU's exit. Otherwise, state that these gates hold their own QMP connection open from before the request until after the event. That is required for `SUSPEND_DISK`, whose exit looks like a soft-off.

7. **Medium - The suspend-to-idle oracle is satisfied by the freeze alone, and on x86_64 it can fail for a correct implementation.**

   The counter program is an application. It is frozen at [step 2](/data/yellow/libersystem/docs/todo/P02M0197.md:77) and thawed [last](/data/yellow/libersystem/docs/todo/P02M0197.md:91). So it is [silent between `sleep: entered` and `sleep: resumed`](/data/yellow/libersystem/docs/todo/P02M0197.md:265) whether or not any core parked. The interval check needs only the timed wake, and the "next value" check tests only the clock. An entry that waits for the timed wake while every core keeps its periodic tick passes all three checks. Yet parked cores and masked interrupts are [what the item says suspend to idle is](/data/yellow/libersystem/docs/todo/P02M0197.md:159). P02M0198a provides the number that would show it, [a per-core idle record](/data/yellow/libersystem/docs/todo/P02M0198.md:66), and it lands [before this part](/data/yellow/libersystem/docs/todo/P02M0197.md:130).

   There is a second problem on x86_64. Kernel serial output is [an asynchronous ring that the idle pass and the tick drain](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:143), which is why [`poweroff` flushes it](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:177). P02M0198a makes [a non-empty ring cap an idle core's sleep at one tick](/data/yellow/libersystem/docs/todo/P02M0198.md:64). This plan instead programs the one-shot ["from the timed wake alone"](/data/yellow/libersystem/docs/todo/P02M0197.md:161). Read literally, `sleep: entered` waits in the ring until the wake. The host then sees both lines together and fails a correct suspend. This continues original finding 6.

   **Correct the suspend-to-idle verification item.** Add a check that can fail: read P02M0198a's per-core record before and after the sleep. It must show no timer interrupt and no device interrupt outside the wake set on any core between the two lines. Also make the entry flush the kernel's serial ring synchronously before the cores park, as `poweroff` does.

8. **Medium - The Time and Alarm Device's clock and alarm are used only on a machine without a CMOS clock, and the gate machine always has one.**

   The TAD's timers carry the timed wake only ["on a machine with no CMOS RTC"](/data/yellow/libersystem/docs/todo/P02M0197.md:203). Its `_GRT` is registered only when ["the FADT says the CMOS RTC is absent"](/data/yellow/libersystem/docs/todo/P02M0197.md:221). QEMU's q35 FADT sets [only the 8042 boot-architecture flag](https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/acpi-build.c#L190), while the tree reads "CMOS RTC not present" from [bit 5 of that field](/data/yellow/libersystem/src/acpi/src/lib.rs:743). P02M0196d's fixture is [an SSDT](/data/yellow/libersystem/docs/todo/P02M0196.md:311), which cannot change the FADT.

   So on the gate machine, `SYS_CLOCK_RTC` never answers the TAD's base, and the CMOS alarm or the one-shot carries the timed wake. Nothing in the system observes the harness ["acting as the TAD's clock and alarm"](/data/yellow/libersystem/docs/todo/P02M0197.md:284). This continues original finding 8, whose design now holds but cannot be reached by its gate.

   **Correct the TAD verification item.** Say how the gate reaches both halves on q35. One way is a development-build switch that makes the kernel treat the CMOS clock as absent: a fw_cfg file, read [as the boot profile is](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:190). Otherwise, narrow the case to what q35 can show and keep the registration host-tested.

9. **Low - The S3 entry item names the wrong trampoline and leaves out the MSI-X table entries the kernel programs.**

   The plan sends the resuming core through the trampoline in [`src/smpboot`](/data/yellow/libersystem/docs/todo/P02M0197.md:142), and says so again [for the per-core path](/data/yellow/libersystem/docs/todo/P02M0197.md:155). But `src/smpboot` is [the logical-id bookkeeping the two device-tree ports share](/data/yellow/libersystem/src/smpboot/src/lib.rs:3) and holds no trampoline. The x86_64 real-mode trampoline is [apboot.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/apboot.rs:1).

   The save list names only [configuration-header state](/data/yellow/libersystem/docs/todo/P02M0197.md:137). The kernel [programs each function's MSI-X table entry itself](/data/yellow/libersystem/src/kernel/device.rs:33), and that entry lives in a BAR, not in the header. QEMU resets virtio-pci functions on the wakeup reset by default ([`x-pcie-pm-no-soft-reset` is off](https://github.com/qemu/qemu/blob/v10.0.0/hw/virtio/virtio-pci.c#L2372-L2373)), and no driver knows the message the kernel wrote there. This continues original finding 3.

   **Correct the S3 entry item.** Name `src/kernel/arch/x86_64/apboot.rs` as the x86_64 path. Add the kernel-programmed MSI-X table entries to what the kernel restores before any driver runs.

10. **Low - Part e still says riscv64's QEMU has no TPM model, which contradicts P02M0190's corrected plan and this machine's QEMU, so riscv64 would report hibernation "not set up" instead of being verified.**

   Part e's constraints say ["riscv64's QEMU has no TPM model"](/data/yellow/libersystem/docs/todo/P02M0197.md:301). Its key item lists ["riscv64 here"](/data/yellow/libersystem/docs/todo/P02M0197.md:331) among the machines where TpmService cannot seal. Its verification lets riscv64 report ["not set up" rather than passing](/data/yellow/libersystem/docs/todo/P02M0197.md:344).

   P02M0190 was corrected on exactly this point in the same commit. It now says this machine's aarch64 and riscv64 QEMUs both offer [`tpm-tis-device`](/data/yellow/libersystem/docs/todo/P02M0190.md:22), and its runtime item binds the TPM on both through [P02M0196a's device-tree half](/data/yellow/libersystem/docs/todo/P02M0190.md:208). `qemu-system-riscv64 -device help` on this machine lists `tpm-tis-device, bus System`. Part e's own rule makes hibernation unavailable only where TpmService cannot seal, so the riscv64 exclusion rests on a false premise. This is a new finding.

   **Correct part e's constraints, key and verification items.** Drop the riscv64 exception, and verify riscv64 as aarch64 is verified: once P02M0190's driver binds its TPM through the device tree.

Validation: This was a read-only inspection of the plan at commit 0dd5da07 (identical to the working tree), its complete audit history, the neighbouring plans, the kernel, services, drivers, manifest and harness sources. It included read-only `qemu-system-x86_64 -device ICH9-LPC,help` and `qemu-system-riscv64 -device help` queries and the harness firmware descriptor. Primary sources were fetched read-only: QEMU v10.0.0 (`hw/i386/pc.c`, `hw/i386/acpi-build.c`, `hw/acpi/core.c`, `hw/tpm/tpm_crb.c`, `hw/virtio/virtio-pci.c`, `hw/input/ps2.c`, `hw/usb/hcd-xhci.c`, `system/runstate.c`), edk2's `Tcg2Pei.c` and Linux's `tpm-interface.c`. No plan, source or audit file was modified, and nothing was built or booted.
