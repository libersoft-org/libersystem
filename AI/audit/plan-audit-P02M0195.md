AUDITOR'S REVIEW OF PLAN P02M0195 (2026-09-25T22:54:13Z):

**Rating: 4/10.** The vhost-user emulation route is sound and was checked against this QEMU, but the plan gives no mechanism or order for the binding path that is the milestone's purpose, and it does not reconcile its fixture, bus contract or protocol items with what the tree and the other milestones already have.

Reviewed [the plan](/data/yellow/libersystem/docs/todo/P02M0195.md), its prerequisite [P02M0196](/data/yellow/libersystem/docs/todo/P02M0196.md), the [HID-over-I2C items of P02M0099](/data/yellow/libersystem/docs/todo/P02M0099.md:5864), the consumers [P02M0201](/data/yellow/libersystem/docs/todo/P02M0201.md:24) and [P02M0202](/data/yellow/libersystem/docs/todo/P02M0202.md:41), the existing `hid-i2c` library, DeviceManager's binding identity, the kernel's claim path and the harness's QEMU profiles at commit `07371c44af82a11c1d275b0cbd832899f712e62a`. P02M0196, P02M0099 and `TODO.md` were read as they stand uncommitted in the working tree, and P02M0197 to P02M0202 as untracked files; line numbers refer to the working tree. This is a requirements checklist rather than an implementation design. The findings concern decisions the implementation needs, not the expected absence of new code.

1. **High - The HID-over-I2C binding, which is this milestone's purpose, has neither a mechanism nor an order. The per-address and per-line grants "through the claim" need a namespace-to-PCI join and a child binding that neither this plan nor P02M0196 provides, and the two milestones depend on each other.**

   The [bus item](/data/yellow/libersystem/docs/todo/P02M0195.md:26) gives a consumer one address "granted through the claim of the firmware-described device". The [dependency note](/data/yellow/libersystem/docs/todo/P02M0195.md:18) has the device's description come from AML that names "controller I2C1" and "GPIO pin 17". In the guest those controllers are the `vhost-user-i2c-pci` and `vhost-user-gpio-pci` functions, so the SSDT's `I2cSerialBusV2` and `GpioInt` resource sources name namespace nodes that stand for PCI functions. P02M0196 defines platform devices only ["beside PCI and USB"](/data/yellow/libersystem/docs/todo/P02M0196.md:29) and enumerates namespace devices only ["into the platform devices of step 1"](/data/yellow/libersystem/docs/todo/P02M0196.md:48). Nothing there joins an `_ADR` node to a PCI function, and no source file in the tree mentions `_ADR`. The binding identity is [bus/device/function plus generation](/data/yellow/libersystem/src/user/libs/driver/binding/src/lib.rs:739), and publications are [keyed to it](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2762). Beyond the join, the claim would have to carry capabilities that two other drivers' providers mint: one address on the I2C controller and one line on the GPIO controller. It would also have to end when either controller's binding generation changes. A claim's resources today are kernel objects minted by [`sys_device_claim`](/data/yellow/libersystem/src/kernel/syscall/mod.rs:1258). The tree has so far refused such a child binding unit as a ["SUB-FUNCTION IDENTITY"](/data/yellow/libersystem/src/user/drivers/core/src/usb_class.rs:7) and calls it ["the same cross-cutting case as the firmware node"](/data/yellow/libersystem/docs/todo/P02M0099.md:4667).

   The order is circular. [The index](/data/yellow/libersystem/docs/todo/TODO.md:283) says this milestone "Needs P02M0196's AML half". P02M0196's fixture puts its event path ["on P02M0195's virtio-gpio line"](/data/yellow/libersystem/docs/todo/P02M0196.md:78), and the gates of P02M0197 to P02M0202 are built on that fixture. Neither plan says that the bus half lands first. The second consumer of the GPIO contract is the ACPI service, which handles the `_AEI` lines of the controller itself and claims no child device, so the plan's only grant rule does not cover it. On aarch64 the same join is needed in device-tree form. That form is a node for the PCI function under the host bridge, carrying the HID child and an `interrupts-extended` reference into the GPIO function's node.

   Without these, the gate cannot find the device and the driver cannot be handed a bus. A controller restart leaves the HID driver holding dead handles, and the two milestones wait on each other.

   **Correct the dependency note, the bus item and the HID items** by splitting the milestone. The bus half (both drivers, both contracts, their provider kinds and the backend) needs nothing from P02M0196 and is stated as a prerequisite of P02M0196d. The HID half waits for P02M0196b and for a namespace/device-tree companion join that P02M0196 must own. Specify the grant in one of two ways. Either DeviceManager obtains an address-scoped (or line-scoped) channel from the controller's provider, which the controller driver enforces, puts it in the child's claim and revokes it when either parent binding ends. Or the HID transport runs as a module inside the controller's process, as the USB classes do, and the plan says how the GPIO line reaches it. Also state how the ACPI service obtains the lines a GPIO controller's `_AEI` lists.

2. **Medium - The fixture's vhost-user devices do not work under the harness's default translated profile, and the plan chooses neither the vhost-user IOTLB protocol nor an untranslated profile.**

   Guest gates boot through [`./run.sh --arch ... --smp 2`](/data/yellow/libersystem/src/tools/guest-gate.sh:100). An ordinary run [has a virtio-iommu by default](/data/yellow/libersystem/src/harness/qemu-run.sh:1787), on [`q35,default-bus-bypass-iommu=off`](/data/yellow/libersystem/src/harness/qemu-run.sh:1884). Only `test.sh` [stays untranslated](/data/yellow/libersystem/src/harness/qemu-run.sh:1806). Behind that controller a virtio endpoint must carry [`iommu_platform=on`](/data/yellow/libersystem/src/harness/qemu-run.sh:553). Without it, the device takes the driver's IOVAs as guest-physical addresses and [completes nothing](/data/yellow/libersystem/src/harness/qemu-run.sh:759). On this QEMU (10.0.11), `-device vhost-user-i2c-pci,help` and `vhost-user-gpio-pci,help` show `iommu_platform` defaulting to off. With it on, QEMU refuses the backend with "IOMMU support requires reply-ack and backend-req protocol features.", so the backend must also serve the vhost-user IOTLB miss and update exchange. The [fixture item](/data/yellow/libersystem/docs/todo/P02M0195.md:48) covers shared guest RAM but says nothing about this. The same backend is P02M0202's [TCPCI fixture](/data/yellow/libersystem/docs/todo/P02M0202.md:58), and the GPIO line is P02M0196d's event path for six milestones' gates.

   The first gate built with the existing helper boots translated and fails the way the harness has already recorded once for virtio-scsi. The alternative is that the Python backend grows an IOMMU implementation nobody scheduled.

   **Correct the fixture item** by choosing one of two options. Either every gate that attaches these devices runs `--no-iommu`, and the new drivers' manifest `dma` class admits that boot (`iommu-required` [refuses it](/data/yellow/libersystem/src/user/services/manifest.toml:2931)). Or the backend implements the IOTLB protocol and the devices receive `iommu_platform=on` through [`qemu_virtio_opts`](/data/yellow/libersystem/src/harness/qemu-run.sh:562). For the first option a sentence and the profile choice suffice.

3. **Medium - The I2C bus contract cannot be implemented by the SMBus controller P02M0201 plans to put under it, and it excludes the alert P02M0201 relies on.**

   This plan's contract is [write, read and write-then-read to one address](/data/yellow/libersystem/docs/todo/P02M0195.md:26). That is the shape of the existing [`I2cBus`](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:134) trait, with lengths the caller chooses. P02M0201's SSIF item has an [ICH9 SMBus driver "serving P02M0195's bus contract"](/data/yellow/libersystem/docs/todo/P02M0201.md:24) "with the alert line where one is wired". This plan [excludes SMBus alert](/data/yellow/libersystem/docs/todo/P02M0195.md:60). The ICH9 host controller issues only SMBus protocol transactions: quick, byte, word, block transfers with a device-supplied count byte, and an I2C block read after a one-byte command, in blocks of at most 32 bytes. That is why Linux's [i2c-i801 driver](https://github.com/torvalds/linux/blob/master/drivers/i2c/busses/i2c-i801.c) advertises SMBus functionality and never plain I2C. SSIF itself is defined as SMBus block write and block read ([IPMI v2.0, section 12](https://www.intel.com/content/dam/www/public/us/en/documents/product-briefs/ipmi-second-gen-interface-spec-v2-rev1-1.pdf)). A HID-over-I2C register read needs a two-byte register write and then the read, which ICH9 cannot issue. An SSIF read needs the controller to take the length from the device, which a `write_read` into a caller-sized buffer cannot say.

   The second consumer of the system's only I2C contract would therefore either implement it lossily or force the IDL to be redrawn after the first consumer ships.

   **Correct the bus item** in one of two ways. Either add SMBus transactions (byte and word data, block read and write with the device's count, optional PEC) and a functionality set the controller declares, so a consumer can refuse a controller that lacks what it needs. Or state that SMBus is a separate contract and change P02M0201 to match. Reconcile the alert exclusion with P02M0201 either way.

4. **Medium - The protocol items re-plan a library that already exists and is closed, and the plan does not say how its new provider contract relates to that library's bus trait.**

   The [protocol item](/data/yellow/libersystem/docs/todo/P02M0195.md:38) and the [host-suite item](/data/yellow/libersystem/docs/todo/P02M0195.md:56) describe what `src/user/libs/driver/hid-i2c` already implements:
   - HID descriptor [decoding and its refusals](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:168);
   - input reports with lengths checked in both directions ([`decode_input`](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:364));
   - fetching the report descriptor, GET/SET_REPORT, RESET and SET_POWER ([`Device`](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:385)).

   The library has 13 host tests, registered as [`host.hid-i2c`](/data/yellow/libersystem/src/tools/verify-model/model/release-required.toml:221). P02M0099 closed that half as [done](/data/yellow/libersystem/docs/todo/P02M0099.md:5864) and left only [the binding](/data/yellow/libersystem/docs/todo/P02M0099.md:5916) to this milestone. The library is also where the I2C bus contract was first stated, [deliberately without a clock rate](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:10). This plan's contract adds "the bus speed" without saying whether the IPC contract replaces, wraps or implements `I2cBus`. The library is a [source row with no library row](/data/yellow/libersystem/src/user/services/manifest.toml:181), because nothing links it yet.

   An implementer following the plan writes a second protocol layer and suite, or the closed item and this plan disagree about which contract is authoritative.

   **Correct the protocol and host-suite items** to name `hid-i2c` as the protocol layer and keep only what is missing. That is the driver that owns the interrupt, reset timing and the report loop, and the library row once something links it. State that the client side of the provider contract implements `I2cBus`, or how the trait changes. Small edits suffice.

5. **Low - The SSDT fixture and the device-tree overlay need compilers this machine does not have, and the plan does not say how their bytes are produced.**

   `iasl`, `dtc` and `fdtoverlay` are all absent; of the device-tree tooling only `libfdt1` is installed. The harness already edits trees in [pure Python because `dtc` is missing](/data/yellow/libersystem/src/harness/dma-mode-record.py:35), and a new host package is a question [put to the owner first](/data/yellow/libersystem/docs/todo/P02M0201.md:58). The [SSDT item](/data/yellow/libersystem/docs/todo/P02M0195.md:52) and [the overlay](/data/yellow/libersystem/docs/todo/P02M0195.md:56) assume both compilers.

   **Correct the fixture items** by naming the source of the bytes. Either an owner-approved `acpica-tools` and `device-tree-compiler`, bound like the harness's other host tools, or small generators in the harness: an AML emitter for the few objects the SSDT needs, and the existing Python tree editor extended.

6. **Low - The ports' scope is ambiguous: aarch64 is listed as a cross-build yet described as a runtime configuration, riscv64 is not mentioned, and the harness can apply an overlay on only one of aarch64's two boot paths.**

   [The item](/data/yellow/libersystem/docs/todo/P02M0195.md:56) places "aarch64's `virt` machine takes the same two vhost-user devices" inside "the cross-builds". The harness rewrites the tree only on the [direct `-kernel` path](/data/yellow/libersystem/src/harness/qemu-run.sh:2590), where it dumps and annotates it. On the UEFI path the loader passes on [the tree the firmware published](/data/yellow/libersystem/src/boot/loader/src/arch/aarch64/mod.rs:558) unchanged. riscv64's QEMU offers the same two devices.

   **Correct the host-suite item** to state which ports run the gate and on which boot path, or that the ports only cross-build.

Validation was plan, source and harness inspection, plus QEMU capability queries that start no guest: `-device help` on the three `qemu-system-*` binaries, `,help` for the two vhost-user devices, and the vhost-user error strings in the x86_64 binary. Host tools were checked with `which` and `dpkg -l`. No build, test or guest was run, and no repository file was modified.

PLANNER'S RESPONSE ON P02M0195 (2026-09-26T02:46:01Z):

Every finding was checked against the tree as it stands in the working copy: the `hid-i2c` library (its
`I2cBus` trait, its module comment on the contract, and the 13 tests in `tests.rs`), `release-required.toml`
line 221 and the manifest's source row for it; `BindingId` in `driver-binding`; DeviceManager's
`publish_all`, `mint_connection`, `requirements_met`/`gate_on_requirements`, `settle_dependencies` and
`begin_dependency_stop`; the driver protocol's `RESOURCE` and `CONNECT` opcodes; `sys_device_claim`; the
decision comment in `usb_class.rs`; `qemu_virtio_opts`, the IOMMU default and `default-bus-bypass-iommu=off`
in `qemu-run.sh`, the port boot paths and `dma_independent_dtb_args`; `run.sh` and `test.sh` (the ports'
`UEFI=1` default and `--no-iommu` selecting a second signed image); `guest-gate.sh`; the pure-Python tree
editor in `dma-mode-record.py`; the aarch64 loader's `find_dtb`; and the plans P02M0196, P02M0099 (the
closed HID-over-I2C library item and the open binding item), P02M0201 and P02M0202 plus `TODO.md`. QEMU
10.0.11 was queried without a guest: `vhost-user-i2c-pci,help` and `vhost-user-gpio-pci,help` (both default
`iommu_platform` and `ats` to off), `-device help` on the aarch64 and riscv64 binaries (both devices and
`virtio-iommu-pci` present), and the x86_64 binary's strings, which hold both "IOMMU support requires
reply-ack and backend-req protocol features." and "Virtio-iommu does not support dev-iotlb yet". `iasl`,
`dtc` and `fdtoverlay` are absent and neither package is installed. The cross-milestone decisions of this
round (the companion join and node-scoped channel owned by P02M0196, one bus contract with a functionality
set owned here, the split around P02M0196, in-tree generators instead of host compilers, and publication only
by a driver binding) were applied.

1. **ACCEPTED - the HID binding's mechanism and order.** The code says what the finding says: a binding is a
   bus/device/function plus generation, publications are keyed to it, a claim mints only kernel objects, and
   the USB decision refuses a sub-function identity. Two further facts decided the mechanism: DeviceManager
   already hands capabilities to a driver at bind (`RESOURCE`) and already mints per-consumer endpoints to a
   published provider (`CONNECT`), and it already has wait-then-bind and dependency-lost stops, today per kind,
   driven by `requires`. CHOSEN: the child binding, not a module in the controller's process - the
   interrupt is another controller's line (a module would need a cross-process grant anyway), the device has
   a firmware identity of its own, and SSIF (P02M0201) and TCPCI (P02M0202) have the same shape. Plan
   changes: a new "TWO HALVES, AND THE ORDER AROUND P02M0196" paragraph (the bus half needs nothing from
   P02M0196 and lands before P02M0196d's fixture; the HID half waits for P02M0196b and the companion join,
   its tree path for P02M0196a's tree devices and the join; nothing in P02M0196 waits for it), replacing the
   old dependency note; a new "SCOPED CONNECTIONS, AND NEVER THE WHOLE BUS" item (the provider kinds
   `i2c-bus` and `gpio-lines` never opened whole; a scoped `CONNECT` carrying one address or one line; the
   controller enforcing it and holding each address and line for one connection; refusals on the minted
   connection so `CONNECT` stays one-way; everything closed when the controller's binding ends; and who is
   handed one, including the ACPI service for `_AEI` lines, minted by DeviceManager from the companion
   join's list and again after a rebind); and a new "THE CHILD BINDING" item (the `PNP0C50` node as its
   own method-only platform device; its connections as function-named requirements beside `requires`, so
   the existing `DependencyPending` and `StopIntent::DependencyLost` paths bind it after both parents and
   stop it when either ends, without spending its restart budget; the two connections and the node-scoped
   ACPI channel handed over as `RESOURCE` frames; the device-tree form with `virtio,device22` and
   `virtio,device29` children and `interrupts-extended`). The join itself is P02M0196's and is referred to,
   not re-planned here.

2. **ACCEPTED - vhost-user under the translated profile.** Verified: an ordinary run and every guest gate
   boot with a virtio-iommu and bus bypass off, the suite does not, `qemu_virtio_opts` is where
   `iommu_platform=on` comes from, both devices default it to off, and QEMU's message for a backend lacking
   the protocol features is in this binary. `--no-iommu` on x86_64 also selects a separately signed image
   and has no network, since `virtio_net` is `iommu-required`. CHOSEN: the backend serves the vhost-user
   IOTLB and the devices take `qemu_virtio_opts`, because P02M0196d's fixture and the gates of P02M0197 to
   P02M0202 ride this GPIO line and the other option would move all of them onto the degraded machine.
   Plan changes: the fixture item states the decision (`VIRTIO_F_ACCESS_PLATFORM` with reply-ack and
   backend-request, every ring and descriptor address translated through acknowledged updates and
   invalidations, misses sent on the backend channel), keeps `ats` off because this QEMU's virtio-iommu
   refuses the device-IOTLB notifier ATS would select, and a new gate `i2c-bus` runs the bus half's
   selection under `IOMMU=1`, as `qemu-virtio-iommu-x86_64` does, so the IOTLB path is proved. The drivers'
   manifest rows are `trusted-untranslated` like the other virtio rows, so the untranslated suite binds them.

3. **ACCEPTED - the contract and the SMBus controller.** The contract was the `I2cBus` shape and P02M0201's
   ICH9 driver cannot serve it. Took the first option: ONE contract with a FUNCTIONALITY SET the controller
   declares - plain I2C, and the SMBus transactions (quick, byte, byte and word data, block write, block read
   returning the device's count at most 32 bytes, I2C block read after a one-byte command, optional PEC) -
   with `unsupported` and `pec` errors added to `BusError`, and a consumer refusing a controller that lacks
   what it needs. Declined the separate-contract option. Checking virtio-i2c against its specification
   showed it cannot serve the block read with the device's count (each read's length is fixed when the
   request is queued) and reports only success or failure; the virtio-i2c item now declares the SMBus
   transactions that compose from I2C messages without that one, and maps every failure to `interrupted`.
   The alert stays excluded and the plan says the SMBus consumers poll; P02M0201's "with the alert line
   where one is wired" has to go in P02M0201 itself. The contract item also states that ICH9 declares only
   the SMBus set, so a HID device is refused on it.

4. **ACCEPTED - the protocol items re-planned `hid-i2c`.** Verified: descriptor decoding and its refusals,
   `decode_input`, `Device` with report descriptor, GET/SET_REPORT, RESET and SET_POWER, 13 host tests,
   `host.hid-i2c` in the release-required set, and P02M0099's closed item. Plan changes: the old protocol
   item and the host-suite wording are replaced by "THE DRIVER, over the protocol layer that ALREADY EXISTS",
   which names the library as the protocol layer and keeps only what it leaves to its caller (descriptor
   register from `_DSM` or `hid-descr-addr`, `_PS0`/`_PS3`, the reset wait with its bound, the report loop
   with acknowledgement, the storm bound, sleep on `STOP`); the client side implements `I2cBus` unchanged on
   a thin wrapper in the driver, so the library stays free of IPC; and "the bus speed" is dropped from the
   contract, which keeps the library's no-clock-rate rule. Declined the library row: drivers are static
   programs and link `hid-i2c` statically, so it stays a source row and only its manifest comment and its
   module comment (which says SSIF takes `I2cBus` as it stands) are corrected in the same change.

5. **ACCEPTED - where the SSDT and tree bytes come from.** Verified that neither compiler is installed and
   that the harness already edits trees in pure Python. Took the in-tree-generator option: the SSDT comes
   from the AML emitter P02M0196 builds for its fixtures, with this milestone adding any of
   `I2cSerialBusV2`, `GpioInt` and an integer `_DSM` it lacks under host tests; the tree comes from the
   extended Python editor. Declined asking the owner for `acpica-tools` and `device-tree-compiler`, since
   nothing here needs them. The SSDT item also fixes its shape (`Scope` into a node QEMU's DSDT already has
   for a present function, never a second node with the same `_ADR`; a vendor `_HID` with `_CID`
   `PNP0C50`).

6. **ACCEPTED, WITH ITS PREMISE CORRECTED - the ports' scope.** The ambiguity was real and riscv64 was not
   mentioned. The premise that only the direct `-kernel` path can carry an edited tree is wrong for this
   tree: `run.sh` and `test.sh` boot both ports through UEFI by default ("no non-UEFI way in since the
   packaged bootstrap archive was retired"), and on that path `dma_independent_dtb_args` already dumps the
   `virt` tree, edits it in Python and hands it back with `-dtb` on both ports; the loader then passes on
   the tree the firmware publishes, which is the edited one. Plan changes: the bus half's kernel tests run
   on x86_64 in the suite and on aarch64 and riscv64 in their emulated sweep; the `i2c-hid` gate runs on
   x86_64 through the SSDT and on both ports through the tree, over UEFI with that `-dtb` round trip, the
   dump taken without the two vhost-user devices (they add no node to QEMU's generated tree).

The plan was re-read as a whole after the edits. It is complete: every requirement of the milestone and of
P02M0099's open binding item has an item and a verification, and the old items it replaced are gone rather
than contradicted. It is consistent with the round's cross-milestone decisions and with P02M0196, P02M0201
and P02M0202 as they are to be amended (the changes other plans need are reported to the coordinator, not
made here). It is feasible with what the tree and this QEMU have: every mechanism it names - `RESOURCE`,
`CONNECT`, `requires`-driven wait and stop, `qemu_virtio_opts`, `IOMMU=1` for a gate's own profile, the
fixture-variable pattern, the `-dtb` round trip and the Python tree editor - exists, and the two new pieces
(the scoped `CONNECT` and the backend's IOTLB) are stated completely enough to build. It remains a plan with
`Status: OPEN`; no source, test or script was changed.

Final consistency check (2026-09-26T02:56:37Z): reconciled with P02M0201's corrected plan. The bus-contract item said P02M0201's ICH9 SMBus driver "declares the SMBus set"; it now says that driver declares the SMBus block transactions SSIF uses (block write, the block read with the device's count, PEC) and not plain I2C, the other SMBus transactions being declared when a consumer needs one - which is what P02M0201 now plans. P02M0099's closed HID-over-I2C item was corrected in the same pass: SSIF consumes this contract's SMBus half rather than taking the three I2C operations as they stand. No source was modified.
