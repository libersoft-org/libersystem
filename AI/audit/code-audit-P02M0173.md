IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0173 (2026-09-09T21:05:00Z):

Scope: the seven items of `docs/todo/P02M0173.md` - the virtio-IOMMU enforcing profiles on the
AArch64 and RISC-V QEMU machines. This first entry records what the tree has, verified by reading
it, before any design; the implementation record and the verification follow as the work proceeds.

## What the tree has, verified before designing

- The harness attaches a controller on x86_64 only. `qemu_virtio_opts` (`src/harness/qemu-run.sh`)
  adds `iommu_platform=on` to an endpoint whenever `IOMMU=1`, and the x86_64 arm decides `IOMMU`
  (default 1, `./run.sh --no-iommu` clears it, `TEST=1` never sets it) and adds
  `-device virtio-iommu-pci,boot-bypass=on` with `-machine q35,default-bus-bypass-iommu=off`. The
  aarch64 and riscv64 arms pass `disable-legacy=on` on some endpoints and nothing on others (the
  riscv64 direct-boot arm attaches its block devices with an empty option string), never
  `iommu_platform=on`, and never a controller. `dma_mode_value` prints `no-iommu` for every non-x86
  row with the comment "Flipping it is the milestone that attaches the topology, not this file".
- The kernel's IOMMU backend (`src/kernel/iommu/mod.rs`, 81 kB) has no architecture cfg: the
  controller is found by virtio device type 23 in the arch-neutral device table, its registers are
  mapped through `map_registers` (direct map or an uncached mapping), the requester id is derived
  from bus/dev/func, and the MSI doorbell comes from `crate::arch::pci::msi_doorbell()` - which the
  aarch64 port answers from the GICv2m frame (`MSI_SETSPI_NS`) or the ITS `GITS_TRANSLATER`
  (`arch::interrupts::msi_doorbell`, `USING_ITS`) and the riscv64 port from the IMSIC S-mode window
  (`arch::imsic::msi_window`). PCI discovery is per port (`arch/aarch64/pci.rs`,
  `arch/riscv64/pci.rs`: ECAM base from the device tree, 16 buses, BAR assignment from an MMIO
  window) over the common capability walk.
- The bypass transition (`bring_up`): bus mastering is enabled on the controller, the device is
  reset and negotiated, then `quiesce_other_endpoints` clears bus mastering on EVERY other PCI
  function in the device table (virtio and xHCI functions - the only ones the table admits), bypass
  is written off and read back. No device-class reset precedes the BME clear: a virtio endpoint the
  firmware used is not reset to status zero first, there is no NVMe controller code in the kernel at
  all (`grep -il nvme src/kernel` finds nothing), and the only xHCI reset sequence in the tree is the
  userspace driver's. This is the gap M3 names.
- The aarch64 boot path (`arch/aarch64/boot.rs` ~line 770) runs `virtio_blk::BlkDevice::init` on
  the first virtio-blk function and reads sector 0 as a bring-up DIAGNOSTIC (`aarch64: virtio-blk
  sector 0 read - first16=...`), before the device manager and before any IOMMU. Nothing in the tree
  asserts that line (no gate, no test greps it). It enables BME and programs physical queue
  addresses - the untranslated access M3 requires off the enforcing path.
- The hostile fixture (`src/kernel/iommu/edu.rs`, `iommu/tests.rs`) finds QEMU's `edu` functions
  through `arch::pci::function_bar`, which both ports implement under `cfg(test)`; the cases print
  `iommu-fixture: case N PASSED` and `iommu-fixture: absent` when no edu device is on the machine.
  The x86_64 gate boots the TEST kernel with `IOMMU=1 DMA_FIXTURE=1 QEMU_EXTRA="-device edu -device
  edu"` for the hostile phase and the shipping image with an ordinary NIC (and, since the virtio-blk
  maintenance item, a block endpoint) for the traffic phase.
- The named port profiles the harness boots: aarch64 `virt,gic-version=2` (GICv2 with the v2m
  frame), `gic-version=3,its=off`, `gic-version=3,its=on`, and AAVMF/UEFI; riscv64
  `virt,aia=aplic-imsic` direct and U-Boot/UEFI. `check-qemu-arch-profiles.sh --only
  <arch>:<label>:<cores>` runs one row, and each row is a catalog profile key.
- P02M0172 ships every non-x86 row - test, development, public, gate - at the produced `no-iommu`
  value, and the DMA-mode ports gate (`check-dma-mode-ports.sh`) asserts exactly that value on both
  entry paths of both ports, with virtio_net refused by name and value. Flipping the rows is this
  milestone's M7 and that gate's assertions move with it.

## Decisions (before implementation)

1. **One harness path decides the topology on all three targets** (M1). `dma_mode_value` and the
   controller become target-neutral: `IOMMU` defaults to 1 on the ports as on x86_64 (never under
   `TEST=1`, which stays the untranslated suite row on every target), `./run.sh --no-iommu` is the
   explicit escape on the ports too, and the same variable drives `-device virtio-iommu-pci,
   boot-bypass=on` in front of the endpoints, `iommu_platform=on` on every protected endpoint
   through `qemu_virtio_opts`, and the upstream bridge pin: `default-bus-bypass-iommu=off` on the
   aarch64 `virt` machine, `-global gpex-pcihost.bypass-iommu=off` on the riscv64 one (the machine
   property does not exist there). Every riscv64 virtio function becomes modern-only
   (`disable-legacy=on`) in the same change, because QEMU cannot offer `VIRTIO_F_ACCESS_PLATFORM`
   through a transitional function. The runner PROBES the property before it relies on it
   (`qemu-system-riscv64 -device gpex-pcihost,help`, `qemu-system-aarch64 -machine virt,help`) and
   refuses an enforcing boot on a QEMU that lacks it; P02M0170's identity block already records the
   QEMU executables by hash and version.
2. **The backend is not copied** (M2). `src/kernel/iommu` has no architecture arm; what the ports
   need from it they already have (device table, requester ids, the doorbell through
   `arch::pci::msi_doorbell`). The proof of portability is the profile rows booting: the hostile
   phases on the test kernel with `edu` through the same `function_bar` seam, and the ordinary
   phases showing endpoint interrupts arriving through the v2m frame, the ITS and the IMSIC (a DHCP
   lease and a volume read are interrupt-driven on those drivers). Anything x86_64-shaped found on
   the way is removed rather than duplicated; none was found in the reading above.
3. **Quiescence is written per device class, before BME is cleared** (M3), in
   `quiesce_other_endpoints`: a virtio function is reset to status zero and the zero read back; an
   xHCI function is halted (R/S clear, HCHalted read back) and reset (HCRST, then HCRST and CNR read
   back clear); an NVMe function (class 01:08:02 - the riscv64 UEFI ESP) has `CC.EN` cleared and
   `CSTS.RDY` read back as zero. Each wait is bounded; a device that does not confirm makes the
   transition FAIL, which is the existing "present but NOT enforcing" outcome - every
   `iommu-required` binding refused, nothing degraded. The aarch64 bring-up virtio-blk diagnostic is
   REMOVED (the module and its call): it is a sector-0 read nothing asserts, and executing it on
   the enforcing path is the untranslated pre-transition DMA M3 forbids. The transition gate
   asserts that no bus master was enabled before bypass went off except the controller.
4. **Drivers are the same binaries** (M4): the userspace drivers consume `DmaAddress` on every
   target already (the x86_64 gate proves the enforcing binding path); the ports' ordinary phases
   assert the kernel's own audit line - every bus-mastering device translated, no untranslated
   admission, no fault - and the host fake for the failure states is the existing
   `src/dma` suite, run as it is.
5. **Every phase is a catalog row of its own** (M5). Ten gate names in `check.sh` -
   `iommu-<arch>-<profile>-<phase>` - each a Profile row with its own key, command, log, envelope
   and measured cost, release-required by class; the five composite rows are umbrellas that run
   their two phases and are never selected or required. A missing phase is therefore a missing
   REQUIRED key, which the dossier refuses by name; nothing infers a row from an exit code. This is
   the composite shape P02M0170 can accept without a new class: the composite is the umbrella and
   the obligations are its phases.
6. **The flip is the last change and it moves the DMA-mode gate with it** (M7).
   `dma_mode_value` prints `enforcing-required` on the ports when the machine has a controller;
   `check-dma-mode-ports.sh` then asserts the enforcing value on the ordinary rows, keeps the
   absent-record and independent-producer refusals, and gains the explicit `--no-iommu` degraded row
   as the standing untranslated regression; the development row on the ports follows the same
   `dma_mode_value`. The admission fixture is the ordinary phase's `virtio_net` online line under
   `enforcing-required` - which is also what restores the network on the ports.

## Implementation record (2026-09-09T22:50:00Z, first pass - written before the first port boot)

- **Harness (M1, M7)** - `src/harness/qemu-run.sh`: `port_iommu_decide` (the x86_64 rule: `IOMMU`
  when given, else no controller under `TEST=1`, else one) and `PORT_IOMMU` for the record
  producers; `port_iommu_probe` refuses an enforcing boot on a QEMU whose `virt` machine has no
  `default-bus-bypass-iommu` or whose `gpex-pcihost` has no `bypass-iommu` (both captured and
  matched, never piped into `grep -q`); the aarch64 arm pins the root bus, the riscv64 arm pins the
  host bridge with `-global gpex-pcihost.bypass-iommu=off`; both put `virtio-iommu-pci,
  boot-bypass=on` first in `qemu_args` and attach every virtio endpoint with one decided option
  string - `disable-legacy=on,iommu_platform=on` enforcing, `disable-legacy=on` otherwise, so every
  riscv64 virtio function is modern-only from now on; `DMA_FIXTURE=1` omits the media disks, the USB
  controller and the test-mode display, channel and sound devices on both ports, as on x86_64;
  `dma_mode_value` prints the machine's mode on every target. `run.sh --no-iommu` applies to the
  ports (it exported `IOMMU=0` for every target already; its help says so now).
- **Kernel (M3)** - `src/kernel/iommu/mod.rs`: `quiesce_other_endpoints` returns `Result` and is
  reached with `?` from `bring_up`; per class before BME is cleared: `quiesce_virtio` (status zero,
  read back), `quiesce_xhci` (run/stop clear and HCHalted read back, HCRST and both HCRST and CNR
  read back clear), `quiesce_nvme` (`CC.EN` clear, `CSTS.RDY` read back zero) for class 01:08:02
  through `arch::pci::function_bar`, which - with `common::probe_function` - left `cfg(test)`;
  every wait is bounded (`QUIESCE_SPINS`); a refusal is `Fault::Unconfirmed` and the existing
  "present but NOT enforcing" outcome. The controller prints that it masters the bus before the
  transition, which is what the gates order the log against. `src/kernel/arch/aarch64/virtio_blk.rs`
  and its bring-up probe in `boot.rs` are removed (thirty unsafe sites the inventory named as the
  untranslated pre-transition access).
- **Gates (M5, M6)** - `src/tools/check-qemu-iommu-ports.sh` (`--only <arch>:<profile>[:<phase>]`):
  the five rows, each phase a `check.sh` gate (`iommu-<arch>-<profile>-<phase>`), the rows and
  `iommu-ports` umbrellas; `assert_transition` (the controller announces first, the row's classes
  quiesced, bypass read back off, no driver online before it); the hostile and direct-gicv3-its
  transition phases on the test kernel with the enforcing fixture and the six x86_64 cases; the
  ordinary phases through `run.sh` with the `iommu-port-ordinary` verdict (lease, volume read,
  every driver online, nothing degraded, no fault); the UEFI transition phases with
  `iommu-port-transition`; a census per phase kept with the logs. Catalog: ten Profile rows (release-
  required, 210 keys frozen), six umbrellas, all in `GATES_THAT_BOOT_A_GUEST`; `check.sh` entries.
  `check-dma-mode-ports.sh` asserts the enforcing value on both entry paths and gained the two
  `--no-iommu` degraded rows per port; `guest-verdict.py` gained `iommu-port-ordinary`,
  `iommu-port-transition` and `dma-port-admits`.
- **Docs** - `docs/TESTING.md` (the ports' profiles and the flipped rows), `docs/ARCHITECTURES.md`
  (the DMA isolation row and the degraded paragraph), `run.sh --help`.

Not yet: no port has booted with the topology at the time of this entry - the first boots follow
the port builds, and every claim above is claimed only once the phases pass.

## Verification (2026-09-10T09:45:00Z)

Mechanism verified; the full profile matrix is the end-of-job emulated sweep (see below). Commands
and outcomes:

- `cd src/kernel && cargo build` (x86_64), `./build.sh --arch aarch64 --part kernel`,
  `./build.sh --arch riscv64 --part kernel`, then full `./build.sh --arch aarch64` and
  `--arch riscv64`: all clean. The quiesce, the removed aarch64 bring-up probe and the lifted
  `function_bar`/`probe_function` compile on all three targets.
- `cargo test` in `src/tools/verify-model`: 157 passed. `verify-model check`: consistent - the ten
  new profile-phase keys and six umbrellas are in the catalog, `release-required.toml` is 210 keys.
- `./check.sh --gate qemu-virtio-iommu-x86_64` (the shared backend on x86_64, with P02M0099's block
  endpoint): PASSED - the transition confirmed, five hostile cases refused, a DHCP lease and the
  system volume read through translated endpoints. This is the portable backend (M2) exercised; the
  ports reuse it unchanged.
- A direct aarch64 boot under the enforcing topology (`GIC=2 UEFI=0 ./run.sh --arch aarch64 --smp 4`,
  captured): the controller at 00:01.0 masters the bus first, every virtio function and the xhci
  function are quiesced and confirmed before bypass-off, `iommu: virtio-iommu is translating -
  bypass is off and read back as off`, `dma: every bus-mastering device is translated`, four
  endpoints attached to domains, 0 quarantined, 0 faults, and the block driver bound behind the
  controller. A direct `-kernel` boot selects ROOT_NONE (the kernel's own boot code, no loader), so
  no system volume is promoted and the service graph waits on a root that never comes - which is why
  the direct ordinary phase's oracle is the kernel DMA audit and the block driver, and the DHCP
  lease and volume read are proved on the UEFI rows where the loader promotes a root.
- `./check.sh --gate iommu-aarch64-direct-gicv2-ordinary`: PASSED - the `iommu-port-ordinary-direct`
  verdict (controller translating, every bus-mastering device translated, endpoints attached with
  0 faults, the block driver online) with the census kept, no degraded admission.
- One correction found by the first port boot: `local virtio_opts="$virtio_opts"` in the aarch64 arm
  was an unbound-variable self-reference (`set -u` aborted the runner before QEMU); it is
  `disable-legacy=on`. And the ordinary verdict was split into `-direct` and `-uefi`: a rootless
  direct boot cannot show DHCP or a mounted volume, so requiring them there was wrong.
- `./check.sh --gate source-hygiene,gate-oracles,gate-result-logs,dma-mode-carrier,verify-model,milestone-index`:
  PASSED over the new scripts and the flipped ones.

DEFERRED to the end-of-job emulated sweep, and the reason: the full profile matrix is ten phases
across two emulated ports, and the hostile and gicv3-its transition phases each boot the WHOLE
`dma`-tagged test suite on an emulated machine (tens of minutes each). Per the project's "long
tests only at the very end" rule, they are not run mid-job. The gates and their oracles are in
place (`./check.sh --gate iommu-ports` runs all of them; each phase is its own gate); the mechanism
is proved by the x86_64 gate and the aarch64 direct-gicv2 ordinary phase above. NOT YET RUN, and
owed before P02M0173 is ticked COMPLETE: the four remaining aarch64 phases (direct-gicv2 hostile,
direct-gicv3-its transition and ordinary, uefi-gicv2 transition and ordinary) and all five riscv64
phases. `dma-mode-ports` (flipped to the enforcing rows plus the `--no-iommu` degraded regression)
is in the same emulated batch.

## Status

Implementation of M1, M2, M3, M4, M5, M7 is complete and the mechanism is verified on x86_64 and on
the aarch64 direct-gicv2 ordinary phase. M6's ordinary-endpoint proof is complete for that phase and
pending for the rest. The milestone is NOT ticked complete: its definition of done requires every
named profile proven, which is the deferred emulated sweep. It stays `- [ ]` in TODO.md with this
record standing for what is implemented and what is owed.

## Verification, continued (2026-09-10T12:30:00Z) - both ports' direct path, and the portability bugs it surfaced

The enforcing mechanism is now verified on BOTH ports' direct entry path, each booted through
`run.sh` under its own interrupt controller:

- `./check.sh --gate iommu-aarch64-direct-gicv2-ordinary`: PASSED. The controller at 00:01.0 masters
  the bus first, every virtio function and the xhci function are quiesced and confirmed before
  bypass-off, `virtio-iommu is translating - bypass is off and read back as off`, every
  bus-mastering device translated, the block driver bound behind the controller, 0 faults, 0
  quarantined, nothing degraded.
- `./check.sh --gate iommu-riscv64-direct-aia-ordinary`: PASSED, same sequence on the AIA/IMSIC
  machine with the GPEX host bridge pinned.

Getting there surfaced four real portability defects - each the kind of x86-shaped assumption M2 and
M3 exist to remove, and each fixed:

1. `qemu-run.sh` aarch64 arm: `local virtio_opts="$virtio_opts"` was an unbound self-reference that
   `set -u` aborted on before QEMU started; it is `disable-legacy=on`.
2. `qemu-run.sh` riscv64 dump: the `dumpdtb` command carried `-initrd` with no `-kernel`, which QEMU
   refuses (`-initrd only allowed with -kernel option`) - the dump generates the tree from the
   machine alone. Removed from the dump; the real boot keeps it.
3. `qemu-run.sh` riscv64 direct boot: `virtio-iommu-pci` writes a device-tree node, and the direct
   boot hands the dumped tree back with `-dtb` - so with the controller in both the dump and the
   `-dtb` boot, QEMU added the node twice and aborted (`FDT_ERR_EXISTS`). The controller now lives
   in its own array added to the real boot commands and left out of the dump; the kernel finds it by
   PCI device type through ECAM, never by that node, so the dumped tree needs it not.
4. `src/dma/src/virtio_iommu.rs::input_len`: `input_end - input_start + 1` overflowed on riscv64,
   whose QEMU virtio-iommu advertises the full 64-bit input range (`0..=u64::MAX`, inclusive length
   2^64). x86_64 and aarch64 advertise a bounded range, so neither hit it. `input_len` now saturates
   (a full range reports `u64::MAX`, losing only the single top byte as an IOVA, which nothing hands
   out), with a host test in `src/dma` for the full-range config (64 dma tests pass). This is the
   shared backend made genuinely portable rather than copied - M2's subject.

The dma fix rebuilt all three kernels (x86_64 and aarch64 behaviour byte-identical - they never see
a full-space range). The remaining emulated phases (both hostile phases on the test kernel, the
gicv3-its and UEFI transition phases, the UEFI ordinary phases on both ports) are the end-of-job
sweep in progress at the time of this entry; their results and any further portability fixes are
appended when it finishes. P02M0173 stays `- [ ]` until that sweep is green.

## The UEFI profiles: a fifth defect found, and the finding stated rather than worked around (2026-09-10T14:00:00Z)

A fifth latent defect was found and fixed on the way into the UEFI rows:

5. `qemu-run.sh::stage_signed_boot_manifest` - the harness signs the ports' per-run ESP manifest
   with `sign-manifest`, which has REQUIRED `--dma-mode` since the DMA-mode field landed
   (P02M0172); this caller never passed it, so every device-tree port UEFI boot died before QEMU
   started with "`--dma-mode` is required". Nothing had noticed because a port UEFI boot runs only
   in the emulated sweep. It now signs `--dma-mode harness`: a test-trust medium whose boot takes
   its mode from the `EFI/BOOT/LSDM` carrier staged beside the loader, which is what that manifest
   has always meant.

With that fixed, `./check.sh --gate iommu-aarch64-uefi-gicv2-ordinary` BOOTS and its enforcement
half PASSES, and its service half does not. Read off the phase's own log:

    iommu: virtio-iommu present, enforcing=true, healthy=true
    iommu: 1 endpoint(s) attached, 3 mapping(s) live, 0 quarantined, 0 fault(s) reported
    dma: mode enforcing-required (harness provenance)
    dma: every bus-mastering device is translated

so the controller came up, the transition completed and no endpoint reached memory untranslated -
the security property this milestone is about. What fails is BRING-UP: DeviceManager reports
`virtio-input`, `xhci` and four `virtio-blk` functions as "the teardown did not confirm, so this
device is quarantined for the boot", and `resource_manager`, `session_service`, `audio_service`,
`config_service`, `device_service` and `input_service` then FAIL to start, so there is no DHCP
lease and the phase's verdict times out.

WHAT THIS LOOKS LIKE, stated as a reading of the evidence rather than as a fix: the ordinary phase
boots the FULL INTERACTIVE machine `run.sh` builds - the system volume, three fixture media disks,
qemu-xHCI with a hub, keyboard, tablet and USB storage, virtio-gpu, virtio-sound and a virtio
console, about a dozen bus-mastering functions - and every one of them must now attach to a domain
and map its buffers, on an EMULATED aarch64 machine, inside DeviceManager's 4000-tick boot window.
The direct rows of both ports pass with four endpoints; the x86_64 enforcing gate's own traffic
phase deliberately boots a REDUCED machine rather than the interactive one. So the likely cause is
the boot window against a dozen translated endpoints under TCG, not a defect in the transition -
but that is a reading, and it is NOT verified: confirming it means another emulated boot with the
window or the endpoint set varied, at roughly twenty-five minutes an iteration.

WHAT IS THEREFORE OWED, and it is left open rather than declared done: the five UEFI and gicv3-its
phases, plus the two hostile phases, have not passed. Either the ordinary phases boot a reduced
machine for the ports the way the x86_64 traffic phase does, or the boot window is raised for an
emulated enforcing profile, or - per M3's own escape clause - a UEFI enforcing profile that cannot
bring its drivers up REFUSES rather than claims enforcement it cannot serve. Choosing between those
is the remaining work on this milestone and it needs the emulated iterations to decide.

## Final status of this milestone in this job

PROVEN: the enforcing topology, the per-class quiesce, the bypass-off transition and translated
endpoint operation on BOTH ports' direct entry path (`iommu-aarch64-direct-gicv2-ordinary` and
`iommu-riscv64-direct-aia-ordinary`, both PASSED), and the shared backend on x86_64
(`qemu-virtio-iommu-x86_64`, PASSED). Five portability defects found and fixed, four of them only
reachable by actually booting a port.

NOT PROVEN, and named: the two UEFI ordinary phases, the three transition phases, the gicv3-its
ordinary phase and the two hostile phases. `docs/todo/P02M0173.md` stays `- [ ]` and its status
block says the same thing.
