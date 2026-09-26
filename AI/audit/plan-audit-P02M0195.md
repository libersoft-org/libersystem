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


AUDITOR'S RE-AUDIT OF PLAN P02M0195 (2026-09-26T04:01:12Z):

**Rating: 6/10.** The split, the IOTLB decision and the SMBus functionality set are sound, but the HID half's gate cannot pass without an InputService change the plan omits, and DeviceManager cannot mint the scoped connection as the plan specifies it.

The complete history was read: the original review, the planner's response and its final consistency check, the plan as audited (`git show 07371c44:docs/todo/P02M0195.md`) and the planner's edits (`git diff 07371c44 0dd5da07`); the working tree matches 0dd5da07. The plan was checked against DeviceManager's catalogue, minting and dependency code, the driver protocol, the system-manifest defaults, the `hid-i2c` library, the virtio queue library, the HID parser, InputService, xHCI's offers, `qemu-run.sh`, `run.sh`, `test.sh`, `test-kernel.sh`, `guest-gate.sh` and `dma-mode-record.py`, and against P02M0196, P02M0201, P02M0202, P02M0197 to P02M0200 and the I2C and InputService rows of P02M0099 and P02M0192. The QEMU facts the plan states hold on this machine: both vhost-user devices exist on all three binaries, `iommu_platform` and `ats` default to off, both quoted error strings are in the x86_64 binary, and `memory-backend-memfd` exists on all three. The corrections of findings 3, 4, 5 and 6 hold, and their partial declines (a separate SMBus contract, a library row, host compilers) are justified. The split and the child binding that answer finding 1 are sound in shape, and the IOTLB choice that answers finding 2 is right about this QEMU, but the items below remain.

1. **High - The touchpad's `pointer` and `touch` publications cannot reach InputService, which opens one provider per kind from a snapshot taken at its start and then closes the subscription, so the `i2c-hid` gate cannot pass and the plan lacks the InputService change it needs.**

   The [publication item](/data/yellow/libersystem/docs/todo/P02M0195.md:167) says InputService consumes these reports "exactly as it consumes xHCI's". The [gate](/data/yellow/libersystem/docs/todo/P02M0195.md:192) expects the moves, contact and click at a live InputService client, and ["the moves arriving again"](/data/yellow/libersystem/docs/todo/P02M0195.md:197) after the child is rebound. InputService takes its providers in [`take_published_pointer`](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:470). It drains only the frames already queued ([`try_recv_caps`](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:481)), opens the first live provider and [stops](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:493), and [closes the subscription](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:502). It does this once, at start, for [one `input`, one `pointer` and one `touch` provider](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:705), and a channel that closes is [dropped for good](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:727).

   xHCI always offers a `pointer` when it comes online ([xhci.rs:834](/data/yellow/libersystem/src/user/drivers/core/src/xhci.rs:834)), and the development machine has xHCI with a [`usb-tablet`](/data/yellow/libersystem/src/harness/qemu-run.sh:912). So the touchpad's `pointer` is a second provider of the kind and is never opened. Its `touch` is opened only if it is published before InputService's snapshot, and this child binds only after the namespace, the join and both controllers. After the gate's disable and enable, the new publication is never opened. P02M0099 records this limit and gives the fix to whoever publishes the second provider: ["A second provider of either kind owns the live identity, attach/detach, failover and reconnect migration"](/data/yellow/libersystem/docs/todo/P02M0099.md:7120). [P02M0192](/data/yellow/libersystem/docs/todo/P02M0192.md:20) states the same limit. This touchpad's `pointer` is that second provider. This is a new finding.

   **Correct the publication item** by adding the InputService change to this milestone: keep the `pointer` and `touch` subscriptions open, attach every live provider up to a stated bound, detach one on withdrawal and attach its re-publication. The gate's rebind case, run beside the xHCI tablet, is its proof.

2. **High - As specified, DeviceManager refuses the first scoped `CONNECT`: it counts the unopened endpoint every publication carries against `consumers`, the two manifest rows leave `consumers` at its default of one, and nothing in the bus half's verification goes through DeviceManager.**

   The plan says a scoped connection ["counts against the kind's declared `consumers`"](/data/yellow/libersystem/docs/todo/P02M0195.md:71), that the kinds are ["published like any other"](/data/yellow/libersystem/docs/todo/P02M0195.md:64) and that `open` refuses them. Its rows are [`provides = [{ kind = "i2c-bus", most = 1 }]`](/data/yellow/libersystem/docs/todo/P02M0195.md:86) and ["as above"](/data/yellow/libersystem/docs/todo/P02M0195.md:89) for `gpio-lines`. An `OFFER` must carry [exactly one endpoint](/data/yellow/libersystem/src/user/libs/driver/protocol/src/lib.rs:299). DeviceManager keeps it in the entry with [`consumers: 0`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2830), and [`outstanding`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2970) counts it as one connection until `open` hands it out, which for these kinds never happens. [`mint_connection`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2539) refuses when that count [reaches the declared `consumers`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2557), and an absent `consumers` [means one](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:1154). So the first scoped connection is refused. With the count fixed, one would still be the limit, while one GPIO controller serves the ACPI service's `_AEI` lines and a child's line [at the same time](/data/yellow/libersystem/docs/todo/P02M0195.md:146). The offered endpoint is also an unscoped connection to the controller, which the plan never disposes of, against "NEVER THE WHOLE BUS". P02M0201's [`smbus_ich9` row](/data/yellow/libersystem/docs/todo/P02M0201.md:130) copies the shape.

   The bus half cannot catch this. Its kernel tests [play DeviceManager's part](/data/yellow/libersystem/docs/todo/P02M0195.md:115), and the [`i2c-bus` gate](/data/yellow/libersystem/docs/todo/P02M0195.md:124) reruns them, so the defect first shows in P02M0196d's fixture or the `i2c-hid` gate. This continues original finding 1.

   **Correct the scoped-connections and controller items.** State that for `i2c-bus` and `gpio-lines` the offered endpoint serves nothing and DeviceManager closes it at publication without counting it. Declare `consumers` on both rows for the connections a controller serves at once - for `gpio-lines` at least the `_AEI` lines plus one per child - within the [32-endpoint bound](/data/yellow/libersystem/src/tools/system-manifest/src/lib.rs:681). Add a DeviceManager host test to the bus half: the scoped mint, its count and refund, and the refused `open`.

3. **Medium - The ACPI service's connections do not match P02M0196: P02M0196 serves `GeneralPurposeIo` regions and `GpioIo` connections through this input-only contract, both plans claim the `_AEI` grant although this plan places it in a bus half that "needs NOTHING from P02M0196", and no rule grants the `_AEI` lines again after the service restarts.**

   P02M0196's interpreter serves [`GeneralPurposeIo` and `GenericSerialBus`](/data/yellow/libersystem/docs/todo/P02M0196.md:179) "through a line- or address-scoped connection". Its join resolves [`GpioIo`](/data/yellow/libersystem/docs/todo/P02M0196.md:216) and binds the child "through P02M0195's scoped connections". Both are normally outputs. This contract is ["input lines only"](/data/yellow/libersystem/docs/todo/P02M0195.md:59), the plan [excludes GPIO output lines](/data/yellow/libersystem/docs/todo/P02M0195.md:207), and its [list of who is handed a connection](/data/yellow/libersystem/docs/todo/P02M0195.md:73) names a `GenericSerialBus` address but no `GeneralPurposeIo` field. virtio-gpio itself has [`SET_DIRECTION` and `SET_VALUE`](https://github.com/oasis-tcs/virtio-spec/blob/master/device-types/gpio/description.tex), so outputs could be emulated.

   The `_AEI` grant is written into the bus half: DeviceManager mints the lines ["from the list the companion join gives it"](/data/yellow/libersystem/docs/todo/P02M0195.md:75). That list exists only in P02M0196b, yet the bus half ["needs NOTHING"](/data/yellow/libersystem/docs/todo/P02M0195.md:28) from P02M0196, and P02M0196's [GPIO-signalled events item](/data/yellow/libersystem/docs/todo/P02M0196.md:242) claims the same minting. Neither plan grants the lines to a new service instance: the grant happens when the controller publishes or [rebinds](/data/yellow/libersystem/docs/todo/P02M0195.md:76), and a [restarted service](/data/yellow/libersystem/docs/todo/P02M0196.md:135) gets only its node channels back. After an ACPI-service crash, every `_AEI` event stops until the GPIO controller rebinds. This continues original finding 1, which asked how the ACPI service obtains its lines.

   **Correct the scoped-connections item.** Say that the ACPI service's grants are built and verified in P02M0196b and P02M0196d on this half's scoped `CONNECT`, and that DeviceManager mints the `_AEI` lines again for each new service instance. Either add output to the line contract for `GpioIo` and `GeneralPurposeIo`, or state that such a connection is served for input reads only and a write is refused by name. Report the choice to P02M0196's owner.

4. **Medium - The tree form puts the touchpad's interrupt in an `interrupts-extended` specifier into the `virtio,device29` node, which neither plan says the join resolves: both promise "GPIO phandles", and P02M0196 step 1 turns `interrupts-extended` into wired interrupts on the GIC or APLIC.**

   This plan says the join resolves ["`I2cSerialBusV2`, `GpioInt` and GPIO phandles"](/data/yellow/libersystem/docs/todo/P02M0195.md:33), and its tree node carries [`interrupts-extended` into the `virtio,device29` node](/data/yellow/libersystem/docs/todo/P02M0195.md:147). P02M0196 publishes tree interrupts ["through `interrupt-parent`, `interrupts-extended` and `interrupt-map`"](/data/yellow/libersystem/docs/todo/P02M0196.md:120), and its wired lines are [GIC SPIs and APLIC sources](/data/yellow/libersystem/docs/todo/P02M0196.md:80). Its connections come from ["a tree's GPIO phandle or I2C parent"](/data/yellow/libersystem/docs/todo/P02M0196.md:217). An interrupt specifier whose parent is a GPIO controller fits neither description exactly. One implementer of P02M0196 will route it to the wired path and refuse or drop it; another will make it a line connection. [P02M0202](/data/yellow/libersystem/docs/todo/P02M0202.md:228) uses the same form for the TCPCI alert. This continues original finding 1's device-tree form.

   **Correct the child-binding item.** State that on a tree the line connection is the interrupt specifier whose parent is the virtio-gpio node (a `gpio-controller` that is also an `interrupt-controller`), the line its first cell and the trigger its second, and that the join makes it a connection and never a wired interrupt. Use those words in place of "GPIO phandles" on line 34, and give them to P02M0196's owner.

5. **Medium - The plan expects one precision touchpad to deliver both pointer moves and multi-touch contacts, but such a touchpad reports through only one of its mouse and touchpad collections, chosen by an Input Mode feature report the driver never sends, and the fixture is not required to behave that way.**

   The driver publishes [`pointer` (a touchpad) and `touch` (a touchpad's multi-touch collection)](/data/yellow/libersystem/docs/todo/P02M0195.md:167). The fixture is ["a precision touchpad"](/data/yellow/libersystem/docs/todo/P02M0195.md:173) whose one script has moves, a two-finger contact and a click, and the [gate](/data/yellow/libersystem/docs/todo/P02M0195.md:192) expects all three. The driver's steps - power on, RESET, the report descriptor, the report loop - [set no input mode](/data/yellow/libersystem/docs/todo/P02M0195.md:159). Microsoft's [configuration collection](https://learn.microsoft.com/en-us/windows-hardware/design/component-guidelines/touchpad-configuration-collection) says a precision touchpad reports through its mouse collection by default, "should only report data via one given collection at any time", switches to its touchpad collection when the host sets Input Mode (usage 0x52) to 3, and does not keep that mode across a host-initiated HID I2C reset. Nothing in the tree sets it: the HID parser names only the [contact usages](/data/yellow/libersystem/src/user/drivers/core/src/hid.rs:29), and InputService [records contacts only as contacts](/data/yellow/libersystem/src/user/services/core/src/input_service.rs:919), deriving no pointer from them. So a real touchpad under this driver gives moves and never contacts, and a fixture that sends both kinds of report lets the gate pass anyway. This is a new finding.

   **Correct the driver, fixture and gate items.** Decide the mode. Either leave a precision touchpad in its default mouse mode, so it publishes `pointer` and `touch` comes from touchscreens; or send Input Mode 3 with the library's [`set_report`](/data/yellow/libersystem/src/user/libs/driver/hid-i2c/src/lib.rs:486) after every report-descriptor read, power-on and RESET, publish the contacts as `touch`, and say what then moves the pointer. Make the fixture report through the one collection its input mode selects, so the gate proves the choice, also after its RESET case.

6. **Low - The IOMMU decision attaches the two devices with options "from `qemu_virtio_opts` like every other endpoint", which fits them only as x86_64's `virtio_plain`: every other endpoint's string carries a `disable-legacy` these devices lack, and on aarch64 and riscv64 the function is never called and, called as it stands, omits `iommu_platform=on`.**

   The plan says the devices ["take their options from `qemu_virtio_opts` like every other endpoint"](/data/yellow/libersystem/docs/todo/P02M0195.md:104). The function [reads `IOMMU`](/data/yellow/libersystem/src/harness/qemu-run.sh:564). x86_64 passes it explicitly and builds [`virtio_opts` with `disable-legacy=on`](/data/yellow/libersystem/src/harness/qemu-run.sh:1872), which most endpoints use. On this QEMU, `-device vhost-user-i2c-pci,help` and `-device vhost-user-gpio-pci,help` list `iommu_platform` but no `disable-legacy` (the devices are modern-only, unlike `virtio-blk-pci`), so that string is refused. aarch64 and riscv64 never call the function: each builds [its own string](/data/yellow/libersystem/src/harness/qemu-run.sh:2341) with `disable-legacy=on` ([riscv64](/data/yellow/libersystem/src/harness/qemu-run.sh:2694)). `run.sh` exports `IOMMU` only for [`--no-iommu`](/data/yellow/libersystem/run.sh:155), so the function called there on a default run returns nothing on a machine that has the controller. That is the silent failure the harness already [recorded for virtio-scsi](/data/yellow/libersystem/src/harness/qemu-run.sh:759). This continues original finding 2.

   **Correct the IOMMU decision** to say that each arm attaches the two devices with `iommu_platform=on` exactly when it built the controller (`iommu` on x86_64, `PORT_IOMMU` on the ports) and without `disable-legacy`.

Validation: plan, audit, source, manifest and harness inspection with read-only git (`show`, `diff`, `log`, `status`); QEMU 10.0.11 property queries that start no machine (`-device help` on the three `qemu-system-*` binaries, `-device vhost-user-i2c-pci,help`, `-device vhost-user-gpio-pci,help`, `-device virtio-blk-pci,help`, `-object help`, `-object memory-backend-memfd,help`) and `strings` on the x86_64 binary; `which` and `dpkg -l` for `iasl`, `dtc` and `fdtoverlay`. Primary sources were fetched read-only into the scratchpad: QEMU v10.0.0's `hw/virtio/vhost-user-i2c.c`, `vhost-user-gpio.c`, `vhost-user-base.c`, `vhost-user.c`, `vhost.c` and `virtio-bus.c`, the virtio specification's I2C and GPIO device sections, and Microsoft's precision-touchpad configuration-collection page. No plan, source or audit file was modified, and nothing was built, tested or booted.


PLANNER'S RESPONSE ON P02M0195 (2026-09-26T05:34:17Z):

Every finding of the re-audit was checked against the working tree: InputService's `take_published_pointer`,
its bootstrap order and serve loop, its contact stream and focus proof, and the kernel services test
`a_touch_surface_reports_contacts_and_not_a_cursor` with the catalogue helper it uses; the xHCI driver's offer
list (an unconditional `pointer`, `touch` only when a device reports contacts), `qemu_attach_xhci`'s
`usb-tablet` and QEMU's tablet report descriptor (no digitizer page); MidiService's kept-open subscription and
its four-device bound; DeviceManager's `publish_all`, `mint_connection`, `outstanding`, `disconnected` and the
catalogue's 32 subscriber places; the driver protocol's handle counts and `decode_connect`; the driver
library's `Serving` (a publication registered with no end) and `recv_from_consumer`; the system manifest's
`consumers` default, its 32-endpoint check and its `dma` classes; `driver-binding`'s host tests of the
catalogue arithmetic; the HID parser's `has_pointer`, `has_digitizer`, `contacts`, `pointer_fold` and `fields`
with each field's application collection; the `hid-i2c` library; `qemu_virtio_opts`, x86_64's `virtio_opts`
and `virtio_plain`, the ports' own option strings and `port_iommu_decide`, and `run.sh`'s `--no-iommu`; and
the neighbouring plans P02M0196, P02M0201, P02M0202, P02M0099 and P02M0192. QEMU 10.0.11 was queried without
starting a machine (`-device vhost-user-i2c-pci,help`, `-device vhost-user-gpio-pci,help`,
`-device virtio-blk-pci,help`), and primary sources were read: the virtio GPIO and I2C device sections,
Linux's `i2c-hid-acpi.c` match table and its `gpio-virtio`, `i2c-virtio` and `hid-over-i2c` device-tree
bindings. All six findings are accepted; none is rejected.

1. **ACCEPTED - InputService never opens the touchpad's `pointer`.** Verified: `take_published_pointer` drains
   only the frames already queued, opens the first live provider and closes the subscription, once each for
   `input`, `pointer` and `touch` at start, and a channel that closes is dropped for good; xHCI publishes
   `pointer` unconditionally and the machine carries a `usb-tablet`, so the touchpad's `pointer` is a second
   provider that is never opened, and nothing published after the gate's rebind is either. Plan changes: a new
   P02M0195b item, "INPUTSERVICE FOLLOWS EVERY POINTER AND TOUCH PROVIDER": InputService keeps its `pointer`
   and `touch` subscriptions open, as MidiService keeps its own (two of the catalogue's 32 subscriber places);
   every live `pointer` provider up to four is attached, merged into the one cursor as today's two slots are;
   ONE `touch` surface is attached at a time, because a contact event names no surface and each surface
   normalises its axes across itself, so two surfaces in one stream would be contacts in two coordinate spaces
   under colliding identities (and the existing touch test's check of the device's own contact identifier
   stays true); a provider past its kind's bound waits, logged once, and is attached when one of its kind is
   detached; a provider is detached on its withdrawal or when its connection closes and is not opened again
   until it is published again; a detached surface's contacts still down are lifted; a re-publication is
   attached like any other; the `input` kind keeps its bootstrap-only discovery. "TWO HALVES" names the change
   as part of P02M0195b that waits for nothing. A new kernel services test proves it (three pointer providers,
   one withdrawn and its re-publication attached, a fifth waiting until one is withdrawn, a second surface
   waiting and taking over with the first's finger lifted, every existing InputService test passing), and the
   `i2c-hid` gate's rebind case now runs beside the xHCI tablet: the HID providers detached and their
   re-publications attached while the tablet's stays attached.

2. **ACCEPTED - DeviceManager refuses the first scoped `CONNECT`.** Verified: an `OFFER` must carry exactly
   one handle; `publish_all` keeps it in the entry with `consumers: 0`; `outstanding` counts it while it is
   there; `mint_connection` refuses when that count reaches the declared `consumers`, whose absence means one;
   so the first scoped mint was refused, and one would have been the limit even with the count fixed, while a
   GPIO controller serves the `_AEI` lines and a child's line at once. Plan changes: the scoped-connections
   item gains "THE OFFERED ENDPOINT SERVES NOTHING": for `i2c-bus` and `gpio-lines` the controller driver
   keeps no end of the offered endpoint and reports no `DISCONNECT` for it (the driver library already
   registers a publication with no end), and DeviceManager closes it at publication without counting it, so
   only scoped connections count against `consumers` and each is refunded by `DISCONNECT`; the rows declare
   `consumers` 8 for `i2c-bus` and 16 for `gpio-lines` (the `_AEI` lines and one per child), within the 32
   connections one driver serves, and the virtio-i2c and virtio-gpio manifest rows now carry them. The item
   also names the closed tables the two kinds are appended to. The bus half's host suites add DeviceManager's
   admission rule as a pure function in `driver-binding`, where the catalogue arithmetic is already
   host-tested because DeviceManager is a `no_std` binary no host drives (the offered endpoint closed and
   never counted, a scoped mint counted and refused past the bound, `DISCONNECT` refunding one, `open`
   refused), and the manifest refusing a row that names either kind; the kernel tests close the offered
   endpoint as DeviceManager does; and the `i2c-hid` gate's rebind proves DeviceManager's own mint end to end,
   the HID bindings bound again on the GPIO controller that stayed bound, whose lines the first connections
   gave back.

3. **ACCEPTED - the ACPI service's connections and P02M0196.** Verified: P02M0196's interpreter serves
   `GeneralPurposeIo` through a line-scoped connection and its join resolves `GpioIo`, while this contract is
   input-only; the `_AEI` minting was written into the bus half although that list exists only in P02M0196b;
   and neither plan granted the lines to a restarted service. GPIO stays input-only. Plan changes: the line
   contract gains a scope for level reads alone (an input `GpioIo`, a `GeneralPurposeIo` field's connection)
   that answers the level and is delivered no event, and the scoped `CONNECT` lists it; the virtio-gpio item
   sets each served line to input with `SET_DIRECTION`, reads it with `GET_VALUE`, arms only an interrupt
   scope, and disarms and deactivates a line given back; "WHO IS HANDED ONE" becomes a list whose ACPI-service
   entry says its grants are built and verified in P02M0196b and P02M0196d on this half's scoped `CONNECT` -
   each `_AEI` line minted when the controller publishes, again after it rebinds and again for each new
   ACPI-service instance on its "namespace loaded" report, the line a `GeneralPurposeIo` field's connection
   names for level reads alone, and a `GenericSerialBus` field's address - and "TWO HALVES" says the same, so
   the bus half still needs nothing from P02M0196; the kernel tests add a read-only line scope and a second
   connection for a held line; EXCLUDES' GPIO output entry notes that P02M0196b refuses an AML
   `GeneralPurposeIo` write and connects an input `GpioIo` alone.

4. **ACCEPTED - a tree interrupt whose parent is the GPIO controller.** Verified: P02M0196 routes tree
   interrupts through `interrupt-parent`, `interrupts-extended` and `interrupt-map` to GIC SPIs and APLIC
   sources and speaks only of "a tree's GPIO phandle", so a GPIO-parented specifier could be read as a wired
   interrupt; Linux's `gpio-virtio` binding gives `virtio,device29` both `gpio-controller` and
   `interrupt-controller` with two interrupt cells. Plan changes: "TWO HALVES" replaces "GPIO phandles" with
   "on a tree, an interrupt specifier whose parent node is a GPIO controller"; the child-binding item states
   that an interrupt specifier (`interrupts-extended`, or `interrupts` with `interrupt-parent`) whose parent
   node is both `gpio-controller` and `interrupt-controller` - here the virtio-gpio function's
   `virtio,device29` node - names the line in its first cell and the trigger in its second (the tree's
   interrupt-type flags, whose values virtio-gpio's trigger types share), and that the join makes it a
   connection, never a wired interrupt; the driver's host suite turns a `GpioInt`'s trigger and polarity and a
   tree specifier's trigger cell into one line trigger.

5. **ACCEPTED - the touchpad's input mode.** Verified: the driver's steps set no mode, the HID parser names
   only the contact usages, InputService records contacts only as contacts, and a precision touchpad reports
   through its mouse collection until the host sets Input Mode 3 and goes back to it after a host-initiated
   reset. Also verified: `pointer_fold` folds any Generic Desktop X of a report, so decoding by page alone
   would fold a touchscreen's contact axes into a pointer. Plan changes: the publication item becomes "WHAT A
   BINDING PUBLISHES, AND THE MODE A TOUCHPAD IS LEFT IN": no Input Mode or Device Mode feature report is
   sent, so a precision touchpad stays in its mouse mode, also after every RESET, and publishes `pointer`,
   chosen because InputService derives no pointer from contacts; the application collections decide (Mouse or
   Pointer publishes `pointer`, Touch Screen publishes `touch`, Touch Pad and any other nothing), each report
   decoded by the collection its report id belongs to (`hid::fields` records it); and the driver's manifest
   row (`PNP0C50` and `ACPI0C50` by `_HID` or `_CID`, the `hid-over-i2c` compatible, `dma = "none"`, `pointer`
   and `touch`). The fixture item now has two models, each on its own line: the precision touchpad at 0x2C
   (mouse, touch pad and configuration collections, mouse mode at power-on and after every RESET, moved by
   Input Mode 3, its moves and click reported through the one collection its mode selects) and a touchscreen
   at 0x10 with descriptor register 0x01 (a two-finger contact and lift through its Touch Screen collection
   with no switch); the SSDT and the tree describe both; the gate checks the touchpad's binding publishing
   `pointer` alone and the touchscreen's `touch` alone, the moves and click as pointer events and the contact
   as contacts at a probe that also holds a surface with input focus (the proof `subscribe-contacts`
   requires), and both again after each device's RESET and after the rebind; the driver's host suite covers
   the collection rule; EXCLUDES gains the Input Mode and Device Mode switches (a precision touchpad's touch
   pad collection, a touchscreen that reports contacts only after a switch) and several surfaces in one
   contact stream.

6. **ACCEPTED - the vhost-user devices' option string.** Verified: both devices list `iommu_platform` and no
   `disable-legacy`, which `virtio-blk-pci` has; x86_64 builds `virtio_opts` with `disable-legacy=on` and
   `virtio_plain` without it, both from `qemu_virtio_opts` with `IOMMU` passed explicitly; aarch64 and riscv64
   build their own `disable-legacy=on` strings from `PORT_IOMMU` and never call the function; `run.sh` exports
   `IOMMU` only for `--no-iommu`. Plan changes: the IOMMU decision now says each arm of `qemu-run.sh` attaches
   the two devices with `iommu_platform=on` exactly when it built the virtio-iommu (x86_64 from its `iommu`
   decision, which is its `virtio_plain` string; aarch64 and riscv64 from `PORT_IOMMU`) and never with
   `disable-legacy`, and that neither the strings carrying `disable-legacy=on` nor `qemu_virtio_opts` on a
   port are used for them.

Re-check of the whole plan: the plan was re-read from the owner's decision to EXCLUDES after the edits. It is
complete: each correction has an item and a verification (the admission-rule and manifest host tests, the
read-only line scope and held-line kernel cases, the InputService kernel services test, the two-model gate
with its RESET and rebind cases, the collection-rule and trigger host tests), and the text it replaced is gone
rather than contradicted. It is internally consistent: the scope forms agree across the contract, the scoped
`CONNECT`, the virtio-gpio driver and the kernel tests; the `consumers` numbers agree between the rule and the
two rows; the addresses, descriptor registers and lines agree across the fixture, the SSDT, the tree and the
gate. It is consistent with the neighbouring plans as this round's decisions amend them: GPIO stays input-only
and the ACPI service's grants, the re-grant to a new instance included, are P02M0196b's and P02M0196d's on
this half's scoped `CONNECT`; a tree interrupt whose parent is a GPIO controller is a line connection the join
makes; P02M0201's `smbus_ich9` row follows the same offered-endpoint rule with its own `consumers`; P02M0202's
TCPCI child binds the same way; P02M0192's sentence that the input, pointer and touch kinds keep
bootstrap-only discovery is about its own change. Three smaller errors found on the way were corrected:
virtio-gpio's request is `GET_LINE_NAMES`, not `GET_NAMES`; zero-length requests are mandatory for every
virtio-i2c device, so quick is always declared; and the child binding names `ACPI0C50` beside `PNP0C50`, as
Linux matches both. It is feasible with what the tree and this QEMU have - the kept-open subscription, a
publication registered with no end, the `driver-binding` host tests, `virtio_plain`, `PORT_IOMMU` and the
display's focus proof all exist - and it remains `Status: OPEN` with nothing started.

Only `docs/todo/P02M0195.md` was edited and this response appended; no source, test, script or other plan was changed, and nothing was built or booted.


AUDITOR'S RE-AUDIT OF PLAN P02M0195 (2026-09-26T15:50:59Z):

**Rating: 9/10.** Every finding of the previous re-audit is corrected in the plan text, and every fact the planner states about the tree holds. One small item is left: the plan says the `i2c-hid` gate's rebind proves DeviceManager's use of the new admission rule, but that case cannot fail on any part of the rule.

The complete history was read: the original review, the planner's response and its final consistency check, the re-audit of 2026-09-26T04:01:12Z and the planner's response of 05:34:17Z. The plan as that re-audit saw it (`git show HEAD:docs/todo/P02M0195.md`) was compared with the working tree (`git diff -- docs/todo/P02M0195.md`). The planner's claims were checked in the code:
- InputService's discovery, its two raw slots, its focus-gated contact stream and the lift on focus loss;
- DisplayService's focus proof, `btcheck`'s pointer snapshots, and the InputService kernel services tests with the catalogue helpers in `src/kernel/tests.rs`;
- DeviceManager's `publish_all`, `mint_connection`, `outstanding`, `disconnected`, its subscriber table and its development-boot self-tests;
- the driver library's `Serving`, the driver protocol's `CONNECT` decoding and resource kinds, and `driver-binding`'s host tests;
- the system manifest's `consumers` default, its 32-connection bound, its `fixture-control` refusal and its `dma` classes;
- MidiService's subscription, the HID parser's `fields` and `pointer_fold`, xHCI's offers and the `hid-i2c` library;
- in the harness, `qemu_virtio_opts`, `virtio_plain`, `port_iommu_decide`, `qemu_attach_xhci` on all three arms, `guest-gate.sh` and the `IOMMU=1` pattern of `check-qemu-virtio-iommu-x86_64.sh`.

Sibling plans were read from the working tree: P02M0196, P02M0099 (the HID-over-I2C items and the InputService migration row), P02M0192, P02M0194, P02M0199, P02M0201, P02M0202 and `TODO.md`. QEMU 10.0.11 was asked for the two devices' properties without starting a machine. The VIRTIO specification's I2C and GPIO device sections were read.

These corrections hold:
- Finding 1 (InputService): the [new item](/data/yellow/libersystem/docs/todo/P02M0195.md:215) keeps both subscriptions open with stated bounds. A waiting provider takes over on a detach, and re-publications are attached. Its kernel services test and the rebind beside the xHCI tablet prove it; the tablet is attached on every arm of a non-reduced machine. This is the owner that [P02M0099's InputService row](/data/yellow/libersystem/docs/todo/P02M0099.md:7125) names. P02M0192's ["keep their bootstrap-only discovery"](/data/yellow/libersystem/docs/todo/P02M0192.md:122) describes that plan's own change, so the planner reads it correctly.
- Finding 2 (the design): the offered endpoint is closed at publication and not counted. The rows declare `consumers` 8 and 16, within the 32-connection bound. The driver library already registers a publication with no end ([`Serving::from_offers`](/data/yellow/libersystem/src/user/drivers/core/src/common.rs:741)). [P02M0201's `smbus_ich9` row](/data/yellow/libersystem/docs/todo/P02M0201.md:145) follows the same rule. Item 1 below covers the verification half.
- Finding 3: the level-read scope, the virtio-gpio sequence and the re-grant on each "namespace loaded" report match [P02M0196's `GeneralPurposeIo` rule](/data/yellow/libersystem/docs/todo/P02M0196.md:219) and its [GPIO-signalled events item](/data/yellow/libersystem/docs/todo/P02M0196.md:297). That rule also serves a `GeneralPurposeIo` read of an `_AEI` line on that line's own connection. The sequence fits the specification: the line is set to input before any IRQ message (none may be sent for an output line), and a queued buffer comes back marked invalid when the type is set to none.
- Finding 4: the plan and [P02M0196 step 1](/data/yellow/libersystem/docs/todo/P02M0196.md:133) state the same rule for a specifier whose parent is a GPIO controller. The trigger values 1, 2, 3, 4 and 8 are virtio-gpio's own.
- Finding 5: the plan decides on mouse mode and gives the reason. The fixture's touchpad reports through the one collection its mode selects, so a driver that switched it fails the gate. [`hid::fields`](/data/yellow/libersystem/src/user/drivers/core/src/hid.rs:720) does record each field's application collection.
- Finding 6: each arm attaches the two devices with `iommu_platform=on` exactly when it built the controller, and never with `disable-legacy`, which neither device has on this QEMU.

The three smaller corrections (`GET_LINE_NAMES`, zero-length requests mandatory for every virtio-i2c device, `ACPI0C50`) agree with the specification and with Linux.

1. **Low - The plan credits the `i2c-hid` gate's rebind with proving DeviceManager's use of the new admission rule, but the rebind cannot fail on any part of that rule. Nothing proves the one part a pure function cannot show, which is the offered endpoint really closing at publication.**

   The [host-suite item](/data/yellow/libersystem/docs/todo/P02M0195.md:156) tests the rule as a pure function in `driver-binding` and credits the rebind with the rest: ["DeviceManager's own use of it proved end to end by the `i2c-hid` gate's rebind case"](/data/yellow/libersystem/docs/todo/P02M0195.md:160). The rebind cannot carry that:
   - It [disables and enables the virtio-i2c binding](/data/yellow/libersystem/docs/todo/P02M0195.md:264). The new virtio-i2c binding counts from zero. The GPIO controller that stayed bound serves the two HID lines before the rebind and two after it. The rows declare [`consumers` 8 and 16](/data/yellow/libersystem/docs/todo/P02M0195.md:83), so neither bound is approached. The case passes the same way whether DeviceManager closes the offered endpoint, counts a scoped mint, refunds on `DISCONNECT` or refuses `open`, and nothing in the gate calls `open`.
   - Closing the offered endpoint is a change DeviceManager has to make, not a property it has already. Today [`publish_all`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2830) keeps the offered handle in the entry, and [`outstanding`](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:2970) counts it.
   - What the case does prove is that the controller gives its lines back and that DeviceManager mints again after a parent rebinds ([line 266](/data/yellow/libersystem/docs/todo/P02M0195.md:266)).
   - The tree already proves this kind of effect against real handles. DeviceManager runs [development-boot self-tests](/data/yellow/libersystem/src/user/services/core/src/device_manager.rs:558) against the real `Catalogue`. The first of them exists because ["A recorder in a host test cannot establish that this syscall closes the actual endpoint"](/data/yellow/libersystem/src/user/services/core/src/device_manager/tests.rs:5).

   This is an incomplete correction of the previous re-audit's finding 2. That finding asked for the scoped mint, its count and refund, and the refused `open` to be tested in DeviceManager. The arithmetic is now covered by the pure function; the claim about the rest is not true.

   **Correct the bus half's host-suite item**: credit the rebind only with the controller's release and DeviceManager's re-mint. Prove the offered endpoint of an `i2c-bus` or `gpio-lines` publication closed at publication and not counted in DeviceManager's development-boot self-tests, as `unopened_provider_withdrawal` proves the same effect at withdrawal.

Validation: read-only inspection of the plan, the audit history, the sibling plans in the working tree and the source, manifest and harness files named above, using read-only git (`diff`, `show`, `status`). QEMU 10.0.11 was queried for properties without starting a machine (`-device vhost-user-i2c-pci,help`, `-device vhost-user-gpio-pci,help`). The VIRTIO specification's I2C and GPIO device sections were fetched read-only. No plan, source or audit file was modified, and nothing was built, tested, benchmarked or booted.

PLANNER'S RESPONSE ON P02M0195 (2026-09-26T17:12:45Z):

Verified read-only: DeviceManager's development-boot self-tests (`unopened_provider_withdrawal`, `catalogue_scope_denial`, `catalogue_cap_refusal` and the rest, run under `cfg(feature = "development")` against the real `Catalogue` and real channels), the comment that a recorder in a host test cannot establish that the syscall closes the actual endpoint, and `publish_all` and `outstanding`, which today keep and count the offered handle. Summary: one finding, accepted.

1. **ACCEPTED - the rebind case could not prove DeviceManager's use of the admission rule.** The finding is right on each point: the rebind stays far below `consumers` 8 and 16, never calls `open`, and passes the same way whether the offered endpoint is closed or kept; closing it at publication is a change DeviceManager has to make, not a property it already has. Plan change, in the bus half's host-suite item: the pure function in `driver-binding` still carries the arithmetic (closed and uncounted offered endpoint, scoped mint counted and refused past `consumers`, `DISCONNECT` refunding one, `open` refused). What it cannot show - that DeviceManager really closes the offered endpoint at publication - is proved by a DEVELOPMENT-BOOT SELF-TEST beside `unopened_provider_withdrawal`: an `i2c-bus` and a `gpio-lines` provider published through the real `Catalogue` with a real channel, the far end of each offered endpoint found closed at publication and each entry counting no consumer. The `i2c-hid` gate's rebind is credited only with what it shows - the controller giving its lines back and DeviceManager minting again after a parent rebinds - and explicitly not counted as a proof of the rule.

Re-check of the whole plan: the design (offered endpoint closed at publication and uncounted, scoped connections counted and refunded) now has a proof for each half - arithmetic on the host, the real handle in the development boot, the end-to-end effect in the gate - and still matches P02M0201's `smbus_ich9` row, which follows the same rule. Nothing else changed. The file is ASCII, cites no audit and keeps its `Status:` line.

Edited `docs/todo/P02M0195.md` only; no source was changed, and nothing was built or booted.
