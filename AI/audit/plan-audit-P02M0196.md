AUDITOR'S REVIEW OF PLAN P02M0196 (2026-09-25T22:54:13Z):

**Rating: 3/10.** The staging into static tables, then AML, then power is sound and the fixture mechanisms are concrete, but the plan omits the join between firmware nodes and PCI functions, the kernel work behind a platform-device claim, the trust boundary of the proposed ACPI service and the kernel side of general-purpose events, and it gives several devices two owners.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0196.md) and the milestones created from it, [P02M0197](/data/yellow/libersystem/docs/todo/P02M0197.md) to [P02M0202](/data/yellow/libersystem/docs/todo/P02M0202.md). Also reviewed: its dependants [P02M0190](/data/yellow/libersystem/docs/todo/P02M0190.md:19) and [P02M0195](/data/yellow/libersystem/docs/todo/P02M0195.md), the [firmware-node prerequisite in P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:1067), [P02M0191](/data/yellow/libersystem/docs/todo/P02M0191.md), `src/acpi`, `src/fdt`, `src/tpm`, the kernel's device table, claim, interrupt and SCI paths, DeviceManager's binding identity, the boot hand-off and the harness. The review is at commit `07371c44af82a11c1d275b0cbd832899f712e62a`. The plan itself, P02M0099, P02M0190 and `TODO.md` were read as they stand uncommitted in the working tree, and P02M0197 to P02M0202 as untracked files; line numbers refer to the working tree. This is a requirements checklist rather than an implementation design. The findings concern decisions the implementation needs, not the expected absence of new code.

1. **High - The plan never joins ACPI namespace nodes or device-tree nodes to the PCI functions they describe, although its own fixture and most of its consumers depend on that join.**

   Step 1 defines a platform device ["beside PCI and USB"](/data/yellow/libersystem/docs/todo/P02M0196.md:29), and step 2 enumerates namespace devices only ["into the platform devices of step 1"](/data/yellow/libersystem/docs/todo/P02M0196.md:48). Much of what AML describes, however, hangs off PCI functions through `_ADR` under a host bridge:
   - The fixture locates ivshmem's BAR ["through a PCI configuration region"](/data/yellow/libersystem/docs/todo/P02M0196.md:77). Such a region is addressed only through the `_ADR` of the device that declares it and the bridge's `_SEG` and `_BBN`.
   - The fixture raises its event through `_AEI` on a GPIO controller that is a [virtio-gpio PCI function](/data/yellow/libersystem/docs/todo/P02M0196.md:78).
   - The panel whose backlight P02M0199 drives is an output device ["under the graphics adapter's namespace node"](/data/yellow/libersystem/docs/todo/P02M0199.md:25), and its 0x86/0x87 notifications are addressed there.
   - USB and network wake go through the PCI device's own [`_PRW`](/data/yellow/libersystem/docs/todo/P02M0197.md:77).
   - HID over I2C names its controllers as [resource sources](/data/yellow/libersystem/docs/todo/P02M0195.md:18), and in the guest those controllers are PCI functions.
   - SSIF's `IPI0001` names its SMBus controller, which on q35 is the ICH9 PCI function 00:1f.3 ([P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:24)).

   Nothing in the tree can carry this association. Binding identity is [bus/device/function plus generation](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:739), DeviceManager builds it [directly from the kernel's PCI row](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2224), and no source file mentions `_ADR`.

   Without the join, `_DSD`, `_DSM`, device power methods, `_PRW` and `Notify` cannot reach a PCI function's driver. A resource source cannot be resolved to the binding that serves it, and a PCI-configuration region has no address. The only other option is to treat such nodes as independent platform devices, which gives a function DeviceManager already binds a second, claimable identity.

   **Correct step 1 and the namespace item** by adding a companion item with four parts:
   - resolve `_ADR` (with `_SEG`, `_BBN` and bridge secondary-bus numbers) and device-tree PCI child nodes (`reg`) to the PCI function;
   - attach the node to the existing PCI binding as firmware data, not as a second claimable device;
   - give a PCI driver the node's `_DSD`/`_DSM`, power methods and `Notify` stream, and resolve resource sources and GPIO phandles to the binding and provider that serve them;
   - test it in 0196d with the fixture's own `_ADR` devices.

2. **High - "A claim through DeviceManager exactly as a PCI function is claimed" is not possible with the claim that exists, and the plan names none of the kernel and ABI work, above all wired interrupts.**

   Every layer of the claim is PCI-shaped:
   - The kernel's [device table](/data/yellow/libersystem/src/kernel/device.rs:17) holds one BAR and a PCI address per row.
   - [`sys_device_claim`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1285) mints [one `DeviceMemory` for that BAR](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1299).
   - DMA admission and IOMMU attach are [by bus/device/function](/data/yellow/libersystem/src/kernel/device.rs:742).
   - The binding id, the [provider origin](/data/yellow/libersystem/src/idl/device.lsidl:230) and the ABI's [two transports](/data/yellow/libersystem/src/abi/src/lib.rs:721) are PCI's.

   P02M0099 states exactly this scope for the prerequisite this step now owns ([identity section](/data/yellow/libersystem/docs/todo/P02M0099.md:1067)).

   The devices step 1 names need wired interrupts, and the PL011 item waiting on this step asks for ["supplied clock/IRQ data"](/data/yellow/libersystem/docs/todo/P02M0099.md:2967). No claim-scoped wired interrupt exists anywhere:
   - On x86_64 a driver can bind only the [15 legacy vectors](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:100), under the [DeviceManager privilege](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1624), through [one I/O APIC at a fixed base whose MADT entries are not enumerated](/data/yellow/libersystem/src/kernel/arch/x86_64/ioapic.rs:10). The dispatch [sends EOI without masking](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:380), so a level-triggered line re-fires until the driver runs.
   - On aarch64 and riscv64 [no wired line can be bound by a driver at all](/data/yellow/libersystem/src/kernel/arch/aarch64/interrupts/mod.rs:207) ([riscv64](/data/yellow/libersystem/src/kernel/arch/riscv64/interrupts/mod.rs:80)).

   The largest part of step 1 is per-architecture interrupt routing with trigger and polarity, revoked with the claim. That part is invisible in the plan, so it can be neither estimated nor sequenced, and "exactly as a PCI function" invites squeezing platform devices into a BDF-shaped row.

   **Correct step 1** by listing the work:
   - a discriminated device row, and a claim that mints several MMIO ranges and P02M0191 port ranges;
   - claim-derived wired-interrupt capabilities on each architecture: on x86, MADT I/O APIC enumeration, GSIs above 15, source overrides and `_CRS` trigger and polarity; on aarch64, GIC SPI configuration; on riscv64, PLIC/APLIC wired sources;
   - mask-until-acknowledge for level-triggered lines, and revocation at release;
   - the DMA-policy answer for non-PCI endpoints (non-mastering unless a stream id is described);
   - a discriminated binding id, provider origin and manifest match vocabulary, and diagnostics.

3. **High - The proposed userspace ACPI service has no stated trust boundary: its `_CRS` answers would decide which physical ranges, ports and lines the kernel hands to drivers, and "only the authorities its operation regions need" bounds nothing.**

   The [interpreter item](/data/yellow/libersystem/docs/todo/P02M0196.md:46) proposes a userspace service "holding only the authorities its operation regions need". [Namespace enumeration](/data/yellow/libersystem/docs/todo/P02M0196.md:48) makes `_CRS` the resource list of step-1 devices. Operation regions are declared by firmware code at run time, and on real machines they reach the SMI command port, the PM1 and GPE blocks, CMOS, EC ports, firmware NVS memory and other functions' PCI configuration. So the authority the service "needs" is whatever AML says.

   Three existing rules conflict with that. P02M0191 lets the kernel mint a port range [only for described ranges, never over one the kernel keeps](/data/yellow/libersystem/docs/todo/P02M0191.md:24), and hands it out [with a device claim](/data/yellow/libersystem/docs/todo/P02M0191.md:37). The ACPI service holds no claim, and if `_CRS` is evaluated in userspace the kernel cannot know what is "described". Today `DeviceMemory` is [minted only under a claim](/data/yellow/libersystem/src/kernel/object/device_memory.rs:63), and PCI configuration ["stays the kernel's"](/data/yellow/libersystem/docs/todo/P02M0191.md:55). The plan's own fixture needs, [from AML](/data/yellow/libersystem/docs/todo/P02M0196.md:77), both an unclaimed function's configuration space and its shared-memory BAR. ivshmem is not a class the kernel resolves, so its row [carries no BAR at all](/data/yellow/libersystem/src/kernel/device.rs:128).

   Two outcomes follow. Either the service holds ambient authority over physical memory and ports, and a defect in it or in firmware code maps kernel memory into a driver. Or the accesses real firmware makes are refused and its methods fail. The "where it runs" decision cannot be made without settling this.

   **Correct the interpreter item** by stating, as part of the placement decision, the policy the kernel enforces:
   - which address classes a SystemMemory region may map: firmware-reserved and ACPI NVS memory, and MMIO outside RAM and outside kernel-owned or claimed windows;
   - which ports are denied or mediated: the PIC, the PIT, PCI configuration, PM1/GPE, CMOS and the SMI command port;
   - how PCI configuration access is mediated by the kernel, and with what restrictions;
   - which checks the kernel applies to a `_CRS`-derived resource before minting it: no overlap with RAM, kernel-owned ranges, PCI windows or another claim.

   Say how the fixture's ivshmem access is authorised under that policy. A policy table and the list of new calls suffice.

4. **High - Nothing divides general-purpose events between the kernel, which owns the SCI, and a userspace interpreter, and the kernel's SCI handler as written cannot survive the first enabled GPE.**

   The kernel routes the shared, level-triggered SCI ([P02M0099's decision](/data/yellow/libersystem/docs/todo/P02M0099.md:3952)). Its handler reads PM1 status for [the two button bits only](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:277), and the file says it ["arms no general-purpose event ... GPEs are AML's"](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:25). An interrupt it cannot decode is [counted as a storm](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:301). After [64 of them](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:64) in one window, it clears PM1 enable and drops its state. Despite its comment, it [does not mask the redirection entry](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:320), and the wired path only [signals and sends EOI](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:380). An asserted GPE, which only a write to its own status register clears, therefore keeps raising the line after the "disarm". The kernel's event channel carries a [one-byte kind with two values](/data/yellow/libersystem/src/kernel/platform_event.rs:38), latched as a bitmap. The FADT's GPE blocks are [parsed](/data/yellow/libersystem/src/acpi/src/lib.rs:686) and used by nothing.

   The plan needs GPEs in three places without saying who handles them:
   - the embedded controller's query events ([EC item](/data/yellow/libersystem/docs/todo/P02M0196.md:50));
   - the [wake `_PRW` GPEs](/data/yellow/libersystem/docs/todo/P02M0196.md:61);
   - the `_Lxx`/`_Exx` methods behind most `Notify` calls on a full-ACPI machine.

   P02M0200 already records ["the kernel keeping PM1 and GPE"](/data/yellow/libersystem/docs/todo/P02M0200.md:39). The chosen fixture raises its events through `_AEI`, so no gate would ever exercise a GPE.

   This is how battery, AC, lid, thermal and EC events reach the operating system on ordinary laptops. Under the userspace default, enabling any GPE with today's handler becomes an interrupt storm that also takes the power button with it.

   **Correct the interpreter item and step 3** with a kernel/service split:
   - the kernel reads GPE0/GPE1 status and enable in the SCI handler, masks each asserted enabled GPE and latches its number;
   - it delivers GPE numbers, not the two-kind bitmap, to the ACPI service;
   - it re-enables a GPE only after the service has run `_Lxx`/`_Exx` and asked for the status bit to be cleared;
   - storms are handled per GPE, by disabling that GPE rather than the SCI;
   - the plan states who owns the runtime and wake enable masks, and how the EC's GPE and query loop work.

   Give the GPE path an oracle (for example ACPI CPU hot-plug, which QEMU's q35 tables signal through a GPE method), and fix the storm path's missing mask in the same item. If AML is placed in the kernel instead, say so.

5. **Medium - Step 1 would publish as claimable devices the kernel already drives, would give one device two identities, and defines no identity for devices a static table describes.**

   "Every device-tree node with a `compatible` and resources" ([static sources](/data/yellow/libersystem/docs/todo/P02M0196.md:35)) takes in the following devices, which the kernel already drives:
   - the PL011 the aarch64 kernel [writes at a fixed address](/data/yellow/libersystem/src/kernel/arch/aarch64/serial.rs:13);
   - the PL031 it [reads the time from at a fixed address](/data/yellow/libersystem/src/kernel/arch/aarch64/mod.rs:304), which is the "ARM RTC" this step says it [unblocks](/data/yellow/libersystem/docs/todo/P02M0196.md:38);
   - riscv64's [goldfish RTC](/data/yellow/libersystem/src/kernel/arch/riscv64/mod.rs:422) and [16550](/data/yellow/libersystem/src/kernel/arch/riscv64/serial.rs:26);
   - the interrupt controllers and PCIe hosts the kernel owns.

   On x86, QEMU's namespace also describes the CMOS clock the kernel reads through [0x70/0x71](/data/yellow/libersystem/src/kernel/arch/x86_64/rtc/mod.rs:1). P02M0191 plans a handoff [for COM1 only](/data/yellow/libersystem/docs/todo/P02M0191.md:42).

   Separately, a PC's TPM is both [the `TPM2` table and an `MSFT0101` node](/data/yellow/libersystem/src/tpm/src/lib.rs:4), and SPCR/DBG2 describe a UART the namespace lists too. Step 1 names identities only as a namespace path with `_HID`, or as a device-tree node path ([identity item](/data/yellow/libersystem/docs/todo/P02M0196.md:29)). A static table has neither before step 2, and step 2 then creates a second identity for the same registers. Claims are per identity, so both could be claimed.

   Two holders of one register block, whether the kernel and a driver or two drivers, is the failure the claim exists to exclude. The CMOS index/data pair is exactly the shared state that [KERN-ARCH-023 had to lock](/data/yellow/libersystem/src/kernel/arch/x86_64/rtc/mod.rs:10).

   **Correct step 1** by stating:
   - a kernel-reserved set that is never published as claimable;
   - a handoff item for each kernel-driven device a driver is wanted for, as P02M0191 does for COM1;
   - an identity form for devices a static table describes;
   - a reconciliation rule: one platform device per resource set, a static description merged into the namespace or tree node that describes the same resources, and an overlap refusal at publication.

6. **Medium - Ownership is contradictory: step 2 keeps device classes that the plan's closing section and other milestones assign elsewhere, and the verification requires an S3 resume that only P02M0197 can deliver.**

   [The EC item](/data/yellow/libersystem/docs/todo/P02M0196.md:50) lists battery, AC, thermal zones, lid, buttons, backlight and the time-and-alarm device as this milestone's classes. Other documents assign the same classes elsewhere:
   - The [closing section](/data/yellow/libersystem/docs/todo/P02M0196.md:100) says battery, AC and thermal stay with P02M0099's ACPI item. That item [owns them](/data/yellow/libersystem/docs/todo/P02M0099.md:4298), and P02M0181 names it as [the producer's owner](/data/yellow/libersystem/docs/todo/P02M0181.md:17).
   - The same section says the Time and Alarm Device ["goes whole to P02M0197 ... because one device has one owner"](/data/yellow/libersystem/docs/todo/P02M0196.md:101), and [P02M0197 says so too](/data/yellow/libersystem/docs/todo/P02M0197.md:81).
   - ACPI backlight is [P02M0199's provider item](/data/yellow/libersystem/docs/todo/P02M0199.md:25).

   The working-tree revision moved the sleep transaction to P02M0197 ([step 3](/data/yellow/libersystem/docs/todo/P02M0196.md:59)), yet 0196d [still requires](/data/yellow/libersystem/docs/todo/P02M0196.md:83) "S3 and resume in QEMU". That needs P02M0197's [resume entry from real mode](/data/yellow/libersystem/docs/todo/P02M0197.md:56) and its [suspend transaction](/data/yellow/libersystem/docs/todo/P02M0197.md:31), while P02M0197 depends on this step.

   An item with two owners gets built twice or by neither. This milestone also cannot close before a milestone that cannot start before it.

   **Correct the EC item and the QEMU verification item.** In 0196b, either keep only the mechanism consumers need (the namespace, `Notify`, `_DSM`/`_DSD`, the EC transport) and move each class to its named owner, or take the classes here and remove them there, once. Replace "S3 and resume" with the platform half this milestone can prove alone: the `\_S3` sleep type read, `_PTS`/`_SST`/`_WAK` evaluated in order against a scripted namespace, and the FACS waking vector written. Leave the QEMU suspend-and-wake oracle to [P02M0197d](/data/yellow/libersystem/docs/todo/P02M0197.md:95).

7. **Medium - The fixture's notification path depends on P02M0195's virtio-gpio driver while P02M0195 depends on this milestone, and the `_AEI` handler has no way to be granted its lines.**

   [0196d](/data/yellow/libersystem/docs/todo/P02M0196.md:78) chooses `_AEI` "on P02M0195's virtio-gpio line". [The index](/data/yellow/libersystem/docs/todo/TODO.md:283) says P02M0195 "Needs P02M0196's AML half" and lists it first. Neither plan says the GPIO half of P02M0195 lands before 0196d. P02M0195's only grant rule is one line or address ["granted through the claim of the firmware-described device"](/data/yellow/libersystem/docs/todo/P02M0195.md:26). The `_AEI` consumer is the ACPI service itself, which holds lines of the controller and claims no child device.

   The fixture that six milestones build on has no delivery order, and its event path has no path for granting authority.

   **Correct the fixture item** by stating the order: P02M0195's bus half (the virtio-gpio driver, the GPIO line contract and the backend) before 0196d, and P02M0195's HID half after 0196b. State how the ACPI service obtains the lines a GPIO controller's `_AEI` lists, from the controller's provider through the companion of finding 1.

8. **Medium - The embedded controller driver has no oracle: QEMU emulates no EC, and the fixture replaces EC regions with a harness memory region, so the driver would ship unexercised.**

   The [EC item](/data/yellow/libersystem/docs/todo/P02M0196.md:50) plans the driver, but `qemu-system-x86_64 -device help` lists no embedded controller, and the [SSDT fixture](/data/yellow/libersystem/docs/todo/P02M0196.md:80) reads "a memory region the harness controls" instead. Every staged driver needs an oracle or a written exception ([component-oracles](/data/yellow/libersystem/src/tools/check-component-oracles.sh:2)), and hardware runs are ["never in place of an oracle"](/data/yellow/libersystem/docs/todo/P02M0196.md:85). This project leaves controllers it cannot emulate for real hardware: P02M0195's [DesignWare controllers](/data/yellow/libersystem/docs/todo/P02M0195.md:14) and P02M0200's [SBSA watchdog](/data/yellow/libersystem/docs/todo/P02M0200.md:61). The item also leaves open whether the EC is a separate driver serving `EmbeddedControl` regions back to the interpreter, which decides the `_REG` and early-access (ECDT) ordering.

   **Correct the EC item** in one of two ways. Either move the driver binding out until there is hardware to prove it on, keeping at most a host-tested EC protocol library, as P02M0099 did for [HID over I2C](/data/yellow/libersystem/docs/todo/P02M0099.md:5864), or name a real oracle. In either case, state where the EC transport sits relative to the interpreter.

9. **Medium - The interpreter item omits the handshakes real firmware gates its behaviour on (`_OSI`, `_OSC`, and the processor `_PDC`/`_OSC`) and dynamic table loading, so steps 2 and 3 would not deliver on the laptops they name.**

   [The interpreter item](/data/yellow/libersystem/docs/todo/P02M0196.md:43) loads "the DSDT and every SSDT", and [step 3](/data/yellow/libersystem/docs/todo/P02M0196.md:63) evaluates `_PSS`, `_CST` and `_CPC`. Firmware commonly depends on these handshakes:
   - it selects behaviour by the `_OSI` strings the OS answers ([ACPI 6.6, chapter 5](https://uefi.org/specs/ACPI/6.6/05_ACPI_Software_Programming_Model.html));
   - it grants native PCIe hot-plug, AER and PME only through the host bridge's `_OSC` ([ACPI 6.6, chapter 6](https://uefi.org/specs/ACPI/6.6/06_Device_Configuration.html)), and advertises CPPC through the platform-wide `_OSC`;
   - on Intel platforms it loads the processor-power SSDTs with `Load`/`LoadTable` only after the OS declares its capabilities through `_PDC` or the processor `_OSC`.

   The kernel already runs PCIe hot-plug and AER natively, which on q35 required [switching the firmware's ACPI hot-plug off](/data/yellow/libersystem/src/harness/qemu-run.sh:2133). Once AML exists, that choice belongs to `_OSC`.

   With no stated policy for OS identity and capability handshakes, battery, I2C devices and processor states can be absent or disabled on real hardware, and the kernel and the firmware can both drive PCIe hot-plug.

   **Correct the interpreter item** to:
   - decide the `_OSI` answers (and `\_OS` and `_REV`);
   - evaluate `_OSC` on host bridges before the kernel keeps native PCIe control;
   - make the platform-wide and processor `_OSC`/`_PDC` calls with the bits the step-3 consumers need;
   - support `Load` and `LoadTable`.

   A paragraph and host tests on scripted namespaces suffice.

10. **Medium - The fixture and the interpreter's conformance suite need an ASL compiler this machine does not have, and the plan neither plans an alternative nor asks for one.**

    0196d [loads SSDTs with `-acpitable`](/data/yellow/libersystem/docs/todo/P02M0196.md:80) and [tests the interpreter](/data/yellow/libersystem/docs/todo/P02M0196.md:71) against ACPICA's AML test suite, which is ASL source. `iasl` is not installed (nor is `dtc`); the harness already avoids `dtc` [for that reason](/data/yellow/libersystem/src/harness/dma-mode-record.py:35), and a new host package is [put to the owner first](/data/yellow/libersystem/docs/todo/P02M0201.md:58). The fixture mechanisms were ["checked on this machine rather than assumed"](/data/yellow/libersystem/docs/todo/P02M0196.md:76), but the toolchain was not.

    **Correct the fixture and host-suite items** by choosing either an owner-approved `acpica-tools`, bound like the harness's other host tools, or a small in-tree AML emitter for the fixture's objects. Make the ACPICA-suite item conditional on the first.

11. **Low - SMBIOS type 38 is listed as a step-1 static source, but nothing hands the SMBIOS entry point to the system.**

    The [static sources item](/data/yellow/libersystem/docs/todo/P02M0196.md:36) lists it. The [boot hand-off](/data/yellow/libersystem/src/boot/protocol/src/lib.rs:179) carries the RSDP and the device tree but no SMBIOS pointer, and no source file mentions SMBIOS. P02M0201 relies on the record [as a cross-check](/data/yellow/libersystem/docs/todo/P02M0201.md:29).

    **Correct the static-sources item** by adding the loader and hand-off work: reading the UEFI SMBIOS configuration table on each port, a hand-off field, a bounded parser, and the statement that direct device-tree boots have no SMBIOS. The change is small.

Validation was plan, source, harness and specification inspection, plus QEMU capability queries that start no guest: `-device help` on the three `qemu-system-*` binaries. Host tools were checked with `which` and `dpkg -l`. No build, test or guest was run, and no repository file was modified.

PLANNER'S RESPONSE ON P02M0196 (2026-09-26T02:51:50Z):

Verified every finding against the working tree and the plans it cites: `src/kernel/device.rs` (one BAR and a bus/device/function per row; the catch-all loop publishes unresolved functions such as ivshmem with no BAR; DMA admission and IOMMU attach by bus/device/function), `sys_device_claim` (one `DeviceMemory` for that BAR) and `sys_interrupt_bind` (legacy vectors under the DeviceManager privilege, with no userspace caller anywhere in the tree), `object/device_memory.rs` (minted only under a claim outside tests), x86_64 `interrupts/mod.rs` (`is_bindable` limited to the sixteen legacy vectors; `dispatch` signals and sends EOI without masking), `ioapic.rs` (one I/O APIC at 0xFEC00000, MADT never enumerated), `sci.rs` (only PWRBTN/SLPBTN decoded; the storm path says "DISARM AND MASK" but only zeroes PM1 enable and drops its state; GSIs above 15 refused), `rtc/mod.rs` (CMOS behind the KERN-ARCH-023 lock), aarch64 and riscv64 `is_bindable` returning false, the fixed-address PL011, PL031, goldfish RTC and riscv64 16550, riscv64's APLIC-in-MSI-mode wired path and the statement that it drives no PLIC, `platform_event.rs` (one-byte kinds latched as a bitmap), `src/acpi` (`gpe0`/`gpe1` parsed and unused; no I/O APIC entries), `src/tpm` (`TPM2` plus `MSFT0101`), `BindingId` and `Node::new`, `provider-info`, the ABI's two transports, `BootInfo` (RSDP and DTB, no SMBIOS; no source mentions SMBIOS), the loaders' RSDP lookup, `qemu-run.sh` near line 2133 and `dma-mode-record.py`. P02M0099's identity section, SCI decision, ACPI class item, PL011 and 16550 items and HID-over-I2C precedent, and P02M0190, P02M0191, P02M0195 and P02M0197 to P02M0202 were read. Host checks: `iasl`, `dtc` and `acpiexec` are not on PATH; this QEMU (10.0.11) has `ivshmem-plain`, `vhost-user-gpio-pci`, `edu` and `tpm-tis-device` on aarch64 and riscv64, and its `ICH9-LPC` exposes `cpu-hotplug-legacy` and `acpi-pci-hotplug-with-bridge-support`. All eleven findings are valid; the plan was rewritten in place, following the cross-milestone decisions made for this round.

1. **ACCEPTED - Firmware node to PCI function join.** Step 2 now has THE COMPANION JOIN: `_ADR` resolved with `_SEG`, `_BBN` and bridge secondary buses (and the tree's PCI child `reg`) to the function, the node attached to that function's row as firmware data and never a second claimable device; the function's driver gets the node's channel (`_DSD`, `_DSM`, power methods, `Notify`) with its claim. A child that is not a PCI function and names no registers (an ACPI video output) is a METHOD-ONLY device of its own, carrying its parent's resolved function and matched by a class from a rule table (first row `video-output`). `I2cSerialBusV2`, `GpioInt`/`GpioIo` sources and tree phandles resolve to the controller's row, and the child binds through P02M0195's scoped connections. The verification checks the fixture's companions for the ivshmem and virtio-gpio functions.

2. **ACCEPTED - The claim and wired interrupts itemised.** Step 1 now lists: a discriminated platform row (several MMIO ranges, P02M0191 port ranges, declared registers, wired lines with trigger and polarity, connections, optional stream id, a state); a claim that mints each resource by kind and index as derived objects revoked at release; claim-scoped wired interrupts per architecture (x86_64 MADT I/O APIC enumeration, overrides, GSIs above 15 from a wired vector pool, `_CRS` trigger/polarity; aarch64 GIC SPI configuration; riscv64 APLIC sources); mask-until-acknowledge through the existing `SYS_INTERRUPT_ACK`; the DMA answer (non-mastering unless a stream id is described); discriminated `BindingId`, `provider-info` and manifest vocabulary (`transport = "platform"` with `hid`/`cid`/`compatible`/`table`/`class`); `lsdev` diagnostics. PART DECLINED: the PLIC half of "PLIC/APLIC" - this kernel drives no PLIC (riscv64 runs `aia=aplic-imsic`), so a machine without AIA refuses wired claims by name and that is an EXCLUDE rather than a new controller driver. The uncalled legacy bind path is retired.

3. **ACCEPTED - Trust boundary of the ACPI service.** The placement is decided (userspace service, new `FirmwareInterpreter` privilege, restart with a fresh namespace, GPEs disabled and regions revoked at its death) and the plan carries the policy table the kernel enforces: SystemMemory classes allowed and refused; SystemIO as `PortRange`s outside P02M0191's reserved set and live claims, with the SMI command port, CMOS NVRAM and PM timer kernel-mediated; PCI configuration as kernel-performed accesses with the refused offsets; the checks on every `_CRS`-derived resource before a row is published. The list of new calls is given. The fixture's ivshmem access is authorised by the FIRMWARE-HELD rule (an unclaimed function the service names as a companion becomes a claim held by the service, which no driver can take), and the boot scan records every function's BARs so the checks have data. A development-build-only carve-out admits a fixture device's `_CRS` range inside that ivshmem BAR, never in a shipping build.

4. **ACCEPTED - GPE split and the storm path.** A new step-2 item splits GPEs: the kernel clears enables and status at init, masks each asserted enabled GPE in the SCI handler, latches and delivers GPE numbers on the service's own channel, re-enables only after the service ran `_Lxx`/`_Exx` and asked for clear-and-enable, owns the runtime and wake masks (P02M0197 arms wake through it), bounds storms per GPE, and handles `GBL_STS`/`GBL_RLS` for the global lock. The existing storm path is fixed in the same item to mask the redirection entry. Oracle: q35's CPU hot-plug GPE (`\_GPE._E02` in QEMU's generated DSDT, raised by QMP `device_add` of a CPU into a free `maxcpus` slot). I could not confirm the method name without starting QEMU, so the plan says it is confirmed against this QEMU's tables before the gate is written; the ICH9 properties that govern CPU hot-plug are present.

5. **ACCEPTED - Kernel-driven devices, double identities, static identities.** Step 1 now has the KERNEL-HELD SET per architecture (published so every device is accounted for, never claimable), named HANDOFF items for the console UARTs (COM1 through P02M0191, PL011 and riscv64's 16550 through their P02M0099 items, which already require the handoff), the RTCs kept by the kernel with no handoff, identity forms for every source (`acpi:`, `dt:`, `table:`, `smbios:`, `kernel:`), and the reconciliation rule: fixed publication order, merge when two descriptions start at the same base and one contains the other (the first description's resources kept, so the TPM's claim stays the table's one page), any other overlap refused and reported. "The ARM RTC" was removed from what step 1 unblocks.

6. **ACCEPTED - One owner per class; no S3 in this milestone.** Step 2 keeps the mechanism (interpreter, namespace, `Notify`, node channel, `_DSM`/`_DSD`, EC transport) and names each class's owner: battery, AC and thermal state with P02M0099's ACPI item; lid, control-method buttons and the Time and Alarm Device with P02M0197; backlight and the ambient-light sensor with P02M0199; fans and cooling with P02M0198; UCSI with P02M0202. Step 3 evaluates the sleep packages and registers the sleep-type pairs with the kernel through P02M0197b's call (also how `\_S5` reaches power-off), runs `_PTS`/`_SST` before and `_WAK`/`_SST` after as steps of P02M0197a's transaction, and evaluates `_PRW`/`_DSW` on the nodes P02M0197 hands it; the entry itself (FACS waking vector, PM1 writes, RTC_EN, SLP_TYP|SLP_EN) is stated as P02M0197b's, so no part of it has two owners. The verification replaces "S3 and resume" with the platform half proved alone on scripted namespaces (sleep types read and registered, `_PTS`/`_SST`/`_WAK` in order); QEMU's suspend-and-wake oracle stays P02M0197d's.

7. **ACCEPTED - Fixture order and `_AEI` authority.** The fixture item now states that P02M0195's bus half (both drivers, both contracts, scoped connections, the vhost-user backend) is a prerequisite of the fixture and its HID half follows P02M0196b and the companion join; the harness raises the line through the GPIO backend's control socket. A GPIO-signalled events item says how the service gets its lines: DeviceManager mints line-scoped connections for exactly the `_AEI` lines through the controller's `CONNECT` and hands them to the service, which runs `_Exx`/`_Lxx`/`_EVT`.

8. **ACCEPTED - EC oracle and placement.** The EC transport sits inside the ACPI service as the `EmbeddedControl` handler, so `_REG` and ECDT-time access are ordered in one process; `PNP0C09` is firmware-held. QEMU has no EC, so the protocol is a host-tested library against a register model plus a recorded run on the owner's laptop (asked when the part starts), with the same standing as the DesignWare and SBSA precedents; no guest oracle is claimed.

9. **ACCEPTED - OS identity, `_OSC`, `_PDC`, dynamic loading.** Decided: `_OSI` true for the Windows strings and implemented feature strings, false for others; `\_OS` "Microsoft Windows NT"; `_REV` 2. Host-bridge `_OSC` is evaluated and, on an ACPI machine, the kernel arms native hot-plug and AER only for what it grants (it arms them at the boot scan today), with firmware-owned slots left unarmed and ACPI-driven PCI hot-plug excluded; the harness option at `qemu-run.sh` 2133 is what makes q35 grant it, and the gate checks both answers. The platform `\_SB._OSC` and processor `_OSC`/`_PDC` bits are named (processor forms limited to what P02M0198's kernel executes). `Load`/`LoadTable` are supported and bounded, `Unload` refused.

10. **ACCEPTED - No ASL compiler.** Verified absent (as is `dtc`). The verification now plans an in-tree AML emitter in the harness with the object set the fixture milestones need, written first in P02M0196b, and the Python device-tree editor extended into a shared module; the ACPICA suite is conditional on the owner approving `acpica-tools`, asked when P02M0196b starts, and without it the conformance suite is the specification's examples encoded through the emitter plus in-tree fixtures.

11. **ACCEPTED - SMBIOS hand-off.** Step 1 now plans the loader reading the UEFI SMBIOS 3.x then 2.x configuration table on each port, a field appended to `BootInfo` with no version change, a bounded host-tested parser, the type-38 record as a static source merged with `IPI0001`, and the statement that a direct device-tree boot has none.

Re-check: the plan is complete (every finding's work is an item with its decision), correct against the code read above, feasible in the stated order (P02M0191's object before step 1's port half and the service's SystemIO; P02M0195's bus half before the fixture; step 2 before P02M0195's HID half), and consistent with P02M0099's SCI decision and with the round's cross-milestone decisions; no item has two owners, and the status line and the closing list of consumer milestones are kept. No source, test or script was changed; only the plan and this response were edited.


AUDITOR'S RE-AUDIT OF PLAN P02M0196 (2026-09-26T04:03:38Z):

**Rating: 6/10.** The rewritten core - platform row and claim, wired interrupts, the kernel's policy, the GPE split and the companion join - is sound, but the fixture region's policy refuses what two consumers put in it, and several interfaces the dependent plans rely on are contradicted or undefined.

The complete history was read: the original review's eleven findings and the planner's response, which accepted all eleven and declined only the PLIC half of finding 2. The plan was checked at commit 0dd5da07 (the working tree is identical; `git diff 07371c44 0dd5da07` read) against the tree: `src/acpi` ([GPE blocks parsed](/data/yellow/libersystem/src/acpi/src/lib.rs:686), [MADT source overrides](/data/yellow/libersystem/src/acpi/src/lib.rs:896)), the kernel's table lookup ([first match only](/data/yellow/libersystem/src/kernel/smp/mod.rs:644)), `src/fdt`, `src/tpm`, the device table ([one BAR per row](/data/yellow/libersystem/src/kernel/device.rs:17)), `sys_device_claim` and [`register_derived`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1311), the x86_64 interrupt window ([fifteen bindable vectors](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:100), [EOI without a mask](/data/yellow/libersystem/src/kernel/arch/x86_64/interrupts/mod.rs:380)), the [fixed I/O APIC](/data/yellow/libersystem/src/kernel/arch/x86_64/ioapic.rs:19), the uncalled [`sys_interrupt_bind`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1624), the SCI [storm path](/data/yellow/libersystem/src/kernel/arch/x86_64/sci.rs:309), the loaders' [`rsdp: 0`](/data/yellow/libersystem/src/boot/loader/src/arch/aarch64/mod.rs:323) on aarch64 and riscv64, [`provider-info`](/data/yellow/libersystem/src/idl/device.lsidl:230), [`BindingId`](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:739) and the harness's [q35 hot-plug option](/data/yellow/libersystem/src/harness/qemu-run.sh:2140); also QEMU 10.0's sources for the q35 DSDT, its `_OSC` and `\_GPE._E02`, the VGA, TPM and IPMI nodes and SMBIOS type 38. All nine consumer plans and the parts of P02M0099 and P02M0181 the plan cites were read against it. The corrections of findings 2, 4, 7, 8, 9, 10 and 11 hold and the PLIC decline is justified (this kernel drives no PLIC); findings 1, 3, 5 and 6 are corrected in substance but leave the gaps below.

1. **High - The SystemMemory policy and the fixture carve-out take the UCSI mailbox away from AML, but UCSI's `_DSM` is AML that reads and writes that mailbox, so P02M0202's transport cannot work in the fixture or on hardware.**

   The policy refuses the service ["a claimed device's ranges"](/data/yellow/libersystem/docs/todo/P02M0196.md:144). The carve-out admits a `_CRS` range inside the ivshmem BAR for a platform device, names ["a harness-played UCSI mailbox"](/data/yellow/libersystem/docs/todo/P02M0196.md:163) as its example, and removes that range from "what the service maps".

   P02M0202 makes the mailbox [the claim's `_CRS` range](/data/yellow/libersystem/docs/todo/P02M0202.md:121) - on hardware firmware-reserved memory, which the policy otherwise admits - and calls `_DSM` function 1 after every write of CONTROL and MESSAGE_OUT and function 2 before every read of CCI and MESSAGE_IN ([transport](/data/yellow/libersystem/docs/todo/P02M0202.md:126)). Its fixture's `_DSM` ["copies CONTROL and MESSAGE_OUT into an outbound staging area"](/data/yellow/libersystem/docs/todo/P02M0202.md:206) and copies the inbound area into CCI and MESSAGE_IN. Those copies are AML field accesses to the carved-out range. A second region over it, declared by the `PNP0CA0` node, is refused as well: it lies in a BAR that is not that node's companion's, and it is a claimed device's range.

   Real firmware has the same shape: the mailbox is both the device's `_CRS` memory and an operation region its own `_DSM` uses. Linux's ACPI UCSI driver said so when it had to release ACPI's reservation before mapping it: "The memory region for the data structures is used also in an operation region" (https://github.com/torvalds/linux/blob/v4.19/drivers/usb/typec/ucsi/ucsi_acpi.c). The driver evaluates the `_DSM` after it holds the claim, so the service first touches the region after the claim, and the policy refuses it. This continues original finding 3.

   **Correct the SystemMemory policy and the carve-out.** A region inside a namespace device's own `_CRS` memory stays mappable by the service while that device is claimed - its driver and its own methods share it - and the carve-out admits the fixture's `_CRS` range for the claim without removing it from the service's mapping.

2. **High - P02M0198's fixture puts processor registers the kernel writes inside the fixture region, and neither the SystemMemory policy nor the carve-out admits them, so P02M0198e cannot pass.**

   P02M0198 refuses a SystemMemory register that is ["inside a claim"](/data/yellow/libersystem/docs/todo/P02M0198.md:102), admits the rest "under the memory-class policy the kernel enforces for the ACPI service's SystemMemory regions (P02M0196b)", and says the kernel writes every processor register, ["the fixture's included"](/data/yellow/libersystem/docs/todo/P02M0198.md:106). Its fixture places the `_PCT` control register and the `_CPC` desired-performance register ["on a page of their own in the region the harness reads"](/data/yellow/libersystem/docs/todo/P02M0198.md:221).

   That region is the [ivshmem BAR](/data/yellow/libersystem/docs/todo/P02M0196.md:312), whose function becomes FIRMWARE-HELD, ["a claim held by the service"](/data/yellow/libersystem/docs/todo/P02M0196.md:146), so the registers are inside a claim. The memory-class policy refuses ["any BAR"](/data/yellow/libersystem/docs/todo/P02M0196.md:144) except the service's own region, and the carve-out admits only ["a `_CRS` memory range"](/data/yellow/libersystem/docs/todo/P02M0196.md:162) for a platform device. No item of this plan admits a register the kernel installs. This continues original finding 3.

   **Correct the carve-out** so that, in the development build only, it also admits the registers P02M0198's install check names inside the firmware-held ivshmem BAR, and say that the service's firmware-held claim does not count as a claim for them.

3. **Medium - The `video-output` class rule requires an adapter with `_DOD` and `_DOS`, while P02M0199, which defines the class, requires `_DOD` or `_DOS`, and its fixture satisfies only the second.**

   This plan: ["a child of a companion that has `_DOD`"](/data/yellow/libersystem/docs/todo/P02M0196.md:215) and `_DOS`. P02M0199: ["a PCI companion that has `_DOD` or `_DOS`"](/data/yellow/libersystem/docs/todo/P02M0199.md:135). P02M0199's fixture extends QEMU's own node for the VGA function and adds only ["an adapter `_DOS`"](/data/yellow/libersystem/docs/todo/P02M0199.md:270), and QEMU gives that node `_S1D`, `_S2D` and `_S3D` and nothing else (https://github.com/qemu/qemu/blob/v10.0.0/hw/display/acpi-vga.c). Built to this plan's text, the output is never classed and P02M0199's ACPI gate cannot pass. Linux's `acpi_is_video_device` tests the OR (https://github.com/torvalds/linux/blob/master/drivers/acpi/scan.c). This continues original finding 1.

   **Correct the class row** to "`_DOD` or `_DOS`".

4. **Medium - The device-tree item delivers a node's own `reg` and properties, which is not what its two device-tree consumers were promised: the TPM's claim is one 4 KiB page, and TCPCI's connector description is a child node.**

   The item translates `reg` and hands the driver ["the node's property block"](/data/yellow/libersystem/docs/todo/P02M0196.md:121). Only the `TPM2` row is cut to the [locality-0 page](/data/yellow/libersystem/docs/todo/P02M0196.md:99); the tree's TPM node appears only in [what step 1 unblocks](/data/yellow/libersystem/docs/todo/P02M0196.md:126). P02M0190 states ["THE RESOURCE RULE the identity step implements"](/data/yellow/libersystem/docs/todo/P02M0190.md:67): 4 KiB at the base, the base being the node's `reg` on the tree, and ["Localities above 0 are excluded"](/data/yellow/libersystem/docs/todo/P02M0190.md:72). QEMU's `tcg,tpm-tis-mmio` node declares a `reg` of 0x5000 (`add_tpm_tis_fdt_node`, https://github.com/qemu/qemu/blob/v10.0.0/hw/core/sysbus-fdt.c), so the claim would map all five localities.

   P02M0202c reads ["the `usb-c-connector` child"](/data/yellow/libersystem/docs/todo/P02M0202.md:159) with `power-role`, `sink-pdos` and `op-sink-microwatt` "from the device tree or `_DSD`", and its tree fixture has [that child](/data/yellow/libersystem/docs/todo/P02M0202.md:229). The claim is the only path this plan gives a driver to tree data, and a sink with no readable description ["runs no Power Delivery at all"](/data/yellow/libersystem/docs/todo/P02M0202.md:161), so the aarch64 and riscv64 cases would negotiate nothing. The ACPI path is covered by `_DSD`'s [hierarchical data](/data/yellow/libersystem/docs/todo/P02M0196.md:224).

   **Correct the device-tree item:** a `tcg,tpm-tis-mmio` node's row is the 4 KiB page at its translated `reg` base, as the `TPM2` row is, and the bounded property block includes the node's child nodes.

5. **Medium - `GeneralPurposeIo` regions and `GpioIo` connections are to be served through P02M0195's line-scoped connections, but P02M0195's line contract is input-only and excludes output lines.**

   This plan serves [`GeneralPurposeIo` fields](/data/yellow/libersystem/docs/todo/P02M0196.md:179) through a line-scoped connection and resolves [`GpioIo` resource sources](/data/yellow/libersystem/docs/todo/P02M0196.md:216) to "P02M0195's scoped connections". P02M0195's contract is ["input lines only"](/data/yellow/libersystem/docs/todo/P02M0195.md:59), its scope is ["one line with its trigger and polarity"](/data/yellow/libersystem/docs/todo/P02M0195.md:67), and it excludes ["GPIO output lines"](/data/yellow/libersystem/docs/todo/P02M0195.md:207). A `GeneralPurposeIo` field write drives the pin: Linux's `acpi_gpio_adr_space_handler` sets the raw output value (https://github.com/torvalds/linux/blob/master/drivers/gpio/gpiolib-acpi-core.c). One implementer would add outputs to a contract whose owner excludes them; another would refuse the writes.

   **Correct the interpreter and connection items:** serve `GeneralPurposeIo` reads and `GpioIo` connections for input lines only, refuse and report writes, and add GPIO output to EXCLUDES - or name the milestone that adds output lines.

6. **Medium - "SMBIOS type 38 and `IPI0001` are one device" rests on a range merge that type-38 records do not fit, and P02M0201's gate boots the case where it does not.**

   The identity is [`smbios:38#n`](/data/yellow/libersystem/docs/todo/P02M0196.md:48), the record is ["merged with its `IPI0001` node by resources"](/data/yellow/libersystem/docs/todo/P02M0196.md:110), and a merge happens when ranges ["START AT THE SAME BASE"](/data/yellow/libersystem/docs/todo/P02M0196.md:52), the row keeping the first description's resources. A type-38 record carries a base and a register spacing, not a length, and for SSIF its base is the SMBus address. QEMU emits such a record for `smbus-ipmi` (`smbus_ipmi_get_fwinfo`, https://github.com/qemu/qemu/blob/v10.0.0/hw/ipmi/smbus_ipmi.c; the address is shifted into the base field in https://github.com/qemu/qemu/blob/v10.0.0/hw/smbios/smbios_type_38.c).

   P02M0201 boots [ISA KCS, BT and SSIF together](/data/yellow/libersystem/docs/todo/P02M0201.md:246) and expects P02M0196a to merge ["when both start at the same base - one device - or refuses and reports the pair when they disagree"](/data/yellow/libersystem/docs/todo/P02M0201.md:123). This plan refuses only overlaps, so the SSIF record becomes an orphan row, or, read P02M0201's way, a refusal of the `IPI0001` node that gate needs. For KCS and BT the merged row keeps the SMBIOS-derived range, from whose length P02M0201's driver [derives the register spacing](/data/yellow/libersystem/docs/todo/P02M0201.md:83), and no item says how that length is computed. This continues original finding 5.

   **Correct the static-sources item:** a type-38 record becomes a range of the interface's register count times its spacing; an SSIF record has no range and is matched to its `IPI0001` node by the `I2cSerialBusV2` address, or kept as an unclaimable description; and say which disagreements are refused.

7. **Medium - `PNP0C01`/`PNP0C02` ranges "join the kernel's reserved list", which is undefined, and read as P02M0191's reserved set it refuses, on real chipsets, the registers other plans mint.**

   [The namespace item](/data/yellow/libersystem/docs/todo/P02M0196.md:201) does not say what joining does. The only reserved set the plans define is P02M0191's, ["refused at every mint"](/data/yellow/libersystem/docs/todo/P02M0191.md:85); it grows at run time, [refuses an addition overlapping a reserved port](/data/yellow/libersystem/docs/todo/P02M0191.md:96), and is where a later kernel item goes ["rather than a new list"](/data/yellow/libersystem/docs/todo/P02M0191.md:98). Firmware's `PNP0C02` covers the whole ACPI I/O block and the POST port: coreboot's ICH9 `LDRC` reserves `DEFAULT_PMBASE` for 0x80 bytes and port 0x80 (https://github.com/coreboot/coreboot/blob/main/src/southbridge/intel/i82801ix/acpi/lpc.asl). Read that way, on such a board it refuses P02M0200's TCO sub-range at [PM base + 0x60](/data/yellow/libersystem/docs/todo/P02M0200.md:160), a WDAT's [port ranges](/data/yellow/libersystem/docs/todo/P02M0200.md:188) and every AML region over the block or port 0x80, while the reservation itself overlaps the FADT's PM1 block and cannot join. q35's only such node is the `PNP0C01` over ECAM (`build_q35_dram_controller`, https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/acpi-build.c), so no gate would show it. Linux records these reservations without making them busy (https://github.com/torvalds/linux/blob/master/drivers/pnp/system.c).

   **Correct the namespace item:** a reservation is accounted for and keeps other rows (and, where the kernel places BARs, BAR assignment) out of its ranges; it does not enter P02M0191's reserved set, and mints inside it stay governed by the reserved set, claims and the service policy.

8. **Medium - Step 3 and P02M0197a/b still depend on each other with no stated order, and neither plan names the interface through which ServiceManager asks the ACPI service for the sleep steps.**

   Step 3 registers the sleep types ["through P02M0197b's registration call"](/data/yellow/libersystem/docs/todo/P02M0196.md:268) and runs `_PTS`, `_SST` and `_WAK` ["as steps of P02M0197a's"](/data/yellow/libersystem/docs/todo/P02M0196.md:270) transaction. P02M0197b's registration takes [step 3's evaluation](/data/yellow/libersystem/docs/todo/P02M0197.md:151), its soft-off writes [the registered `\_S5`](/data/yellow/libersystem/docs/todo/P02M0197.md:171), and its gate needs it [with the service running](/data/yellow/libersystem/docs/todo/P02M0197.md:276). Neither plan says which lands first, that they land together, or who introduces the call, as this plan does for the claim's [mint call](/data/yellow/libersystem/docs/todo/P02M0196.md:70). So original finding 6's "cannot close before a milestone that cannot start before it" has moved from the gate into the item. P02M0197's step 5 has ServiceManager ["hand the ACPI service the namespace nodes"](/data/yellow/libersystem/docs/todo/P02M0197.md:84) and run `_PTS`; the node channel refuses [`_PTS` and `_WAK`](/data/yellow/libersystem/docs/todo/P02M0196.md:227), and the only contract this plan names for the service is [P02M0198's](/data/yellow/libersystem/docs/todo/P02M0196.md:278).

   **Correct step 3:** state that its sleep item lands with P02M0197a/b, the registration call introduced by whichever lands first, and name the contract the service serves ServiceManager, through a manifest client role, for `_PTS`, `_SST`, `_WAK` and the wake methods.

Validation: plan, audit, source and harness inspection; `git show 07371c44:docs/todo/P02M0196.md` and `git diff 07371c44 0dd5da07`; read-only QEMU 10.0.11 property queries that start no machine (`-device help` on the three binaries, `-device ICH9-LPC,help`, `-machine q35,help`); primary sources fetched read-only (QEMU v10.0.0, Linux, coreboot). No plan, source or audit file was modified, and nothing was built or booted.


PLANNER'S RESPONSE ON P02M0196 (2026-09-26T05:34:17Z):

Every finding of the re-audit was checked against the plan, the consumer plans as they stand (P02M0190, P02M0191, P02M0195, P02M0197 to P02M0202, and P02M0099's identity, UART and ACPI class items), the tree and the primary sources. From the tree: `object/device_memory.rs` (a `DeviceMemory` is always mapped uncached); the loader's `region_kind` in `src/boot/uefi/src/memory.rs`, which folds EfiMemoryMappedIO into `MEM_RESERVED`; `kernel/device.rs` (`attach_events`, the device-event channel DeviceManager listens on); `smp/mod.rs` (the first-match table lookup); and ServiceManager's `bootstrap.rs` and `lifecycle.rs` (each service's report channel is kept as its control channel, which carries `STOP` and LogService's `FLUSH`). From the primary sources: QEMU v10.0.0's `hw/core/sysbus-fdt.c`, `hw/display/acpi-vga.c`, `hw/smbios/smbios_type_38.c`, `hw/acpi/ipmi.c`, the IPMI interface models and `hw/i386/acpi-build.c`; Linux's `drivers/acpi/scan.c`, `drivers/pnp/system.c`, `drivers/usb/typec/ucsi/ucsi_acpi.c`, `drivers/gpio/gpiolib-acpi-core.c` and the `gpio-virtio.yaml` binding; coreboot's ICH9 `lpc.asl`; and a ThinkPad X1 Carbon 6 UCSI DSDT excerpt. All eight findings are valid, and the plan now also carries the two cross-plan decisions that land in it. Summary: 8 accepted, 0 rejected.

1. **ACCEPTED - The policy took the UCSI mailbox away from the node's own AML.** The plan refused the service "a claimed device's ranges", and the carve-out removed the fixture's `_CRS` range "from what the service maps". P02M0202b calls `_DSM` function 1 after every write of CONTROL and MESSAGE_OUT and function 2 to read VERSION and to poll CCI through the reset, and the firmware refreshes CCI and MESSAGE_IN in the node's own code before its `Notify`; its fixture's `_DSM` and notification path copy between the mailbox and staging areas. Real firmware has the same shape. In the X1 Carbon 6 DSDT, `\_SB.UBTC` (`_CID` `PNP0CA0`) returns a `Memory32Fixed` at `UBCB` from `_CRS` and, in the same node, declares `OperationRegion (USBC, SystemMemory, UBCB, 0x38)`. Its `_DSM` functions 1 and 2 (`ECWR`, `ECRD`) and its `NTFY` method (`ECRD` just before `Notify(0x80)`) access that region. Linux maps the same range `MEMREMAP_WB` for the driver. So both hold the range after the claim, and the policy refused one of them. Two gaps also showed. First, the plan named no memory type, while `DeviceMemory` is always uncached. Second, the loader folds firmware MMIO into "reserved", so the kernel cannot yet tell firmware-reserved RAM from MMIO. Plan changes:
   - THE POLICY's SystemMemory bullet now has TWO ADMISSIONS. The first: a region that a claimed namespace device's OWN node declares, inside that device's own `_CRS` memory range, stays mappable while the device is claimed, because the driver and the node's `_DSM` and pre-`Notify` code share that memory. A region any other node declares over a claimed range stays refused, and a claim is refused, and reported, while another node's region maps any part of its ranges, so the rule holds whichever comes first. The second admission is the existing firmware-held PCI rule.
   - The same bullet adds ONE MEMORY TYPE PER RANGE for the service's mappings and a claim's alike: write-back for ACPI NVS, ACPI-reclaimable and firmware-reserved memory, uncached for MMIO. The loader then keeps MMIO as a boot-memory-map kind of its own, appended.
   - The `_CRS`-derived check reads "not over RAM - ACPI NVS and firmware-reserved memory excepted, since firmware places mailboxes there".
   - The carve-out's `_CRS` range is admitted for the claim and stays reachable by that device's own node.
   - THE REGION gains a LAYOUT rule: the companion's region covers the harness's own pages only, and each owner's device declares its own region over its `_CRS` range.
   - The x86_64 verification claims the fixture device with a `_CRS` range in the ivshmem BAR. Its own method reads and writes that range while the claim holds it, and another node's region over it is refused.
   - The host suites test both admissions and the claim refused under another node's region.

2. **ACCEPTED - P02M0198's fixture registers were refused by every rule.** P02M0198's install check refuses a register "inside a claim" and otherwise defers to this plan's memory-class policy, and its fixture puts the `_PCT` and `_CPC` registers on a page of their own in the ivshmem BAR. That BAR's function becomes FIRMWARE-HELD ("a claim held by the service"), the policy refuses any BAR, and the carve-out admitted only a `_CRS` range, so nothing admitted a register the kernel installs. Plan changes:
   - The FIXTURE CARVE-OUT (development build only) now admits two things inside the firmware-held `ivshmem-plain` (1af4:1110) BAR. The first is the `_CRS` range. The second is the SystemMemory registers of a processor table P02M0198's install check names, admitted for the kernel's install. The service's firmware-held claim does not count, for them, as a claim, and they are carved out of what the service maps: no region of the service's covers them while the table stands. "Every shipping build refuses both."
   - THE REGION's layout gives processor registers a page no region covers.

3. **ACCEPTED - `video-output` must be "`_DOD` or `_DOS`".** QEMU v10.0.0's `acpi-vga.c` gives the VGA node `_S1D`, `_S2D` and `_S3D` and nothing else. P02M0199's fixture adds only an adapter `_DOS`, P02M0199 defines the class with "or", and Linux's `acpi_is_video_device` tests `_DOD || _DOS`. Plan change: the class row in THE COMPANION JOIN reads "a child of a companion that has `_DOD` or `_DOS`, itself having `_BCL` and `_BCM`".

4. **ACCEPTED - The tree TPM row was not one page, and child nodes were not delivered.** `add_tpm_tis_fdt_node` sets `reg` to 0x5000 and emits no interrupt, and P02M0190's resource rule is one 4 KiB locality-0 page with no interrupt. P02M0202c reads the `usb-c-connector` child, and a sink without it negotiates nothing. Plan changes:
   - THE DEVICE TREE: a row carries its translated `reg` whole, except a `tcg,tpm-tis-mmio` node's. That row is ONE MMIO range, the 4 KiB locality-0 page at its translated `reg` base, with no interrupt, exactly as the `TPM2` row is. The bounded property block includes the node's CHILD NODES and their properties.
   - The aarch64 and riscv64 verification publishes the TPM node "as its locality-0 page".
   - The host suites check the TPM node cut to its page and child nodes in the property block.

5. **ACCEPTED - `GeneralPurposeIo` and `GpioIo` must be input-only.** P02M0195's line contract is input-only, its scope is one line with its trigger, and it excludes GPIO output. Linux's `acpi_gpio_adr_space_handler` shows that a field write drives the pin. It also shows that a field read may use a line that `_AEI` already holds. Because P02M0195's controller grants each line to one connection, the plan must say how such a read is served. Plan changes:
   - THE INTERPRETER serves `GeneralPurposeIo` FOR INPUT READS ONLY through a line-scoped connection, and a write is refused by name and reported. A read of a line the service already holds for `_AEI` is served on that connection. `GenericSerialBus` goes through an address-scoped connection, and a protocol the controller does not declare is refused.
   - CONNECTIONS takes a `GpioIo` only as an input line and refuses one restricted to output by name.
   - EXCLUDES gains GPIO output lines (a `GeneralPurposeIo` write, an output `GpioIo`) "until a board or a consumer needs one".
   - The emitter's list names `GpioIo` and a field's `Connection`, the hostile-AML suite includes a `GeneralPurposeIo` write, and the x86_64 verification reads such a field and refuses a write to it.

6. **ACCEPTED - SMBIOS type 38 cannot be merged by range.** `smbios_type_38.c` ORs 1 into an I/O base (QEMU's KCS at 0xCA2 appears as 0xCA3), shifts an SSIF address left by one, and records a register spacing but no length. QEMU's `IPI0001` nodes carry `_CRS` (an I/O range, or an `I2cSerialBusV2` whose source is the parent) and `_IFT`. The PCI forms emit no record. A base-and-containment merge therefore never matches SSIF and needs an invented length for KCS and BT. Plan changes:
   - IDENTITY FORMS: an SMBIOS IPMI record is no row's identity. `smbios:38#n` is a match id the agreeing `IPI0001` row gains.
   - The merge rule's examples no longer name SMBIOS.
   - THE STATIC SOURCES: SMBIOS type 38 creates NO ROW. When step 2 publishes an `IPI0001` row, the agreeing record is attached as that row's match id. Agreement means the record's interface type equals the node's `_IFT`, and its address equals the node's. For KCS and BT, the record's decoded base (its low bit marks I/O space, and the modifier byte gives address bit 0) must equal the base of the node's `_CRS` range. For SSIF, the base shifted right by one (the seven-bit address) must equal the `I2cSerialBusV2` address. A record that no node takes, or that disagrees, is reported and changes nothing, and the node is used either way.
   - NAMESPACE DEVICES reports an `IPI0001` node with its `_IFT` for that check.
   - WHAT STEP 1 UNBLOCKS is restated, and the host suites test the attach rule.

7. **ACCEPTED - "Join the kernel's reserved list" was undefined and harmful.** P02M0191's reserved set is refused at every mint and refuses additions that overlap it. Coreboot's ICH9 `LDRC` (`PNP0C02`) reserves the whole PM block (0x80 bytes), port 0x80, 0xB2 and the GPIO base, so under that reading the TCO sub-range, a WDAT's ports and AML regions would be refused. q35's only such node is the `PNP0C01` over ECAM. Linux's `drivers/pnp/system.c` requests these ranges and clears `IORESOURCE_BUSY` "after PCI claim BARs, but before PCI assign resources". Plan changes:
   - The row states gain RESERVATION.
   - NAMESPACE DEVICES: `PNP0C01` and `PNP0C02` are RESERVATIONS - rows listed by `lsdev`, never claimable, never merged, and reported before the other devices of each walk. A reservation keeps every new row published after it (and BAR placement, where the kernel places BARs) out of its ranges. It leaves alone the rows published before it and the descriptions merged into them. It never enters P02M0191's reserved set, and a mint inside it (the TCO sub-range, a WDAT port range, an AML region) stays governed by the reserved set, claims and the service policy.
   - The merge rule and the `_CRS`-derived check defer to that reservation rule, the new calls report a reservation, and the host suites test the rule.

8. **ACCEPTED - Step 3 and P02M0197a/b had no order and no interface.** The plan relied on "P02M0197b's registration call" and "steps of P02M0197a's transaction", while P02M0197b needs step 3 and the node channel refuses `_PTS` and `_WAK`. ServiceManager does hold a control channel per service (its report channel, which carries `STOP` and `FLUSH`). Plan changes:
   - The step 3 sleep item LANDS TOGETHER WITH P02M0197a AND P02M0197b. The registration call is introduced by P02M0197b and admitted by the `FirmwareInterpreter` privilege.
   - The steps around the entry travel on the control channel ServiceManager holds for the service, the one its sleep notice travels on (declared in its manifest row). They form a `platform-sleep` interface in `liber:process@1` beside `system-sleep`. `prepare(state, wake nodes)` evaluates `_PRW` (turning on its power resources) and `_DSW` or `_PSW`, sets the wake GPEs, then runs `_PTS` and `_SST`. `wake(state)` runs `_WAK` and `_SST`, then disarms what `prepare` armed.
   - The node-scoped channel still refuses `_PTS` and `_WAK`.
   - The host suites test `prepare` and `wake` in order, and the closing list notes the joint landing under P02M0197.

Coordinated changes: two decisions made for other plans' findings land in this plan.
- The `_AEI` re-grant. WHERE AML RUNS now says DeviceManager reaches the service through a manifest client role, for node channels and to hand over connections. A dead instance's connections close and are granted again to the new instance. Each instance reports "namespace loaded" through the kernel, which passes the report to DeviceManager on its device-event channel after every row the walk published, naming the instance. GPIO-SIGNALLED EVENTS has DeviceManager mint the `_AEI` lines AGAIN for each new instance on that report, as well as after the controller rebinds, and grants the lines and addresses the service's own `GeneralPurposeIo` and `GenericSerialBus` fields name the same way. THE NEW CALLS report those lists with the controller's companion or row, and the namespace loaded. Because these grants are built here on P02M0195a's scoped `CONNECT`, the x86_64 verification now joins the virtio-i2c companion, reads a `GeneralPurposeIo` field, reads and writes a `GenericSerialBus` field at the bus fixture's 0x50, and delivers the `_AEI` line again after the service is killed and restarted.
- The tree GPIO interrupt. THE DEVICE TREE says an interrupt specifier (`interrupts-extended`, or `interrupts` with `interrupt-parent`) is a WIRED line where its parent is the kernel's interrupt controller. It is a LINE CONNECTION where the parent node is both `gpio-controller` and `interrupt-controller` (the virtio-gpio function's `virtio,device29` node, whose binding sets `#interrupt-cells` to 2): the line is the first cell and the trigger the second, it is made by the companion join, and it is never a wired interrupt. An interrupt whose parent is neither is listed as unresolved, and a `reg` below an I2C controller's node is a bus address, not MMIO. CONNECTIONS replaces "a tree's GPIO phandle" with those words. The aarch64 and riscv64 verification extends the tree with the virtio-gpio function's node and a fixture-only node whose `interrupts-extended` names it: the node binds through a line connection and receives a raised line. The tree host suite covers it too.

Re-check of the whole plan: the plan was re-read end to end against the consumer plans, as their planners have revised them today, and the code above. Four inconsistencies were found and fixed.
- WHAT DEPENDS ON IT put the lid and buttons among P02M0181's producers, although the closing section gives them to P02M0197.
- The method-only definition ("a node below a companion ... names no registers of it", with a claim "carrying only the channel") also covered QEMU's ISA `IPI0001` KCS and BT nodes. Those sit below the LPC function's companion and have port resources. The join now says that such a node is a platform device with its own `_CRS` resources, and is method-only only when those name no memory, port or wired line (the video output, HID over I2C, SSIF). Its claim then carries the channel and its connections.
- The x86_64 "every device accounted for" list gains "refused" for descriptions the checks refuse.
- The `_CRS`-derived check now says a description merged into an earlier row adds no resource and is not refused by it. Otherwise the `PNP0501` node over COM1, whose ports P02M0191 keeps in the reserved set while the kernel drives them, would have been refused instead of merged.
The order still holds: P02M0191's object comes before step 1's port half, P02M0195's bus half before the fixture, and step 2 and the join before P02M0195's HID half. Step 3's sleep item now lands with P02M0197a/b, which follow P02M0198a. Every consumer's expectation matches the text:
- P02M0190: the TPM page on both paths.
- P02M0195: the `_AEI`, `GeneralPurposeIo` and `GenericSerialBus` grants, and input-only lines.
- P02M0197: `platform-sleep` and the wake GPE interface.
- P02M0198: the carve-out and the processor contract.
- P02M0199: `_DOD` or `_DOS`, and the parent `_DOS`.
- P02M0200: WDAT rows untouched by reservations, and the TCO mint.
- P02M0201: the attach rule.
- P02M0202: the own-node admission and the connector child.
The owner's approval stands. The deferred questions (installing `acpica-tools`, and the laptop for the embedded controller) are still asked when their part starts. The status line and the closing list are kept, the text is ASCII with every line at most 112 columns (the widest original lines), and nothing points at an audit or a coordination number.

Only `docs/todo/P02M0196.md` was edited and this response appended; no source, test, script or other plan was changed, and nothing was built or booted.


AUDITOR'S RE-AUDIT OF PLAN P02M0196 (2026-09-26T15:50:59Z):

**Rating: 8/10.** The last round's eight corrections hold, and the consumer plans now rely on what this plan provides. Three things are left: where the ACPI service starts and how it restarts, one kernel-held device the SystemIO policy leaves reachable, and reservations that the resource checks refuse.

The complete history was read: the original review's eleven findings and the planner's response, the previous re-audit's eight findings (6/10), and the planner's latest response. That response accepted all eight and added two coordinated changes, the `_AEI` re-grant and the device-tree GPIO interrupt. The plan's latest changes were read with `git diff -- docs/todo/P02M0196.md`. Every consumer was read from the working tree for what it takes from this plan:
- P02M0099: the `kernel:com1` row.
- P02M0190: the TPM page on both paths.
- P02M0191: the reserved set, the mint sources and reservations.
- P02M0195: the line contract and the service's grants.
- P02M0197: `platform-sleep`, the joint landing and the registration call.
- P02M0198: the carve-out and the install check.
- P02M0199: `video-output` and the parent `_DOS`.
- P02M0200: the WDAT rows and the TCO sub-range.
- P02M0201: the type-38 attach.
- P02M0202: the shared mailbox and the connector child.
- `TODO.md`.

The planner's claims about the tree were checked in the code: `region_kind` in `src/boot/uefi/src/memory.rs`, `DeviceMemory`, the device-event channel in `src/kernel/device.rs`, and ServiceManager's control channel in `bootstrap.rs` and `lifecycle.rs`. For the new "namespace loaded" and client-role text, these were read too: the service manifest, the role rules in `system-manifest`, DeviceManager's bind phases and ServiceManager's restart ladder. Primary sources: QEMU v10.0.0's `hw/i386/acpi-build.c` and `hw/isa/lpc_ich9.c`, and coreboot's ICH9 `lpc.asl`.

These corrections hold and are not repeated:
- finding 1: the own-node admission, and one memory type per range;
- finding 2: the carve-out's processor registers, which now match P02M0198;
- finding 3: `_DOD` or `_DOS`;
- finding 4: the tree TPM cut to its page, and child nodes in the property block;
- finding 5: input-only GPIO, served on P02M0195's level-read scope;
- finding 6: the type-38 attach, which matches P02M0201;
- finding 8: the joint landing with P02M0197a/b, and `platform-sleep`, which match P02M0197.

The two coordinated changes match P02M0195 and P02M0202. Finding 7's correction matches P02M0191 and P02M0200 but leaves the gap in finding 3 below.

1. **Medium - The plan does not say where the ACPI service starts, and three things this round ties to that start need positions this tree cannot give together: DeviceManager's manifest client role, the first bind round's wait and the restart the x86_64 gate performs.**

   The plan says four things:
   - DeviceManager ["reaches it through a manifest client role"](/data/yellow/libersystem/docs/todo/P02M0196.md:152);
   - a crashed service ["is restarted with a fresh namespace"](/data/yellow/libersystem/docs/todo/P02M0196.md:153);
   - ["DeviceManager's first bind round waits, bounded, for that report"](/data/yellow/libersystem/docs/todo/P02M0196.md:159);
   - the gate [kills and restarts the service](/data/yellow/libersystem/docs/todo/P02M0196.md:400).

   No position in this tree satisfies all of them:
   - A role's provider must be a declared dependency, or `system-manifest` refuses the manifest ([the rule](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1478)). DeviceManager is [pinned](/data/yellow/libersystem/src/user/services/manifest.toml:903), its only dependency is [`log_service`](/data/yellow/libersystem/src/user/services/manifest.toml:3517), and StorageService [depends on DeviceManager](/data/yellow/libersystem/src/user/services/manifest.toml:3796). So the role makes the ACPI service start before DeviceManager, before any volume is mounted. The service must therefore be pinned.
   - A pinned service cannot be restarted today. The ladder ["relaunches from the volume"](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1376), both relaunch paths launch from it ([ordinary](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1500), [plan-driven](/data/yellow/libersystem/src/user/services/core/src/service_manager.rs:1574)), and no pinned service in the manifest is `transparent`. The plan adds no relaunch from the init package, so the gate's restart case has no mechanism to use.
   - The ladder can restart a volume-staged service, but that service cannot be DeviceManager's dependency. It is loaded from the volume, and StorageService mounts the volume only after DeviceManager's phase one. That also breaks the wait. DeviceManager's first bind round is phase one, which binds only the boot-critical drivers staged in `init.pkg` to mount the volume ([phase one](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:1198), [everything else waits for a volume](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:7014)). A volume-staged service cannot report before that round ends, so the wait would run to its bound on every ACPI boot.

   This is a new finding. The client role and the restart case were added in this round; the first-round wait was already in the plan.

   **Correct the WHERE AML RUNS item:** state the service's stage, and make the role, the wait and the restart follow from it. There are two consistent choices:
   - The service is volume-staged and restarted by the existing ladder. The wait comes before DeviceManager's phase two. DeviceManager gets the service's endpoint without a manifest role of its own on it, for example from ServiceManager, which already hands it StorageService with the [`DRIVERS` message](/data/yellow/libersystem/src/user/services/core/src/service_manager/bootstrap.rs:371).
   - The service is pinned and starts ahead of DeviceManager. The relaunch from the init package that its restart needs is then added as an item.

2. **Medium - The SystemIO policy refuses only P02M0191's reserved set and live claims. So the ports of the ISA DMA controller are minted to the ACPI service whenever AML declares a region over them, although this plan withholds that controller from every claim because nothing translates its DMA.**

   - The kernel-held set includes ["the ISA DMA controller (a bus master nothing translates)"](/data/yellow/libersystem/docs/todo/P02M0196.md:60).
   - SystemIO regions are ["refused over P02M0191's reserved set ... and inside a live claim's range"](/data/yellow/libersystem/docs/todo/P02M0196.md:178). A kernel-held row is neither. P02M0191 mints the service's regions ["with the same reserved-set and exclusivity checks"](/data/yellow/libersystem/docs/todo/P02M0191.md:77) and nothing more.
   - P02M0191's fixed set lists what the kernel drives: the PIC, the PIT, 0x61 and the CMOS pair ([fixed set](/data/yellow/libersystem/docs/todo/P02M0191.md:88)). The DMA controller's channel registers at 0x00-0x1F and 0xC0-0xDF are not in it. P02M0191 reserves all of fw_cfg ["because its DMA address registers would let a holder make the device write any physical memory"](/data/yellow/libersystem/docs/todo/P02M0191.md:91). That is the reason this plan gives for holding the DMA controller. P02M0191 also assumes that kernel-held rows ["record the reserved ports they describe"](/data/yellow/libersystem/docs/todo/P02M0191.md:109), and this row does not.
   - q35 has the controllers: `ich9_lpc_realize` calls `i8257_dma_init` (https://github.com/qemu/qemu/blob/v10.0.0/hw/isa/lpc_ich9.c). A transfer needs a device on a DMA channel: QEMU's floppy controller when one is attached, or a Super I/O floppy or ECP port on a board. Where such a device exists, programming the controller writes into the first 16 MiB of physical memory, with no IOMMU in the path. The service can name any base and length for SystemIO, so AML or a defect in the service reaches kernel memory that way. Original finding 3 asked the policy to exclude exactly that. The plan's own premise is that a defect ["must cost a restart rather than the machine"](/data/yellow/libersystem/docs/todo/P02M0196.md:150).

   This is a new finding: a contradiction inside the plan's trust boundary, and with P02M0191's assumption.

   **Correct the SYSTEM I/O bullet:** also refuse regions over the DMA controller's channel and control registers, 0x00-0x1F and 0xC0-0xDF. The page registers at 0x81-0x8F cannot start a transfer and can stay mintable. That keeps firmware's POST-code regions at 0x80 working where they are wider than one byte.

   P02M0191's reserved-set item has to say the same thing, because the ACPI service's `PortRange` goes through its mint checks. P02M0191's re-audit of this date reports the gap from that side, with the same ranges.

3. **Low - The `_CRS`-derived checks refuse the reservations that the last correction introduced. Both reservations that correction was written for cover kernel-held MMIO or reserved ports, and the checks exempt only a merged description.**

   - The checks refuse MMIO over ["kernel-held ranges"](/data/yellow/libersystem/docs/todo/P02M0196.md:188) and ports ["in the reserved set"](/data/yellow/libersystem/docs/todo/P02M0196.md:189). The only exemption is ["A description merged into an earlier row"](/data/yellow/libersystem/docs/todo/P02M0196.md:191). A reservation is ["never merged"](/data/yellow/libersystem/docs/todo/P02M0196.md:244), and it is reported ["with its `_CRS` resources"](/data/yellow/libersystem/docs/todo/P02M0196.md:207) like a device.
   - q35's only reservation is `\_SB.DRAC` (`PNP0C01`). Its `_CRS` is the MCFG window (`build_q35_dram_controller`, https://github.com/qemu/qemu/blob/v10.0.0/hw/i386/acpi-build.c), the ECAM that this plan lists as [kernel-held MMIO](/data/yellow/libersystem/docs/todo/P02M0196.md:164).
   - coreboot's ICH9 `LDRC`, the example the correction answered, lists 0x61, 0xB2 (SWSMI) and the whole PM block (https://github.com/coreboot/coreboot/blob/main/src/southbridge/intel/i82801ix/acpi/lpc.asl). 0x61 is in P02M0191's [fixed set](/data/yellow/libersystem/docs/todo/P02M0191.md:88). The SMI command port, PM1, the PM timer and GPE0 are in its [FADT-derived part](/data/yellow/libersystem/docs/todo/P02M0191.md:93).
   - As written, both reservations are refused and reported instead of recorded, so the [reservation rule](/data/yellow/libersystem/docs/todo/P02M0196.md:245) never applies on either machine. On q35 the gate's ["reservation"](/data/yellow/libersystem/docs/todo/P02M0196.md:396) category stays empty, and the gate still passes.

   This is an incomplete correction of the previous re-audit's finding 7.

   **Correct the `_CRS`-derived bullet:** a reservation mints nothing, so these checks do not refuse it, just as they do not refuse a merged description. Its overlaps with rows follow the reservation rule.

Validation: read-only inspection of the audit history, the plan and its working-tree diff (`git diff`), the sibling plans and `TODO.md` in the working tree, and the source files named above (`grep`, `sed`). Primary sources were fetched read-only: QEMU v10.0.0's `hw/i386/acpi-build.c`, `hw/isa/lpc_ich9.c` and `hw/i386/pc.c`, and coreboot's ICH9 `lpc.asl`. No plan, source or audit file was modified, and nothing was built, tested, benchmarked or booted.

PLANNER'S RESPONSE ON P02M0196 (2026-09-26T17:12:45Z):

Verified read-only:
- `system-manifest`: every role whose provider is a service must name that service as a declared dependency, or the manifest is refused;
- the manifest, parsed: DeviceManager is `pinned`, `escalate`, depending on LogService alone; StorageService (pinned) depends on DeviceManager; every pinned service is `escalate`, and the `transparent` ones are all volume-staged;
- ServiceManager: `restart_service` relaunches through `launch_from_volume` and `relaunch_planned` through `launch_service_from_volume`, and nothing relaunches from the init package; the bring-up loop sends `DRIVERS` the moment `storage_service` is Ready and then blocks in `drive_runtime_drivers` on phase two's tagged answers, before `process_service` - which depends on `storage_service` - is started; the development agent's launcher reaches DeviceManager late, as `DEVPERM` on its control channel, and ConfigService's restart has a step of its own in the ladder;
- DeviceManager: phase one binds only boot-critical drivers staged in `init.pkg`, and everything else waits for a volume;
- QEMU v10.0.0's `build_q35_dram_controller` (`DRAC`, `PNP0C01`, the MCFG window) and ACPICA's protected-port table.
Summary: three findings, all accepted; the first with a correction to the placement its first option proposed.

1. **ACCEPTED - the plan did not say where the ACPI service starts, and no position satisfied the role, the wait and the restart together.** Every fact holds. One more fact decides between the two options: the auditor's first option puts the wait "before DeviceManager's phase two", but phase two also runs before a volume-staged service can exist - ServiceManager sends `DRIVERS` as soon as StorageService is ready and waits on phase two's answers before ProcessService starts - so that wait would still run to its bound on every ACPI boot. THE CHOICE: the service is VOLUME-STAGED AND `transparent`, and NO BIND ROUND WAITS. The pinned alternative would need a relaunch path from the init package for this one service, and it would still walk the namespace before any GPIO or I2C controller is bound, so the connections its fields name would be missing during that walk either way; the volume-staged form costs a late companion join, which the restart path already makes every driver handle. Plan changes, in "WHERE AML RUNS", a new paragraph "ITS STAGE, AND WHAT FOLLOWS FROM IT":
   - the service is launched through ProcessService once that is up (dependencies LogService and ProcessService) and restarted by ServiceManager's existing ladder, which relaunches from the volume and from nothing else;
   - DeviceManager holds NO MANIFEST ROLE on it; ServiceManager hands DeviceManager a client of each instance on its control channel as it comes up, a restarted one included - the late hand-off the development agent's launcher already takes, and a ladder step for this one service as ConfigService has one;
   - NO BIND ROUND WAITS FOR THE NAMESPACE: its platform devices are bound as their rows arrive, as a hot-plugged function is; a companion joined after its function's driver bound is attached then, and that driver asks for the node channel as it does after a restart; a node-channel request that reaches DeviceManager before an instance's report is answered at the report. The old sentence about the first bind round's bounded wait is gone;
   - EXCLUDES gains a system volume on a device the firmware describes only in AML, since the service starts from that volume.
   The restart case in the x86_64 gate now has its mechanism (the existing ladder), and P02M0195's "again for each new ACPI-service instance on its namespace-loaded report" and P02M0197's "SERVICEMANAGER RESTARTS NOTHING WHILE THE DRIVERS ARE SUSPENDED, because every launch reads the volume" both hold for this service unchanged.

2. **ACCEPTED - the SystemIO policy left the ISA DMA controller's registers mintable.** Plan change, the SYSTEM I/O bullet: regions are refused over P02M0191's reserved set, whose fixed part now holds the DMA controllers' channel and control registers, `0x00..0x1F` and `0xC0..0xDF`, so no region reaches the kernel-held controller and starts a transfer nothing translates; the page registers from `0x81` and a POST-code region at `0x80` stay mintable. The host suite's policy checks add a SystemIO region over a DMA channel register refused and one over `0x80..0x8F` minted.

3. **ACCEPTED - the `_CRS`-derived checks refused the reservations the last correction introduced.** `DRAC`'s `_CRS` is the ECAM, and coreboot's `LDRC` lists `0x61`, the SMI command port and the PM block, so both would have been refused and reported and the reservation rule would never have applied. Plan changes: the `_CRS`-derived bullet now says A RESERVATION MINTS NOTHING, so these checks do not refuse one, as they do not refuse a merged description; its overlaps with rows follow the reservation rule, and a reservation over kernel-held MMIO or reserved ports - `DRAC` over the MCFG window, an ICH9 board's `LDRC` over `0x61`, the SMI command port and the PM block - is recorded as one. The host suites add a reservation over kernel-held MMIO and reserved ports recorded rather than refused, and the x86_64 gate's enumeration names q35's `DRAC` among the reservations, so the gate's reservation category can no longer stay empty and pass.

Coordinated changes: P02M0191's reserved set holds the same DMA ranges (its response of this date).

Re-check of the whole plan: the service's stage now fixes how it is started, restarted and reached, and nothing in the plan waits for it; step 1's static rows (TPM2, WDAT, SPCR, the kernel-held set, `kernel:com1`) never needed the service and are unaffected. The SystemIO, SystemMemory, `_CRS` and reservation rules agree with P02M0191a and with each other. Every consumer plan read in this round still gets what it takes from this plan. The file is ASCII, cites no audit and keeps its `Status:` line.

Edited `docs/todo/P02M0196.md` (and P02M0191 for the shared ranges); no source was changed, and nothing was built or booted.
