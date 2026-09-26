AUDITOR'S REVIEW OF PLAN P02M0200 (2026-09-25T22:59:57Z):

**Rating: 4/10.** The separation from ServiceManager's software watchdog is correct, and QEMU's `WATCHDOG` event is a sound oracle. But none of the three emulated devices can be armed with the resources the plan assigns. And the contract has no answer for sleep, for SystemManager's liveness, or for a watchdog that survives a reboot.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0200.md) at commit `07371c44af82a11c1d275b0cbd832899f712e62a`; the plan itself is a new, untracked file, as are P02M0197 to P02M0202, and P02M0190, P02M0196 and P02M0099 carry uncommitted edits; all were read in the working tree, and line numbers refer to it. Also reviewed the plans it depends on: [P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md:24), [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:35), [P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:31) and [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:44). In the tree, the review covered:

- the kernel's PCI resource table, its configuration-space policy, and its SCI and power paths;
- DeviceManager's catalogue;
- SystemManager and ServiceManager supervision;
- the harness's QMP client.

Device behaviour was checked in two sets of sources:

- QEMU 10.0.0: `wdt_i6300esb.c`, `ich9_tco.c`, `ich9.c`, `ich9.h` and `watchdog.c`;
- Linux: the `i6300esb`, `iTCO_wdt`, `lpc_ich`, `wdat_wdt`, `acpi_watchdog` and `ipmi_watchdog` drivers.

The QEMU 10.0.11 on this machine was only queried with `-device ...,help` and `-help`. The plan's description of ServiceManager's watchdog - a software heartbeat over one canary - is accurate ([service_manager.rs](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:198)). The findings concern decisions the plan must make. They do not concern the expected absence of implementation.

1. **High - A ring-3 driver cannot arm or stop the `i6300esb`: its enable, lock and mode bits are PCI configuration registers, and the plain-PCI resource profile cannot even select the device.**

   The [device item](/data/yellow/libersystem/docs/todo/P02M0200.md:38) makes it "a plain PCI driver ... on the plain-PCI resource profile". The device is controlled almost entirely through configuration space:
   - in QEMU, the timer starts and stops only through writes to configuration register 0x68 (enable, lock, free-run), and is configured through 0x60 (reboot enable, clock scale, interrupt type), in [`i6300esb_config_write`](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/hw/watchdog/wdt_i6300esb.c);
   - the 16-byte memory BAR holds only the two preload values, the interrupt status and the reload register;
   - Linux's [driver](https://raw.githubusercontent.com/torvalds/linux/master/drivers/watchdog/i6300esb.c) arms the device with `pci_write_config_byte`.

   This system gives a driver no way to do that:
   - ["a ring-3 driver cannot read PCI config space itself"](/data/yellow/libersystem/src/kernel/device.rs:9);
   - no syscall reaches configuration space - [`SYS_PCI_INFO`](/data/yellow/libersystem/src/abi/src/lib.rs:215) returns the identity the boot scan recorded, for `lspci`;
   - P02M0191 [keeps the configuration mechanism in the kernel](/data/yellow/libersystem/docs/todo/P02M0191.md:55).

   The resource profile cannot select the device either. It is a [class-triple table](/data/yellow/libersystem/src/kernel/arch/common/pci/mod.rs:26) that gives standard families a BAR and an MSI-X vector, and it has no vendor or device key. The i6300esb is class 0x0880, "other system peripheral" (same QEMU file). A row for that class would resource every such function, which is the line the table says it deliberately does not cross.

   This is the one device the plan proves on all three targets. As written, no gate can arm a watchdog on any of them.

   **Correct the i6300esb item** by adding the missing mechanism: a write to named configuration registers of the claimed function, declared in the registry and performed by the kernel for the claim holder. Here that is 0x60 as a word and 0x68 as a byte. Any other offset is refused, the right is revoked at release, and it works on the ECAM targets too. Resolve the device by vendor and device (8086:25ab) rather than by class. Say whether this belongs to P02M0191's authority work or here.

2. **High - The ICH9 TCO item asks for one port sub-range, but the timer is found through the LPC bridge's configuration registers, cannot reset the machine without a memory-mapped chipset register, and no plan can mint either range.**

   The [TCO item](/data/yellow/libersystem/docs/todo/P02M0200.md:39) needs only "the TCO's own sub-range" from P02M0191. The timer needs more than that:
   - Linux's [`lpc_ich`](https://raw.githubusercontent.com/torvalds/linux/master/drivers/mfd/lpc_ich.c) finds the block at the ACPI base (LPC configuration 0x40) plus 0x60..0x7F;
   - for ICH9, the No-Reboot bit is in GCS, at the root-complex base (LPC configuration 0xF0) plus 0x3410, in memory. [`iTCO_wdt`](https://raw.githubusercontent.com/torvalds/linux/master/drivers/watchdog/iTCO_wdt.c) clears that bit through the memory resource;
   - QEMU 10.0 resets GCS to `0x00000020`, which is No-Reboot set ([ich9.h](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/include/hw/southbridge/ich9.h));
   - its [`tco_timer_expired`](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/hw/acpi/ich9_tco.c) calls `watchdog_perform_action()` only on the second expiry, and only when both the `noreboot` strap and GCS.No-Reboot are clear. The strap is off by default on this QEMU.

   An [upstream patch of September 2025](https://www.mail-archive.com/qemu-devel@nongnu.org/msg1140528.html) proposes changing that GCS default. Whether this 10.0.11 build carries it has not been checked.

   Nothing can mint these ranges:
   - P02M0191 mints only for [`_CRS` descriptors and COM1..COM4, and refuses only the PIC, the PIT and the configuration ports](/data/yellow/libersystem/docs/todo/P02M0191.md:24);
   - a range read from a PCI function's configuration register is not one of its sources, and the driver cannot read that register itself (finding 1);
   - "the kernel keeping PM1 and GPE" is not one of its exclusions, and it has no memory path;
   - the LPC bridge is [inventoried with no resources](/data/yellow/libersystem/src/kernel/test_suites/hardware.rs:1601), and the plan does not say who claims it;
   - the kernel already drives [PM1a](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:7), and [writes PM1a control to power off](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:179).

   Without GCS, neither the TCO gate nor the WDAT-over-TCO gate ever sees a `WATCHDOG` event on this QEMU. Without the configuration registers, the driver cannot find its ports.

   **Correct the TCO item, and its dependency on P02M0191**, with:
   - a claim on the LPC bridge (8086:2918). Through it the kernel reads the PM and root-complex bases, mints exactly the TCO block, and mints the GCS dword as a memory range. It refuses PM1, the PM timer and GPE0;
   - that mint source and those exclusions, added to P02M0191;
   - No-Reboot cleared when the timer is armed;
   - a timeout mapping in the contract that states the TCO resets on its second expiry.

   Check GCS's reset value on this QEMU first.

3. **Medium - The WDAT item omits the action and the flag that the fixture and the sleep path need, its range check has no independent source of ranges, and nothing stops it binding the same registers as the TCO driver.**

   The [WDAT item](/data/yellow/libersystem/docs/todo/P02M0200.md:41) lists countdown, ping, start, stop, running state and status. It leaves out part of the [table's action set](https://raw.githubusercontent.com/torvalds/linux/master/include/acpi/actbl3.h) and one of its flags:
   - `SET_REBOOT` and `GET_REBOOT` are missing (as are `SET_SHUTDOWN` and `GET_SHUTDOWN`). Linux runs `SET_REBOOT` at probe and on resume ([wdat_wdt.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/watchdog/wdat_wdt.c)). For a WDAT over this QEMU's TCO, it is the only action that can clear GCS.No-Reboot (finding 2);
   - the table's `STOPPED` flag (0x80, "stopped in sleep") is not read, although Linux's suspend decision rests on it.

   The range check cannot fail as written. The platform device's ranges come from the table itself; Linux builds them from the WDAT entries ([acpi_watchdog.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/acpi/acpi_watchdog.c)). So "a register outside the ranges its platform device was given" can fail only if the kernel mints a filtered subset, and P02M0191 has [no static-table source](/data/yellow/libersystem/docs/todo/P02M0191.md:24) to mint from at all.

   The [device gate](/data/yellow/libersystem/docs/todo/P02M0200.md:56) runs the TCO driver and a WDAT over the same registers, and the plan never says which one binds. Linux creates no TCO device when an ACPI watchdog exists: "If we have ACPI based watchdog use that instead" ([lpc_ich.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/mfd/lpc_ich.c)).

   **Correct the WDAT and device-gate items**:
   - add the reboot and shutdown actions, and run `SET_REBOOT` at bind and on resume;
   - honour `STOPPED`;
   - define the device's ranges as the subset of the table's registers the kernel mints, after it refuses kernel-held and already-claimed ranges. Add that source to P02M0191, so the hostile-table test has something to fail against;
   - state that a WDAT suppresses the TCO driver (or the reverse), and that overlapping mints are refused.

4. **Medium - Neither this plan nor P02M0197 says who disarms or extends an armed watchdog around a sleep, so any suspend longer than one timeout resets the machine.**

   Here the timer [stays armed](/data/yellow/libersystem/docs/todo/P02M0200.md:30), and only ["an orderly power-off or reboot"](/data/yellow/libersystem/docs/todo/P02M0200.md:32) disarms it. P02M0197 has no answer either:
   - it [freezes processes and quiesces drivers](/data/yellow/libersystem/docs/todo/P02M0197.md:31);
   - its [driver contract](/data/yellow/libersystem/docs/todo/P02M0197.md:37) has no watchdog clause;
   - it proves [suspend to idle on all three targets](/data/yellow/libersystem/docs/todo/P02M0197.md:53). In that state the device keeps counting, and nothing pets it.

   Linux stops the TCO for suspend to idle "because it stops the ticks and timekeeping, so the watchdog cannot be pinged while in that state" ([iTCO_wdt.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/watchdog/iTCO_wdt.c)). `wdat_wdt` stops or pings the timer according to the table's `STOPPED` flag.

   **Correct the contract** with a sleep clause:
   - the watchdog driver's suspend step: disarm where the device allows it; otherwise set the longest timeout and pet a last time, with the sleep transaction's own bound below it;
   - re-arm first on resume;
   - read the WDAT flag.

   Add the matching line to P02M0197's driver contract, and add a gate: a suspend to idle longer than the timeout, with no `WATCHDOG` event.

5. **Medium - The petting condition requires ServiceManager to confirm that SystemManager is alive, which no path in the system can do, while the deaths it would catch already reboot the machine.**

   The [supervision item](/data/yellow/libersystem/docs/todo/P02M0200.md:26) pets "ONLY while the supervisor confirms ... that it and SystemManager are alive".

   Death is already handled:
   - the kernel reboots when SystemManager [ends after boot](/data/yellow/libersystem/src/kernel/main.rs:497);
   - SystemManager [tears the branch down](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:237) and exits when [ServiceManager's channel closes](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:290).

   What is left is a hang, and nothing can detect one:
   - ServiceManager reaches SystemManager only through `system-power`, whose only operations are [reboot and power-off](/data/yellow/libersystem/src/idl/process.lsidl:188);
   - SystemManager's loop only [serves those and relays reports](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:235);
   - ServiceManager's heartbeat probes [the canary and nothing else](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:198).

   "ServiceManager restarts it" is also conditional. It holds only for a service declared [transparent](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1472) and listed in [`plan_relaunchable`](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1561), and only within a [restart budget, three by default](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:289).

   **Correct the supervision item** to define the confirmation:
   - who asks whom, on which channel, with what deadline;
   - either an operation SystemManager answers from its serving loop to prove it responds, or SystemManager dropped from the condition with its death left to the kernel;
   - which services' heartbeats count;
   - the watchdog service's restart declaration, relaunch path and budget.

6. **Medium - The orderly-shutdown clause has no hook to act through, and it matters only for a watchdog that survives the platform reset, which the gate does not exercise.**

   `reboot` and `power-off` are each [one `SYS_SYSTEM_POWER` call](/data/yellow/libersystem/src/user/services/system_manager/src/main.rs:434), which goes [straight to `arch::reset` or `arch::poweroff`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1924). Nothing tells any service, and there is no ["shutdown's own bound"](/data/yellow/libersystem/docs/todo/P02M0200.md:32).

   The i6300esb and the TCO are reset with the platform, so for them the clause is moot. A BMC watchdog ([0200b](/data/yellow/libersystem/docs/todo/P02M0200.md:46)) keeps counting across the host's reset. Linux's IPMI watchdog disables itself at power-off, and at restart stretches its timeout to at least 120 s ([ipmi_watchdog.c](https://raw.githubusercontent.com/torvalds/linux/master/drivers/char/ipmi/ipmi_watchdog.c)).

   The gate ["an orderly power-off with no event"](/data/yellow/libersystem/docs/todo/P02M0200.md:52) cannot fail, because QEMU exits.

   The BMC provider has its own gap. [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:44) places it in the BMC service, but only driver bindings publish into the catalogue: [`publish_all`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2762) is the only path, and the [catalogue interface](/data/yellow/libersystem/src/idl/device.lsidl:360) has no publish operation.

   **Correct the clause and its gate**:
   - add the step to SystemManager's power path - tell the watchdog service, within a bound, before the syscall - or name the shutdown sequence that will own it;
   - require the step for providers that survive a reset;
   - gate it with the BMC provider across a reboot, once P02M0201 names that provider's publisher.

7. **Low - The timeout is not defined for the two-stage devices the plan names, and no device can prove the pre-timeout warning.**

   Both emulated devices act only at the end of a second stage:
   - QEMU's i6300esb runs stage 1, then stage 2, and acts at the end of stage 2. At the end of stage 1 it only prints "I would send APIC 1 INT 10 here if I knew how" ([wdt_i6300esb.c](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/hw/watchdog/wdt_i6300esb.c));
   - the TCO acts on its second expiry. Its first expiry raises only an SMI, and only when SMI_EN.TCO_EN is set ([ich9_tco.c](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/hw/acpi/ich9_tco.c)).

   The [contract](/data/yellow/libersystem/docs/todo/P02M0200.md:22) arms "with a timeout" and offers "a pre-timeout warning where it has one". The [gate](/data/yellow/libersystem/docs/todo/P02M0200.md:51) expects the event "within one timeout and a stated margin".

   **Correct the contract and the gate**: define the timeout as the time from the last pet to the reset, and have each driver divide it across its stages. Mark the pre-timeout path as unprovable on the emulated devices, or drop it until a device can prove it. This is a small change.

8. **Low - The harness cannot yet observe the oracle, and every q35 guest will carry a TCO that can be armed, whose default the plan does not state.**

   The harness's QMP client cannot see the event:
   - `lab.py` opens a QMP connection [per batch](/data/yellow/libersystem/src/harness/lab.py:3281) and [skips events](/data/yellow/libersystem/src/harness/lab.py:3325);
   - QMP sends an event only to a connected client, so without a standing listener every "no event" assertion passes by default.

   Every x86_64 run is exposed. QEMU enables the TCO on q35 by default ([ich9.c](https://raw.githubusercontent.com/qemu/qemu/v10.0.0/hw/acpi/ich9.c)), and `-action watchdog=` defaults to reset (`-help` on this QEMU). Once the driver and the service exist, arming by default would turn a stalled test guest into a silent reset.

   **Correct the verification item** with a reliable oracle - either of:
   - a persistent event listener per run, which must also see the positive case;
   - `-action watchdog=pause`, with `query-status` reporting `watchdog`, as a durable oracle.

   State whether the service arms by default, and what `-action watchdog=` every other run passes.

Validation was by inspection of the plans, the source and primary sources: QEMU 10.0.0's watchdog and ICH9 sources, and Linux's watchdog, LPC and IPMI drivers. The QEMU binary on this machine was only queried for device properties and help text. No plan, source file or git state was modified, and nothing was built or booted.

PLANNER'S RESPONSE ON P02M0200 (2026-09-26T02:49:13Z):

Every finding was checked against the tree and the primary sources. In the tree: `src/kernel/device.rs` (header, claim release order), `SYS_PCI_INFO` and the claim syscalls in `src/abi/src/lib.rs`, the class-triple `RESOURCED` table and the `ConfigAccess` trait in `src/kernel/arch/common/pci/mod.rs` (dword `read32`/`write32` only, with `read16`/`read8` cut out of a dword and `update32` as the read-modify-write), the unresourced-inventory test in `test_suites/hardware.rs`, `arch/x86_64/sci.rs` and the PM1a power-off at 0x604 in `arch/x86_64/mod.rs`, `resident_manager_lost` in `main.rs`, SystemManager's serving loop and its two `SYS_SYSTEM_POWER` calls, `system-power` in `process.lsidl` and `supervisor` in `observability.lsidl`, ServiceManager's heartbeat (a bring-up self-test; the standing loop has "no timer"), `restartable`/`plan_relaunchable`/`MAX_RESTARTS`, the `!poweroff`/`!reboot` path and `shutdown_all` in `service_manager/lifecycle.rs` (LogService `FLUSH` sent without waiting, then SIG_KILL in reverse-dependency order - no service is told), DeviceManager's `publish_all` and driver rebind, `provider-kind` and the catalogue in `device.lsidl`, the manifest's existing `pci-vendor`/`pci-product` narrowing in `system-manifest`, and `lab.py`'s per-batch QMP client. Outside it: QEMU v10.0.0 `wdt_i6300esb.c`, `ich9_tco.c`, `ich9.c`, `lpc_ich9.c`, `ich9.h`, `hw/watchdog/watchdog.c` and `ipmi_bmc_sim.c`; `ich9.h` at the upstream v10.0.11, v10.0.8 and v10.0.6 tags (GCS default 0x20, No-Reboot set) and at master (0, set only by the strap); OVMF's `PlatformInitLib` (PM base 0x600, root-complex base 0xFED1C000 enabled); Linux's `actbl3.h` and `lpc_ich.c`; and `-device ICH9-LPC,help`, `-device i6300esb,help`, `-device help` on all three QEMUs and `-help` on this 10.0.11 build. Nothing was booted. The plans of P02M0191, P02M0196, P02M0197, P02M0198 and P02M0201 were read as they stand now, and this plan was aligned with them.

1. **ACCEPTED - the `i6300esb` needs configuration-register writes and a vendor:device row.** Confirmed, and worse than stated: QEMU answers 0x60 only as a word and 0x68 only as a byte, so the kernel's existing dword accessors and `update32` would reach the generic handler and do nothing. Plan changes: a new P02M0200b item, DECLARED REGISTERS, owned here - a second kernel table beside `RESOURCED` keyed by (vendor, device), shared with P02M0201 (whichever lands first adds it), each row naming the BAR, the declared registers (space, offset, width, write mask) and any chipset derivation; one claim-stamped capability; a syscall that accesses exactly one declared register at exactly its width (0xCFC + (offset & 3) on x86, a sized ECAM access elsewhere); everything else refused; revoked at release with nothing written. The `i6300esb` item now names the row 8086:25ab, 0x60 as a word and 0x68 as a byte, BAR 0 resourced only when no other function's BAR shares its page (a 16-byte BAR placed by OVMF can), DMA policy `none`, and the arm, pet, disarm and last-reset sequences. Driver matching uses the manifest's existing `pci-vendor`/`pci-product` narrowing, so no new manifest vocabulary is needed.

2. **ACCEPTED - the TCO needs the LPC configuration registers, GCS and a mint source.** GCS was checked by reading QEMU's source at the upstream v10.0.11 tag: 0x20, No-Reboot set. `-device ICH9-LPC,help` cannot show a reset value, so the plan records that the Debian build's value is read at the part's first boot. Plan changes: the TCO item is now P02M0191's source (c) with this milestone's ICH9 row. The row is 8086:2918; during the boot scan the kernel reads the PM base, the ACPI enable and the root-complex base, and refuses the row unless decode is on and the PM base equals the FADT's PM1a event block. It records exactly PM base + 0x60..0x7F; PM1, the PM timer, GPE0 and SMI_EN stay the kernel's. The claim has DMA policy `none` and changes no decode bit. The driver clears No-Reboot when arming and reads it back; if it stays set, the arm fails as `unsupported`. The contract's timeout mapping states that the TCO acts on its second expiry, so the count is T / 1.2 s. PARTLY DECLINED: GCS is not minted as a memory range. The 4 KiB page holding it also holds the interrupt-routing registers at +0x3140..0x3150, and a page is the smallest thing a mapping can hand over. GCS is therefore a declared register whose write mask is the No-Reboot bit alone, reached through the kernel's own mapping. The P02M0191 half is already in P02M0191's current source (c) and reserved set, so no change there is needed.

3. **ACCEPTED - the WDAT actions, `STOPPED`, the range source and the WDAT/TCO binding.** Plan changes: the WDAT item now lists every action. `SET_REBOOT`/`GET_REBOOT` are added; `SET_REBOOT` runs at bind and on every resume, and on a WDAT over this QEMU's TCO it is what clears No-Reboot. `GET_SHUTDOWN` is read and `SET_SHUTDOWN` is never run. `GET_STATUS`/`SET_STATUS` are included. The header flags (ENABLED, `STOPPED`) are read, and the sleep step uses `STOPPED`. The item names the four actions a table must have to be a watchdog. The ranges now have a source of their own: port ranges are what the kernel mints from the table's register list through P02M0191's static-table source (a), after refusing the reserved set and existing grants. System-memory registers are declared registers, checked against RAM, kernel-held ranges, PCI windows and claims. An action that names a register outside what was minted is refused, and if it is a required action or a listed `SET_REBOOT`, the device is not a watchdog. A WDAT SUPPRESSES THE TCO ROW, as in Linux's `lpc_ich`, and overlapping mints are refused by P02M0196a's reconciliation rule. The WDAT fixture states 1200 ms per count and OVMF's addresses. The hostile-table suite gains the missing-action and ENABLED-clear cases.

4. **ACCEPTED - behaviour across a sleep.** Plan change: a new P02M0200a item, ACROSS A SLEEP, aligned with P02M0197's current transaction. P02M0197 freezes applications only, so the watchdog service keeps petting until the watchdog's binding is suspended, which P02M0197 does last. The driver's step disarms where the device allows it. Otherwise it sets the longest timeout, pets a last time and answers an "awake by" bound, which P02M0197 uses to cap the sleep's timed wake. A device that stops counting in that state (WDAT `STOPPED`, or a chipset timer that loses power in S3) answers no bound. On resume the watchdog is resumed first and re-armed with the longer of the configured timeout and the resume bound. WDAT runs `SET_REBOOT` again. The gate case is carried by whichever of P02M0197 and P02M0200 lands second, and the case is also listed in P02M0200c. P02M0197 already names the watchdog's step in its driver contract.

5. **ACCEPTED - the liveness condition, SystemManager and the restart declaration.** Plan change: the supervision item now defines ONE LIVENESS EXCHANGE. The watchdog service asks ServiceManager `alive` on a one-operation `supervisor-liveness` interface in `liber:observability@1`, served from the standing supervise loop's wait set on a plan-delivered role channel. The service pets only after an answer within D; T, P and D are config keys with T at least 3P. Only ServiceManager's answer counts: its standing loop runs no heartbeat, and the canary's is a test-boot self-test. SystemManager is dropped from the condition: its death already reboots the machine through the kernel, and an operation that proved it responds would widen `system-power`. The ONCE-ARMED item states `restart = transparent`, membership of the plan-relaunchable set with every role plan-resolved, and the ordinary budget (3, with the 0.1 s x attempt back-off). When the budget is spent, the machine resets within one timeout. On a driver's death, release writes nothing to the timer and the rebound instance takes it over. The takeover item defines a bounded bridge (at most 120 s until the first consumer attaches) and halts a TCO that cannot reset the machine. It halts that TCO because QEMU starts the count on the first reload, and an unarmed count would record expiries that reset nothing.

6. **ACCEPTED - the orderly-shutdown notice, its gate and the BMC's publisher.** Verified for the coordinated decision: ServiceManager's orderly `!poweroff`/`!reboot` sends LogService `FLUSH` without waiting and then SIG_KILLs every service, so a service's orderly stop is not a notice. Plan change: a bounded notice step inside that ONE orderly sequence, the same sequence P02M0198d's `system-shutdown` `power-off` reaches, placed before the kills. It is `shutdown-notice.prepare(action)` in `liber:process@1` (not inside `system-power`), sent on the control channel `FLUSH` uses to every service whose manifest row declares it. It runs under one bound and never past a forced power-off deadline P02M0198d has armed. Immediate `system-power` calls, including `power-off-within`, stay notice-free. The watchdog service's answer is defined per provider. A provider that survives the reset is disarmed at power-off; at reboot it gets the boot bound and a last pet. The unfailable gate is replaced. For a platform-reset timer the case checks only that the notice was delivered, answered and acted on before the first service stopped, and the plan states why nothing else about it can fail. The case that can fail is the BMC's: an orderly reboot with the timer armed short leaves no expiry flag, and the same reboot with the notice skipped by a hook sets one. Its oracle is the BMC's own flags, because the simulator's expiry is a chassis reset and raises no `WATCHDOG` event. The BMC provider's publisher is named as P02M0201's transport driver, which matches P02M0201's current text. PARTLY DECLINED: the step is not put in SystemManager's power path, because immediate power requests stay notice-free by decision and the orderly sequence lives in ServiceManager.

7. **ACCEPTED - the two-stage timeout definition and the pre-timeout.** Plan changes: the new contract item says THE TIMEOUT IS ONE NUMBER, the time from the last pet to the reset. The `i6300esb` splits it into two equal preloads (about 0.98 ms units, 20 bits). The TCO uses a count of T / 1.2 s (2.4 s to 1227.6 s). WDAT uses the timer period times the count. The BMC uses one countdown in 100 ms units. Requests are rounded down and refused below the minimum. The pre-timeout warning is DROPPED from the contract and moved to EXCLUDES with the crash record, which is its only use. QEMU does not deliver the `i6300esb`'s stage-one interrupt, and the TCO's first expiry raises only an SMI, so the drivers leave both off. The simulated BMC could raise its pre-timeout, but with nothing to write it is not armed.

8. **ACCEPTED - the oracle and the default.** Plan changes: the verification item now uses a DURABLE ORACLE, `-action watchdog=pause` and `query-status`. An expiry leaves the run state `watchdog` until `cont`, so a case ending `running` proves that no expiry ever happened. The expiry case is bounded between T0 + T - P and T0 + T, with a stated margin, by polling. The reset case runs without `-no-reboot`. The arming item makes arming policy. Development images default to OFF, so every test boot except the watchdog gates is unarmed; the shipping default is an owner question. Every other harness QEMU run passes `-action watchdog=pause` as a tripwire. The plan now spells `-action watchdog=`, with `-watchdog-action` noted as the older form. The persistent-listener alternative was not taken, because the durable run state needs no listener.

RE-CHECK. The plan now stands on its own. It says what QEMU and OVMF actually do, and each device has an authority path the kernel can enforce: declared registers, the ICH9 derivation row, WDAT mints with memory registers declared, and the BMC through P02M0201's driver. The contract defines the timeout, takeover, liveness, restart, shutdown and sleep behaviour. Every gate case can fail and has an oracle this harness can read. It is consistent with the coordination decisions and with the current texts of P02M0191 (source (c), the reserved set, configuration writes left to this milestone), P02M0197 (the watchdog binding suspended last, resumed first, the "awake by" bound, the gate carried second), P02M0198d (one orderly sequence, `power-off-within`) and P02M0201 (the transport driver publishes, the (vendor, device) table is shared, the BMC service declares the notice). Owner questions are recorded as asked when the part starts: T, P and D (proposed 60, 15 and 5 s), the shipping arming default (proposed on), the notice bound (proposed 2 s) and the boot bound (proposed 120 s). Versions stay 1, and every interface change is additive. Only `docs/todo/P02M0200.md` and this response were written; no source, test, script or other plan was changed, and git state was not touched.


AUDITOR'S RE-AUDIT OF PLAN P02M0200 (2026-09-26T04:01:12Z):

**Rating: 8/10.** The mechanisms and the QEMU facts now hold, but three medium gaps remain: which provider the one watchdog service feeds, when ServiceManager can answer it, and what the shared sleep case contains.

The complete history was read: the original review's eight findings and the planner's response. The plan at commit 0dd5da07 was checked against the tree (`device.rs`, the PCI resolver and `ConfigAccess`, the claim and PCI syscalls, the kernel's power and recovery paths, ServiceManager's bring-up, standing loop, restart ladder and `shutdown_all`, SystemManager's power calls, DeviceManager's publication and rebind, the three IDL packages, the manifest, `qemu-run.sh`, `test-kernel.sh` and `lab.py`), against QEMU 10.0.11 and master sources, edk2-stable202502's OVMF and Linux v6.16's `wdat_wdt.c`, and against P02M0191, P02M0195, P02M0196, P02M0197, P02M0198, P02M0201 and P02M0099. Every correction the planner describes is in the text, the declared-register mechanism and the ICH9 row agree with P02M0191 and P02M0196, and both partial declines are justified: GCS shares its page with the interrupt-routing registers (+0x3140..0x3150, +0x31FF), and the notice sits in the orderly sequence the original finding offered as the alternative. What follows is what the corrections of original findings 3, 4, 5 and 8 left open, plus one new gap.

1. **Medium - The plan never says what its one watchdog service does when more than one `watchdog` provider is published, and two of its x86_64 runs boot with the TCO's provider beside the device under test.**

   The kind admits one consumer per provider, and that consumer is the watchdog service ([admitting ONE consumer](/data/yellow/libersystem/docs/todo/P02M0200.md:43), [the kind's one consumer](/data/yellow/libersystem/docs/todo/P02M0200.md:60)). Every later item speaks of one timer. No item says how many providers the service takes, which one it arms, or what it does with the others.

   q35 carries the TCO by default ([`enable_tco` is on by default](/data/yellow/libersystem/docs/todo/P02M0200.md:23)). The LPC row applies whenever ACPI decode is on and the PM base matches the FADT, and only a WDAT suppresses it ([conditions](/data/yellow/libersystem/docs/todo/P02M0200.md:159), [a WDAT suppresses the row](/data/yellow/libersystem/docs/todo/P02M0200.md:167)). So the TCO driver publishes on every x86_64 boot without a WDAT; the plan says "no TCO provider" only of the WDAT run ([device gate](/data/yellow/libersystem/docs/todo/P02M0200.md:220)). The x86_64 `i6300esb` run ([gate](/data/yellow/libersystem/docs/todo/P02M0200.md:208)) and the BMC case ([BMC case](/data/yellow/libersystem/docs/todo/P02M0200.md:221)) therefore boot with two providers. The BMC case cannot leave q35: `-device help` on this machine's aarch64 and riscv64 QEMUs lists no `ipmi-*` device. `query-status` says `watchdog` whichever timer fired, so a service that arms the TCO passes the x86_64 `i6300esb` run for the wrong device.

   The neighbours assume a rule this plan does not state. P02M0201 relies on ["P02M0200's watchdog service holds one watchdog"](/data/yellow/libersystem/docs/todo/P02M0201.md:133), and P02M0197 suspends ["the watchdog's binding"](/data/yellow/libersystem/docs/todo/P02M0197.md:83) last, in the singular. A provider the service does not hold, whose timer runs at bind, is fed only by its driver's 120 s bridge ([bridge](/data/yellow/libersystem/docs/todo/P02M0200.md:93)) and then resets the machine. That is the TCO on upstream QEMU: No-Reboot now starts clear ([later upstream sources](/data/yellow/libersystem/docs/todo/P02M0200.md:27)) and TCO1_CNT starts with the halt bit clear (`TCO1_CNT_DEFAULT = 0x0000` in [ich9_tco.c](https://raw.githubusercontent.com/qemu/qemu/v10.0.11/hw/acpi/ich9_tco.c)), which the driver counts as running and takes over ([takeover](/data/yellow/libersystem/docs/todo/P02M0200.md:170)).

   **Correct the WHO KEEPS IT FED item and the device gates.** State what the service does when several providers are published: which one it arms (a stated order, or a policy key naming it), and that it disarms every other one where the device allows it and otherwise keeps it fed. Say that the sleep step applies to every binding that publishes a watchdog. Make the x86_64 `i6300esb` run and the BMC case arm only the device under test, and check that the reported last reset is that device's.

2. **Medium - Petting needs an `alive` answer from ServiceManager's standing loop, and the plan leaves two stretches in which that loop cannot answer: bring-up, and the suspend transaction before the watchdog binding's step.**

   The service pets only after ServiceManager answers, and ServiceManager answers from its standing supervise loop ([liveness](/data/yellow/libersystem/docs/todo/P02M0200.md:60), [standing loop](/data/yellow/libersystem/docs/todo/P02M0200.md:63)). ServiceManager enters that loop only after every service is up, the test-boot drills have run and it has reported online ([step 5](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1065), ["after bring-up"](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1698)). An emulated aarch64 or riscv64 guest "takes minutes to bring its whole chain up" ([lab.py](/data/yellow/libersystem/src/harness/lab.py:3025)).

   The plan does not say when the service arms, only that it arms when the policy is on ([arming](/data/yellow/libersystem/docs/todo/P02M0200.md:76)). A service that arms when it starts gets its first answer only when bring-up ends. With the proposed 60 s timeout, the `i6300esb` gate on aarch64 and riscv64 ([all three targets](/data/yellow/libersystem/docs/todo/P02M0200.md:218), with the policy on ([gate](/data/yellow/libersystem/docs/todo/P02M0200.md:208))) would then stop in `watchdog` during boot. The takeover bridge has the same hole: the driver feeds a running timer only "until its first consumer attaches" ([bridge](/data/yellow/libersystem/docs/todo/P02M0200.md:93)), and a consumer that attaches during bring-up cannot pet until the loop answers.

   Across a sleep, the plan says the service "keeps petting until the watchdog's binding is suspended" ([sleep](/data/yellow/libersystem/docs/todo/P02M0200.md:121)). ServiceManager itself runs that transaction ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:72)): the announce, whose inhibitors may delay an idle sleep "by a bounded amount" ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:75)), the freeze, the flush and every other driver's suspend. ServiceManager runs its sequences inline today: the power verb's whole teardown runs inside the admin handler its loop calls ([loop](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1864), [power verb](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1921)). Neither plan requires it to keep answering `alive` meanwhile. If it does not, nothing pets until the binding's step, and a transaction longer than the timeout resets the machine. This continues original findings 5 and 4.

   **Correct the WHO KEEPS IT FED, TAKEOVER and ACROSS A SLEEP items.** Say that the service arms, or takes a running timer over, at its first answered `alive`, and that the driver's bridge ends at the consumer's first pet rather than at its attach. For the sleep, either require ServiceManager to keep answering `alive` while it runs the transaction, or have the service set the longest timeout when the sleep is announced - it already takes the matching resume notice ([resume notice](/data/yellow/libersystem/docs/todo/P02M0200.md:129)).

3. **Medium - This plan's sleep gate case is half of the case P02M0197 says the two plans share.**

   P02M0197's case has two halves: a suspend to idle longer than the timeout ending `running`, and "`watchdog` after a resume made to hang (a test hook)". Whichever plan lands second carries it, and the first "records that it is owed" ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:278)). This plan states the case twice, both times without the hang half ([sleep item](/data/yellow/libersystem/docs/todo/P02M0200.md:130), [verification](/data/yellow/libersystem/docs/todo/P02M0200.md:227)).

   The hang half is the only check of this plan's own resume rule, that the binding is re-armed "so a hung resume is still caught" ([resume](/data/yellow/libersystem/docs/todo/P02M0200.md:127)). The `running` half still passes when the driver disarms at suspend and never re-arms. If this plan lands second and follows its own text, that rule ships unverified. This continues original finding 4.

   **Correct the ACROSS A SLEEP item and the sleep case.** State the case as P02M0197 does, with both halves: `running` after a suspend to idle longer than the timeout, and `watchdog` after a resume a test hook makes hang.

4. **Low - The sleep step reads WDAT's `STOPPED` flag as "stops counting in the requested state", but the flag covers the firmware sleep states S1 to S5 and never suspend to idle.**

   A device "that stops counting in the requested state (WDAT's `STOPPED` flag ...) answers no bound" ([sleep step](/data/yellow/libersystem/docs/todo/P02M0200.md:125)), and the flag is read as "the timer stops in sleep" ([WDAT item](/data/yellow/libersystem/docs/todo/P02M0200.md:179)). P02M0197 uses the same words ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:122)), and suspend to idle is the state it proves on all three targets ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:263)). Linux defines the flag as "stopped by the firmware in S1-S5" ([wdat_wdt.c](https://github.com/torvalds/linux/blob/v6.16/drivers/watchdog/wdat_wdt.c#L35)) and stops the timer for suspend to idle whatever the flag says, because there "firmware is not involved" ([suspend](https://github.com/torvalds/linux/blob/v6.16/drivers/watchdog/wdat_wdt.c#L478)).

   The flag only matters for a table that cannot be disarmed, and the plan admits such tables: `SET_STOPPED_STATE` is not a required action ([required actions](/data/yellow/libersystem/docs/todo/P02M0200.md:186)). For such a table with `STOPPED` set, the step answers no bound for suspend to idle. The sleep's timed wake is then not capped, and the timer, still counting, resets the machine during the sleep. This continues original finding 3.

   **Correct the ACROSS A SLEEP and WDAT items.** Say that `STOPPED` covers S3 and deeper only, so for suspend to idle a WDAT that cannot be disarmed answers its "awake by" bound.

5. **Low - The `pause` tripwire is applied to every harness run, but test-mode runs have no QMP socket, and there it replaces a reset report the harness already makes with a silent stall.**

   The plan has "EVERY OTHER harness QEMU run" pass `-action watchdog=pause`, "so a stray expiry is a paused guest `query-status` names" ([tripwire](/data/yellow/libersystem/docs/todo/P02M0200.md:205)). On x86_64 the test branch passes `-no-reboot` ([qemu-run.sh](/data/yellow/libersystem/src/harness/qemu-run.sh:2203)) and exits ([exit](/data/yellow/libersystem/src/harness/qemu-run.sh:2234)) before the QMP socket is created ([QMP socket](/data/yellow/libersystem/src/harness/qemu-run.sh:2269)). On aarch64 and riscv64 the QMP socket exists only under `DEV_PROFILE` ([aarch64](/data/yellow/libersystem/src/harness/qemu-run.sh:2476), [riscv64](/data/yellow/libersystem/src/harness/qemu-run.sh:2825)), which test mode excludes ([exclusive](/data/yellow/libersystem/src/harness/qemu-run.sh:1510)).

   Today a stray expiry in a test run is a reset that `-no-reboot` turns into QEMU's exit, and the runner reports it at once as a `GUEST RESET` naming the test it happened in ([runner](/data/yellow/libersystem/src/harness/test-kernel.sh:770)). With `pause` it becomes a silent stall until the fifteen-minute bound ([timeout](/data/yellow/libersystem/src/harness/test-kernel.sh:88)), with no status anyone can read. This continues original finding 8.

   **Correct the oracle item.** Apply `pause` to the runs that have a QMP socket - the `lab` and scenario boots - and leave test-mode runs on the default, whose reset the runner already reports.

Validation: inspection only. The plan, the audit history and the neighbouring plans were read at commit 0dd5da07, with a clean tree. Source files were read and searched. QEMU v10.0.0, v10.0.11 and master, edk2-stable202502 and Linux v6.16 sources were fetched read-only. The local QEMU 10.0.11 binaries were queried only with `-device help`, `-device i6300esb,help`, `-device ICH9-LPC,help`, `-machine q35,help` and `-help`. No plan, source or audit file was modified, no git state was changed, and nothing was built or booted.
