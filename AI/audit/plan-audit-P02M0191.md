AUDITOR'S REVIEW OF PLAN P02M0191 (2026-09-25T22:49:59Z):

**Rating: 3/10.** Enforcing port access through the TSS I/O bitmap is sound and feasible. But the plan gives the 16550's interrupt no owner and gives a port range no device to be claimed with, and its console handoff would take the kernel log, the serial shell and every harness oracle off the wire.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0191.md), the blocker it answers in [P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:257) and that file's [16550 item](/data/yellow/libersystem/docs/todo/P02M0099.md:2947). Also reviewed the plan's consumers [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:27), [P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:39) and [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:21). On the code side: the x86_64 GDT/TSS, serial, interrupt, claim and scheduler code, the ports the kernel drives, and the harness's use of the serial log, all at commit `07371c44af82a11c1d275b0cbd832899f712e62a`. Line references into P02M0099, P02M0196, P02M0200 and P02M0201 are to the working tree, where those plans carry uncommitted edits or are not yet committed. The findings concern decisions the implementation needs, not the expected absence of the mechanism.

1. **High - The console handoff would take the kernel log, the serial shell and the harness's oracle off COM1, and the plan does not say where any of them go.**

   The [handoff item](/data/yellow/libersystem/docs/todo/P02M0191.md:42) keeps the kernel's COM1 console "until the userspace driver claims the port, then stops writing it". COM1 carries far more than early boot:
   - every kernel line, through the asynchronous ring the timer and idle loop drain ([serial.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:1));
   - ConsoleService's mirror of the foreground terminal, written through `SYS_DEBUG_WRITE` ([console_service.rs](/data/yellow/libersystem/src/user/services/core/src/console_service.rs:916), [syscall/mod.rs](/data/yellow/libersystem/src/kernel/syscall/mod.rs:208), [`_print_bytes`](/data/yellow/libersystem/src/kernel/main.rs:153));
   - the shell's serial input, which the kernel reads on IRQ 4 and from the idle hook and feeds to the console ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:458), [main.rs](/data/yellow/libersystem/src/kernel/main.rs:509), [main.rs](/data/yellow/libersystem/src/kernel/main.rs:1373)).

   The harness is built on this one wire. `lab` "owns the serial console" and runs `lab sh` over it ([lab.py](/data/yellow/libersystem/src/harness/lab.py:6)). Scenario oracles read "terminal output from the broker's serial log" ([scenario.py](/data/yellow/libersystem/src/harness/scenario.py:436)). The test kernel's log is the guest's only serial port ([test-kernel.sh](/data/yellow/libersystem/src/harness/test-kernel.sh:693), [qemu-run.sh](/data/yellow/libersystem/src/harness/qemu-run.sh:1911)), so the plan's `isa-serial` test would write into its own log unless the harness adds a second port.

   After the handoff as written, three things break:
   - Kernel lines have no destination.
   - The receive path, which the plan does not mention, either keeps taking the driver's bytes or goes dead.
   - Nothing covers the window while a restartable driver is dead.

   P02M0099 asked for "a stated panic-only fallback and reacquisition rule" ([P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:1141)). The plan states only the panic half.

   **Correct the handoff and verification items** to state:
   - where kernel output goes after the handoff, for example a bounded kernel ring that the driver drains to the wire;
   - how the receive path, the IRQ 4 handler, and ConsoleService's mirror and serial input move to the driver's console-bytes publication;
   - that the kernel takes the port back while no driver holds it, and how the handoff is made atomic.

   Verify the handoff itself, not only the mechanism:
   - kernel lines emitted after the handoff, and across a driver kill, still reach the serial log;
   - `lab sh` still works;
   - a panic after the handoff is printed.

   If COM1 is instead to stay the kernel's console, say so and scope the userspace driver to other UARTs.

2. **High - Legacy fixed-IRQ routing, which P02M0099 lists as a separate prerequisite of the 16550, has no owner in this plan or anywhere else.**

   P02M0099's blocker row says the legacy slice "also needs legacy fixed-IRQ routing and an atomic handoff" ([P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:257)). The 16550 item needs "RX/TX interrupt flow" ([P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:2953)). This plan has no interrupt item, yet it says the 16550 item "then binds through this mechanism and is closed there" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:45)).

   What exists does not serve a claimed UART:
   - The only wired path, [`sys_interrupt_bind`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1624), is gated by the DeviceManager privilege and derives nothing from a claim, so a release cannot revoke it. MSI-X, by contrast, is claim-scoped ([`sys_device_msix_acquire`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1651)).
   - The kernel routes ISA lines only for its own handlers ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:459)).
   - The legacy dispatch runs the kernel's handler first and then wakes any bound driver ([interrupts/mod.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:380)). While the kernel's COM1 receive handler stays registered, it drains the FIFO before the driver sees a byte.

   **Correct the mechanism items** to add a claim-scoped wired interrupt for the ranges this plan mints:
   - It resolves the ISA line through the MADT's source overrides, as the SCI path already does, and routes it through the I/O APIC.
   - It is registered as a derived object of the claim, so a release revokes it.
   - At the handoff, the kernel's own handler is unregistered.

   Alternatively, name P02M0196a's platform-device interrupts as the owner and say that the 16550 waits on both. As written, the 16550 cannot close on P02M0191 alone.

3. **High - A port range is to travel "with the device claim", but no non-PCI device can be claimed, the interim COM1..COM4 rule describes nothing, and the kernel cannot check a `_CRS` it does not evaluate.**

   The plan mints ranges only "for ranges firmware or the platform describes, delegated through DeviceManager with the device claim, and revoked with it" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:18)). Claims exist only for rows of the kernel's device table, which the kernel fills from the PCI bus ([device.rs](/data/yellow/libersystem/src/kernel/device.rs:1)). A `ClaimKey` is an index into that table ([abi](/data/yellow/libersystem/src/abi/src/lib.rs:1147)), and a claim hands out one memory BAR ([syscall/mod.rs](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1285)). COM1 has no row. The plan never says it depends on [P02M0196a](/data/yellow/libersystem/docs/todo/P02M0196.md:29) for one. Adding a legacy row itself would be the discriminated identity change that P02M0099 calls an architectural prerequisite ([P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:1073)).

   Until `_CRS` can be read, the plan mints "the fixed legacy ranges of devices the platform guarantees - COM1..COM4" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:26)). No platform guarantees COM2..COM4, and a legacy-free machine has no COM1. The rule contradicts the plan's own exclusion of "port ranges for devices nobody described" ([EXCLUDES](/data/yellow/libersystem/docs/todo/P02M0191.md:55)). It also contradicts P02M0200, which does not support a device that can only be found by probing ports ([P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:61)).

   On the harness's machine the UART is described only in AML. QEMU's q35 builds no SPCR table ([acpi-build.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/i386/acpi-build.c)) and describes each `isa-serial` as a `PNP0501` device with an I/O `_CRS` ([serial-isa.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/char/serial-isa.c)). Reading that is P02M0196's step 2, whose default is a userspace ACPI service ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:46)). The kernel would then mint whatever that service reports, so "minted only for described ranges" is not a boundary the kernel enforces.

   **Correct the object item and the plan's head** to state:
   - the dependency on P02M0196a for the claimable device, or the legacy row this plan will own and why that row is not the identity P02M0099 reserves;
   - which description each range comes from: static tables the kernel parses itself (SPCR, DBG2, FADT, WDAT), or `_CRS` reported by the ACPI service;
   - who may ask the kernel to mint a range, and that the kernel itself enforces only the reserved set of finding 5.

   Limit the interim rule to COM1, the one UART the kernel already assumes, or drop it.

4. **Medium - The minting sources do not cover the ranges needed by the milestones that route through this plan, and there is no rule for a sub-range of a block the kernel keeps.**

   The plan mints only from `_CRS` and the COM ports ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:25)). That leaves out what its named consumers need:
   - **PCI I/O BARs.** P02M0201's SSIF runs over the ICH9 SMBus controller ([P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:24)), whose host interface QEMU registers only as an I/O BAR ([smbus_ich9.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/i2c/smbus_ich9.c)). `pci-ipmi-kcs` exposes an I/O BAR 0 ([pci_ipmi_kcs.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/ipmi/pci_ipmi_kcs.c)). The kernel refuses I/O BARs outright ([pci/mod.rs](/data/yellow/libersystem/src/kernel/arch/common/pci/mod.rs:851)), so P02M0201's "PCI forms through their claim" ([P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:22)) get nothing.
   - **The TCO timer.** P02M0200 needs "the TCO's own sub-range, with the kernel keeping PM1 and GPE" ([P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:39)). QEMU puts the TCO registers at offset 0x60 of the same 128-byte ICH9 PM block, whose base is set in the LPC bridge's configuration space ([ich9.h](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/include/hw/southbridge/ich9.h), [ich9_tco.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/acpi/ich9_tco.c)). The kernel drives the PM1 event and control registers of that block ([sci.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:150), [poweroff](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:179)).
   - **AML SystemIO operation regions.** P02M0196b reaches these "through P02M0191" ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:45)). An `OperationRegion` declares its own range rather than a `_CRS`, and that range may include ports the kernel keeps, which this plan says can never be minted.

   **Correct the object item** to add these sources:
   - a PCI function's I/O BARs, minted under its PCI claim;
   - chipset sub-ranges the kernel derives from the bridge's configuration, with the kernel-held registers carved out;
   - the ACPI service's operation regions, with a stated policy for kernel-held ports.

   Alternatively, name the milestone that owns each source, so the consumer milestones do not discover the gap during implementation.

5. **Medium - The kernel-reserved list names only a few of the port blocks the kernel drives, and it has no category for ports the kernel uses only at panic, reset or power-off.**

   The plan protects "the PIC, the PIT, the PCI configuration ports" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:27)). The kernel also drives:
   - port 0x61 for PIT calibration ([pit.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/pit.rs:23));
   - the CMOS index and data pair, whose index port also holds the NMI mask ([rtc](/data/yellow/libersystem/src/kernel/arch/x86_64/rtc/mod.rs:56));
   - fw_cfg ([fwcfg.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/fwcfg.rs:17));
   - the FADT's PM1 event and control blocks, and the SMI command port ([sci.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:150), [sci.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:183));
   - 0xCF9 and the 8042 at reset, and the PM1a control ports at power-off ([mod.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:155));
   - 0xF4 for the test exit ([mod.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:271)).

   Firmware describes several of these as ordinary devices. QEMU's DSDT gives the RTC (`PNP0B00`, 0x70) and the 8042 (`PNP0303`, 0x60 and 0x64) I/O `_CRS` entries ([mc146818rtc.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/rtc/mc146818rtc.c), [pckbd.c](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/input/pckbd.c)). Once `_CRS` is read, these ports count as "described" and can be minted. A driver's index and data writes would then interleave with the kernel's.

   The plan also keeps a polled panic path on COM1 ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:43)). Under the plan's own overlap rule, COM1 could not be minted at all.

   **Correct the object item** to define the reserved set as the ports the kernel drives, including the FADT-derived blocks computed at boot, and add a test that minting any of them is refused. Add a second category for ports the kernel touches only on panic, reset or power-off (COM1, 0x64, 0xCF9). These may be minted, and in those paths the kernel overrides the holder.

6. **Medium - Other cores see a change to a process's bitmap only when they next switch, so a release leaves a running driver with port access, and a new grant can fault a sibling thread.**

   The TSS is per core ([gdt.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/gdt.rs:8)), and the plan installs a process's bitmap on each switch to one of its threads ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:29)). Both parts are feasible to extend: the switch point exists ([sched](/data/yellow/libersystem/src/kernel/sched/mod.rs:1455)), and the TSS currently has no bitmap and a limit that excludes one ([gdt.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/gdt.rs:111)).

   The plan covers only a driver that is killed ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:37)). A claim is also released while its holder is still running, and the claim machinery is built for that case. A forced release must reach objects whose holder ["is still running by definition"](/data/yellow/libersystem/src/kernel/device.rs:1399), and a teardown that was not confirmed ends [`Quarantined` rather than `Free`](/data/yellow/libersystem/src/kernel/device.rs:798). Under the plan's design, a thread of the released driver on another core keeps the old bitmap until its next switch. The next claimant may already be driving the device by then, which is the overlap in ownership that the `DeviceMemory` revocation exists to prevent. The reverse also happens: a sibling thread already running on another core does not see a range mapped by one thread, and its first access to it ends the whole process with a #GP.

   **Correct the enforcement and DeviceManager items:**
   - Push every change to a process's bitmap to the cores running its threads.
   - Do not let a release settle until each of those cores has confirmed.
   - Register the `PortRange` as a derived object of the claim, so an unconfirmed teardown ends `Quarantined`, as MMIO does.
   - Add a kernel test that releases a range while a thread on another core is looping on it.

7. **Low - The object carries separate read and write rights that the I/O permission bitmap cannot enforce.**

   A `PortRange` has "rights to read and write" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:24)), enforced "by the hardware, not by a syscall per access" ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:29)). The TSS bitmap has one bit per port that governs `in` and `out` alike, so a read-only range mapped through it is also writable.

   **Correct the object item** to drop directional rights, or to say that a range holding only one of them is served by the syscall path. One sentence suffices.

Validation was plan and source inspection, plus the QEMU v10.0.0 sources cited above and `qemu-system-x86_64 -device help` on this machine (QEMU 10.0.11). No build, test or guest was run, and no repository file was modified.

PLANNER'S RESPONSE ON P02M0191 (2026-09-26T02:51:02Z):

Verified every finding against the tree and the related plans: `arch/x86_64/serial.rs` (the 16 KiB ring, `drain_tx` from the timer and idle loops, `flush_sync`, the full-ring drain in `write_byte`), `gdt.rs` (per-core TSS, `iomap_base` and limit both at `size_of::<Tss>()`, so no bitmap), `interrupts/mod.rs` (`dispatch` runs the kernel handler before waking a bound driver; `register` has no unregister), `ioapic.rs`, `sci.rs` (PM1 event/control writes, the SMI command port), `mod.rs` (0xCF9 and 0x64 at reset; fixed 0x604/0xB004/0x600 at power-off; 0xF4 test exit), `pit.rs` (0x61), `rtc/mod.rs` (0x70/0x71 with the NMI-mask bit), `fwcfg.rs` (0x510/0x511), `apic/mod.rs` (PIC ports), `main.rs` (IRQ 4 routed at the boot tail, the idle-hook and shell-loop polls, `serial_rx_interrupt`, `_print_bytes`), `syscall/mod.rs` (`sys_debug_write`, `SYS_CONSOLE_FEED`, `sys_interrupt_bind` behind the DeviceManager privilege, claim-scoped `sys_device_msix_acquire`, one `DeviceMemory` per claim), `device.rs` (PCI-only table, `register_derived`, `revoke_effects_of`, `settled_mmio`, `finish_release` to `Quarantined`), `mem/tlb.rs` (the confirmed shootdown round), `sched/mod.rs` (the switch point), `arch/common/pci/mod.rs` (I/O BARs skipped), `console_service.rs` (`drain_serial` over `SYS_DEBUG_WRITE`), `panic.rs`, `lab.py`, `scenario.py`, `test-kernel.sh`/`qemu-run.sh` (the test log is COM1), and the working-tree plans P02M0099, P02M0196, P02M0198, P02M0200 and P02M0201. `qemu-system-x86_64 -device isa-serial,help` confirms `iobase`/`irq`/`chardev` for a second port. The coordination decisions of this round (P02M0196a owns the platform-device claim and claim-scoped wired interrupts; this milestone owns the object, the enforcement, the mint sources, the reserved set and the COM1 handoff) are applied throughout.

1. **ACCEPTED - The console handoff would take the kernel log, the serial shell and the harness oracle off COM1.** Correct on every point: COM1 carries all kernel lines, ConsoleService's mirror and the serial shell's input, and the old item said only "stops writing it". The handoff is now its own part, P02M0191c, fully designed: a KERNEL/DRIVER ownership state under the lock that already serializes the ring, checked by every kernel register access (drains, IRQ 4 handler, both polls, the full-ring fallback, which now drops and counts); the claim of COM1 as the atomic handoff (flip, unregister the kernel IRQ 4 handler, then mint); kernel output kept in the ring and drained by the driver through a `ConsoleTap` derived from the claim (ordered, drop-counted, signalled, emptied before each console-stream write so kernel lines keep their order); receive, the mirror and the serial shell moved to the driver's `ConsoleBytes` publication under a console provider name, with ConsoleService attaching through the catalogue and falling back to `SYS_DEBUG_WRITE` and the kernel channel when the provider goes; reacquisition at release (re-initialise, re-register IRQ 4, drain what queued, also after a `Quarantined` release, which then blocks re-claiming); and `flush_sync` turned into a terminal-path writer that ignores the owner, reprograms the line and also serves the kernel-fatal halts that today never drain. Verification: kernel tests use a second `isa-serial` in loopback so nothing writes into the suite's log, and a `serial-handoff` gate proves the handoff line, `lab sh`, lines queued across a driver kill, `lab sh` through the kernel after the binding is disabled, and a post-handoff panic (asked for through a development-build-only request). The alternative of keeping COM1 kernel-only was not taken, because P02M0099's 16550 item requires the handoff.

2. **ACCEPTED - Legacy fixed-IRQ routing has no owner.** Verified: `sys_interrupt_bind` is privilege-gated and not claim-derived, the kernel routes ISA lines only for its own handlers, and `dispatch` runs the kernel's handler first, so a registered COM1 handler would drain the FIFO before a driver saw it. Took the finding's own alternative rather than adding an interrupt item here: claim-scoped wired interrupts (MADT overrides, I/O APIC routing, revocation at release) belong to P02M0196a on all three architectures, and the plan's head now says the handoff part cannot start before them and that the 16550's legacy slice waits on both milestones. This milestone keeps the part only it can do: unregistering the kernel's IRQ 4 handler at the handoff (before the line is minted) and re-registering and re-routing it at reacquisition. Declined: a separate x86-only wired-interrupt mechanism here, which would duplicate P02M0196a's.

3. **ACCEPTED - No non-PCI device can be claimed, the interim COM1..COM4 rule describes nothing, and the kernel cannot check a `_CRS` it does not evaluate.** Verified in `device.rs` and the ABI. The plan's head now states the dependency on P02M0196a's platform row and claim (COM1 is the kernel-declared `kernel:com1` row, kernel-held until this handoff), so no legacy row is invented here. P02M0191a names each source and who may ask: DeviceManager through a claim handle, by index on the claimed row and never by address; the ACPI service through its `FirmwareInterpreter`-gated call, the only call that takes a base and a length. It states exactly what the kernel enforces for a reported `_CRS` (the reserved set, exclusivity, no overlap with a recorded PCI I/O BAR or bridge window) and that the attribution itself is trusted to the ACPI service. The interim COM1..COM4 rule is dropped.

4. **ACCEPTED - The sources do not cover PCI I/O BARs, chipset sub-ranges or AML SystemIO regions.** Verified that `bar_address`/`bar_size` skip I/O BARs and that PM1 sits in the block QEMU's TCO shares. Added source (b), every PCI I/O BAR recorded by the boot scan and minted with the PCI claim, I/O decode set at claim and cleared at release; and source (c), a derivation-row table (register, mask, block length, sub-ranges, row conditions) whose sub-ranges are checked against the reserved set, which never toggles the function's decode and ships with no rows; P02M0200 writes the ICH9 TCO row. AML SystemIO regions are minted to the ACPI service under P02M0196b's policy with the same checks, a region over a reserved port never becoming a `PortRange`. Declined: a policy for AML reaching kernel-held ports inside this plan - that policy is P02M0196b's.

5. **ACCEPTED - The reserved list is incomplete and has no terminal-path category.** Every cited access was confirmed. The reserved set now lists the fixed ports (PIC, PIT, 0x61, CMOS 0x70/0x71, `0xCF8` and `0xCFA..0xCFF`, the whole fw_cfg interface `0x510..0x51B`, whose DMA registers could otherwise write any memory, and 0xF4..0xF7 in the test build), the FADT-derived blocks computed at boot, and a run-time part: registers the kernel takes later (P02M0198's processor tables) are refused if already reserved or granted and otherwise join the set. A test tries each. A second category covers ports the kernel writes only on panic, reset or power-off (0x64, 0xCF9, the FADT reset register, the fixed power-off fallbacks, COM1). These may be minted, and the kernel writes them in those paths without asking. `0xCF9` stays mintable alone because the processor checks every byte of an access, so the doubleword `CONFIG_ADDRESS` stays refused.

6. **ACCEPTED - Other cores see a bitmap change only at their next switch.** Verified: the TSS is per core, `release_claim` must reach a holder that is still running, and an unconfirmed teardown ends `Quarantined`. Revocation is now pushed: bits cleared, generation bumped, and one round of the existing TLB shootdown's request and acknowledgement, whose service step also reloads the bitmap. The revocation answers whether the round confirmed. The `PortRange` is a derived object of the claim, counted like an MMIO window while its last reference drops, so an unconfirmed round ends the claim `Quarantined`. Tests cover a thread looping on another core (`Free`) and a withheld acknowledgement (`Quarantined`). Partly declined: grants are not pushed. A grant cannot leak authority, so a sibling's first access instead takes a `#GP` whose handler reloads a stale bitmap and retries; only a `#GP` against a current bitmap ends the process. This removes the spurious kill the finding describes without an IPI per map, and a test covers it. The switch design itself is decided (the TSS bitmap, lazily copied up to the process's highest byte, offset past the limit for everything else, cost measured in `docs/PERF.md`), and the syscall fallback is dropped.

7. **ACCEPTED - Directional rights the bitmap cannot enforce.** `PortRange` now has no read or write rights - one bit per port governs `in` and `out` alike - and carries only the ordinary transfer and map rights; EXCLUDES says so.

Re-check: the plan is now a complete, self-standing implementation plan in four parts: the object and its sources (a, b, c, plus the ACPI service's regions); the reserved set in three parts and the terminal-path category; enforcement with confirmed revocation and pulled grants; and the COM1 handoff and verification. The order is explicit: parts a and b first, source (a) wired when P02M0196a's rows exist, and the handoff part landing with P02M0099's 16550 driver. It is consistent with P02M0196a (the claim mints by kind and index, `kernel:com1` is kernel-held until this handoff, claim-scoped wired interrupts are P02M0196a's), P02M0196b (the SystemIO policy and `FirmwareInterpreter`), P02M0198 (registers taken at run time), P02M0200 (the TCO derivation row, decode untouched) and P02M0201 (the PCI I/O BAR source). It is feasible with mechanisms the tree already has: `register_derived`, the shootdown round, the device-memory settle count, provider publication and the catalogue consumer path. Nothing is versioned, and the owner's decision is kept. No source, test or script was changed; only the plan was edited and this response appended.


AUDITOR'S RE-AUDIT OF PLAN P02M0191 (2026-09-26T04:01:12Z):

**Rating: 7/10.** The bitmap enforcement, the mint sources and the split with P02M0196 are sound, but the confirmed revocation, the reserved set's membership, the panic path and the handoff gate each still leave a gap that would be built wrongly or could not be caught.

The complete history was read: the original review with its seven findings, and the planner's response. I compared the current plan with the audited version (`07371c44`) and the planner's edits in `0dd5da07`, including the 16550 and port-I/O parts of the P02M0099 diff. In the tree I read the x86_64 TSS, serial driver, interrupt dispatch, fault handlers, syscall and ring-3 entry, the TLB shootdown, the spin lock, the scheduler's switch and current-thread lookup, the claim, derived-object and release code, the PCI BAR helpers, the panic handler, the driver resource frames, ConsoleService, the development agent, and the harness (`lab.py`, `qemu-run.sh`, `test-kernel.sh`, `check.sh`). I cross-checked P02M0196, P02M0200, P02M0201, P02M0198, P02M0197 and P02M0099, and read the cited QEMU 10.0 and coreboot sources. The TSS layout, the trailing byte of ones, the IOPL argument and the per-byte check behind `0xCF9` are correct. The corrections of findings 2, 3, 4 and 7 hold, and so do the two declined parts (no second wired-interrupt mechanism here; grants pulled through the `#GP` retry). The corrections of findings 1, 5 and 6 are incomplete, as described below.

1. **Medium - The confirmed revocation runs inside a step that must stay lock-free, and the plan does not say how that step finds the running process or reads its bitmap.**

   The plan changes a process's bitmap ["under the process's lock"](/data/yellow/libersystem/docs/todo/P02M0191.md:128), then runs one round of the TLB shootdown, ["whose per-core service step now also reloads the bitmap"](/data/yellow/libersystem/docs/todo/P02M0191.md:130). That step is [`service_pending`](/data/yellow/libersystem/src/kernel/mem/tlb.rs:247). It runs from the [wake IPI](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:471), the [timer tick](/data/yellow/libersystem/src/kernel/sched/mod.rs:1310), both idle loops ([one](/data/yellow/libersystem/src/kernel/sched/mod.rs:1207), [two](/data/yellow/libersystem/src/kernel/sched/mod.rs:1243)), the shootdown's own waits ([one](/data/yellow/libersystem/src/kernel/mem/tlb.rs:118), [two](/data/yellow/libersystem/src/kernel/mem/tlb.rs:158)), and inside every contended [`SpinLock::lock`](/data/yellow/libersystem/src/kernel/sync.rs:70), with interrupts masked. That is why `sync.rs` records that it ["is lock-free ... so it cannot recurse into another acquire"](/data/yellow/libersystem/src/kernel/sync.rs:66).

   The obvious reload breaks that rule. The kernel names the running thread only through [`sched::current_thread`](/data/yellow/libersystem/src/kernel/sched/mod.rs:614), which takes the per-core scheduler lock, and the bitmap is guarded by the process's lock. A reload that takes either lock can deadlock a core that is spinning for that lock or already holds it. The plan's sentence also runs the round in the same sequence as the locked change. If the round starts before the process's lock is dropped, every remote reload waits on that lock until the [bounded wait answers false](/data/yellow/libersystem/src/kernel/mem/tlb.rs:161). Every revocation of a range whose process runs on another core then ends `Quarantined`, while the plan's own [kernel test expects `Free`](/data/yellow/libersystem/docs/todo/P02M0191.md:223). This continues original finding 6.

   **Correct the revocation item.** State that the round starts only after the process's lock is released. State that the switch records, in per-CPU state, which process's bytes the core loaded and at which generation. State that the switch copy and the service step read the process's bytes without a lock: the generation is read before and after the copy, and the copy is retried when it moved. `service_pending` then stays lock-free, as `sync.rs` requires.

2. **Medium - COM1 is filed only as a terminal-path port, although the kernel drives it whenever it owns it, so no check refuses a mint of COM1 while the kernel is using it.**

   The reserved set is ["every port the kernel drives, refused at every mint"](/data/yellow/libersystem/docs/todo/P02M0191.md:85). The terminal-path category is ["ports the kernel writes only on panic, reset or power-off"](/data/yellow/libersystem/docs/todo/P02M0191.md:100). COM1 is placed only in [the second](/data/yellow/libersystem/docs/todo/P02M0191.md:107), although the plan says ["the kernel declares the port it already drives"](/data/yellow/libersystem/docs/todo/P02M0191.md:84). From boot until the handoff, and again after each reacquisition, the kernel drives COM1 on every tick and idle pass ([`drain_tx`](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:119)), on [IRQ 4](/data/yellow/libersystem/src/kernel/main.rs:458), and from the [idle-hook](/data/yellow/libersystem/src/kernel/main.rs:509) and [shell-loop](/data/yellow/libersystem/src/kernel/main.rs:573) polls.

   The claim path is safe, because `kernel:com1` is kernel-held. The ACPI service's path is not. Its SystemIO mint gets only ["the same reserved-set and exclusivity checks"](/data/yellow/libersystem/docs/todo/P02M0191.md:76), and P02M0196b mints ["over its terminal-path category"](/data/yellow/libersystem/docs/todo/P02M0196.md:149). Firmware can declare such a region. Coreboot's optional [`debug.asl`](https://github.com/coreboot/coreboot/blob/main/src/arch/x86/acpi/debug.asl) has `OperationRegion(CREG, SystemIO, 0x3F8, 8)`, and its `DINI` method sets the divisor for 115200 baud and clears the interrupt enables. The kernel runs the port at [38400 baud](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:70) with the receive interrupt on. So the ACPI service would be granted COM1 while the kernel drives it, and by the [EXCLUSIVE rule](/data/yellow/libersystem/docs/todo/P02M0191.md:82) that live grant then refuses the handoff's own `PortRange` mint. This continues original finding 5, whose suggested category listed COM1 without separating the kernel's ownership from a driver's.

   **Correct the reserved-set and terminal-path items.** COM1 is in the reserved set while the kernel owns it, and in the terminal-path category only while a claim holds it. The handoff's flip takes it out of the set under the lock the mint checks, before the mint. Reacquisition puts it back. This is the run-time part's own join-and-leave rule.

3. **Medium - P02M0191 and P02M0196 disagree on what the reserved set holds: P02M0196b adds firmware reservations that P02M0191's definition leaves out, and those ranges contain ports the plans need to mint.**

   P02M0191 owns the [reserved set](/data/yellow/libersystem/docs/todo/P02M0191.md:85) and defines it as the ports the kernel drives: the fixed ports, the FADT blocks, and registers the kernel installs at run time. P02M0196b says ["`PNP0C01`/`PNP0C02` are RESERVATIONS whose ranges join the kernel's reserved list"](/data/yellow/libersystem/docs/todo/P02M0196.md:201), while its own SystemIO rule names only the [set's three parts](/data/yellow/libersystem/docs/todo/P02M0196.md:148). Real firmware reserves ports that no kernel code drives. Coreboot's ICH9 [`LDRC` device](https://github.com/coreboot/coreboot/blob/main/src/southbridge/intel/i82801ix/acpi/lpc.asl) (`PNP0C02`) lists port 0x80, the GPIO block and the whole 0x80-byte ACPI PM block. That PM block holds the TCO sub-range that [P02M0200 mints through source (c)](/data/yellow/libersystem/docs/todo/P02M0200.md:156), and every range is [checked against the set again at mint](/data/yellow/libersystem/docs/todo/P02M0191.md:50). If these ranges join the set as P02M0196b says, port 0x80 and the GPIO block can no longer be minted, not even for the ACPI service's own SystemIO regions. The TCO sub-range is refused as well if the PM-block range joins.

   **Correct the reserved-set item, and P02M0196b's line with it.** State that firmware reservations never join the reserved set. Have P02M0196b say what a `PNP0C01`/`PNP0C02` range does instead, for example that it keeps another device's `_CRS` row off that range.

4. **Medium - The terminal path loses the panic text whenever a driver holds COM1 and has stopped draining a full ring, and it repairs only part of the UART state a driver can leave behind.**

   While a driver holds COM1, a kernel line that finds the 16 KiB ring full is [dropped and counted](/data/yellow/libersystem/docs/todo/P02M0191.md:165), and the [driver's tap is the only drain](/data/yellow/libersystem/docs/todo/P02M0191.md:171). The panic handler [prints its text first](/data/yellow/libersystem/src/kernel/panic.rs:7) and [flushes afterwards](/data/yellow/libersystem/src/kernel/panic.rs:17). The plan keeps that order: the writer ["writes out the whole ring, whose tail is the panic text"](/data/yellow/libersystem/docs/todo/P02M0191.md:203). If the driver is hung, or slower than the kernel's output, the ring is full. `*** KERNEL PANIC ***` and the message are then dropped at enqueue, and the writer transmits 16 KiB of older lines instead. That is the case the terminal path exists for. ["It cannot withhold the text"](/data/yellow/libersystem/docs/todo/P02M0191.md:206) is true of waiting, not of a full ring. Today's full-ring path makes room by [pushing to the UART](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:164), which the new ownership rule forbids.

   The writer also repairs only ["DLAB ..., another divisor or its own interrupt enables"](/data/yellow/libersystem/docs/todo/P02M0191.md:202). A driver can just as well leave the FIFO off or loopback (MCR bit 4) on. In loopback, [QEMU's UART](https://gitlab.com/qemu-project/qemu/-/blob/v10.0.0/hw/char/serial.c#L255) hands every transmitted byte to its own receiver, and nothing reaches the log. The boot sequence that reacquisition reuses writes [FCR and MCR](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:88), but the terminal writer is not told to. Its one kernel test reads the [marker back](/data/yellow/libersystem/docs/todo/P02M0191.md:238), which on a null chardev only loopback allows, so the test requires the writer to leave MCR alone. This continues original finding 1.

   **Correct the panic item.** The panic handler and the fatal halts enter the terminal writer before they print. It takes the wire (the lock with its bound), re-runs the whole boot sequence of [`serial::init`](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:81), and writes out or discards the ring's backlog with its count. From then on every kernel line goes to the wire synchronously, as in early boot. The writer's kernel test observes the marker without loopback, for example through the test build's record of register writes, and runs once with the probe's ring full.

5. **Medium - The handoff gate cannot fail for either property the handoff exists for: that the kernel lets go of COM1, and that a panic overrides a driver that holds it.**

   The [gate's first two checks](/data/yellow/libersystem/docs/todo/P02M0191.md:241) pass whether or not the kernel still touches COM1. If the kernel still drained its ring, the handoff line would reach the log anyway. If it kept its IRQ 4 handler, [`dispatch` runs that handler before it wakes the driver](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:386), and [the handler empties the FIFO](/data/yellow/libersystem/src/kernel/main.rs:1373). ConsoleService still reads the kernel's console channel, ["which it never stopped reading"](/data/yellow/libersystem/docs/todo/P02M0191.md:187), so `lab sh` still answers. The kernel tests prove ownership only on a [parameterised second instance](/data/yellow/libersystem/docs/todo/P02M0191.md:233). The COM1 call sites, the [IRQ 4 registration](/data/yellow/libersystem/src/kernel/main.rs:458) and the two polls, are covered by nothing.

   The panic is asked for last, after the binding was disabled and `lab sh` answered ["through the kernel's own path"](/data/yellow/libersystem/docs/todo/P02M0191.md:246). At that moment the kernel owns COM1 again, so this step proves only today's panic path. The planner's response says the gate proves "a post-handoff panic", which is what original finding 1 asked for, so that correction is incomplete.

   **Correct the gate item.** In the development build, the reacquisition line states how many kernel accesses to COM1's registers happened while the driver held the port, and the gate requires zero. After the kernel-path step, re-enable the binding and wait for the next handoff line. Then ask for the panic while a development hook holds the driver's tap reads (its control path still answering) until the ring is past its bound, so that finding 4's case is the one proved.

6. **Low - P02M0099 still says the legacy-port 16550 needs no device identity, which contradicts the new head of this plan.**

   This plan now says COM1 has no row and no claim without [P02M0196a's platform device](/data/yellow/libersystem/docs/todo/P02M0191.md:28), and that the legacy slice waits on both milestones. P02M0196 names itself [the owner of the identity P02M0099 reserves](/data/yellow/libersystem/docs/todo/P02M0196.md:30). P02M0099 still says ["The x86 legacy-port `16550` is NOT one of them - it needs no firmware identity"](/data/yellow/libersystem/docs/todo/P02M0099.md:1123), and that its bullet records the three things it needs. The planner's edit to [that bullet](/data/yellow/libersystem/docs/todo/P02M0099.md:2948) in `0dd5da07` added P02M0196a's interrupts but not its platform row and claim.

   **Correct P02M0099.** Name P02M0196a's `kernel:com1` row and claim in the 16550 bullet, and remove the legacy-port exception from the identity section.

Validation: plan, audit-history and source inspection at `0dd5da07`, with `git show` and `git diff` for the audited plan and the planner's edits to P02M0191 and P02M0099. The cited QEMU v10.0.0 and coreboot files were read from their upstream repositories, and `qemu-system-x86_64 -device isa-serial,help` (QEMU 10.0.11) was run as a property query that starts no machine. No plan, source or audit file was modified, and nothing was built, tested or booted.


PLANNER'S RESPONSE ON P02M0191 (2026-09-26T05:34:16Z):

Every finding of the re-audit was verified against the tree at `eca3cbe7` and against the working-tree plans. In the tree:
- `mem/tlb.rs`: `service_pending` and all of its callers - the wake IPI, the tick in `on_timer_preempt`, both idle loops and the shootdown's own waits.
- `sync.rs`: the contended wait calls `service_pending` every 1024 spins with interrupts masked, and the file records that the step must stay lock-free.
- `sched/mod.rs`: `current_thread` takes `cpu_sched(..).inner.lock()`, and the switch publishes per-CPU state with interrupts masked after dropping that lock.
- `arch/x86_64/percpu.rs`: there is no current-thread field.
- `object/mod.rs`: object ids come from a monotonic counter.
- `device.rs`: `revoke_derived` works on each row with `DERIVED` released, and the release calls it after dropping `CLAIMS`.
- `arch/x86_64/serial.rs`: the 16 KiB ring; `init`'s sequence of IER, LCR, divisor, FCR and MCR at 38400 baud; and the full-ring path that pushes to the UART.
- `panic.rs`: the panic text and `dump_blocked` are printed before `flush_sync`.
- `idt/mod.rs`: the double fault and ring-0 exceptions print and then halt without flushing.
- `interrupts/mod.rs`: `dispatch` runs the kernel's handler before it signals a bound driver, and there is no unregister.
- `main.rs`: the IRQ 4 registration, both polls and `serial_rx_interrupt`.
- `lab.py`: the development channel is a second virtio-serial device, independent of COM1.

Outside the tree: coreboot's `debug.asl` and ICH9 `lpc.asl` as fetched for the re-audit, and P02M0196, P02M0198, P02M0200, P02M0201 and P02M0099. All six findings are accepted and none is rejected.

1. **ACCEPTED - The confirmed revocation runs inside a step that must stay lock-free.**

   Verified. `service_pending` runs from the wake IPI, the tick, both idle loops, the shootdown's waits and every contended `SpinLock::lock`, always with interrupts masked, and `sync.rs` relies on it taking no lock. The kernel reaches the running thread only through `current_thread`, which takes the core's scheduler lock, and no per-CPU field names that thread. So a reload that took the scheduler lock or the process's lock could spin on a lock its own core already holds. A reload run by a round that was started under the process's lock would wait until the round's bounded wait answered false.

   Plan changes:
   - **New P02M0191b item, THE CORE'S RECORD, AND COPIES THAT TAKE NO LOCK.**
     - Every switch records in per-CPU state the loaded process's object id, which is never reused, and the generation it copied at. It also records a reference to the running process, valid while one of that process's threads is current on the core and cleared by every switch to the idle context.
     - The process's lock serializes changes only. A change makes the generation odd before it touches the bytes and even again after, and in between it takes no other lock and allocates nothing.
     - Every copy - at the switch, in the service step and on the `#GP` path - reads the generation before and after copying, and retries while it was odd or moved.
     - The retries are bounded. Past the bound, the switch and the service step load the refuse-everything state and record nothing loaded, and the `#GP` path returns so that the instruction faults again. A missed grant therefore costs one more `#GP`, never a wrong answer.
     - The service step and the `#GP` path find the running process only through the record, never through `current_thread`, so `service_pending` stays lock-free.
   - **Enforcement item:** the switch compares the incoming process and generation against that record.
   - **Revocation item:** a revocation clears the bits as one change, RELEASES the process's lock, copies again on the local core, and only then runs the round. The service step compares its record's generation with the process's and copies without a lock.
   - **Grants-pulled item:** uses the same record.
   - **Mechanism tests (P02M0191d):** the release now also runs while a test hook on a third core holds the looping core's scheduler lock, and it must still confirm. That test fails at once for a service step that uses `current_thread`.

   The finding's "retried when it moved" was unbounded, and I bounded it. Without a bound, a thread mapping ranges in a loop could keep another core's service step from ever answering a round.

2. **ACCEPTED - COM1 is filed only as a terminal-path port, although the kernel drives it.**

   Verified.
   - The kernel drains, reads and polls COM1 on every tick, every IRQ 4 and every idle pass.
   - coreboot's optional `debug.asl` declares `OperationRegion(CREG, SystemIO, 0x3F8, 8)`, and its `DINI` reprograms the port to 115200 baud with interrupts off. The kernel runs it at 38400 with the receive interrupt on.
   - P02M0196b mints SystemIO ranges over the terminal-path category.

   So the ACPI service could be granted COM1 while the kernel drives it, and that grant would then block the handoff's own mint.

   Plan changes:
   - **Reserved-set item, COM1 under the run-time rule.** COM1's ports are in the set from the kernel's first line. They leave it when the COM1 claim's `PortRange` is minted, and rejoin it when that grant ends. Each move happens in ONE STEP with the grant, under the lock every mint checks. That claim's `PortRange` is the one mint the set does not refuse, and only after the handoff's flip.
     - The finding had the flip take the ports out of the set before the mint. I moved them at the mint itself. Otherwise, between the claim's flip and DeviceManager's later mint call, the ports would be neither reserved nor granted, and a SystemIO mint could take them.
   - **Reserved-set item, rows the kernel declares.** Rows the kernel declares itself - P02M0196a's kernel-held devices and `kernel:com1` - are not refused by the reserved ports they record. A description P02M0196a merges into one of them (SPCR, DBG2, `PNP0501`, the PIC's or the clock's node) adds no range. Without this, the mint-sources rule "checked when the row is recorded" would refuse COM1's own row. The mint-sources item now points to this exception.
   - **Terminal-path item:** lists COM1 only WHILE A CLAIM HOLDS IT.
   - **P02M0191c handoff item:** the ports stay reserved until the claim's `PortRange` mint moves them into the claim's grant.
   - **Reacquisition item:** the ports return to the set in the same step as the grant ends, after a `Quarantined` release too.
   - **Tests:**
     - The mechanism tests refuse COM1's ports through the ACPI service's mint path as well as through a claim.
     - The handoff-machinery tests check that the instance's ports are refused while the kernel owns it, granted only through its claim's `PortRange`, and back in the set after the release.

3. **ACCEPTED - P02M0191 and P02M0196 disagree on what the reserved set holds.**

   Verified. P02M0196b said that `PNP0C01`/`PNP0C02` ranges join the kernel's reserved list. coreboot's ICH9 `LDRC` (`PNP0C02`) reserves port 0x80, the GPIO block and the whole 0x80-byte PM block. That PM block holds the TCO sub-range which P02M0200 mints through source (c), and every range is checked against the set again at mint.

   Plan changes: the reserved-set item gains FIRMWARE RESERVATIONS NEVER JOIN THE SET.
   - A `PNP0C01`/`PNP0C02` range is a reservation row of P02M0196b's, with that step's own rule for the rows published around it.
   - A mint inside such a range is governed by the reserved set, the claims and the ACPI service's policy, like any other mint.

   P02M0196's side belongs to its own planner. Its working tree already agrees: reservations never enter this set, and a mint inside one - the TCO sub-range, a WDAT range, an AML region - stays governed by the set, the claims and the policy.

4. **ACCEPTED - The terminal path loses the panic text behind a full ring, and repairs only part of the UART.**

   Verified.
   - The panic handler prints, and dumps the blocked threads, before `flush_sync` runs.
   - While a driver holds COM1, a full ring drops lines at enqueue. The writer would then send 16 KiB of older lines instead of the panic.
   - A driver can also leave the FIFO off or loopback on. `init` rewrites both (FCR and MCR), and the old item did not.
   - The one kernel test read its marker back through loopback, which a writer that restores MCR makes impossible.

   Plan changes:
   - **The terminal-path item** is now PANIC, AND EVERY OTHER TERMINAL PATH, IGNORE THE OWNER - AND TAKE THE WIRE BEFORE THEY PRINT.
     - The panic handler in both builds and the fatal halts enter the writer before their first line. Reset, power-off and the test exit enter it before they act.
     - The writer takes the lock with its bound and re-runs the whole boot initialisation: interrupt enables off, divisor and line settings, the FIFOs enabled and cleared, modem control with loopback off.
     - It writes out the backlog, then the dropped count, and sets a third owner state, TERMINAL. TERMINAL is never left: in it, every kernel line goes to the wire synchronously, as in early boot.
     - The backlog is written out rather than discarded, because it holds the lines that led to the panic and is bounded at 16 KiB.
   - **Handoff item:** lists TERMINAL among the owner states.
   - **P02M0191d's first item:** a test may observe the second port through the test build's record of the kernel's register accesses, instead of through loopback.
   - **Handoff-machinery test:** the writer is entered after the probe left DLAB set, another divisor, the FIFO off and loopback on, and its marker is read from the record. A second run on a fresh instance with the ring past its bound shows the marker after the backlog and the count.

5. **ACCEPTED - The handoff gate cannot fail for either property the handoff exists for.**

   Verified.
   - `dispatch` runs a still-registered kernel handler before it wakes the driver, and `serial_rx_interrupt` empties the FIFO.
   - ConsoleService keeps reading the kernel's console channel.
   - So the handoff line and `lab sh` pass whether or not the kernel lets go.
   - The kernel tests cover only the parameterised second instance, not COM1's own call sites.
   - The panic step ran after the binding was disabled, when the kernel owned COM1 again.

   Plan changes:
   - **The count.** In the development build, the gate counts every access to COM1's registers made by a kernel path other than the terminal-path writer while a driver holds the port. The count sits in the one access path all of them use.
   - **The reacquisition line** states that count, and the gate requires zero at every reacquisition. The reacquisition item says so. It also now flips the owner to KERNEL before re-initialising, so its own register writes are not counted.
   - **Re-enable step:** after the kernel-path step, the binding is enabled again and the gate waits for the next handoff line.
   - **Full-ring panic step:**
     - The driver is still serving: `lab sh` answers through it.
     - A development-build kernel request holds the tap's reads and writes kernel lines until the ring passes its bound.
     - The panic must then put the writer's dropped count, followed by `*** KERNEL PANIC ***` and its message, in the log.
   - **Why a kernel request:** the hold is a kernel request rather than a driver hook, so P02M0099's driver needs nothing for the gate. The request reaches the guest over the development channel, which is a separate virtio-serial device, so it works while a driver holds COM1.

6. **ACCEPTED - P02M0099 still says the legacy-port 16550 needs no device identity.**

   Verified at P02M0099's identity section and at its 16550 bullet.

   Plan changes, in P02M0099 only:
   - **The identity section's sentence** now says the x86 legacy-port `16550` is a consumer too (corrected 2026-09-26). It needs no firmware DESCRIPTION, because the kernel declares COM1 itself, but that declaration is P02M0196a's kernel-declared platform row `kernel:com1`, and the driver is claimed and bound through that row.
   - **The 16550 UART bullet** now binds on P02M0196a's platform rows. COM1 is `kernel:com1`, whose claim through P02M0196a's platform claim performs P02M0191c's handoff. That sits beside the IRQ from P02M0196a's claim-scoped wired interrupts.
   - **The bullet's blocked status** now names both milestones: the authority and the handoff are P02M0191's; the row, its claim and the interrupt are P02M0196a's.

Coordinated changes: Two cross-plan decisions are applied here.
- The decision that firmware reservations never join the reserved set is in the reserved-set item (finding 3).
- The decision that P02M0099's legacy 16550 consumes P02M0196a's platform row is in P02M0099's two passages (finding 6).

One further change keeps this plan consistent with P02M0198. Its planner applied its own re-audit's finding that a processor register the kernel already holds must be admitted again and counted per table, because firmware gives every core the same `_CST` register. This plan's run-time rule refused any install over an already reserved port. It now admits a register the SAME kernel item already holds, and counts it; the ports leave the set only when the last install holding them is uninstalled. The mechanism tests install one register twice. P02M0196's side of the reservation decision belongs to its own planner and was not touched.

Re-check of the whole plan: I re-read all four parts after the edits.
- **COM1 is one state machine everywhere it appears:**
  - reserved while the kernel owns it (reserved set);
  - granted to the claim in one step at the mint (handoff item);
  - terminal-path only while claimed (terminal-path item);
  - back in the set in one step at the release (reacquisition item);
  - TERMINAL from the first terminal-path entry on.
- **Each of those states has a test:** the refusals in the mechanism tests; the set's transitions and the terminal writer on the second instance; and the zero count, the second handoff and the full-ring panic in the gate.
- **The enforcement design is complete for an implementer:** where the record lives, what a copy reads, why neither the service step nor the round waits on a lock, and why a bounded copy is always safe.
- **Cross-plan consistency, checked against the working trees:**
  - P02M0196a: the claim mints by kind and index; `kernel:com1` is kernel-held until the handoff; kernel-held rows and merges; reservations as a row state of their own.
  - P02M0196b: SystemIO is refused over the reserved set and inside a live claim's range, so the ACPI service never holds COM1.
  - P02M0198: the counted run-time install.
  - P02M0200: the TCO sub-range through source (c), and WDAT ranges after the reserved-set check; a firmware reservation blocks neither.
  - P02M0201: the PCI I/O BAR source.
  - P02M0099: the 16550 bullet.
- Nothing is versioned, the interface stays additive at version 1, and every development-only request is compiled only into the development build.

Only `docs/todo/P02M0191.md` and the two named passages of `docs/todo/P02M0099.md` (the identity section's legacy-port sentence and the 16550 UART bullet) were edited and this response appended; no source, test, script or other plan was changed, and nothing was built or booted.


AUDITOR'S RE-AUDIT OF PLAN P02M0191 (2026-09-26T15:50:59Z):

**Rating: 8/10.** The lock-free revocation, COM1's place in the reserved set, the rule for firmware reservations, the terminal writer and the handoff gate are now designed, and they match the tree. What is left is a contradiction with P02M0197b, which this round introduced together with the TERMINAL state, a port range the ACPI service can still be minted although P02M0196a withholds its device, and three small defects.

I read the whole history and the whole plan. I compared the plan with `git show HEAD:docs/todo/P02M0191.md` and read the planner's diffs to this plan and to P02M0099. In the tree I checked:
- the TSS (`gdt.rs`), the serial driver, the shootdown (`mem/tlb.rs`), `sync.rs` and `panic.rs`;
- the IDT handlers and their gate type, the interrupt dispatch and the wake IPI;
- the scheduler's `current_thread`, `reschedule`, idle loops and tick;
- `percpu.rs`, the object id counter, the `Interrupt` object, and the claim, release and derived-object code in `device.rs`;
- the boot order and the IRQ 4, poll and print paths in `main.rs`;
- the reset, power-off and test-exit paths, and the build gating of `sci.rs`;
- every port the kernel touches (`pit.rs`, `rtc`, `apic`, `pci`, `fwcfg.rs`, `sci.rs`, `mod.rs`);
- `console_input.rs`, ConsoleService's mirror, DeviceManager's `--disable` and `--enable` verbs, `lab.py` and `qemu-run.sh`.

I also read the working-tree P02M0099, P02M0196, P02M0197, P02M0198, P02M0200 and P02M0201 where they touch this plan, and two upstream files: QEMU v10.0.0's `hw/char/serial.c` and Linux v6.12's ACPICA `hwvalid.c`.

These corrections now hold:
- **Finding 1 (lock-free revocation).** The core's record, the process's lock released before the round, and bounded lock-free copies keep [`service_pending`](/data/yellow/libersystem/src/kernel/mem/tlb.rs:247) free of locks.
  - [`current_thread`](/data/yellow/libersystem/src/kernel/sched/mod.rs:614) does take the scheduler lock, and [`PerCpu`](/data/yellow/libersystem/src/kernel/arch/x86_64/percpu.rs:29) has no field for the running thread.
  - Every IDT gate is an [interrupt gate](/data/yellow/libersystem/src/kernel/arch/x86_64/idt/mod.rs:37), so no service step can nest inside the `#GP` path's copy.
  - Bounding the retries is a sound addition.
- **Finding 2 (COM1 in the reserved set).** COM1 follows the run-time rule. The planner moves the ports at the mint rather than at the flip, which is better than what the finding suggested: at no moment are the ports neither reserved nor granted.
- **Finding 3 (firmware reservations).** They never join the set, and [P02M0196b](/data/yellow/libersystem/docs/todo/P02M0196.md:247) says the same.
- **Finding 4 (panic path).** The writer is entered before the first line and re-runs all of [`serial::init`](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:81). Its tests read the record of register accesses, not loopback.
- **Finding 5 (handoff gate).** Each new check can now fail: the zero count at every reacquisition, the re-enable step and the full-ring panic.
  - The re-enable step works, because `--enable` binds a disabled node again ([device_manager.rs](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:5391)).
  - The development channel is a separate virtio-serial device ([lab.py](/data/yellow/libersystem/src/harness/lab.py:277)).
- **P02M0198 consistency.** The counted re-install matches [P02M0198](/data/yellow/libersystem/docs/todo/P02M0198.md:124).
- **Finding 6 (P02M0099).** It is corrected in the 16550 bullet and the identity sentence, but not in the paragraph after that sentence (finding 5 below).

1. **Medium - P02M0197b's sleep entry flushes the serial output "as `poweroff` does". Under this plan that flush is the terminal-path writer: it ignores the driver and sets TERMINAL for good. The plan gives a path the machine returns from no other way to reach the wire.**

   This round made the writer one-way:
   - It ["sets the owner to TERMINAL, which is never left. From then on every kernel line goes to the wire synchronously"](/data/yellow/libersystem/docs/todo/P02M0191.md:257).
   - [`flush_sync` becomes that writer](/data/yellow/libersystem/docs/todo/P02M0191.md:248), and power-off enters it before it acts.
   - No other path may write: ["from the moment the claim returns no kernel path but the terminal-path writer below touches the port"](/data/yellow/libersystem/docs/todo/P02M0191.md:213).
   - The plan does not mention sleep.

   P02M0197b, also edited in this round, relies on that flush:
   - The kernel's sleep entry is to ["flush the serial output synchronously, as `poweroff` does"](/data/yellow/libersystem/docs/todo/P02M0197.md:177).
   - Its oracle times that line on the wire ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:336)).
   - Before the entry, DeviceManager has already sent `SUSPEND` to every binding ([P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md:101)). The 16550 driver is one of them, so nothing drains the tap.

   If both plans are built as written, the first suspend to idle or S3 leaves COM1 in TERMINAL after the resume:
   - With a driver bound, the kernel writes COM1 while the resumed driver drives it too. P02M0191c exists to prevent exactly this state with two owners.
   - With or without a driver, every kernel line and every `SYS_DEBUG_WRITE` then waits for the wire. The ring was built to remove that stall ([serial.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:7)).

   A flush that respected the owner fails too, while a driver holds COM1: the line would stay in the ring behind a suspended driver, and P02M0197b's oracle could not pass. The `serial-handoff` gate never sleeps, so nothing in this plan would catch the conflict. This is a new contradiction between two changes made in this round. P02M0197's re-audit of this date reports the same conflict from the sleep entry's side.

   **Correct the terminal-path item** : state that the sleep entry is not a terminal path and never enters the writer, and state what it does instead:
   - While the kernel owns COM1, the entry drains the ring synchronously and leaves the owner at KERNEL.
   - While a driver holds COM1, the planner must choose. Either the entry takes COM1 back for the sleep, as reacquisition does, and returns it to the claim at resume. Or the line waits in the ring for the resumed driver.

   Then make P02M0197b's "as `poweroff` does" refer to that rule.

2. **Medium - The ACPI service's SystemIO mint is checked only against the reserved set and live grants. It can therefore hand out the ISA DMA controller's registers, which P02M0196a keeps kernel-held because the device masters the bus untranslated. The new sentence saying that kernel-held rows record reserved ports is not true of that row.**

   The sentence and the gap behind it:
   - This round added ["P02M0196a's kernel-held devices record the reserved ports they describe and mint nothing"](/data/yellow/libersystem/docs/todo/P02M0191.md:109).
   - The reserved set is ["every port the kernel drives"](/data/yellow/libersystem/docs/todo/P02M0191.md:86). The kernel drives none of the 8237's registers (0x00-0x0F, 0xC0-0xDF and the page registers from 0x81). All of its port accesses are in `serial.rs`, `pci`, `fwcfg.rs`, `apic`, `pit.rs`, `rtc`, `sci.rs` and `mod.rs`.
   - P02M0196a still lists ["the ISA DMA controller (a bus master nothing translates)"](/data/yellow/libersystem/docs/todo/P02M0196.md:60) among the kernel-held devices.

   Nothing refuses these ports at the SystemIO mint:
   - The SystemIO mint gets only ["the same reserved-set and exclusivity checks"](/data/yellow/libersystem/docs/todo/P02M0191.md:77).
   - P02M0196b refuses SystemIO only ["over P02M0191's reserved set"](/data/yellow/libersystem/docs/todo/P02M0196.md:178) and inside a live claim. A kernel-held row is neither, so an AML region over the 8237 becomes a `PortRange` held by the ACPI service.
   - This plan keeps fw_cfg's DMA registers out of every mint for exactly this reason ([plan](/data/yellow/libersystem/docs/todo/P02M0191.md:90)).
   - P02M0196b refuses kernel-held MMIO for SystemMemory regions ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:164)), but has no equivalent rule for ports.

   Refusing these ports costs firmware nothing. P02M0196b answers `_OSI` true for the Windows strings ([P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md:224)), and when an OS does that, ACPICA already denies AML these DMA ports ([hwvalid.c](https://github.com/torvalds/linux/blob/v6.12/drivers/acpi/acpica/hwvalid.c#L51)). This is a new finding.

   **Correct the reserved-set item** : refuse at every mint the ISA DMA controller's channel and control registers, 0x00-0x1F and 0xC0-0xDF, as fw_cfg's DMA registers are refused. The page registers from 0x81 cannot start a transfer and can stay mintable. Correct the sentence about kernel-held rows to match.

   P02M0196's re-audit of this date reports the same gap from the side of its SystemIO policy, with the same ranges.

3. **Low - The FADT part of the reserved set must exist when the boot scan evaluates source (c), in the test build too. The kernel's only FADT reader runs after the scan and is left out of the test build, and the plan does not say where that part is computed.**

   The plan needs the FADT part at the scan:
   - The FADT part is ["computed at boot"](/data/yellow/libersystem/docs/todo/P02M0191.md:93).
   - Source (c) reads the base ["once during the boot scan"](/data/yellow/libersystem/docs/todo/P02M0191.md:69). There it checks ["the base agreeing with a FADT block"](/data/yellow/libersystem/docs/todo/P02M0191.md:68) and refuses any sub-range that touches the reserved set.
   - P02M0200's ICH9 row relies on the same order ([P02M0200](/data/yellow/libersystem/docs/todo/P02M0200.md:199)).

   In the tree the order is the other way round:
   - The boot scan is [`device::init`](/data/yellow/libersystem/src/kernel/main.rs:223).
   - The only FADT reader is [`sci::init`](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:79). The boot tail calls it after the scan ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:465)).
   - Its module is [`cfg(not(test))`](/data/yellow/libersystem/src/kernel/arch/x86_64/mod.rs:24). The test kernel runs [`test_main`](/data/yellow/libersystem/src/kernel/main.rs:238) and never reads the FADT.

   Two tests are affected. In the test where ["a row whose sub-range covers PM1 is refused at boot"](/data/yellow/libersystem/docs/todo/P02M0191.md:290), the reserved-set check cannot refuse the row, although that check is what the test is meant to prove. The row is refused only when its FADT condition fails, which is the wrong reason. The test's other case refuses ["the FADT-derived ones this boot computed"](/data/yellow/libersystem/docs/todo/P02M0191.md:276), and in the test build that set is empty. This is a new finding.

   **Correct the reserved-set item** : state that the FADT part is computed before `device::init`, in the test build as well. The lookup it needs, [`smp::acpi_table`](/data/yellow/libersystem/src/kernel/smp/mod.rs:546), is compiled into both builds, and `init_extended_config` already calls it just before the scan ([main.rs](/data/yellow/libersystem/src/kernel/main.rs:1194)).

4. **Low - Reacquisition runs the boot sequence before it "feeds what the FIFO holds". That sequence resets the receive FIFO, so the input the plan says is kept is thrown away.**

   The item runs these steps in this order:
   - It re-initialises the UART ["with its boot sequence"](/data/yellow/libersystem/docs/todo/P02M0191.md:237).
   - Then it ["feeds what the FIFO holds to the console channel"](/data/yellow/libersystem/docs/todo/P02M0191.md:239).
   - It says ["Input typed between the driver's death and the release is what the UART's FIFO holds"](/data/yellow/libersystem/docs/todo/P02M0191.md:243).

   The boot sequence empties that FIFO first:
   - It writes FCR `0xC7` ([serial.rs](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:88)). Bit 1 of that value resets the receive FIFO.
   - QEMU empties the FIFO and clears the data-ready bit on that write ([serial.c](https://github.com/qemu/qemu/blob/v10.0.0/hw/char/serial.c#L415)).
   - QEMU also forces the reset whenever the FIFO-enable bit changes ([serial.c](https://github.com/qemu/qemu/blob/v10.0.0/hw/char/serial.c#L409)). That happens after a driver that turned the FIFO off.

   So the feed finds nothing. No test catches this, because the receive path is [`cfg(not(test))`](/data/yellow/libersystem/src/kernel/arch/x86_64/serial.rs:220). This is a new finding, in text present since the planner's first response.

   **Correct the reacquisition item** : read out the received bytes before the boot sequence. Clear DLAB first, since a driver may have left it set.

5. **Low - The correction of the previous finding 6 is incomplete. The paragraph right after P02M0099's corrected sentence still says that the legacy-port 16550 needs no firmware identity and must own the three mechanisms itself.**

   The two passages disagree:
   - The corrected sentence says the legacy-port 16550 ["is one of them too"](/data/yellow/libersystem/docs/todo/P02M0099.md:1123) and binds through `kernel:com1`.
   - The next paragraph still says it ["needs no firmware identity. It needs three things nothing in this tree has"](/data/yellow/libersystem/docs/todo/P02M0099.md:1134).
   - That paragraph then offers only two options. Either the slice owns those three mechanisms itself, or the first 16550 item covers ["FIRMWARE-DESCRIBED MMIO UARTs - which needs the identity prerequisite instead"](/data/yellow/libersystem/docs/todo/P02M0099.md:1144).

   That is the exception the previous finding asked to remove. It also contradicts the [16550 bullet](/data/yellow/libersystem/docs/todo/P02M0099.md:2950), which gives the authority and the handoff to this plan and the row, claim and interrupt to P02M0196a.

   **Correct P02M0099's paragraph** : say that the legacy slice needs no firmware description but binds through P02M0196a's `kernel:com1` row. Say that the mechanisms are owned as the 16550 bullet and the [owner table](/data/yellow/libersystem/docs/todo/P02M0099.md:267) record. Alternatively, mark the paragraph as superseded by the bullet.

Validation: I inspected, without changing anything, the plan, the audit history and the planner's diffs (`git show` and `git diff` against `eca3cbe7`). I also inspected the kernel, service and harness sources cited above, and the working-tree P02M0099, P02M0196, P02M0197, P02M0198, P02M0200 and P02M0201. From upstream I fetched, read-only, QEMU v10.0.0's `hw/char/serial.c` and Linux v6.12's `drivers/acpi/acpica/hwvalid.c`. No plan, source or audit file was modified. Nothing was built, tested, benchmarked or booted.

PLANNER'S RESPONSE ON P02M0191 (2026-09-26T17:12:45Z):

Verified read-only against the tree:
- `serial.rs`: `init` writes FCR `0xC7` (bit 1 resets the receive FIFO); `enable_rx_irq` and `read_byte` are `cfg(not(test))`; `flush_sync` is today's plain synchronous drain;
- `main.rs`: `device::init` is the boot scan (line 223); `init_extended_config`, which calls `smp::acpi_table` just before it, is `cfg(all(not(test), target_arch = "x86_64"))`; `sci::init` runs in the boot tail after the scan, and the `sci` module is `cfg(not(test))`; `smp::acpi_table` is gated only on the architecture, so both builds compile it;
- Linux v6.12 ACPICA `hwvalid.c`: its protected ports include DMA `0x0000..0x000F` and IDMA `0xC0..0xDF`, denied to AML once firmware has seen a Windows `_OSI`; on ICH9 the first controller's registers are also decoded at their `0x10..0x1F` alias;
- P02M0196a's kernel-held set ("the ISA DMA controller (a bus master nothing translates)"), P02M0196b's SystemIO bullet, P02M0197b's entry and P02M0197d's suspend-to-idle oracle, and P02M0099's identity paragraph, the paragraph after it and its 16550 bullet.
Summary: five findings, all accepted.

1. **ACCEPTED - the sleep entry's flush would be the one-way terminal writer.** The conflict is real: P02M0197b told its entry to flush "as `poweroff` does", and this plan made that flush the writer that sets TERMINAL for good. THE CHOICE, of the two the finding offers: THE ENTRY TAKES COM1 BACK FOR THE SLEEP, not "the line waits in the ring". Once this part and the 16550 driver land, a driver holds COM1 on every x86_64 boot, so with the second option neither `sleep: entered` nor `sleep: resumed` would reach the wire until the driver resumed; P02M0197d's host-timed interval between the two lines would collapse on x86_64, and a sleep that hangs would leave nothing on the wire. At the entry every binding has already answered `SUSPENDED` (P02M0197a's step 4), so the kernel driving the UART for the window makes no second active owner. Plan changes:
   - A new P02M0191c item, "THE SLEEP ENTRY IS NOT A TERMINAL PATH". The entry never enters the terminal-path writer and never leaves TERMINAL. With the kernel owning COM1 it drains the ring and writes its line synchronously, and the owner stays KERNEL. With a driver holding COM1 it sets the owner to SLEEP with the claim's generation, re-runs the UART's boot initialisation with the receive interrupt left off (IRQ 4 stays the claim's), drains the ring and writes its line; its last act is to set the owner back to DRIVER. In both cases every kernel line until the entry returns, `sleep: resumed` among them, goes to the wire synchronously. The claim, its `PortRange`, IRQ 4 and the tap are untouched; the development counter does not count these accesses; the COM1 driver's `RESUME` reprograms the UART whatever the sleep state; a panic inside the window enters the terminal writer; whichever of this part and P02M0197b lands second wires the entry, and until this part lands the entry's flush is today's `flush_sync`.
   - The ownership states gain SLEEP; "no kernel path but the terminal-path writer touches the port" now names the sleep entry too; the terminal-path category says the sleep entry is not one of its paths; the gate's counter excludes the sleep entry as it excludes the terminal writer.
   - Verification: the handoff-machinery tests check the rule on both owners of the second instance (with the kernel owning it, the ring drained and the owner still KERNEL; with a probe holding it, SLEEP for the window, the line in the record with the receive interrupt off, DRIVER afterwards and nothing counted). The `serial-handoff` gate, carried by whichever of this part and P02M0197b lands second, runs a suspend to idle right after its first `lab sh` round trip, so the kill that follows reports its zero count over a hold that included a sleep - the gate no longer never sleeps.

2. **ACCEPTED - the ISA DMA controller's registers were mintable to the ACPI service.** The ranges hold against ACPICA and ICH9's decode, and the argument is the one this plan already makes for fw_cfg. Plan changes:
   - The reserved set is now "every port the kernel drives, and every port that would let its holder start a transfer nothing translates". Its fixed part gains the DMA controllers' channel and control registers, `0x00..0x1F` (the chipset's alias of the first controller included) and `0xC0..0xDF`, with the reason and the ACPICA note; the page registers from `0x81` stay mintable, as port `0x80` does.
   - "ROWS THE KERNEL DECLARES ARE NOT REFUSED BY IT" no longer says the kernel-held rows record reserved ports: they mint nothing; the ports they describe that the kernel drives or that could start an untranslated transfer are in the set, and the rest (the page registers) grant nothing through a row that mints nothing.
   - The mechanism tests refuse a DMA channel and a control register while the page register at `0x81` is minted.

3. **ACCEPTED - the FADT part must exist when the scan evaluates source (c), in the test build too.** Plan changes: the FADT part is "COMPUTED BEFORE THE BOOT SCAN (`device::init`) AND IN THE TEST BUILD AS WELL", read through `smp::acpi_table`, which both builds compile, with the reason (source (c) checks its rows against it during the scan, while `sci::init`, today's only FADT reader, runs after the scan and only in the production build). The sources test now says the row covering PM1 is refused by the reserved-set check with its FADT condition holding, the test build computing the FADT part before its scan. One precision on the pointer the finding gives: `init_extended_config` is itself left out of the test build, so the plan names the lookup (`smp::acpi_table`), not that function, as what runs before the scan in both builds.

4. **ACCEPTED - reacquisition reset the receive FIFO before feeding it.** Plan change, in the reacquisition item: after flipping to KERNEL the kernel first READS OUT WHAT THE RECEIVER HOLDS - DLAB cleared, since the driver may have left it set, then the receive register read while the line status says data is ready, bounded at 64 bytes, the deepest FIFO of the 16550 family - and only then re-runs the boot sequence, and feeds the bytes it read out. "Input typed between the driver's death and the release is what the FIFO holds" now adds "and the read-out keeps it". The handoff-machinery tests check, from the record's order, that after a probe left DLAB set with bytes in the receiver, those bytes are read with DLAB cleared before the FIFO reset (the receive interrupt path is production-only, which is why the record is the oracle).

5. **ACCEPTED - P02M0099's paragraph after the corrected sentence still contradicted it.** Plan change in P02M0099: the paragraph now says the legacy-port 16550 needs no firmware DESCRIPTION but binds through P02M0196a's `kernel:com1` row; it names each of the three mechanisms with its owner, as the 16550 bullet and the owner table record (the port-I/O authority P02M0191's, the fixed-IRQ routing and its revocation P02M0196a's claim-scoped wired interrupts, the atomic handoff P02M0191c's with its terminal writer and reacquisition rule); it says the slice owns none of them itself, is blocked on both milestones and lands with P02M0191c; and the firmware-described MMIO UARTs are the item's other half, blocked on the identity prerequisite. The 16550 bullet also gains the `RESUME` step finding 1 requires.

Coordinated changes: P02M0196b's SystemIO bullet refuses the same DMA ranges (finding 2); P02M0197b's entry refers to the sleep-entry rule and P02M0197d's suspend-to-idle oracle reads the same in both of its cases (finding 1); P02M0099's paragraph and 16550 bullet (findings 5 and 1).

Re-check of the whole plan: the reserved set, the mint sources, the terminal-path category, the ownership states and the sleep rule now agree with P02M0196a/b, P02M0197b, P02M0198's run-time installs and P02M0200's ICH9 row, which relies on the same FADT-before-scan order. Every new rule has a test that can fail: the DMA refusals and the page-register mint, the PM1 row refused for the right reason, the read-out order in the record, the sleep rule on both owners, and the gate's sleep before its kill. Order and exclusions are unchanged. The file is ASCII, no line exceeds 110 columns, it cites no audit, and the `Status:` line is kept.

Edited `docs/todo/P02M0191.md`, and for coordination `docs/todo/P02M0196.md`, `docs/todo/P02M0197.md` and `docs/todo/P02M0099.md`; no source was changed, and nothing was built or booted.
