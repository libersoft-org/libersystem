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
