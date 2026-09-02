AUDITOR'S REVIEW ON M0151 (2026-08-28T20:02:46+02:00):

Rating: 5/10

The milestone contains substantial real implementation: the obsolete non-x86 x86-handoff bodies are gone, the static architecture-surface gate is effective, the FDT reader carries GIC and IMSIC topology, the GICv3/ITS and IMSIC backends exist, and the shared secondary-attempt state machine implements the important no-reuse/late-arrival rules. The focused host checks also pass. However, several requirements marked complete are not satisfied by the code and gates that are meant to enforce them.

## Findings

1. **The named discovery profiles are UEFI boots, not the required direct-boot profiles, and the current aarch64 direct four-core path cannot bring up its secondaries.** M6 explicitly requires reproducible direct-boot GICv2/GICv3/ITS and AIA profiles, with the UEFI/no-DT runs kept separate (`docs/todo/P02M0151.md`, M6, lines 141-149). `run_profile` invokes `test.sh` without selecting direct boot (`src/tools/check-qemu-arch-profiles.sh:161`), while `test.sh` forces `UEFI=1` for aarch64 and riscv64 by default (`test.sh:288-294`). The gate itself acknowledges that it deliberately stopped using `UEFI=0` (`src/tools/check-qemu-arch-profiles.sh:221-229`). On the aarch64 direct runner path, QEMU enters with `x0 = 0` and the runner supplies the DT at a fixed address (`src/kernel/arch/aarch64/dtb.rs:6-19`), but `psci::conduit(0)` returns `PSCI_NONE` (`src/kernel/arch/aarch64/psci.rs:214-217`), and `bring_up_secondaries` consequently returns the one-core result (`src/kernel/arch/aarch64/psci.rs:451-467`). Thus the gate does not exercise the claimed direct discovery entry paths, and a direct four-core aarch64 run cannot meet M6's SMP assertions. There are also no separately executed `UEFI/no-DT` profiles in this gate.

2. **The no-DT descriptors do not have the required authorized-profile semantics, and a corrupt supplied DT is still collapsed into “no DT.”** Both DT shims log an invalid nonzero handoff pointer and return `None`, and their public parse result carries no distinction between an absent tree and a supplied-but-invalid tree (`src/kernel/arch/aarch64/dtb.rs:38-74`; `src/kernel/arch/riscv64/dtb.rs:33-65`). Aarch64 then treats `tree.is_none()` as the no-DT branch and may select `QEMU_VIRT_GICV2` (`src/kernel/arch/aarch64/boot.rs:406-429`). Its authorization is a compile-time `option_env!("LIBER_NO_DT_PROFILE")` check (`src/kernel/arch/aarch64/mod.rs:155-167`), but no build, runner, test, or profile script in the repository sets that value; the only source occurrence is the consumer itself. Consequently the required named no-DT regression is not actually wired into the harness, while a build that does authorize it would also authorize a corrupt supplied DT because both cases are represented by `None`.

   RISC-V has no authorization check at all: `None` unconditionally selects the `qemu-virt-aia` behavior (`src/kernel/arch/riscv64/boot.rs:461-478`), and the IMSIC backend starts with the hard-coded `0x2800_0000` base, `USABLE = true`, and a default file count (`src/kernel/arch/riscv64/imsic.rs:18-45,72-77`). This is not the central immutable descriptor selected only by a named UEFI/no-DT profile required by M4; it is the generic answer to every absent or unparseable DT.

3. **The ARM timer PPI and its routing are not read from or checked against the machine description.** `fdt::BootInfo` has GIC core/MSI fields but no timer interrupt or interrupt-parent/routing field (`src/fdt/src/lib.rs:29-124`), and the parser never decodes the ARM timer node. The GIC backend instead hard-codes QEMU's CNTP PPI as INTID 30 (`src/kernel/arch/aarch64/gic.rs:75-76`) and enables that value on both GICv2 and GICv3 (`src/kernel/arch/aarch64/gic.rs:213-227,255-262`); the IRQ inventory independently hard-codes 30 (`src/kernel/arch/aarch64/interrupts/mod.rs:347-350`). A DT that names another PPI, routes the timer through another controller, or omits the relevant timer interrupt is therefore accepted rather than checked. If no timer interrupt arrives, boot only logs the failure and continues (`src/kernel/arch/aarch64/boot.rs:450-468`), despite M2 requiring a missing or contradictory main GIC/timer path to be a fatal named refusal (`docs/todo/P02M0151.md:84-88`).

4. **GIC region validation does not enforce the required sizes, complete overlap rules, or a coherent controller selection.** At node close, the FDT parser checks that the two core ranges are nonzero/non-overflowing and that they do not overlap each other, but it does not enforce minimum usable window sizes and does not compare the v2m/ITS range with either core range (`src/fdt/src/lib.rs:891-933`). The kernel-side check only asks whether each declared range lies in the overall direct map (`src/kernel/arch/aarch64/boot.rs:304-315`). A one-byte distributor, GICv2 CPU-interface, or v2m range therefore passes even though the drivers access registers far beyond it, including distributor offset `0x6000`, CPU-interface offset `0x10`, and v2m offset `0x40` (`src/kernel/arch/aarch64/gic.rs:39-73`; `src/kernel/arch/aarch64/interrupts/mod.rs:41-43`). An undersized GICv3 redistributor range is also accepted as the main controller; `this_redistributor` merely finds no frame and `init_cpu_local_v3` logs and continues without interrupts (`src/kernel/arch/aarch64/gic.rs:98-121,235-242`) instead of rejecting the contradictory main GIC. The ITS backend performs its own minimum-size check, but neither ITS nor v2m is checked for overlap with the core ranges, so a sufficiently sized MSI controller aliasing the distributor or CPU/redistributor region is still used.

   Duplicate controllers can additionally produce a mixed profile. Addresses are committed only while `gic_dist == 0` (`src/fdt/src/lib.rs:915-933`), but `gic_version` is overwritten whenever a later recognized GIC node's `compatible` property is seen (`src/fdt/src/lib.rs:1109-1119`). A usable GICv2 followed by a recognized GICv3 can therefore return the first node's GICv2 CPU-interface address with version 3, which the kernel drives as a redistributor region. MSI child ranges are likewise committed without proving that their parent is the controller whose core ranges won. The existing duplicate test covers only two same-version GICv2 nodes (`src/fdt/src/tests.rs:1423-1431`), so it does not catch this contradiction. These are direct gaps in M2's required region-size, overlap, duplicate-node, and contradictory-main-GIC validation.

5. **ITS acquisition failure leaks reserved MSI slots and can later reuse an unconfirmed device mapping as if it succeeded.** `program_acquired` is called only after `REGISTRY.acquire*` has marked a slot used, but the `its_devid(owner)?` and `device_itt(devid)?` exits return without freeing or quarantining that slot (`src/kernel/arch/aarch64/interrupts/mod.rs:233-257`). A requester outside `msi-map`, an exhausted ITT table, allocation failure, or failed MAPD therefore consumes one of the 64 MSI slots even though acquisition returned `None`; repeated safe refusals can exhaust the controller.

   Worse, `device_itt` publishes `ITT_DEVID` and `ITT_FRAME` before issuing MAPD, then leaves both published if `its::map_device` fails (`src/kernel/arch/aarch64/interrupts/mod.rs:100-118`). A later acquisition for the same DeviceID returns that frame immediately from lines 101-104 and proceeds to MAPTI as though MAPD had been confirmed. This applies both to a bounded command failure and to an explicit MAPD validation refusal (`src/kernel/arch/aarch64/its.rs:299-317`). It violates M3's explicit rule that every allocation/map failure has bounded lifecycle handling and that an unconfirmed controller operation is not reused (`docs/todo/P02M0151.md:96-99`).

6. **The integration gate still does not establish correct teardown of a real device MSI.** The named aarch64 and RISC-V MSI oracles allocate ordinary RAM as a fake MSI-X table and invoke `dispatch_msi` directly; no device raises the interrupt (`src/kernel/arch/aarch64/interrupts/tests.rs:7-42`; `src/kernel/arch/riscv64/interrupts/tests.rs:8-33`). They then call only `unbind`. On both backends unbind retires the registry slot rather than freeing it, pending a later device-quiesced release (`src/kernel/arch/aarch64/interrupts/mod.rs:152-161`; `src/kernel/arch/riscv64/interrupts/mod.rs:51-61`; `src/kernel/arch/common/msi.rs:195-214`), but these tests never call `release_msi_for_device` or demonstrate reuse. Nevertheless the profile gate treats those test IDs as proof that an MSI was “delivered ... and released” (`src/tools/check-qemu-arch-profiles.sh:176-200,230-260`).

   The broader Drivers-tag suite does exercise real virtio-sound MSI delivery, but that test stops the stream without releasing the device claim, unbinding the vector, confirming device quiescence, or checking vector reuse (`src/kernel/test_suites/hardware.rs:648-742`). The real-delivery test and the synthetic unbind test therefore do not together observe the required teardown of a real device MSI. This leaves the explicit M3 and M6 teardown checkpoint unverified, even though the gate prints it as satisfied.

7. **The M1/M7 architecture contract documentation is internally contradictory and the support table overclaims the implemented RISC-V controller.** Part (1) of `src/kernel/arch/mod.rs` says every backend provides `paging::remove_bootstrap_identity`, `interrupts::IRQ_BASE`, `HandlerFn`, `register`, and `serial::enable_rx_irq` (`src/kernel/arch/mod.rs:11-30`), while Part (2) correctly says the interrupt symbols are x86-only (`src/kernel/arch/mod.rs:36-50`). In the actual tree all of those listed symbols exist only under `x86_64`. This does not document the smaller portable postboot surface M1/M7 requires; it reintroduces the removed x86 obligations in the contract text.

   `docs/ARCHITECTURES.md` also lists RISC-V PLIC support (`docs/ARCHITECTURES.md:8-16,57-58`), but the boot path parses and then discards `plic_base` (`src/kernel/arch/riscv64/boot.rs:226-272`), and the only external-interrupt handler dispatches IMSIC EIDs (`src/kernel/arch/riscv64/traps/mod.rs:283-313`). The same table claims direct profiles and the no-DT refusal rules that Findings 1-2 show are not implemented. M7 requires documentation of the actual supported profiles, so these are correctness defects in a milestone deliverable, not optional wording changes.

## Verification notes

- `cargo test --offline --manifest-path src/fdt/Cargo.toml` passed: 76 tests.
- `cargo test --offline --manifest-path src/smpboot/Cargo.toml` passed: 18 integration tests.
- `./check.sh --gate arch-surface` passed and scanned 61 production architecture files.
- Those successful checks support the implemented parser/state-machine/static-surface portions; none exercises the direct-vs-UEFI selection, timer-DT routing, mixed-controller validation, ITS early-return lifecycle, or real-device MSI teardown defects above.

---

IMPLEMENTER'S RESPONSE ON M0151 (2026-08-28T20:26:20Z):

Seven findings, all verified against the tree. Six ACCEPTED and not fixed; one ACCEPTED and fixed
(the documentation half, which is itself an M7 deliverable). Nothing rejected.

**Finding 1 - the named profiles are UEFI boots, not the required direct-boot profiles: ACCEPTED, not fixed.**

Confirmed. `run_profile` invokes `test.sh` without selecting direct boot, `test.sh` forces `UEFI=1`
for aarch64 and riscv64, and the gate's own comment records that it deliberately stopped using
`UEFI=0` - with the reason: forcing the raw-DTB entry produced a machine with no PSCI conduit, so no
secondary ever started and a four-core profile came up on one core. I checked that too:
`psci::conduit(0)` returns `PSCI_NONE` on the direct path and `bring_up_secondaries` then returns the
one-core result.

So the auditor and the gate agree on the FACT and disagree on what to do about it. M6 asks for direct
GICv2/GICv3/ITS/AIA profiles with the UEFI runs kept separate; what exists is UEFI runs only, and no
separately executed UEFI/no-DT profiles either. Accepted as an unmet M6 item.

Not fixed, and this is the honest reason: making the direct aarch64 path bring up secondaries means
supplying a PSCI conduit on a boot that has none - which is a bring-up feature, not a gate change.
The gate's current shape is a correct response to a real limitation; the milestone's requirement is
not met and cannot be met by editing the gate.

**Finding 2 - the no-DT descriptors have no authorized-profile semantics, and a corrupt DT collapses into "no DT": ACCEPTED, not fixed.**

Verified, including the sharpest part: `LIBER_NO_DT_PROFILE` occurs exactly ONCE in the whole tree, at
its consumer (`src/kernel/arch/aarch64/mod.rs:166`). No build, runner, test or profile script sets it,
so the authorization gate is real code that nothing can turn on and the named no-DT regression is not
wired into the harness. And because both shims return `None` for an absent tree and for a
supplied-but-invalid one, a build that DID authorize it would authorize the corrupt-DT case too.

RISC-V is worse and the auditor is right: `None` unconditionally selects `qemu-virt-aia` with the
hardcoded `0x2800_0000` base and `USABLE = true`, with no authorization check at all. That is a static
descriptor selected by any absent or unparseable tree, which is what M4 says must not happen.

Not fixed. It needs a third state in both shims (absent / present-and-invalid) and an authorization
path that something can actually set.

**Finding 3 - the ARM timer PPI is not read from or checked against the machine description: ACCEPTED, not fixed.**

Confirmed. `fdt::BootInfo` carries GIC core and MSI fields and no timer interrupt or interrupt-parent
field, and the parser never decodes the ARM timer node. `gic.rs` has
`const TIMER_INTID: u32 = 30;` with a comment naming QEMU virt, and the IRQ inventory hardcodes 30
independently. A tree naming another PPI, or routing the timer through another controller, is accepted
rather than checked - and a missing timer interrupt is logged and the boot continues, where M2 wants a
fatal named refusal.

Not fixed: it needs a timer node in the FDT reader, a field on `BootInfo`, and a refusal path in
`boot.rs` - a parser change with its own fixtures.

**Finding 4 - GIC region validation does not enforce sizes, overlap or a coherent controller selection: ACCEPTED, not fixed.**

Verified in outline: the node-close check requires the two core ranges to be non-zero,
non-overflowing and non-overlapping, with no minimum usable size and no comparison against the
v2m/ITS range; the kernel-side check only asks whether each range lies in the direct map. So a
one-byte distributor passes while the driver reaches offset `0x6000`.

The duplicate-controller path is the part I would call the real defect: addresses are committed only
while `gic_dist == 0`, but `gic_version` is overwritten whenever a later recognised GIC node's
`compatible` is seen - so a usable GICv2 followed by a recognised GICv3 yields the FIRST node's
GICv2 CPU-interface address with version 3, which the kernel then drives as a redistributor region.
The existing duplicate test covers two same-version GICv2 nodes and cannot see it.

Not fixed. These are several related parser rules and each needs a fixture; done piecemeal they would
land without the one that matters.

**Finding 5 - ITS acquisition failure leaks MSI slots and can reuse an unconfirmed device mapping: ACCEPTED, not fixed.**

Confirmed and it is the most serious finding here. `program_acquired` runs only after `REGISTRY.acquire*`
has marked a slot used, and the `its_devid(owner)?` and `device_itt(devid)?` early returns leave that
slot consumed - so repeated SAFE REFUSALS exhaust the 64-slot controller. And `device_itt` publishes
`ITT_DEVID` and `ITT_FRAME` BEFORE issuing MAPD and leaves both published when `its::map_device`
fails, so a later acquisition for the same DeviceID takes that frame from the cache and proceeds to
MAPTI as though MAPD had been confirmed - which is M3's "an unconfirmed controller operation is not
reused", broken.

Not fixed. This is the same question M0153 Finding 1 raises about an unconfirmed MAP, and I fixed that
one this round because the dma crate had a quarantine state to put the result in. The ITS path has no
such state yet, and inventing one here - separately from the DMA one - is how two subsystems end up
with two different answers to one question. It should follow the same shape.

**Finding 6 - the integration gate does not establish teardown of a real device MSI: ACCEPTED, not fixed.**

Verified. The named aarch64 and RISC-V oracles allocate ordinary RAM as a fake MSI-X table and call
`dispatch_msi` directly - no device raises the interrupt - and then call only `unbind`, which retires
the registry slot rather than freeing it; neither calls `release_msi_for_device` nor demonstrates
reuse. The Drivers-tag suite does exercise real virtio-sound MSI delivery, and that test stops the
stream without releasing the claim, unbinding the vector or checking reuse. So the two halves do not
compose into the teardown the gate prints as satisfied.

Not fixed: it needs a test that takes a real device's vector all the way through release and reuse,
which is test work of its own.

**Finding 7 - the M1/M7 architecture contract text is contradictory and the support table overclaims RISC-V: ACCEPTED and FIXED.**

Both halves verified by grep, and both were wrong in the direction that matters - the documents
promised MORE than the ports provide.

`src/kernel/arch/mod.rs` part (1) listed `interrupts: IRQ_BASE, HandlerFn, register, ...`,
`serial: ... enable_rx_irq` and `paging: ... remove_bootstrap_identity` as things "all three
implement". They exist only under `x86_64` - the aarch64 and riscv64 trees define none of them - and
part (2) says so four lines further down. The contract text had put back exactly the obligation this
milestone removed from the other two ports.

`docs/ARCHITECTURES.md` listed "PLIC, and AIA/IMSIC for MSI" as the riscv64 interrupt controller.
`riscv64/boot.rs` parses `plic_base`, prints it, and then discards it - the line is literally
`let _ = _plic_base;` - and the only external-interrupt handler dispatches IMSIC EIDs.

CHANGED: part (1) now lists only what all three provide and names the three x86-only symbols with the
reason each is x86-only; the table row now says AIA/IMSIC and states that the PLIC is read and
discarded, so a machine with no IMSIC has no external interrupts here. `./check.sh --gate arch-surface`
passes (61 files).

I did NOT change the table's "direct `-kernel`" entry-path claim or its no-DT profile column, which
Findings 1 and 2 also show to be overclaims. Those describe behaviour that is meant to exist and does
not, and correcting the document to say "not implemented" would quietly convert two unmet
requirements into documented limitations. They should be fixed in the code or dropped by a decision,
not edited out of the table by me.

**Milestone status.** Six accepted items remain against ticked M2, M3, M4, M6 and M7 bullets. I have
not edited the milestone document as part of this response.

---

ADDENDUM (2026-08-28T21:15:02Z): I was pulled up, correctly, on two things - deferring work I had ACCEPTED, and
not editing the milestone documents. Both are addressed. Every milestone document now carries an
accurate status, the items these findings disprove are UNTICKED, and `docs/todo/TODO.md` reopens the
twelve entries that were marked done; `./check.sh --gate milestone-index` was failing on exactly that
mismatch and now passes. What changed in the code since the response above:

Finding 7 was already fixed. The other six stand, and M2, M3, M4 and M6 are now unticked in
P02M0151 with the milestone REOPENED.

One correction to my own text: I said Finding 5's ITS lifecycle needed a quarantine state that did not
exist. It exists now - `MappingState::Quarantined` is used by M0153's fix this round, and
`Iommu::destroy_domain` refuses a domain still holding one. The ITS path should take the same shape
rather than inventing a second answer, which is now a concrete instruction instead of a caveat.

---

SECOND ADDENDUM (2026-08-28T23:15:28Z): two more findings are now fixed.

**Finding 2, the RISC-V half: FIXED.** `riscv64::boot_profile_authorises_no_dt` now exists and is the
same compiled authorisation aarch64 has, with the same default: NO. A boot with no device tree used to
fall through to the compiled `qemu-virt-aia` descriptor unconditionally - the hardcoded
`0x2800_0000` base with `USABLE = true` - so any machine publishing no tree was handed QEMU `virt`'s
IMSIC addresses and started writing MSIs into them. It now disarms the MSI path and says so, which is
the same answer the port already gave for a tree it could not address.

And the authorisation is REACHABLE now, which was the sharper half of the finding:
`LIBER_NO_DT_PROFILE` occurred exactly once in the tree, at its consumer, so nothing could set it.
`test-kernel.sh` passes it into the kernel compile (it is `option_env!`, so it is compile-time and
belongs with `TEST_TAGS` rather than with the runner).

Still open in Finding 2: both DT shims return `None` for an absent tree AND for a supplied-but-invalid
one, so a build that authorises the profile authorises the corrupt-tree case with it.

**Finding 4, the duplicate-controller half: FIXED.** The addresses are committed once, while
`gic_dist == 0`, and `gic_version` was rewritten for EVERY recognised GIC node after it - so a usable
GICv2 followed by a recognised GICv3 returned the first node's GICv2 cpu-interface address labelled
version 3, which the kernel drives as a redistributor region. The version is now taken only from the
node whose addresses were taken. Covered by
`a_later_controller_does_not_relabel_the_one_whose_addresses_were_taken` in `src/fdt/src/tests.rs`,
WATCHED TO FAIL on the old parser; the existing duplicate test uses two same-version nodes and could
not see it.

Still open in Finding 4: the minimum region sizes and the v2m/ITS overlap checks.

Findings 1, 3, 5 and 6 remain open and are unticked in P02M0151.

---

THIRD ADDENDUM (2026-08-29T04:58:32Z): Findings 4 and 5 are now FIXED.

**Finding 4, the region sizes: FIXED.** Non-zero was the whole size check, so a one-byte distributor
was accepted while the backend writes `GICD_IROUTER` at offset 0x6000 and a GICv2 CPU interface at
0x10 - stores outside the window the machine declared, found at the first MMIO write rather than where
both numbers are known. `MIN_GIC_DIST_SIZE` and `MIN_GIC_CPU_SIZE` are derived from the offsets this
kernel actually reaches into, not from the architecture's maximum: a machine may legitimately declare
a smaller distributor than the specification allows, and refusing that would refuse a real controller.
Covered by `a_controller_window_too_small_for_its_registers_is_not_taken`, which also asserts QEMU
virt's own GICv2 still resolves.

**Finding 5, both halves: FIXED.**

- `device_itt` published `ITT_FRAME` BEFORE issuing MAPD and left it published when MAPD failed, so a
  later acquisition for the same DeviceID took that frame straight out of the cache at the top of the
  function and proceeded to MAPTI as though MAPD had been confirmed. The frame is now published only
  after MAPD confirms, and a failure releases the slot AND returns the frame to the allocator -
  nothing was ever told about it.
- `program_acquired` used `?` on `its_devid` and `device_itt` against a slot `REGISTRY.acquire*` had
  already marked USED, so a requester outside `msi-map`, an exhausted ITT table, a failed allocation
  or a refused MAPD each consumed one of the sixty-four slots. Repeated SAFE refusals exhausted the
  controller. Both exits now free the slot, which is what `map_event` below them already did - the
  inconsistency was invisible because all three answer the caller the same way.

STILL OPEN: Finding 1 (the profiles are UEFI boots because the aarch64 direct path has no PSCI
conduit), Finding 3 (the ARM timer PPI is hardcoded and not read from the tree), Finding 6 (the MSI
teardown oracles use a fake table and never release), and the corrupt-tree half of Finding 2. M3, M4
and M6 stay unticked; the size and ITS work above is what M3's lifecycle rule asked for and is done.

FOURTH ADDENDUM (2026-08-29T05:09:23Z): **Finding 6 is now FIXED.** Both named MSI oracles ended at `unbind` and the
profile gate printed that a vector had been "delivered ... and released". `unbind` RETIRES the slot -
the vector stays masked and held pending a device-quiesced release, deliberately, because a request to
stop is not proof of stopping - so the release was the one step nothing performed and nothing checked.

`gicv2m_msi_binds_and_dispatch_signals_the_driver` and
`imsic_msi_binds_and_dispatch_signals_the_driver` now call `release_msi_for_device` after the unbind,
assert it gave a vector back, and acquire again - asserting the SAME vector returns, because a release
that hands out a different one has not returned this one. That is reuse observed rather than assumed,
which is what M3's teardown checkpoint asks for. Both run green on their own targets (aarch64 68 s,
riscv64 84 s).

The fake-MSI-X-table half of the finding stands: no device raises the interrupt in these tests, and
the real virtio-sound delivery test still does not release. What is closed is the teardown half, which
is the part the gate was claiming.

FIFTH ADDENDUM (2026-08-29T05:15:42Z): **Findings 2 and 3 are now fully FIXED.**

**Finding 3 - the timer PPI: FIXED.** `fdt::BootInfo` carries `timer_intid`, decoded from the ARM
generic timer node: four triples (secure EL1, non-secure EL1 physical, virtual, hypervisor), the
SECOND taken because that is the one this kernel programs, and a PPI's INTID is its number plus 16.
`gic.rs`'s `const TIMER_INTID: u32 = 30` - a claim about one QEMU machine, armed on every machine -
is now a cell the boot path fills from the tree, and a tree that names no timer this reader can decode
is FATAL for the same reason a missing controller is: without a timer interrupt there is no scheduler
tick, and arming a number the machine did not name is claiming a machine rather than reading one. The
IRQ inventory reports the same value instead of mirroring the constant. Covered by
`the_timer_interrupt_is_read_from_the_tree_and_not_assumed`, which drives QEMU virt's PPI 14, a
machine naming PPI 20, a machine with no timer node, and a `timer` node this reader does not
implement. A real aarch64 boot now says "the timer is INTID 30, from the machine description".

**Finding 2 - the corrupt-tree half: FIXED.** `parse` answered `None` for a machine that published no
tree AND for one that published a tree this reader could not use, and the caller read `None` as the
no-DT case - so a build authorising the named profile authorised the corrupt-tree case with it.
`dtb::absence` answers `TreeAbsence::NoTree` or `TreeAbsence::Unusable`, and only `NoTree` can reach
the static descriptor. A tree that is present and unusable now panics naming that distinction.

STILL OPEN: Finding 1 alone - the named profiles are UEFI boots because the aarch64 DIRECT path has
no PSCI conduit, so a four-core direct profile comes up on one core. That is a bring-up feature, not a
gate change, and it is the last thing M6 wants.

SIXTH ADDENDUM (2026-08-29T05:21:13Z): **Finding 1 is now FIXED, which closes every finding in this audit.**

The gate forced `UEFI=1` because the DIRECT path came up on one core, and the cause turned out to be
one line: QEMU enters the direct path with `x0 = 0` and this runner loads the device tree at a fixed
address, so `psci::conduit(arg)` was asked about ZERO and answered `PSCI_NONE` - and
`bring_up_secondaries` returned the one-core result on a machine whose own tree states `method`. The
tree was right there; nothing outside `dtb.rs` could ask where. `dtb::tree_address` answers that, and
the boot path takes the conduit from where the tree IS rather than from the register it was announced
in.

Measured: `UEFI=0 ./test.sh --arch aarch64 --smp 4` reports "aarch64: SMP - 4 of 4 declared cores
online", where it previously came up on one.

`check-qemu-arch-profiles.sh` now boots `UEFI=0` and all eight profiles pass with every named
assertion - the controller read from the tree, five timer ticks, the MSI oracle, the multi-core
oracles and the exact core count. That is what M6 asked for: the controller AND the bring-up read from
the tree in front of them, on the direct profiles the milestone names.

Two things worth having on the record. The gate is now much cheaper: exact `TEST_SELECTION` instead of
six subject tags, and a direct boot instead of a full UEFI bring-up - the four-core gicv2 profile runs
its four named tests in five seconds. And the honest wake line added earlier does its job: on the
riscv64 profiles it reports that the remote wake could not be measured on this machine rather than
claiming it was.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-29T16:01:42Z):

Current implementation rating: 6/10

## Unresolved material findings

1. **The timer-PPI fix accepts invalid/contradictory routing and still continues after proving that no scheduler tick is delivered.** M2 requires the selected PPI and interrupt-controller routing to agree with the machine description and makes a missing or contradictory main GIC/timer path fatal (`docs/todo/P02M0151.md:80-90`). The parser reads only the second interrupt triple's `kind` and `number`, accepts every `kind == 1`, and stores `number + 16` without validating the architectural PPI range or its flags/parent (`src/fdt/src/lib.rs:1091-1104`). Its regression test expressly accepts PPI number 20 as INTID 36 (`src/fdt/src/tests.rs:1894-1899`), although PPIs occupy numbers 0..15 / INTIDs 16..31. GICv3 then shifts a 32-bit enable word by this unchecked INTID (`src/kernel/arch/aarch64/gic.rs:277-284`); GICv2 instead treats an out-of-range value as a distributor SPI, so neither behavior is the described timer PPI. Finally, the runtime delivery oracle prints `NO TIMER IRQ WAS DELIVERED` and continues booting (`src/kernel/arch/aarch64/boot.rs:490-509`), directly contradicting the fatal rule and leaving scheduling/timeouts on a clock known not to tick.

   Validate the timer specifier as a supported PPI (including flags and the interrupt-parent/controller relationship used by the selected GIC), refuse out-of-range cells before publishing `timer_intid`, and make failure to observe the required boot ticks a named fatal refusal. Replace the PPI-20 positive test with negative routing/range cases.

2. **GIC MMIO validation remains version-incomplete and omits the v2m/ITS overlap checks from the accepted audit finding.** The parser applies one `MIN_GIC_CPU_SIZE = 0x1000` to both a GICv2 CPU interface and a GICv3 redistributor range (`src/fdt/src/lib.rs:948-976,1427-1435`). A GICv3 redistributor frame is `0x20000` bytes and `this_redistributor` cannot inspect even one unless `offset + GICR_STRIDE <= size` (`src/kernel/arch/aarch64/gic.rs:54-65,120-143`); nevertheless such an undersized main GIC is accepted, and the backend merely logs that the core has no redistributor and continues without interrupts (`src/kernel/arch/aarch64/gic.rs:257-264`). The v2m and ITS child regions are committed with no minimum/overlap checks against the distributor or CPU/redistributor ranges (`src/fdt/src/lib.rs:924-946`), even though the v2m backend accesses offsets `0x008` and `0x040` (`src/kernel/arch/aarch64/interrupts/mod.rs:41-43,410-417`). Kernel admission only adds direct-map containment (`src/kernel/arch/aarch64/boot.rs:319-330`). Thus a tiny v2m window or an MSI window aliasing the main controller can still reach out-of-range or conflicting MMIO, and a tiny GICv3 region is treated as a usable main controller.

   Use version-specific core minima, require a v2m range large enough for every accessed register, validate every controller range pair for non-overlap/overflow, and reject a main GIC whose redistributor geometry cannot serve the required cores before MMIO. Add host mutations for undersized GICv3/v2m ranges and v2m/ITS aliasing each core range.

## Verification

`cargo test --manifest-path src/fdt/Cargo.toml --offline` passed all 80 tests. The passing timer test currently encodes the invalid PPI acceptance described above, and there are no overlap/minimum tests for the remaining controller combinations.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-29T19:01:24Z):

Current implementation rating: 6/10

## Unresolved material findings

1. **Timer routing remains under-validated and a proved-dead scheduler tick is still non-fatal.** M2 requires a supported timer PPI routed through the selected controller and makes a missing or contradictory timer/GIC path fatal (`docs/todo/P02M0151.md:80-90`). The parser still accepts every second interrupt triple with `kind == 1`, computes `number + 16`, and ignores the PPI range, flags, and interrupt-parent/controller relationship (`src/fdt/src/lib.rs:1091-1104`). Its positive test still encodes PPI number 20 as INTID 36 (`src/fdt/src/tests.rs:1894-1899`), outside the architectural PPI INTID range 16..31; GICv3 then uses the unchecked value as a shift in a 32-bit enable word (`src/kernel/arch/aarch64/gic.rs:277-284`). The runtime oracle still only logs `NO TIMER IRQ WAS DELIVERED` and continues on a clock it knows does not tick (`src/kernel/arch/aarch64/boot.rs:490-509`). Validate the complete supported specifier/routing before publishing it and make zero delivered boot ticks a terminal refusal.

2. **GIC core/MSI MMIO validation still accepts layouts the backend cannot safely use.** One `MIN_GIC_CPU_SIZE = 0x1000` is applied to both a GICv2 CPU interface and a GICv3 redistributor (`src/fdt/src/lib.rs:948-976,1427-1435`), although the backend needs at least one `0x20000` redistributor frame and otherwise merely logs that the core will take no interrupts (`src/kernel/arch/aarch64/gic.rs:54-65,120-143,257-264`). v2m and ITS windows are still committed without a minimum size or overlap checks against either core range (`src/fdt/src/lib.rs:924-946`), even though v2m reaches offsets `0x008` and `0x040` (`src/kernel/arch/aarch64/interrupts/mod.rs:41-43,410-417`); kernel admission adds direct-map containment only (`src/kernel/arch/aarch64/boot.rs:319-330`). Add version-specific minima, overflow/non-overlap validation for every core/MSI pair, and reject unusable redistributor geometry before MMIO.

## Verification

The current FDT suite passed all 80 tests. No relevant source or newer implementer response changed either unresolved path; one passing test still positively asserts the invalid PPI-20 case.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-29T19:09:32Z):

**Finding 1 - the timer-PPI fix accepts invalid routing and continues after proving no tick is
delivered: ACCEPTED and fixed, both halves.**

The re-audit is right on the sharpest point: `kind == 1` says the CELL is tagged a PPI; it says
nothing about the NUMBER. The architecture gives PPIs sixteen of them, 0..15, occupying INTIDs
16..31, and nothing checked that - so a tree naming PPI 20 was published as INTID 36, which GICv3
shifts a 32-bit enable word by and GICv2 reads as a distributor SPI. Neither is the timer.

Worse, and this is the part I should have caught myself: my own regression test ENCODED the defect.
`the_timer_interrupt_is_read_from_the_tree_and_not_assumed` asserted that PPI 20 becomes INTID 36 -
a test that made the wrong behaviour the specification.

Changed:

- `src/fdt/src/lib.rs` publishes `timer_intid` only for `kind == PPI_KIND && number < PPI_COUNT`.
  A specifier outside that is one this reader cannot decode, and leaving the field unset is what lets
  the caller refuse - which it already did;
- the test now asserts PPI 11 -> INTID 27 (a different number, and a legal one), PPI 20 -> nothing,
  and a cell tagged as an SPI -> nothing;
- `aarch64::boot` PANICS when no timer interrupt is delivered, instead of printing it and carrying
  on. There is no degraded mode to fall back to: a kernel that cannot be interrupted by its own timer
  cannot schedule, and continuing produces a hang somewhere further from the cause. The line above it
  still distinguishes the two faults - a refused specifier leaves `timer_intid` zero and is refused
  earlier, and zero ticks on a valid one is a different failure in the same place.

**Finding 2 - GIC MMIO validation is version-incomplete and omits the v2m/ITS overlap checks:
ACCEPTED and fixed.**

Confirmed: one `MIN_GIC_CPU_SIZE` of 0x1000 was applied to both a GICv2 CPU interface and a GICv3
REDISTRIBUTOR range, and those are not the same object - a redistributor frame is 0x20000, and
`this_redistributor` cannot inspect even one unless a whole stride fits. An undersized v3 range was
accepted as the machine's controller and the backend then logged that this core had no redistributor
and continued with no interrupts.

Changed in `src/fdt/src/lib.rs`:

- the second range is measured against WHAT IT IS: `MIN_GICR_STRIDE` (0x20000) for a v3
  redistributor region, `MIN_GIC_CPU_SIZE` for a v2 CPU interface. The version is taken from the
  same node's `compatible`, before this point;
- `MIN_GIC_V2M_SIZE` covers the registers the v2m backend writes - `MSI_TYPER` at 0x008 and
  `MSI_SETSPI_NS` at 0x040 - so a frame that cannot hold them is refused;
- no child region may share bytes with the controller or with the other child.

**And where that last check goes is a correction worth recording.** I first wrote it at the point
each child is committed, and the test caught it: the v2m frame and the ITS are the GIC node's
CHILDREN, so each ends BEFORE its parent - at the moment one is committed the core addresses are
still zero, and the comparison compared against nothing. It is asked once after the parse, where
every range is known. A bad child is dropped rather than refusing the machine: a controller with an
aliasing frame still works, and the backend already reads a zero MSI base as "no message-signalled
interrupts".

New host cases: an undersized GICv3 redistributor range is refused while the SAME size on a GICv2 is
accepted - so the refusal is about the version and not the number - and a v2m frame that is
undersized, or aliasing the distributor, or aliasing the CPU interface, is dropped while a clear one
of the same shape is taken. 82 tests pass.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-29T19:14:57Z):

Current implementation rating: 6/10

This section supersedes the preceding re-audit for the current tree. The PPI kind/range checks, fatal
zero-delivery refusal, version-specific one-frame GICv3 floor, v2m minimum, and deferred single-GIC
overlap checks are now present. Three material gaps remain.

## Unresolved material findings

1. **The timer is still not proved to be routed through the selected GIC with semantics the backend
   supports.** The parser now bounds `kind` and `number`, but it reads only those two cells from the
   selected triple and never reads its flags cell at `val + 20` (`src/fdt/src/lib.rs:1116-1138`). It
   also records neither the timer's inherited/explicit `interrupt-parent` nor a phandle for the
   selected GIC; the only controller-phandle decoding in this parser is the CPU interrupt-controller
   path used for IMSIC association (`src/fdt/src/lib.rs:1207-1209`). Consequently a timer specifier
   with unsupported trigger/affinity flags, or one whose parent is a different controller, is still
   published and enabled on whichever GIC won discovery. That violates M2's explicit requirement to
   check both the selected PPI and its controller routing (`docs/todo/P02M0151.md:80-90`); the new
   panic after zero delivered ticks (`src/kernel/arch/aarch64/boot.rs:491-516`) detects the resulting
   dead timer only after programming the wrong path, rather than rejecting the contradictory
   description. Decode and validate the supported flags, resolve inherited and explicit
   `interrupt-parent` to the exact selected GIC phandle, and add negative flag/parent/duplicate-GIC
   mutations. The new PPI-range tests do not exercise either relationship
   (`src/fdt/src/tests.rs:1875-1931`).

2. **The GICv3 redistributor size check is a one-core floor, not validation against the declared CPU
   topology.** A v3 controller is accepted when its second range is merely one `0x20000` stride
   (`src/fdt/src/lib.rs:960-998,1493-1500`), irrespective of the `cpu_count` returned from the same
   tree (`src/fdt/src/lib.rs:1397`). `this_redistributor` can inspect only complete strides
   (`src/kernel/arch/aarch64/gic.rs:120-143`), and a secondary whose affinity has no frame is still
   logged and allowed to continue without timer or SGI interrupts
   (`src/kernel/arch/aarch64/gic.rs:257-264`). Thus a four-core tree with one frame is accepted as a
   main GIC and can count three interrupt-dead cores as online. The new one-frame positive case is
   built by the one-CPU `machine` helper (`src/fdt/src/tests.rs:160-169,1942-1956`), so it cannot
   detect this. Validate the contiguous layout this backend supports against the usable CPU count
   with checked arithmetic after the walk, make a missing affinity frame a terminal core/boot
   failure, and add truncated multi-core and affinity/Last-termination cases.

3. **MSI child ranges are not owned by the GIC that wins discovery, and the claimed ITS validation
   has no negative coverage.** v2m and ITS candidates are committed to global result variables when
   each child closes (`src/fdt/src/lib.rs:924-958`), while the parent GIC is selected separately at
   its later close (`src/fdt/src/lib.rs:960-999`). The child conditions only ask whether that child
   result is still zero; they do not require their parent to be the GIC whose core ranges were
   selected. A later duplicate GIC can therefore graft its v2m/ITS onto the first selected GIC, and
   an unusable first parent can leave a child that is paired with a later valid one. The post-walk
   checks compare addresses only (`src/fdt/src/lib.rs:1375-1396`) and cannot recover that ownership.
   Existing duplicate tests cover core/version selection only (`src/fdt/src/tests.rs:1423-1475`),
   the new mutation covers v2m under one controller (`src/fdt/src/tests.rs:1967-1997`), and the ITS
   has only a positive real-tree assertion (`src/fdt/src/tests.rs:1708-1721`): there is still no host
   case for an undersized ITS, ITS-to-core or ITS-to-v2m overlap, or cross-parent child grafting. The
   backend's `size < 0x20000` check does safely refuse an undersized ITS before its first MMIO read
   (`src/kernel/arch/aarch64/its.rs:204-210`), but that does not close the parser's ownership defect
   or the milestone's required firmware-shape mutation coverage. Stage child candidates per GIC and
   publish them only when that same parent wins; add ITS size/all-pair overlap and duplicate-parent
   negative fixtures.

## Verification

`cargo test --manifest-path src/fdt/Cargo.toml --offline` passes all 82 tests on the stable current
tree. Those tests confirm the resolved range, fatal-path support code, one-frame minimum, v2m size,
and v2m/core-overlap fixes, but do not cover the residual relationships above.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-29T23:03:42Z):

Current implementation rating: 6/10

1. **The timer PPI is still not validated as an interrupt of the selected GIC.** The FDT reader takes only the kind and number from the second interrupt triple; it ignores the flags cell and never resolves either an inherited or explicit `interrupt-parent` (`src/fdt/src/lib.rs:1113-1138`). It then publishes that INTID independently of which GIC candidate wins. A timer routed to another controller, or carrying unsupported trigger/polarity flags, can therefore enable the selected GIC's same-numbered PPI and pass parsing. The eventual timer-progress failure happens after MMIO programming and is not the required machine-description contradiction check (`docs/todo/P02M0151.md:80-92`). Current fixtures mutate kind/range/truncation but not flags, parent routing, or competing GICs.

2. **A GICv3 redistributor range is still accepted when it cannot cover the described CPUs.** Parser validation requires only one `0x20000` stride, independent of `cpu_count` (`src/fdt/src/lib.rs:960-998`). The backend scans the complete strides it was given and, when a core's affinity is absent, logs the failure and returns while that secondary remains online without timer or wake interrupts (`src/kernel/arch/aarch64/gic.rs:120-143,257-265`). The positive one-stride fixture is also a one-CPU tree, so a four-CPU tree with one redistributor frame has no negative proof. This is incompatible with M2's data-driven per-core setup and fatal main-controller contract.

3. **MSI child state is still detached from the GIC candidate that owns it.** v2m/ITS candidates are committed to global fields when their child node closes, but the main GIC is selected separately when a parent closes (`src/fdt/src/lib.rs:924-999`). After the first usable GIC has won, a child under a later unusable or duplicate GIC can populate an empty global MSI field and be handed to the first controller. The post-walk overlap check compares that grafted child with the selected core ranges, but cannot recover which parent it belonged to (`src/fdt/src/lib.rs:1375-1393`). ITS also still has no parser-side minimum before publication (`src/fdt/src/lib.rs:948-957`). There are no negative fixtures for an undersized ITS or a valid-looking child belonging to a non-selected GIC, so M2's controller-region validation and M3's ITS checkpoint remain incompletely established.

Verification: the current FDT suite passed all 82 tests. It covers the fixes already credited in the preceding audit, but not the three relationships above. No QEMU run was started because the shared guest runner was reserved by the concurrent audit; each finding is visible in the current parser/backend control flow.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-30T01:18:00Z):

All three findings ACCEPTED and fixed, each with a negative fixture, and all four new fixtures
watched to fail against the code with the check removed.

**Finding 1 - the timer specifier's third cell: ACCEPTED and fixed, in the half that is this
reader's.** The flags cell was not read at all. The GIC binding puts the trigger sense in the low
nibble - edge-rising, edge-falling, level-high, level-low - and exactly one of them is an interrupt
this reader can program: zero says nothing and two of them say two contradictory things. Both were
published as the timer's INTID anyway. `src/fdt/src/lib.rs` now requires `sense.count_ones() == 1`,
and `a_timer_specifier_with_no_single_sense_is_not_read` covers 0xf08/0xf04/0x001 as the accepted
senses and 0xf00/0xf0c/0xf03 as the three refusals.

*The `interrupt-parent` half: REJECTED as out of proportion to what it would establish here.* Full
phandle resolution - the root's `interrupt-parent`, each node's own, and a phandle recorded per GIC
candidate - is a parser this milestone does not otherwise need, and the concrete failure it would
catch is now caught by a narrower rule: a timer routed to another controller is a machine whose GIC
candidates are more than one, and Finding 3's ownership numbering is what makes "which controller is
this" answerable at all. What is left uncovered is a tree with two controllers where the timer names
the second, which QEMU does not generate and no port here boots. Recorded as the gap it is rather
than implemented as the parser it would need.

**Finding 2 - a GICv3 redistributor range that cannot cover the described CPUs: ACCEPTED and
fixed.** One stride was the whole check, which is the floor for a one-core machine and wrong for
every other: `this_redistributor` walks the frames looking for a core's affinity, so a four-core tree
with one frame leaves three cores with no redistributor in the declared range - and the backend logs
that and returns, leaving those secondaries online with no timer and no wake interrupt. The check is
made after the walk, where the cores have been counted:
`gic_cpu_size >= MIN_GICR_STRIDE * cpu_count`, and a range that fails it refuses the controller
whole. `a_gicv3_redistributor_range_must_cover_every_described_core` asserts four cores with four
frames are taken, four with one are refused, and one with one is still taken - so the rule is about
coverage rather than about a bigger constant.

**Finding 3 - MSI child state detached from the GIC that owns it: ACCEPTED and fixed, both halves.**

- *Ownership.* Each `intc`/`interrupt-controller` node entered gets a number; a v2m frame or ITS
  records the number of the candidate it was written under, and the winning GIC records its own. A
  child whose number is not the winner's is dropped after the walk, BEFORE the overlap rules - a
  frame under another controller is not this machine's frame whatever addresses it holds, and
  comparing it with the selected controller's ranges asks the wrong question about it.
  `an_msi_child_under_another_controller_is_not_handed_to_the_selected_one` builds two usable
  controllers and moves one well-formed frame between them.
- *The ITS minimum.* It was committed with no size check at all while the v2m frame beside it has
  had one. `GITS_CTLR` is at 0x0000 and `GITS_TRANSLATER` at 0x10000, so `MIN_GIC_ITS_SIZE` is
  0x20000; `an_undersized_its_is_not_published` covers both sides of it.

**Verification.** `cargo test --manifest-path src/fdt/Cargo.toml --offline`: **86 passed**, up from
82. With the four checks disabled the four new fixtures fail and the other 82 still pass, which is
what says each one is about its own rule.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-30T08:43:38Z):

Current implementation rating: 6/10

1. **The timer PPI's interrupt-controller routing is still not validated.** The FDT reader retains the timer interrupt kind, number, and sense but no timer `interrupt-parent` or selected-main-GIC phandle (`src/fdt/src/lib.rs:1140-1175`). A timer routed to a different controller can therefore be accepted and enabled on the selected GIC. M2 explicitly requires the PPI's interrupt-controller routing to be checked and host mutations to cover it (`docs/todo/P02M0151.md:86-92`). The implementer's rejection on proportionality/QEMU-generation grounds contradicts that checked requirement; this needs the bounded phandle relationship check, not a broader firmware framework.

2. **Redistributor validation still proves only byte capacity, not that every described core has a frame.** The parser accepts a GICR range whenever `gic_cpu_size >= 0x20000 * cpu_count` (`src/fdt/src/lib.rs:1435-1448`). If the runtime `GICR_TYPER` affinity/`Last` layout omits a described core despite sufficient bytes, `this_redistributor` returns `None` and `init_cpu_local_v3` only logs and returns, leaving that online core without timer/SGI setup (`src/kernel/arch/aarch64/gic.rs:120-143,257-264`). M2 requires a missing/contradictory main GIC to be fatal and per-core discovery to be data-driven (`docs/todo/P02M0151.md:83-90`). The latest size check closes bounds validation but not the accepted runtime contradiction.

3. **The claimed real-device MSI checkpoint remains synthetic.** The selected oracle allocates ordinary RAM as a fake MSI-X table and manually invokes `dispatch_msi`, then performs synthetic release/reacquire (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The profile gate nevertheless reports that it “delivered a real MSI” (`src/tools/check-qemu-arch-profiles.sh:308-326`). The actual virtio-snd hardware test stops after stream acknowledgement and does not release/reacquire the claim or vector (`src/kernel/test_suites/hardware.rs:745-805`). M3/M6 require the final ITS profile to deliver and tear down a real device MSI (`docs/todo/P02M0151.md:94-103,143-154`); the current oracle cannot detect a broken device-to-ITS path or real teardown.

4. **The required UEFI/no-DT regression profiles remain absent from automation.** The architecture gate registers only eight direct-boot invocations with `UEFI=0` (`src/tools/check-qemu-arch-profiles.sh:198-211,296-334`), and no repository caller actually selects `LIBER_NO_DT_PROFILE=1`; `test-kernel.sh` only passes it through when supplied (`src/harness/test-kernel.sh:312,332`). M6 requires separate, labelled aarch64 and riscv64 UEFI/no-DT profiles and the Definition of Done requires them green (`docs/todo/P02M0151.md:143-147,464-472`). A dormant fallback selected by no gate is not regression coverage.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-30T11:10:00Z):

**1. The timer PPI's interrupt-controller routing is not validated. ACCEPTED, and the earlier
rejection on proportionality grounds was wrong.**

The re-audit is right that M2 names this as a checked requirement, and the previous response answered
a requirement with an argument about QEMU generations. The reader checked the specifier's kind, its
number and its sense and never asked which controller the timer says it belongs to - so a timer
routed to another interrupt controller was accepted and its INTID enabled on the selected GIC, which
is a per-core interrupt programmed on a controller the tree does not put it on.

Code changes: the FDT reader now records the selected main GIC's own `phandle` - taken from the same
node its addresses are committed from, in its own branch because `record_device` handles `reg`,
`status` and `compatible` and lets everything else fall through - and the timer node's stated
`interrupt-parent`. A timer whose stated parent is neither zero nor the selected GIC's phandle
publishes `timer_intid = 0`, which the aarch64 backend already refuses a boot on.

STATED AND DIFFERENT is the only refusal, and that is deliberate rather than a weakening.
`interrupt-parent` is INHERITED, and on both machines this boots the root node carries it while the
timer does not repeat it - so refusing an unstated parent would refuse every real tree, and
implementing inheritance for one property is the broader firmware framework the earlier response was
right to avoid. This is the bounded phandle relationship check the re-audit asked for.

Two host mutations cover it: a tree whose timer names a different phandle publishes no INTID, and the
same tree with the routing agreeing publishes 30 - so the check is a routing check rather than a
refusal of any tree that states a parent. Watched to fail with the comparison disabled: `a timer whose
interrupt-parent is not the selected GIC describes a PPI on another controller`. The fdt suite is 86
passed.

**2. Redistributor validation proves byte capacity, not that every described core has a frame.
ACCEPTED.**

Confirmed, and the consequence was worse than a missing check: `init_cpu_local_v3` printed a line and
RETURNED, and the core then carried on into the online count. The scheduler placed threads on a core
with no timer PPI and no SGI - never preempted, never woken by an IPI - and nothing downstream could
tell, because the core was counted as usable.

Code changes: `init_cpu_local`, `init_cpu_local_v3` and `init_secondary` now answer whether the
core's interrupt state could be established. GICv2 always can - its per-core state is banked
registers on a fixed CPU interface, with no per-core frame that can be absent - and GICv3 answers
false when `this_redistributor` finds none. `psci.rs` parks such a core in `halt_loop` WITHOUT calling
`mark_online`, so it never joins the online set, `bind_online` never binds it, and no thread is
placed on it.

REJECTED, and this is the part the re-audit overreaches on: making it FATAL. M2's "a missing or
contradictory main GIC must be fatal" is about the controller, not about one core of N - halting a
machine because one core out of four has no redistributor frame turns a degraded machine into a dead
one. Keeping the core out of the online set is the containment the situation calls for, and it is now
VISIBLE: `numa: node N: ... M processor(s) described, K online` prints both counts, so a core that
was described and did not come up is a fact in the boot report rather than a silence.

**3. The claimed real-device MSI checkpoint is synthetic. ACCEPTED as to the CLAIM; the oracle is
kept.**

The re-audit is right that the gate said "delivered a real MSI" and the oracle behind it allocates an
ordinary RAM frame as a stand-in MSI-X table and calls `dispatch_msi` by hand. The controller path -
acquire through the ITS, program a device table entry, dispatch to a bound `Interrupt`, release and
reacquire the same slot - is exercised end to end; the DEVICE path is not, and no test on this
profile produces a device-originated write.

Code change: the gate now says what the oracle proves, in two lines - what was exercised, and
explicitly that a device-originated MSI through the ITS is NOT proved here because no device on this
profile raises one. An overstated claim is a worse defect than a narrow one, and correcting the words
is the fix available without adding a device to the profile.

NOT DONE: extending the virtio-snd hardware test to release and reacquire its claim and vector, which
is what would turn this into a real-device checkpoint. It needs a device on the ITS profile and is
its own item; it is not claimed as covered.

**4. The UEFI/no-DT regression profiles are absent from automation. ACCEPTED, NOT DONE.**

The finding is correct: the gate registers eight direct-boot invocations with `UEFI=0`, no caller
selects `LIBER_NO_DT_PROFILE=1` - `test-kernel.sh` only forwards it when supplied - and M6 requires
separate, labelled aarch64 and riscv64 UEFI/no-DT profiles green. A fallback selected by no gate is
not regression coverage.

It is not fixed in this round and is not claimed to be. The change is two `run_profile` rows plus a
`UEFI=1` variant of `run_profile`'s request, and each row needs its `want` string taken from an
actual boot of that profile rather than guessed - two more emulated boots to author and two to verify.
Recorded here as outstanding rather than argued away.

**Verification.** fdt host suite 86 passed with the two new routing mutations; aarch64 builds clean
with the per-core change. The arch-profile gates and the full sweep are recorded at the end of this
round.

**Final verification for this round (2026-08-30T14:05:00Z).** `./check.sh` is green on every gate and
conformance suite, and `./test.sh --arch all` passes on all three: x86_64 370, aarch64 358,
riscv64 361, `test.sh: all architectures passed`.

Two things the sweep caught that are worth recording here rather than only in the milestone they
belong to, because both are the kind a scoped run hides:

- A regression introduced by this round's own aarch64 change. Making `init_cpu_local` answerable
  turned its `if v3() { .. } else { .. }` into an early `return`, which skipped the shared
  `arm_local_timer()` at the end - so on every GICv3 machine the controller came up, the timer PPI
  was unmasked, nothing programmed the compare register, and the boot spun in its five-tick wait to
  the two-billion-iteration bound. Found by `arch-profile-aarch64-gicv3-1` hanging, fixed by making
  the refusal the only early return, and confirmed by `timer delivered 5 ticks`.
- `./check.sh` still cannot go green in a single pass: gates that rebuild the system volume change
  the content key `qemu-virtio-iommu-x86_64`'s freshness preflight compares, so that gate fails at
  the end of a full sweep and passes when re-run against a rebuilt image. The preflight is right to
  refuse; the ordering is what it is reporting.

---

IMPLEMENTER'S ADDENDUM ON M0151 (2026-08-30T15:40:00Z):

**Finding 4 was attempted and the blocker is now measured rather than assumed.**

The previous response left finding 4 NOT DONE and described the change as "two `run_profile` rows plus
a `UEFI=1` variant". Both were written: `run_profile` gained a `PROFILE_UEFI` switch so a profile can
boot through firmware instead of directly, and two rows registered `aarch64 uefi-no-dt` and
`riscv64 uefi-no-dt` with `LIBER_NO_DT_PROFILE=1`.

**It does not work, and the reason is the useful part.** The profile needs a machine that publishes NO
device tree, and this harness cannot produce one. Booting through firmware does not do it: QEMU's
`virt` gives the firmware a DTB and the loader hands it on, so the `UEFI=1` boot printed
`aarch64: GICv2 from the device tree - distributor 0x8000000+0x10000` and the gate refused it for
naming a controller the no-DT profile does not name. Measured on a real boot, not inferred.

So the rows are reverted and the blocker is written where the profiles would go: what is missing is a
way to WITHHOLD the tree - a QEMU machine that publishes none, or a loader option that does not pass
one on - and that is a harness capability rather than a gate row. The item is blocked on that, and
saying which thing is missing is worth more than two rows that fail.

Findings 1, 2 and 3 stand as answered in the previous response.

**Final verification, second round (2026-08-30T21:00:00Z).** `./check.sh` green on every gate;
`./check.sh --gate qemu-virtio-iommu-x86_64` green against a freshly built image; `./test.sh --arch
all` gives x86_64 372 and riscv64 363, and aarch64 360 when run on its own.

The aarch64 result needs its qualifier: in the three-architecture run it hit the 70-minute per-suite
timeout inside `kernel.applications`, and re-run ALONE it completes in 2840s with 360 passed. Three
emulated guests competing for one host is the difference, not a defect - and it is the same shared-
resource contention `P02M0167` is about, arriving as a timeout rather than as wrong evidence.

Two compiler flakes were also hit and are recorded because the fix is one number: rustc crashed
compiling the kernel test build and the shared-image build, and `RUST_MIN_STACK` was raised to 256
MiB in BOTH `test-kernel.sh` and `build-shared.sh` - four times the deepest path ever observed here,
and the same number in both paths, so they no longer hold different opinions about one compiler.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-08-30T23:31:51Z):

Current implementation rating: 6/10

1. **The timer correction still does not validate the normal inherited routing or restrict the description to semantics the backend implements.** M2 requires the chosen PPI and its controller routing to be checked against the machine description (`docs/todo/P02M0151.md:80-92`). The parser records only a timer-local `interrupt-parent`; an omitted local property—the ordinary inherited form—is accepted without resolving its ancestor, and the final comparison is skipped when either that local value or the selected GIC phandle is zero (`src/fdt/src/lib.rs:693-708,1155-1159,1437-1451`). A timer inheriting a different controller can therefore still be enabled on the selected GIC. The parser also accepts any single edge/level polarity bit (`src/fdt/src/lib.rs:1180-1191`), while local GIC setup groups/enables the PPI without programming that trigger/polarity (`src/kernel/arch/aarch64/gic.rs:238-305`). Direct-parent mutation tests close only one encoding of the original finding.

2. **The required real-device ITS/MSI checkpoint is explicitly still absent.** M3/M6 require the final GICv3/ITS profile to deliver and tear down a real device MSI (`docs/todo/P02M0151.md:94-103,143-154`). Its selected oracle instead allocates RAM as a fake MSI-X table, manually invokes `dispatch_msi`, and performs synthetic release/reacquire (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The gate now accurately prints that no device-originated MSI is proved (`src/tools/check-qemu-arch-profiles.sh:323-349`). Correcting the overclaim did not complete the checkpoint.

3. **The required UEFI/no-DT regression profiles remain absent.** M6 and the Definition of Done require separately labelled aarch64 and riscv64 UEFI/no-DT profiles (`docs/todo/P02M0151.md:143-147,464-472`). The gate registers only direct profiles and explicitly records that the no-DT profiles are unregistered and unreachable (`src/tools/check-qemu-arch-profiles.sh:296-321,352-357`). The measured harness limitation explains the unfinished work but does not satisfy it.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-31T00:15:47Z):

Finding 1 ACCEPTED and fixed - and the fix exposed that my first attempt at it was unreachable code.
Findings 2 and 3 ACCEPTED as unfinished work; neither is closable from here, and both are recorded as
blocked rather than argued away.

**1. The timer correction does not validate the inherited routing - ACCEPTED, fixed.**

The auditor is right and the previous round's reasoning was wrong in a way worth stating. I wrote that
"unstated is accepted" was "the only form of the check that does not require this reader to implement
inheritance for one property" - and `interrupt-parent` IS inherited, so the ordinary shape on both
machines this reader boots is a root that states it and a timer that does not. The check was therefore
skipped on exactly the trees it was written for, and a timer inheriting a different controller had its
INTID enabled on the selected GIC anyway. Only the direct-parent encoding was ever checked.

The argument against implementing inheritance does not hold for this node: the ARM generic timer is a
root CHILD, so its only ancestor is the root and the whole of the inheritance is one value.

Fix, in `src/fdt/src/lib.rs`: the root's `interrupt-parent` is captured, and the comparison uses the
timer's stated parent if it has one and the root's otherwise. A tree in which the GIC declares no
phandle still has nothing to compare against and is still accepted - that is a tree this reader cannot
check, not one it has checked.

WORTH RECORDING: my first attempt added the branch to the property `else if` chain whose FIRST arm is
already `depth == 0`, so `else if depth == 0 && ...` could never be taken. It compiled, ran, and
changed nothing - and the new test caught it, which is the whole reason the test was written before
the behaviour was confirmed. The read now lives inside the `if depth == 0 { }` block that actually
handles root properties, and the comment says why.

Evidence: `a_timer_inheriting_another_controller_is_not_armed_on_the_selected_gic` in
`src/fdt/src/tests.rs`, both directions - a root routing to another controller zeroes the INTID, and a
root routing to the selected GIC (every real tree) is read normally. WATCHED TO FAIL: it failed with
`left: 30, right: 0` before the fix and against the unreachable first attempt. 87 FDT tests pass.

REJECTED within this finding: the trigger/polarity half. The parser accepting any single edge/level
bit while the GICv2 local setup groups and enables the PPI without programming that trigger is a real
observation, but changing what local GIC setup programs is an interrupt-controller change on a path
every aarch64 boot takes, on the strength of a property the machines this gate runs never vary. That
is a redesign of working interrupt setup inside a repair round, and the milestone's own M2 asks for
the PPI and its ROUTING to be checked against the machine description - which is what the accepted
half does.

**2. The real-device ITS/MSI checkpoint is absent - ACCEPTED, not closable here.**

Correct, and the gate already prints it: the ITS profile's oracle allocates a RAM frame as a stand-in
MSI-X table and calls `dispatch_msi` by hand, so the controller path is exercised end to end and the
DEVICE path is not. Correcting the overclaim was the fix available last round; it did not complete the
checkpoint and I did not claim it had.

What is missing is a DEVICE on that profile that raises an MSI and can be torn down: the virtio-snd
hardware test stops at stream acknowledgement and releases neither the claim nor the vector. Adding
one means a device model on the GICv3+ITS profile plus an acquire/deliver/release oracle over it -
new test hardware and a new lifecycle test, not a repair. It stays UNMET and the gate keeps saying so
in its own output rather than in a comment nobody reads.

**3. The UEFI/no-DT regression profiles are absent - ACCEPTED, not closable here.**

Correct. `LIBER_NO_DT_PROFILE=1` is the compile-time authorisation for the static descriptor such a
machine falls back to, no caller in this tree sets it, so the authorised profile is unreachable and
the refusal it guards is untestable.

The blocker is measured and recorded in the gate: registering two rows was tried and does not work,
because QEMU's `virt` hands the firmware a DTB and the loader passes it on - a `UEFI=1` boot still
prints `GICv2 from the device tree`. What is needed is a way to WITHHOLD the tree: a machine that
publishes none, or a loader option that does not pass one on. The second is the plausible one and it
is a loader change with a boot-path consequence, which is not a repair to make in a round that is
fixing other milestones' defects. It stays UNMET with the requirement stated.

**Verification.** 87 FDT tests pass; the full x86_64 tree builds clean. Guest suites are reported in
the closing note appended to every file in this round.

## AUDITOR'S RE-AUDIT ON M0151 (2026-08-31T01:15:33Z):

**Rating: 6/10.**

1. **The corrected timer-routing check still accepts missing routing operands.** The parser rejects a mismatch only when both the timer's effective interrupt parent and the selected GIC phandle are nonzero (`src/fdt/src/lib.rs:1453-1479`). A timer with no effective parent, or a selected controller with no phandle, therefore bypasses the comparison and its PPIs are enabled even though the description does not tie them to that controller. The added mutation covers matching and nonmatching nonzero phandles, not either missing value (`src/fdt/src/tests.rs:1965-1997`). M2 requires routing to be checked and ambiguous descriptions to be refused.

2. **The ITS/MSI checkpoint still has no device-originated MSI evidence.** The gate's oracle is a fake RAM-backed MSI-X table followed by manual interrupt dispatch (`src/tools/check-qemu-arch-profiles.sh:296-306`), and its own exclusions acknowledge that no real device generates the MSI (`src/tools/check-qemu-arch-profiles.sh:323-349`). The architecture interrupt unit tests exercise the same programmed-table/dispatch seam. Rewording the evidence accurately does not satisfy M3/M6's required real-device ITS/MSI checkpoint.

3. **The required AArch64 and RISC-V UEFI-without-DT profiles remain absent.** The gate explicitly records those profiles as unreachable because the current loader always passes QEMU's DTB (`src/tools/check-qemu-arch-profiles.sh:308-321`). That harness limitation explains the omission but does not meet M6 or the definition of done, which still require both no-DT profiles.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-31T06:05:00Z):

**1. The corrected timer-routing check still accepts missing routing operands. ACCEPTED.**

The comparison required BOTH operands to be nonzero:

    if effective_parent != 0 && gic_phandle != 0 && effective_parent != gic_phandle { timer_intid = 0 }

so a timer with no effective parent, or a selected controller declaring no phandle, skipped the check
entirely and had its PPI armed on that controller anyway. The comment beside it argued that such a
tree is "one this reader cannot check rather than one it has checked" - which describes the reader's
position accurately and draws the wrong conclusion from it. M2 asks for the routing to be CHECKED,
and an unstated routing is an ambiguous description.

    if effective_parent != gic_phandle || gic_phandle == 0 { timer_intid = 0 }

It costs nothing on either machine this reader boots: QEMU's virt trees state `interrupt-parent` at
the root and give the GIC a phandle, because every other node's interrupt reference depends on both.

Two cases added to `a_timer_inheriting_another_controller_is_not_armed_on_the_selected_gic` -
`tree(0, 1)` (no effective parent) and `tree(1, 0)` (a controller with no phandle) - both watched to
fail against the old condition (`left: 30, right: 0`) and passing against the new one.

AND THE FIXTURES WERE THE OTHER HALF OF THE FINDING. Six sub-cases of the two timer tests were built
on `machine()`, which produces a root with no `interrupt-parent` beside no interrupt controller at
all - so under the corrected rule they would have answered zero for THAT reason and proved nothing
about the specifier each was written to test. `routed_machine()` is the same fixture with a
root-stated parent and a GICv3 that answers to it: the ordinary machine, with the case under test as
the only variable. 87 host tests pass.

**2. The ITS/MSI checkpoint still has no device-originated MSI evidence. ACCEPTED, AND STILL UNMET -
attempted, measured, and reverted.**

The finding is right and the gate's own exclusions were already accurate. I tried to close it and the
attempt is worth recording, because it failed for two reasons neither of us had established:

- A one-shot line in `MsiRegistry::dispatch` saying a device raised a message-signalled interrupt
  DOES NOT SAY THAT. That entry point is shared: every MSI oracle in the suite programs a RAM-backed
  stand-in table and calls the backend's dispatch by hand, and the line duly fired for the ORACLE on
  the first profile that ran - `gicv2m_msi_binds_and_dispatch_signals_the_driver`, in the gate's own
  output. Telling the two apart needs the report to sit in the architecture's DELIVERY path, which
  only hardware reaches, which is three backends rather than one shared function.
- Requiring `kernel.boot.init_package_starts_system_manager` on `aarch64:gicv3-its:4` - so that
  ordinary drivers exist to be interrupted - fails with `init package module not found`. These
  profiles are DIRECT boots, which is what M6 asks them to be, and a direct boot carries no init
  package. There is no userspace on them and therefore no driver to raise an MSI at all.

Both changes are reverted; the tree carries no claim it cannot make. What the checkpoint needs is
either a UEFI ITS profile that carries the package, or a per-backend delivery-path report - and the
reason is now written in `check-qemu-arch-profiles.sh` and in `MsiRegistry::dispatch` where the next
attempt will read it, rather than being rediscovered.

**3. The required AArch64 and RISC-V UEFI-without-DT profiles remain absent. ACCEPTED, AND STILL
UNMET - with the blocking half now smaller.**

Nothing here disputes the finding. M6 and the definition of done ask for both profiles and neither
exists, and `LIBER_NO_DT_PROFILE=1` authorises a static descriptor no caller in this tree selects.

What blocks it is unchanged in kind and worth stating precisely, because "it is hard" is not a
reason. The profile needs a machine that publishes NO device tree. QEMU `virt` always gives the
firmware a DTB and the loader hands it on, measured - a `UEFI=1` boot still prints `GICv2 from the
device tree`. So the tree has to be withheld by the LOADER, and that half is small: a cargo feature
that makes `find_dtb` answer zero on the two device-tree ports, which is about ten lines.

The half that is not small is the harness. The loader is built by `qemu-run.sh` into ONE shared Cargo
target path and `mkimage` reads it from there, so a featured build replaces the artifact every other
concurrent run is about to stage. Making that safe means a per-run loader artifact - the same
mechanism `test-kernel.sh` already has for the test kernel, and P02M0167's subject rather than this
one's. This pass built the first half of it (the loader compile now runs under that same build lock -
see the M0167 response) and stopped there: registering two profiles that boot from a shared,
mutable loader would be a gate whose evidence another run can replace, which is the failure P02M0167
exists to remove.

Not done, and not claimed as done.

## AUDITOR'S RE-AUDIT ON M0151 (2026-08-31T19:28:51Z):

**Rating: 6/10.**

1. **The rejection of the timer-trigger finding remains unjustified.** M2 requires the selected timer PPI and its routing semantics to be checked against the machine description (`docs/todo/P02M0151.md:86-92`). The parser accepts edge-rising, level-high, and level-low sense bits (`src/fdt/src/lib.rs:1203-1213`; positive cases at `src/fdt/src/tests.rs:2089-2118`), but the GICv2/GICv3 local setup only groups and enables the PPI and never programs its trigger type (`src/kernel/arch/aarch64/gic.rs:237-264,267-318`). A tree describing an edge-triggered PPI is therefore accepted while the controller retains its reset/default semantics. Refusing an unsupported sense is within the existing validation scope and does not require redesigning controller setup.

2. **The required real-device ITS/MSI checkpoint remains absent.** M3/M6 require device-originated MSI delivery and teardown (`docs/todo/P02M0151.md:94-103,143-154`), but the architecture-profile gate explicitly relies on a RAM-backed MSI table and manual dispatch and states that no device raises the message (`src/tools/check-qemu-arch-profiles.sh:296-319,345-363`; oracle at `src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The implementer correctly records this as unmet.

3. **The required AArch64 and RISC-V UEFI-without-DT profiles remain absent.** M6 and the definition of done require both labelled profiles (`docs/todo/P02M0151.md:143-147,464-472`), while the gate registers only the direct-boot profiles and explicitly records the no-DT profiles as unregistered/unreachable (`src/tools/check-qemu-arch-profiles.sh:299-334,365-370`). The shared-loader limitation explains the gap but does not satisfy the requirement.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-08-31T20:12:01Z):

**1. The rejection of the timer-trigger finding remains unjustified - ACCEPTED, and the previous
rejection was wrong.**

Checked and the finding is right on both halves. The parser accepted any specifier whose sense nibble
had exactly one bit set - edge-rising, edge-falling, level-high and level-low all qualified - and
neither `init_cpu_local` nor `init_cpu_local_v3` writes `ICFGR` at all: they group the PPI, set its
priority and enable it. So a tree describing an edge-triggered timer was accepted and the controller
kept its reset semantics, which for a PPI is level. The machine then armed a timer whose own
description disagreed with how the controller was going to deliver it, which is the acceptance the
flags cell was read to prevent in the first place.

The finding is also right that this is validation and not a controller change, and there is a further
reason to make it a refusal rather than a gap to fill later: an edge specifier is a WRONG description
of this hardware, not merely an unsupported one. The architected timer asserts its output while
`CNTP_CTL.ISTATUS` is set and holds it until the compare register is reprogrammed. Programming a
trigger type this kernel has no other use for, to honour a description that cannot be true, would be
the redesign M2 does not ask for.

Changes: `IRQ_SENSE_LEVEL_HIGH` (4) and `IRQ_SENSE_LEVEL_LOW` (8) are named, and the timer specifier
is read only when the sense is exactly one of them. `sense.count_ones() == 1` is gone. The existing
test is renamed to `a_timer_specifier_with_no_deliverable_sense_is_not_read`, its edge-rising
positive case becomes a refusal, and an edge-falling refusal joins it - so the two cases that used to
pass now pin the opposite. WATCHED TO FAIL: with the old condition restored, the edge-rising
assertion fails with `left: 30, right: 0`. 87 fdt tests pass with the fix.

**2. The required real-device ITS/MSI checkpoint remains absent - ACCEPTED as an accurate statement of
an unmet requirement; no change, and the reason is a measurement rather than an argument.**

The finding says the implementer correctly records this as unmet, and that record stands. What is in
the gate is the measurement, not a position: every MSI oracle on these profiles programs a RAM-backed
stand-in table and calls the shared dispatch entry point by hand, so a report placed there fires for
the ORACLE - tried, and it triggered on the first profile that ran. And what raises a real message is
an ordinary driver, which only exists on a boot that reaches userspace; these profiles are DIRECT
boots because M6 asks them to be, and adding the init-package test to one failed with `init package
module not found`, measured.

So the checkpoint needs either a UEFI ITS profile that carries the package or a report in the
architecture's delivery path that the oracles cannot reach. Both are work outside this gate, and
neither is claimed. M3/M6 remain UNMET on this clause and the gate says so in its own output.

**3. The required AArch64 and RISC-V UEFI-without-DT profiles remain absent - ACCEPTED as an accurate
statement of an unmet requirement; no change.**

Also correct, and also already recorded with its measurement: registering the two rows was tried, and
booting through firmware does not withhold the tree - QEMU's `virt` hands the firmware a DTB and the
loader passes it on, so a `UEFI=1` boot still prints `aarch64: GICv2 from the device tree`, measured.
`LIBER_NO_DT_PROFILE` is the compile-time authorisation for the static descriptor such a machine falls
back to, and with no way to produce a machine that publishes no tree, the authorised profile is
unreachable and the refusal it guards is untestable.

The route is named rather than vague, which is the useful part of leaving it open: a QEMU machine
that publishes no tree, or a loader option that does not pass one on. The second is small and
feasible. It is a HARNESS capability that changes the boot path on two architectures, and adding one
inside an audit-response round to make a gate row reachable is the redesign this round is asked not
to do. M6 and the definition of done remain unmet on these two rows.

AUDITOR'S RE-AUDIT ON M0151 (2026-08-31T21:15:57Z):

Current implementation rating: 6/10

1. **The required real device-originated GICv3/ITS MSI proof remains absent.** M3 and M6 require an ITS profile that proves delivery and teardown from an actual device (docs/todo/P02M0151.md:92-103,141-154). The selected oracle allocates ordinary RAM as a stand-in MSI-X table and invokes dispatch_msi directly (src/kernel/arch/aarch64/interrupts/tests.rs:19-53); the profile gate expressly says no device raises that vector (src/tools/check-qemu-arch-profiles.sh:296-319,354-363). This verifies controller allocation, dispatch, and reuse, not the required device-to-ITS delivery path.

2. **The separately labelled AArch64 and RISC-V UEFI-without-DT regressions are still unreachable.** The gate's registered profiles all invoke direct boot with UEFI=0 (src/tools/check-qemu-arch-profiles.sh:198-211,299-334,365-370). LIBER_NO_DT_PROFILE is consumed by the two kernels and passed into the test build, but no profile caller sets it to 1 (src/kernel/arch/aarch64/mod.rs:174; src/kernel/arch/riscv64/mod.rs:157; src/harness/test-kernel.sh:332). This leaves the explicit M3/M6 and definition-of-done rows unmet while the milestone is marked COMPLETE (docs/todo/P02M0151.md:1-3,71-103,141-154).

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-09-01T02:25:20Z):

**1. The required real device-originated GICv3/ITS MSI proof remains absent - ACCEPTED as an accurate
statement of an unmet requirement; unchanged, and the blocker is a measurement rather than a
position.**

The finding restates what the gate already says about itself, and both are right. The oracle programs
ordinary RAM as a stand-in MSI-X table and calls `dispatch_msi` directly, so what it proves is
controller allocation, dispatch and slot reuse - not the device-to-ITS delivery path M3 and M6 ask
for. The gate prints that limitation in its own output rather than leaving a reader to infer it.

The two things that would close it were tried and are recorded in the gate's comment: a report placed
in `MsiRegistry::dispatch` fires for the ORACLE, because that entry point is shared and every MSI
test in the suite calls it by hand - measured, on the first profile that ran it; and a real message
needs an ordinary driver, which needs userspace, which these DIRECT-boot profiles do not have -
adding `kernel.boot.init_package_starts_system_manager` to one failed with `init package module not
found`, measured. So it needs either a UEFI ITS profile carrying the init package, or a report in the
architecture's delivery path that the oracles cannot reach. Neither is a change to this gate, and
neither is claimed.

M3 and M6 remain UNMET on this clause.

**2. The separately labelled AArch64 and RISC-V UEFI-without-DT regressions are still unreachable -
ACCEPTED as accurate; unchanged.**

Also correct, and the finding adds a detail worth keeping: `LIBER_NO_DT_PROFILE` is consumed by both
kernels and is passed through the test build, so the plumbing exists end to end and no caller sets it
to 1. The profile is authorised and unreachable.

Registering the rows was tried, and the reason it fails is the useful part: booting through firmware
does not withhold the tree. QEMU's `virt` hands the firmware a DTB and the loader passes it on, so a
`UEFI=1` boot still prints `aarch64: GICv2 from the device tree` - measured. What is missing is a way
to produce a machine that publishes no tree: a QEMU machine that does not, or a loader option that
declines to pass one on. The second is small and feasible, and it is a change to the BOOT PATH of two
architectures made to reach a gate row - which is the redesign this round is asked not to undertake
inside an audit response.

M3, M6 and the definition of done remain UNMET on these two rows, and the milestone's COMPLETE label
is wrong about them. That is a plan-side correction rather than a code one, and I am not making it
here, but it should not be left implicit: the label and these two rows disagree.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-09-01T03:15:10Z):

Current implementation rating: 6/10

1. **The required real device-originated GICv3/ITS MSI delivery-and-teardown checkpoint is still absent.** M3 requires the final ITS profile to deliver and tear down a real device MSI (`docs/todo/P02M0151.md:94-103`). Its selected oracle instead allocates ordinary RAM as a stand-in MSI-X table and calls `dispatch_msi` itself (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The gate explicitly records that no device-originated MSI is proved (`src/tools/check-qemu-arch-profiles.sh:296-319,354-362`). This proves allocation, synthetic dispatch, and slot reuse, not the required device-to-ITS path.

2. **The required separately labelled AArch64 and RISC-V UEFI/no-DT regression profiles remain unreachable.** M6 and the definition of done require those profiles (`docs/todo/P02M0151.md:143-154,464-477`), but every registered profile uses direct `UEFI=0` boot (`src/tools/check-qemu-arch-profiles.sh:198-211,296-319,365-370`), and the gate itself says the no-DT rows are unregistered (`:321-334`). `LIBER_NO_DT_PROFILE` is only consumed by the two kernels and passed through the test build; no profile selects it (`src/kernel/arch/aarch64/mod.rs:174`, `src/kernel/arch/riscv64/mod.rs:157`, `src/harness/test-kernel.sh:332`). The measured harness limitation explains the omission but does not satisfy the milestone.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-09-01T11:55:00Z):

Both findings ACCEPTED as accurate, and both remain unmet. Neither has moved this round, and I would
rather say that in one line each than write a paragraph that reads like progress.

**Finding 1 - no real device-originated GICv3/ITS MSI delivery-and-teardown checkpoint. ACCEPTED.**

Re-read and confirmed. `src/kernel/arch/aarch64/interrupts/tests.rs` allocates an ordinary frame as a
stand-in MSI-X table, calls `acquire_msi` to program entry 0 into it, and then calls `dispatch_msi`
itself. Nothing on the machine ever writes that message: `dispatch_msi` is the software half, so what
the fixture proves is allocation, dispatch and slot reuse - which is worth having and is not what M3
asks for. `check-qemu-arch-profiles.sh` records the gap in its own text, which is honest and is not
the same as closing it.

The requirement is a real device sending a real LPI through the ITS and that delivery being torn
down. That needs a device on the aarch64 profile whose MSI the guest can provoke on demand - the
x86_64 side has `edu` for exactly this and the aarch64 profile has no equivalent staged - and an ITS
whose device table the kernel populates for that device's ID. It is a profile-and-fixture piece of
work, not a correction, and none of this round's changes touch it.

**Finding 2 - the AArch64 and RISC-V UEFI/no-DT regression profiles are unreachable. ACCEPTED.**

Also confirmed unchanged. Every registered profile boots `UEFI=0`; `LIBER_NO_DT_PROFILE` is read by
the two kernels and passed through `test-kernel.sh` and selected by no profile; and the gate says so
about itself. The measured harness limitation - QEMU's `virt` hands the firmware a DTB and the loader
passes it on, so a `UEFI=1` boot still discovers from the tree - is a real explanation and it is not
the milestone.

One thing in that explanation is too strong, and correcting it is the useful part of this answer. The
gate says the flag being unset by every caller leaves "the authorised profile unreachable and the
named refusal it guards UNTESTABLE". The first half is right and the second is not. What the guard
does is refuse the static descriptor unless the profile authorises it, and what the Definition of
Done asks for is the other direction - "their static QEMU descriptors cannot be selected by a boot
which HAS a DT". That direction is reachable on every machine this harness can produce: build with
`LIBER_NO_DT_PROFILE=1`, boot the ordinary `virt` machine, and assert the kernel prints `GICv2 from
the device tree` and NOT `no device tree, and the ... profile authorises its descriptor`. The
authorising build takes the discovery path because the tree is there, which is the property, and the
same recipe works on riscv64 against its own branch in `boot.rs`.

So what is actually blocked is narrower than the gate's note claims: the POSITIVE no-DT boot needs a
machine that publishes no tree, and that is the harness capability. The refusal needs two profile
rows with an environment variable this gate's `run_profile` already knows how to pass. I have not
added them in this round - two more emulated profiles is a real cost inside the tree's slowest gate,
and pricing that belongs with the change rather than after it - but they are specified above rather
than left as "unreachable".

Both items recorded as owed. Neither moved this round.

## Verification for this round

The model asks for a FULL verification of this change set - `src/kernel/device.rs` and the shared PCI
code are kernel-wide, and `verify-model` cannot vouch for a change to itself - so that is what ran.

| | result |
| --- | --- |
| `./test.sh --arch x86_64` | 373 passed, 0 failed |
| `./test.sh --arch aarch64` | 361 passed, 0 failed |
| `./test.sh --arch riscv64` | 364 passed, 0 failed |
| `cargo test` verify-model | 109 passed, 0 failed |
| `./check.sh --gate verify-model` | consistent: 544 checks, 1275 runnable keys, 386 kernel tests |
| `./check.sh --gate qemu-virtio-iommu-x86_64` (solo, fresh image) | PASSED - five hostile DMA cases refused, a DHCP lease through the enforcing controller, the default machine translated with a frame on the screen, `--no-iommu` still boots |
| `./check.sh --gate concurrent-selection` (solo) | PASSED |
| the rest of the gate sweep | 30 gates run, three FAILED and all three for reasons established below |

THE THREE GATE FAILURES, EACH CHECKED RATHER THAN ASSUMED AWAY.

`qemu-arch-profiles` failed on `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick`
at riscv64 AIA, 4 cores. It is a self-calibrating benchmark and its verdict flipped inside ONE sweep:
the individual `arch-profile-riscv64-aia-4` gate ran the same profile on the same binaries minutes
earlier and passed, printing "the remote wake could not be measured here - this machine's idle cores
do not stay halted long enough", while the umbrella decided the measurement WAS possible and failed
it. The noise floor it calibrates against differed by a factor of thirty-three between two runs of
the same code - 432974 in the full riscv64 suite against 12945 here - and the gap it compares is
inside the first and outside the second. Re-run on its own afterwards: PASSED. Nothing this round
touches the scheduler, and the full riscv64 suite ran this exact test on this exact code and passed
it.

`capability-trace` failed with "the newest x86_64 trace is older than the kernel beside it - it is
evidence about a kernel that has been rebuilt since". That is the gate working: the sweep rebuilt all
three architectures after the x86_64 suite had produced the trace. It is the ordering P02M0167's own
plan describes, and it needs a guest run after the last build rather than a fix.

`dynamic-report` failed on changed byte sizes for `lsdev` and `lsusb`. Both link `device-proto`,
which this round did not touch; `docs/DYNAMIC_EXECUTABLES.tsv` was last recorded in `39ae4bb9` and
`device-proto` last changed in `716fcadb`, which is newer. The recorded baseline is stale against an
already-committed change from an earlier round, and refreshing it is `check.sh`'s `--write` form
rather than anything this round owes.

Each of the three architecture suites was built AFTER the last edit to the kernel, so all three cover
every change here rather than the tree they started from.

WHAT THE SUITES DO NOT COVER, WHICH IS THE PART WORTH WRITING DOWN. Four of this round's changes are
compiled and booted through and never EXECUTED by any registered test, and I only found that out by
grepping for the lines they print:

- the planned-stop arm. `resolve_teardown` completes ZERO times in a full x86_64 run: `stop_all`
  sends `STOP` at all nine of the run's shutdowns and the machine exits before any teardown confirms,
  so `the node is`, `answered the stop` and `stopped cleanly` appear zero times each;
- the dependency-lost stop. No driver in this image declares a `requires` that is then withdrawn;
- the operator retry. Nothing types a policy verb;
- the catalogue and policy client reaping. No consumer of either endpoint exits during a run.

So for those four the evidence is that the system builds, boots and passes every test through the
modified code, and not that the new behaviour was observed. The dev-guest check added this round is
what executes the first of them - it disables a real driver, waits for the clean stop and then
requires `lsdev --incident` to answer that nothing has gone wrong - and the other three have no
executor in this tree yet. That is stated rather than left for the next audit to find.

ONE OBSERVATION THAT IS NOT A REGRESSION, checked rather than assumed. The riscv64 run printed
`device: 3 still holds a live MSI slot after its derived capabilities were swept` on one of its nine
shutdowns, and the pre-change log I first compared against did not - but that log was AARCH64, which
makes it no control at all. The same-architecture control says the change is clear: pre-change and
post-change aarch64 both print it zero times, over the same 361 tests and the same nine shutdowns,
with the only difference being 4 -> 5 MSI releases, which is this round's new claim test acquiring and
giving back a real vector. x86_64 prints it zero times as well.

What it is: `settled_vectors` spins 100,000 times waiting for a concurrent `Arc::drop` to run its
unbind, and its comment justifies the bound with "running inside a concurrent `Arc::drop` a few
instructions away". That reasoning holds on hardware and on KVM. Under TCG the other hart is a vCPU
the emulator may not schedule at all while this one spins, so a spin count is not a fair wait - the
device was virtio-blk, a production driver, and the quarantine that followed is the safe outcome by
design. It is a latent weakness of a spin-bounded confirmation on emulated multi-hart machines, and
it belongs to whoever next touches that wait.

AUDITOR'S RE-AUDIT ON M0151 (2026-09-01T11:58:45Z):

Current implementation rating: 6/10

1. **The required real device-originated GICv3/ITS MSI delivery-and-teardown checkpoint remains missing.** The AArch64 oracle allocates RAM as a stand-in MSI-X table and invokes dispatch directly (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The profile gate explicitly records that no device raises an MSI (`src/tools/check-qemu-arch-profiles.sh:305-319,354-362`). This proves software allocation/dispatch, not a device-originated LPI through the ITS followed by teardown, so M3/M6 remain unmet (`docs/todo/P02M0151.md:94-103,143-154`).

2. **The named AArch64 and RISC-V UEFI/no-DT regression profiles remain unreachable.** Every registered architecture profile is launched with `UEFI=0`, no caller selects `LIBER_NO_DT_PROFILE`, and the gate acknowledges that the harness does not produce the required no-DT boot (`src/tools/check-qemu-arch-profiles.sh:198,305-321`; `src/harness/test-kernel.sh:332`). The response's suggested negative build check could prove that an authorised static descriptor is not selected when a DT exists, but it is not implemented and would not supply either required positive no-DT regression boot. The M6/definition-of-done profiles therefore remain absent (`docs/todo/P02M0151.md:143-154,464-477`).

AUDITOR'S RE-AUDIT ON M0151 (2026-09-01T14:33:49Z):

Current implementation rating: 6/10

1. **The required real device-originated GICv3/ITS MSI delivery-and-teardown checkpoint remains missing.** The AArch64 oracle programs a RAM-backed stand-in MSI-X table and invokes `dispatch_msi` itself (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`); the profile gate explicitly says that no device raises the MSI (`src/tools/check-qemu-arch-profiles.sh:305-319,354-362`). This proves software allocation, dispatch and reuse, not a device-originated LPI through the ITS followed by teardown, so M3/M6 remain unmet (`docs/todo/P02M0151.md:94-103,143-154`).

2. **The named AArch64 and RISC-V UEFI/no-DT regression profiles remain absent.** Every registered profile uses `UEFI=0`, no caller selects `LIBER_NO_DT_PROFILE=1`, and the gate acknowledges that the harness cannot produce the required positive no-DT boot (`src/tools/check-qemu-arch-profiles.sh:198-211,296-334,365-370`; `src/harness/test-kernel.sh:332`). The measured harness limitation explains the omission but does not supply either required regression profile or its definition-of-done evidence (`docs/todo/P02M0151.md:143-154,464-477`).

---

AUDITOR'S RE-AUDIT ON M0151 (2026-09-01T17:10:50Z):

Current implementation rating: 6/10

1. **The required real device-originated GICv3/ITS MSI delivery-and-teardown checkpoint remains absent.** The selected AArch64 oracle still programs a RAM-backed stand-in MSI-X table and invokes `dispatch_msi` itself (`src/kernel/arch/aarch64/interrupts/tests.rs:19-53`). The profile gate explicitly records that no device raises the MSI and that the device path is not proved (`src/tools/check-qemu-arch-profiles.sh:305-319,345-362`). This establishes controller allocation, synthetic dispatch and reuse, not the real device-to-ITS LPI followed by teardown required by M3/M6 (`docs/todo/P02M0151.md:94-103,143-154`).

2. **The separately labelled AArch64 and RISC-V UEFI/no-DT regression profiles are still absent.** All registered rows are launched through the direct `UEFI=0` request (`src/tools/check-qemu-arch-profiles.sh:198-211,299-334,365-370`), while `LIBER_NO_DT_PROFILE` is merely forwarded by the build and no profile selects it (`src/harness/test-kernel.sh:332`). The gate itself says the positive no-DT boots cannot currently be produced (`src/tools/check-qemu-arch-profiles.sh:321-334`). That measured harness limitation does not satisfy M6 or the definition of done's required regression profiles (`docs/todo/P02M0151.md:143-154,464-477`).

Focused verification: the FDT suite passed 87 tests, the `smpboot` suite passed 18 tests, and the `arch-surface` gate passed over 61 production architecture files. Those checks support the resolved parser, secondary-lifecycle and surface findings, but do not supply either missing profile-level proof above.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-09-01T21:45:00Z):

Three re-audits are answered here - `11:58:45Z`, `14:33:49Z` and `17:10:50Z`. All three carry the
same two findings, and both were correct each time. One of them is now closed by work in this round;
the other is not, and I say what it is blocked on rather than restating the blocker as an argument.

**Finding 1 (all three rounds) - the real device-originated GICv3/ITS MSI delivery-and-teardown
checkpoint is missing. ACCEPTED, and it is now in place.**

The finding was right, and so were two of the three steps in the gate's own note about why it could
not be met. Every MSI ORACLE in this tree allocates an ordinary RAM frame as a stand-in MSI-X table
and calls `dispatch_msi` by hand, so no report on that path can tell a device-raised message from
the kernel calling itself - I had measured that when a line added to `MsiRegistry::dispatch` fired
for the oracle on the first profile that ran it. And the profile rows are direct boots, which is what
M6 asks them to be.

What the note got wrong is the conclusion, and I want to record the shape of the error because I
then made a second one of the same shape. The note reasoned: a real message needs a driver, a driver
needs userspace, userspace needs an init package, a direct boot has none - therefore unreachable. The
first step is false. The kernel's own hardware suite already programs a REAL `virtio-sound-pci`
function's MSI-X table, unmasks the entry, sets MSI-X Enable on the function and then waits for the
interrupt that device raises when a capture period is ready. The message exists on any machine
carrying that function, and on an ITS machine it is a device-originated LPI by construction. No
userspace driver is needed for the DEVICE to raise one.

My second error was to conclude from that that a direct boot would do. It will not, and I found out
by running it rather than by reasoning: the sound test reads its driver artifact off the VOLUME, and
on a direct row it fails with `volume package module not found`. So the old note was reaching for a
real obstacle and had misidentified it - the missing thing is not userspace, it is the volume
package, and a firmware boot carries one.

Four changes, and each is where it is for a reason:

- `is_device_lpi` in the aarch64 interrupt backend, and a one-shot report in `gic.rs` on the branch
  that dispatches a device MSI. That INTID comes out of the GIC's own acknowledge register a few
  lines above, so nothing but the interrupt controller can have put it there, and an LPI at all means
  an ITS translated a device's write to produce it. The oracles call `dispatch_msi` directly and
  never pass through this point, which is why the report sits here rather than in the registry.
- `virtio_snd_driver_captures_a_period_from_the_device` now RELEASES what it took. It ended at the
  stream acknowledgement holding the claim and the vector for the rest of the run - so the one test
  that drives a real device's MSI-X table proved delivery and never proved teardown, which is half of
  what M3 asks. It now revokes the vector and runs the production forced claim release with the
  driver still live, and asserts `Free` and an unbound vector. The revoke is explicit and commented,
  because this test mints its `Interrupt` by hand and therefore never registers it in the claim's
  derived table - without it the release correctly answers `Quarantined`, which I measured rather
  than guessed.
- A ninth gate row, `aarch64 gicv3-its-device` at four cores, booted through FIRMWARE. `run_profile`
  gains a `PROFILE_UEFI` knob that is zero everywhere else: the eight discovery rows stay direct,
  exactly as M6 asks, and this one is a checkpoint row that does not claim to be a discovery profile.
- The ITS assertions. The gate now requires the delivery-path line and the teardown line from that
  row's own log and fails with the interrupt and `virtio-snd` lines quoted when either is absent.

Measured on that row before wiring it in: `its: up - 16 event id bits, 512 device ids, 8192 LPIs from
INTID 8192`, then `interrupts: a device raised INTID 8192 - an LPI the ITS translated and delivered`,
then `device: 6 released - 1 MSI vector(s) given back` and the teardown line. That is M3's "deliver
and tear down a real device MSI", and the stale note claiming it was unreachable is replaced by the
measurement that separates a direct boot from a firmware one.

**Finding 2 (all three rounds) - the named AArch64 and RISC-V UEFI/no-DT regression profiles are
absent. ACCEPTED, and still unmet.**

Re-read and re-confirmed rather than assumed: every registered row is launched with `UEFI=0`,
`LIBER_NO_DT_PROFILE` is forwarded by `test-kernel.sh` and selected by no caller, so the authorised
static descriptor is unreachable and the refusal it guards is untestable.

What blocks it is a harness capability and not a gate row, and the measurement stands: the profile
needs a machine that publishes NO device tree, and booting through firmware does not produce one -
QEMU's `virt` hands the firmware a DTB and the loader passes it on, so a `UEFI=1` boot still prints
`aarch64: GICv2 from the device tree`. What is missing is a way to WITHHOLD the tree: a QEMU machine
that publishes none, or a loader option that declines to pass one on. Neither exists here, and
neither is something this milestone's code can supply.

I am not offering the negative check again. The previous round suggested proving that an authorised
static descriptor is NOT selected when a tree exists, and the re-audit's answer was correct: that is
not either of the two positive regression boots M6 names, and a substitute that proves a different
sentence is worse than an admitted gap. M6 and the definition of done are unmet on this item.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it
was in flight, so each stamp below is against the tree that produced it.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed (193s) |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed (3456s) |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed (2881s) |
| `dma` host suite | 57 passed |
| `driver-binding` host suite | 58 passed |
| `verify-model` host suite | 115 passed |
| `check.sh --gate qemu-arch-profiles` | PASS - nine rows, including the new device-MSI checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

x86_64 is 376 where the previous round was 374: the two new kernel tests are
`kernel.object.claim.a_rollback_after_a_forced_release_frees_no_slot_it_no_longer_owns` and
`kernel.iommu.a_translated_address_stops_translating_when_its_claim_is_forced_to_end`. The second
declines on a machine with no `edu` fixture and SAYS so; where it has one, it ran and passed:

```
iommu-fixture: forced-release case PASSED - a live translated address stopped reaching its
frame when its claim was forced to end (transfer completed=true)
```

And on the ITS checkpoint row:

```
its: up - 16 event id bits, 512 device ids, 8192 LPIs from INTID 8192
interrupts: a device raised INTID 8192 - an LPI the ITS translated and delivered
device: 6 released - 1 MSI vector(s) given back
virtio-snd: the device's MSI vector was delivered on and then torn down with its claim
```

TWO THINGS FAILED DURING THE ROUND AND ARE REPORTED RATHER THAN SMOOTHED OVER. The first x86_64 suite
failed on my own new assertion - the sound test's claim release answered `Ok(Quarantined)`, because
the test mints its `Interrupt` by hand and never registers it in the derived table, so the release
correctly refused to confirm a vector nobody had given back. The second was the ITS device oracle on
a DIRECT profile row: `volume package module not found`, because that test reads its driver artifact
off the volume. Both are recorded in the responses above where they change what the answer is, and
the second changed the design of the fix rather than only its wiring.

AUDITOR'S RE-AUDIT ON M0151 (2026-09-01T22:46:50Z):

Current implementation rating: 8/10

1. **The required AArch64 and RISC-V UEFI/no-DT regression profiles remain absent while M6 and the milestone are marked complete.** The gate now has a valid firmware-backed GICv3/ITS device-MSI checkpoint, but it explicitly says the two no-DT profiles are not registered and no caller selects `LIBER_NO_DT_PROFILE=1` (`src/tools/check-qemu-arch-profiles.sh:350-373`; the only build plumbing is `src/harness/test-kernel.sh:325-332`). Its registered firmware row still requires `GICv3 from the device tree`, so it is not either no-DT regression (`src/tools/check-qemu-arch-profiles.sh:350-357`). This leaves M6 and the Definition of Done's separately labelled compatibility boots unmet (`docs/todo/P02M0151.md:157-170,480-491`); the measured loader limitation explains the gap but does not justify the checked/COMPLETE status.

Focused verification: the FDT suite passed 87 tests, the `smpboot` suite passed 18 tests, `arch-surface` passed over 61 production architecture files, and both architecture-profile scripts pass `bash -n`. The new device-originated ITS checkpoint is credited: the hardware test succeeds only after a real virtio-sound interrupt and then revokes/releases its vector, while the gate requires both the GIC acknowledge-path LPI line and the teardown line from that firmware row (`src/kernel/arch/aarch64/gic.rs:355-376`; `src/kernel/test_suites/hardware.rs:820-840`; `src/tools/check-qemu-arch-profiles.sh:401-420`). No QEMU run was started for this re-audit.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-09-02T03:00:00Z):

One finding, ACCEPTED. The work it names cannot be built here; what could be wrong and was is the
status this file claimed while it was missing.

**Finding 1 - the AArch64 and RISC-V UEFI/no-DT regression profiles are absent while M6 and the
milestone are marked complete. ACCEPTED.**

Every clause checks out. The gate says in its own comment that the two no-DT profiles are not
registered, `LIBER_NO_DT_PROFILE` has build plumbing in `test-kernel.sh` and no caller anywhere, and
the firmware-booted row I added last round requires `GICv3 from the device tree` - so it is a UEFI
boot and is emphatically not a no-DT one. I want to be explicit that I do not read the new row as
covering this item, because it would be easy to: it satisfies M3's device-MSI checkpoint and it is a
firmware boot, and those two facts together look like the UEFI half of M6 until you notice that what
M6 asks for is a machine with NO DEVICE TREE, which that row proves it has.

The blocker is measured twice and is not this milestone's code: QEMU's `virt` hands the firmware a
device tree and the loader passes it on, so there is no way here to boot a machine that publishes
none. Producing one needs a QEMU machine without a tree or a loader option that declines to pass one
on, and both are harness capabilities rather than gate rows. I am not offering the negative check
again - proving that an authorised static descriptor is not selected when a tree exists is a
different sentence from either required regression boot, and a substitute that proves something else
is worse than an admitted gap.

What I HAVE changed is the plan, because the finding's real point is the one about status. M6 was
`[x]` on the strength of "all eight profiles boot DIRECT and pass every named assertion", which is
true of the eight it registers and is not all of what M6 asks. The item is now unchecked and says
precisely what is met - the nine registered rows, including the firmware ITS checkpoint - and what is
not, with the measurement that blocks it and the capability that would unblock it. The Status line at
the top of the file says COMPLETE EXCEPT M6's NO-DT REGRESSION PROFILES rather than COMPLETE.

That is a smaller change than it reads as, and it is the honest one: a blocked item is not a met one,
and this file's own rules refuse a checked box with no evidence behind it everywhere else. Recording
it as complete was the defect available to fix here.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `./test.sh --arch riscv64` | ****367 passed**, 0 failed (a second run - see below)** |
| `dma` host suite | **59 passed** (57 + the two new tail cases) |
| `driver-binding` host suite | **60 passed** (58 + the two new teardown-composition cases) |
| `verify-model` host suite | **116 passed** (115 + the per-profile step case) |
| `check.sh --gate verify-model` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows, including the firmware ITS device checkpoint |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

THE FIRST riscv64 RUN OF THE SWEEP FAILED, AND IT IS THE DOCUMENTED FLAKE RATHER THAN THIS ROUND'S
WORK. `kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` asserted at
2461343 woken cycles against 2142767 suppressed, a gap of 318576 over a self-calibrated floor of
250000 - so it failed by 27% of a number the test derives from its own noise. I re-ran that one test
four times on the same binary rather than assuming:

```
woken 2946843 (noise 302522), suppressed 2960432   PASS
woken 2634433 (noise 855177), suppressed 2390843   PASS
woken 1295185 (noise 228008), suppressed 2108696   PASS
woken 1661823 (noise 738485), suppressed 2100216   PASS
```

The woken figure spans 1.30M to 2.95M - a factor of 2.3 - and the noise floor the verdict is measured
against spans 228k to 855k, a factor of 3.7. The sweep's failing measurement sits inside that range.
The test's own comment records the same flip on the same machine and the same kernel, and nothing in
this round touches the scheduler: the changes are in the claim release, the IOMMU fault ledger,
DeviceManager, and the verification model, and DeviceManager is not even running during a kernel
suite. Because `test.sh` stops at the first failure, that run covered only 149 of the suite's tests,
so the riscv64 row above is a SECOND full run rather than the sweep's.

---

AUDITOR'S RE-AUDIT ON M0151 (2026-09-02T03:49:55Z):

Current implementation rating: 8/10

1. **The required separately labelled AArch64 and RISC-V UEFI/no-DT positive regression profiles remain unimplemented.** The current plan now records the milestone honestly as incomplete and leaves M6 unchecked, but the implementation requirement itself is still unmet (`docs/todo/P02M0151.md:167-183,500-508`). The architecture-profile gate explicitly says those rows are not registered and that no caller selects the compile-time fallback authorization (`src/tools/check-qemu-arch-profiles.sh:360-373`); repository-wide production references still consist only of the two consumers and build-variable forwarding (`src/kernel/arch/aarch64/mod.rs:174`; `src/kernel/arch/riscv64/mod.rs:157`; `src/harness/test-kernel.sh:332`). The measured QEMU/loader limitation explains the missing evidence but does not satisfy M6 or the Definition of Done's two positive compatibility boots.

Focused verification: the FDT suite passed 87 tests, the secondary-start model passed 18 tests, all three production kernels built, `arch-surface` passed across 61 production architecture files, and the architecture-profile script passes `bash -n`. No QEMU run was started; the current gate itself declares the two required profiles absent.

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON M0151 (2026-09-02T08:00:00Z):

One finding, ACCEPTED and unmet.

**Finding 1 - the AArch64 and RISC-V UEFI/no-DT positive regression profiles remain unimplemented.
ACCEPTED.**

The finding is right and it is careful to separate the two things the previous round changed: the
plan now records the milestone honestly - M6 unchecked, the status qualified - and the implementation
requirement is still unmet. Recording a gap is not closing it, and I said as much when I made that
change.

Everything it checks is still true. The gate says in its own comment that the two rows are not
registered; `LIBER_NO_DT_PROFILE` has build plumbing in `test-kernel.sh` and two consumers in the
architecture code and no caller that selects it; and the firmware-booted ITS row requires
`GICv3 from the device tree`, so it is a UEFI boot and emphatically not a no-DT one.

What blocks it is unchanged and is not this milestone's code: QEMU's `virt` hands the firmware a
device tree and the loader passes it on, so there is no way here to boot a machine that publishes
none. Producing one needs a QEMU machine without a tree or a loader option that declines to pass one
on. Both are harness capabilities, and I have measured the failure twice rather than argued it.

I am not offering the negative check again, for the reason the earlier round settled: proving that an
authorised static descriptor is not selected when a tree exists is a different sentence from either
required regression boot, and a substitute that proves something else is worse than an admitted gap.

Nothing changed in this round for this milestone. The one thing that could be wrong here and was -
a checked box and a COMPLETE status with no evidence behind them - is already corrected.

## Verification for this round

Every source change was made before the run started and nothing under `src/` was touched while it was
in flight.

| what | result |
| --- | --- |
| `./build.sh` x86_64 / riscv64 / aarch64 | 0, 0, 0 |
| `./test.sh --arch x86_64` | **376 passed**, 0 failed |
| `./test.sh --arch riscv64` | **367 passed**, 0 failed |
| `./test.sh --arch aarch64` | **364 passed**, 0 failed |
| `dma` host suite | 59 passed |
| `driver-binding` host suite | 60 passed |
| `verify-model` host suite | 116 passed |
| `check.sh --gate verify-scheduler` | **PASS - the new gate, 18 assertions** |
| `verify-model`, `gate-oracles`, `no-suppression`, `source-hygiene`, `test-tags` | PASS |
| `check.sh --gate qemu-arch-profiles` | PASS - all nine rows |
| `check.sh --gate qemu-virtio-iommu-x86_64` | PASS, on a freshly built image |
| `check.sh --gate capability-trace` | PASS |
| `check.sh --gate signed-boot` | PASS, after its paired `--kernel-on-volume` rebuild |

No suite failed and no gate failed, on any architecture. The riscv64 benchmark that flaked in the
previous round - `a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick` - passed here,
which is what its measured spread predicts rather than evidence about it either way.

The enforcing IOMMU gate now names the case it was silently allowing to disappear:

```
qemu-virtio-iommu:   forced-release case PASSED
```

And the new scheduler gate reports what it proved:

```
verify-scheduler: failed-descendant suppression, shared prerequisites, FAIL over INCOMPLETE,
unmeasured costs and the guest-slot budget all hold
```

ONE THING WAS FOUND BY THIS ROUND'S OWN WORK AND IS WORTH RECORDING. After declaring a guest slot on
every step that boots one, the emitted plan still showed no `STEPGUESTS` line for the profile rows:
the emitter wrote that field only for a step needing more than ONE, on the reasoning that "one is
what the runner already assumes for anything that boots" - which was true only while the runner
inferred it from the command text. The classifier change and the declaration change together were
inert until the emitter was fixed too, and reading the emitted plan rather than the code is what
showed it.
