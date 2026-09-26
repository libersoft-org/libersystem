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
