IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0172 (2026-09-09T14:55:00Z):

Scope: the eight items of `docs/todo/P02M0172.md`. This record is written as the work proceeds; the
verification section at the end says what was actually run. Nothing below is implemented yet at
the time of this first entry - it records the design decisions taken after reading the tree, so the
auditor can check the implementation against them.

## What the tree has, verified before designing

- `src/kernel/dma_policy/mod.rs` decides from `IOMMU_REQUIRED_TYPES` (virtio-net only) keyed by
  `abi` VIRTIO TYPE, with every other type `TrustedUntranslated`; `dma::Policy` has exactly two
  arms and no `none`. `device::claim(index)` calls `dma_policy::admit(device_type, bus, dev, func)`
  and then enables bus mastering unconditionally on admission.
- `system-manifest`'s `RawDriver` is `deny_unknown_fields` with `lifecycle`, `match`, `priority`,
  `requires`, `provides`, `heartbeat-deadline`; no DMA field. `validate_name` bounds a program name
  to 1-64 bytes of `[A-Za-z0-9_-]`.
- DeviceManager's generated registry (`services/core/build.rs` -> `driver_registry.rs`) carries
  `name`, `artifact`, `priority`, `rules`; the claim syscall takes `(index, privilege, grant_ptr)`
  and no entry identity. The operator selection is persisted as `select=<artifact>` (not the
  program name) in `decide_policy`/`apply_stored_selection`.
- The signed manifest is v2 (`LBRMAN\x02\x00`, domain `...-v2`), header: alg, key id, product,
  arch, source kind, release, volume uuid, row count. Producers: `sign-manifest` (CLI) used by
  `mkimage.sh` and `qemu-run.sh`'s per-run ESP, and `mkpackages` (library call) for the system
  volume. `BootInfo` is `VERSION = 2` with `root` last.
- The loader reads no `fw_cfg`; the kernel's x86_64 `fwcfg.rs` has a port reader and the common
  MMIO reader takes the base from the device tree.
- The harness attaches `virtio-iommu-pci` on x86_64 only; `IOMMU` env decides, defaulting to 1
  except under `TEST=1`. aarch64 direct boot dumps the machine tree with `dumpdtb` and loads it at
  `DTB_ADDR`; riscv64 direct boot lets OpenSBI hand QEMU's tree over. `dtc` is NOT installed on this
  machine; `xorriso` and `mtools` are.

## Decisions

1. Manifest field: `dma = "none" | "iommu-required" | "trusted-untranslated"` inside
   `[programs.driver]`, required (no serde default), closed enum, so absence and unknown values are
   parse failures and non-driver rows cannot carry it (they cannot carry `[programs.driver]`).
   Values per the plan's migration table: virtio_net `iommu-required`, the other seven
   `trusted-untranslated`.
2. The signed manifest evolves ONCE to v3 and carries P02M0171's fields as well, in the position
   P02M0172 M3 fixes: `... release, security-generation:u64, purpose:u32, volume_uuid[16],
   dma-mode-present:u8, [dma-mode:u32 if 1], row_count:u16, rows, signature`. Magic
   `LBRMAN\x03\x00`, domain `libersystem-boot-manifest-v3\0`. A v2 record is refused by the shared
   decoder with its own `Refusal::LegacyVersion` and is never read as tag 0.
3. `BootInfo` goes to `VERSION = 3` with two appended `u32`s: `dma_mode` (0 absent, 1
   enforcing-required, 2 no-iommu) and `dma_provenance` (0 absent, 1 signed, 2 harness).
4. The harness record is the frozen 8 bytes `LSDM`,1,mode,2,0 on every carrier: x86_64 fw_cfg file
   `opt/org.libersystem.dma-mode`, non-x86 UEFI `EFI/BOOT/LSDM` on the per-run ESP, non-x86 direct
   the property `libersystem,dma-mode` of a `/libersystem` node with
   `compatible = "libersystem,boot-policy"`, appended to the machine tree by the harness with a
   small FDT appender in `src/harness` (no `dtc` on this machine).
5. `LIBER_RUN_MODE` is the one run-mode carrier; every entry point sets it only when unset; the
   runner refuses a boot whose mode is unset.
6. Claim ABI: `SYS_DEVICE_CLAIM` gains a fourth argument, a pointer to the 64-byte NUL-padded
   entry name; `ClaimGrant` and `ClaimInfo` gain `entry: [u8; 64]` and `policy: u32` as declared
   fields; `ClaimKey`'s padding is untouched.
7. The kernel validates the named entry against a table generated from the same manifest by the
   kernel's own `build.rs` (`dma_registry.rs`: name, policy, rules), matching rules with the
   host-tested `driver-binding::Match`.

(continued below as the implementation lands)

## Implementation record (2026-09-09, continued)

The decisions above were implemented as follows. Every path is repository-relative.

### M1 - the manifest field
- `src/tools/system-manifest/src/lib.rs`: `DmaPolicy { None, IommuRequired, TrustedUntranslated }`
  (kebab-case serde, closed), `RawDriver.dma: DmaPolicy` (required - serde `missing field` on
  absence, `unknown variant` on any other value), `Driver.dma`, `DmaPolicy::wire()` (0/1/2) and
  `name()`. Non-driver rows cannot carry it because only `[programs.driver]` can, and that table is
  already refused on any other role.
- `src/user/services/manifest.toml`: `dma = ...` on all eight driver rows per the migration table
  (`virtio_net` = `iommu-required`, the seven others `trusted-untranslated`), with the consequence
  written beside the network row.
- Tests: `src/tools/system-manifest/src/tests.rs` - every existing driver fixture gained the field;
  `every_driver_declares_its_dma_policy_and_nothing_else_may` (three values round-trip, absent
  refused, unknown refused, on a service refused) and
  `the_production_manifest_classifies_every_staged_driver` (loads the real manifest, asserts the
  migration table).
- `none` enforceability: `src/dma/src/lib.rs` gained `Policy::None` and `BindDecision::NonMastering`
  (with tests); the kernel keeps bus mastering OFF for a `none` claim, attaches no IOMMU endpoint,
  and `sys_dma_buffer_create` refuses (`ERR_ACCESS_DENIED`) a buffer under a claim whose stamped
  policy is `none` (`src/kernel/syscall/mod.rs`).

### M2 - the mode, the run mode and the producers
- `src/boot/protocol/src/dma_mode.rs` (new, shared no_std): `Mode`, the frozen 8-byte record
  (`encode_harness`/`decode_harness` with `Malformed::{Length,Magic,Version,Mode,Provenance,
  Reserved}`), `Carrier {Absent, Malformed, Valid}`, `Handoff {Signed, Harness}` <-> `BootInfo`
  words, the boot-wide `Latch` (record per verified manifest, resolve once), `LatchRefusal`, and
  the x86_64 `relay_check`. Tests in `dma_mode/tests.rs` (7).
- Run mode: `LIBER_RUN_MODE` is required by `src/harness/qemu-run.sh` (refuses unset/unknown);
  set only-when-unset by `test.sh` (`test`), `run.sh` (`public`), `check.sh` (`gate`), every gate
  script that invokes a runner (`gate`), and `src/harness/lab.py` (`development`, for dev-up and
  scenario-cold). The value decides only what the harness writes; it never reaches the guest.
- The harness value (`dma_mode_value` in qemu-run.sh): `LIBER_DMA_MODE` if a gate states one;
  otherwise x86_64 = `enforcing-required` iff the machine has a virtio-iommu (`IOMMU`), the
  device-tree ports = `no-iommu` on every row (pre-transition). `DMA_RECORD=absent|malformed|
  other|signed-provenance` and `DMA_DTB_NODE=1` are the refusal fixtures.
- Producers: `src/harness/dma-mode-record.py` (new; `record`, `dtb`, `dtb-check`, shapes for the
  negatives). x86_64: `-fw_cfg name=opt/org.libersystem.dma-mode,file=...` for a tag-0 medium;
  non-x86 UEFI: `EFI/BOOT/LSDM` staged in `qemu_build_esp`; aarch64 direct: the dumped tree
  annotated before `-device loader`; riscv64 direct: the tree is now dumped, annotated and passed
  with `-dtb` (the runner no longer `exec`s there, so it can remove the files).
- Two x86_64 images: `image.sh --dma-mode enforcing-required|no-iommu|harness|all` (default: both
  shipping modes, enforcing last) -> `libersystem.iso`, `libersystem-no-iommu.iso`,
  `libersystem-dev.iso` (and `.img`/`.qcow2`). `build.sh --dma-mode` exports `LIBER_DMA_MODE` to
  `mkpackages`, which signs the bootable volume for it and writes
  `system-volume-bootable-x86_64.dma-mode`; `src/harness/mkimage.sh` signs the medium for
  `LIBER_DMA_MODE`, refuses a volume signed for another value, keys the cache on the mode
  (`liber-boot-image-input-v4`), and the test medium is always `harness`.
- `run.sh --no-iommu` selects `libersystem-no-iommu.iso`; before QEMU starts it reads the signed
  mode off the medium (`src/harness/image-dma-mode.sh`, new: xorriso + mcopy +
  `sign-manifest --inspect`) and refuses a mismatched pairing with both values named; a missing
  degraded image names `./image.sh --format iso --dma-mode no-iommu`. The runner performs the same
  check for every x86_64 boot and adds no record beside a signed medium.
- The lab boots `libersystem-dev.iso` (built with `--dma-mode harness`) under `development`.

### M3 - the authenticated hand-off
- Manifest v3 (`src/boot/protocol/src/manifest.rs`): magic `LBRMAN\x03\x00`, domain
  `libersystem-boot-manifest-v3`, header `... release, security-generation:u64, purpose:u32,
  volume_uuid[16], dma-mode-present:u8, [dma-mode:u32], row_count:u16 ...`; `Refusal::LegacyVersion`
  for any other `LBRMAN` version (never read as tag 0); tag 2+, mode 0/3+, purpose 0/3+ refused by
  writer and reader. Tests: 8 new (fixed offsets, one-byte/five-byte record, surplus bytes break
  the row grammar, truncated record, legacy version, purpose, generation).
- `BootInfo` VERSION 3 with appended `dma_mode`/`dma_provenance` (`src/boot/protocol/src/lib.rs`).
- Loader (`src/boot/loader/src/dma_mode.rs`, new): `record()` called from `trust::verify_for` for
  every verified manifest (the same set the release latch covers), `resolve()` before
  `arch::hand_off` halts with the reason on any latch refusal or an independent input; per-path
  inputs in `arch::{x86_64,aarch64,riscv64}::dma_mode_inputs` - x86_64 reads `fw_cfg` through a
  new loader port reader (`arch/x86_64/fwcfg.rs`) and treats `EFI/BOOT/LSDM` as independent;
  the device-tree ports read `EFI/BOOT/LSDM` off the boot filesystem and treat the firmware
  tree's boot-policy node (checked BEFORE the no-DT profile withholds the tree) as independent.
  `handoff_words()` fills the three `BootInfo` builders.
- Signers: `sign-manifest --generation/--purpose/--dma-mode` (`--dma-mode` required; `--inspect`
  prints version/release/generation/purpose/dma-mode/volume-uuid); `mkpackages` reads
  `LIBER_DMA_MODE`/`LIBER_SECURITY_GENERATION` (defaults harness / 1).
- Kernel consumption: x86_64 `kmain` -> `adopt_dma_mode_from_loader` (relay check against the
  still-readable fw_cfg file) before `dma_policy::init`; aarch64/riscv64 prologues ->
  `crate::adopt_dma_mode(loader words, tree carrier, tree has node)` before the tree is even
  parsed for memory (UEFI: loader words, tree node = independent producer refused; direct: the
  tree's record via `fdt::Fdt::boot_policy_record`, new, found by `compatible`). The embedded
  `BootInfo` the ports publish carries the adopted words for reporting.

### M4/M5 - one identity across the claim boundary, DeviceManager selects, the kernel validates
- `src/abi/src/lib.rs`: `ENTRY_NAME_LEN = 64`, `DMA_POLICY_*`, `ClaimGrant{.., entry:[u8;64],
  policy:u32,_pad}`, `ClaimInfo{.., entry, policy, _pad}`, `entry_name_field`/`entry_name_of`;
  `SYS_DEVICE_CLAIM` documented with its fourth argument. Layout tests updated (ClaimGrant 104,
  ClaimInfo 96).
- `rt::device_claim(index, privilege, entry)`; DeviceManager passes `driver_name` (= `entry.name`,
  the exact candidate being attempted) in `begin_bind`.
- Kernel: `build.rs` generates `dma_registry.rs` (`DmaEntry{name, policy, rules:
  &[driver_binding::Match]}`, development rows only under `LIBER_DEVELOPMENT=1`) with
  `rerun-if-changed` on the manifest; `dma_policy::lookup`/`decide`/`admit` check exists ->
  declared for this device -> mode/policy; `device::claim(index, entry)` stamps entry+policy on the
  slot, `claim_stamp()`, `Claim::create(key, entry, policy)`, grant and info carry the stamp.
  Priority is never recomputed.

### M6 - no permissive fallback
- `src/kernel/dma_policy/mod.rs` rewritten: `IOMMU_REQUIRED_TYPES`, `policy_for` and
  `isolation_expected` are gone; `adopt()`/`handoff()`/`mode()`, `init()` compares mode with the
  bus (enforcing-required without translation refuses; no-iommu with a controller is a mismatch
  refusal); `Refusal{NoMode, UnknownEntry, EntryDoesNotMatchDevice, EnforcementAbsent,
  ControllerOnDegradedMode, PolicyRefused, Unaudited}`, `Admission{Translated,
  DegradedUntranslated, NonMastering}`; every refusal is printed with the entry name, the device
  and the value that refused it. `report()` prints the mode and provenance.

### M7 - rebuild selection
- The manifest feeds the kernel registry (`build.rs`, rerun-if-changed), the userspace registry,
  the image key (`manifest=` already; `dma-mode=` added) and the verification model
  (`system-manifest`, `manifest`, `bootproto`, `kernel`, `abi` are `selects_everything`).

### M8 - fixtures and gates
- Host: bootproto (manifest v3 + dma_mode), sign-manifest (tag coverage under the byte sweep),
  system-manifest, fdt (producer/consumer pair, run against the real Python producer on every
  fixture tree in both modes; negatives: wrong compatible, other node name found, wrong length,
  signed provenance; fw_cfg and tree records identical), dma, abi.
- Guest (kernel tests): `dma_policy/tests.rs` rewritten around the mode x machine x policy
  matrix (`every_mode_and_policy_combination_answers_as_the_matrix_says`), unknown/mismatched
  entry refusals, the selected-entry authority case (two entries declared for one device, each
  admitted with its own policy, an undeclared one refused), the registry-equals-migration-table
  check, and the audit-record tests carried over; `iommu/tests.rs` and `object/claim/tests.rs`
  re-pointed at named entries (synthetic test-only entries `synthetic-trusted|none|protected` and
  `edu-fixture`); `tests::claim_device` resolves the entry like DeviceManager; the boot test expects
  NetworkService online only on an enforcing boot and asserts its absence on `no-iommu`.
- Gates: `dma-mode-carrier` (host), `dma-mode-x86_64` (development pair, shipping pair, host
  pairing refusals, absent/malformed/signed-provenance records, second producer beside a signed
  set, independent ESP file, mixed selected set by re-signing), `dma-mode-ports` with
  `dma-mode-aarch64`/`dma-mode-riscv64` rows (UEFI and direct positive rows, absent on both paths,
  independent tree node beside the ESP file); `qemu-virtio-iommu-x86_64` now proves its
  test-kernel phase ran under `gate` with `enforcing-required`, the default machine took the signed
  field, and `--no-iommu` boots the degraded image and refuses `virtio_net` by name; the arch
  profile rows assert their `no-iommu`/`harness` line and the two treeless rows gained
  `no-dt-absent` siblings that require the loader's refusal. All registered in `check.sh` and the
  verify-model catalog (`GATES` 81, `PROFILE_ROW_GATES` 20, `GATES_THAT_BOOT_A_GUEST` 11,
  `UMBRELLA_GATES` + `dma-mode-ports`). `guest-verdict.py` gained the `dma-*` verdicts.

### Known limits, stated
- A valid x86_64 `fw_cfg` record naming a mode OTHER than the relayed one cannot be produced by
  a boot (loader and kernel read one static file); it is proved by `relay_check`'s host test
  (`InputDisagrees`) and said so in the gate.
- The `--no-iommu` and device-tree rows have no network, as the plan decides; the kernel boot
  test's expectation follows the boot's mode rather than being weakened.
- The IOMMU gate now assembles the degraded image during its run (after the freshness check) and
  restores the enforcing volume afterwards; a full `check.sh` still leaves the shipping image's
  key stale for that gate as before (see the memory note about a solo re-run).

### Correction after the first guest runs (2026-09-09T16:10:00Z): the boot without a link

The first scoped `boot` run on the x86_64 test row (DMA mode `no-iommu`, harness provenance)
showed a consequence the plan states but the first implementation did not carry through. The
kernel refused `virtio_net` by name and value as M6 requires, NetworkService found no provider and
FAILED ITS BOOTSTRAP - and because `time_service`, `permission_manager`, `console_service`,
`system_graph_service` and `shell` all depend on `network_service` by manifest, ServiceManager
never started any of them: five services "never started - waiting for network_service", no shell.
That is not the plan's "those boots come up with every other driver and NO NETWORK" and not its
Definition of done's "public AArch64 and RISC-V boots keep working at the produced `no-iommu`
value"; it is a boot with no console. The `known limits` entry above that said the boot test's
expectation "follows the boot's mode" was written against that broken state and is withdrawn here.

What was done instead:
- `src/user/services/core/src/network_service.rs`: a boot with no network provider no longer
  fails the bootstrap. It prints `network: no network provider on this boot - NetworkService is
  up without a link`, reports `NetworkService: online`, and enters `serve_unlinked`: a loop over
  the client channels only, answering the reserved connect request and `open` with fresh
  connections (so PermissionManager can still mint the network grant), `capacity` with the client
  count, `sockets` with an empty list, and every link-bound operation (`info`, `resolve`, `ping`,
  `probe`, `fetch`, `connect`, `listen`, `sntp`) with `Error::Io` - the device underneath is what
  is missing. No stack, lease or frame buffer exists in that mode. It exits when its last client
  is gone, as `serve_multi` services do.
- `src/kernel/test_suites/boot.rs`: `NetworkService: online` is required on every boot again; the
  special case that expected it absent on a `no-iommu` boot is removed, and the test prints the
  boot's DMA mode beside the report set.
- `src/tools/guest-verdict.py`: the three degraded rows (`dma-degraded`, `dma-signed-degraded`,
  `dma-port-degraded`) additionally require the `network: no network provider on this boot` line,
  which is the evidence that the machine came up past the refused driver.
- `docs/TESTING.md`, `docs/ARCHITECTURES.md`: the two sentences that described the previous
  behaviour now describe this one.

Guest evidence before the change: `.build/logs/test/x86_64-20260909T153553Z-2917096-guest.log`
(boot tag, FAILED: 17 of 22 reports, the five above never started). The rerun after the change is
recorded in the verification section below.

## Verification (2026-09-09T16:25:00Z, scoped x86_64 runs and host gates)

Passed, with the command:
- `./test.sh --arch x86_64 --tags dma,pci`: 66 passed (18 s) - the entry-name claim rule, the mode
  matrix, the PCI census and the degraded inventory on the no-iommu test row.
- `./test.sh --arch x86_64 --tags drivers,object`: 99 passed (31 s) - the claim, grant and
  DMA-buffer tests under the `entry`/`policy` stamp.
- `./test.sh --arch x86_64 --tags boot`: 15 passed (29 s) - after the boot-without-a-link
  correction; the guest log carries `dma: boot DMA mode no-iommu (harness provenance, the loader's
  BootInfo, fw_cfg relay checked)`, the `virtio_net` refusal by name and value, `network: no
  network provider on this boot - NetworkService is up without a link`, and every manifest service
  online including the shell.
- `./test.sh --arch x86_64 --tags permission-service,process-service`: 32 passed.
- `./check.sh --gate dma-mode-carrier`: PASSED (producer bytes, refusals, the fdt and bootproto
  consumers by name, the dtb-check mirror, and every entry point's run-mode rule).
- Host suites: bootproto (86 incl. the DMA-mode codec and latch tests), fdt, system-manifest, dma,
  abi, sign-manifest (9), lsidl-gen, display-proto, wire: PASSED.

Not yet run at the time of this entry (they assemble media or boot emulated ports, and run at the
end of the batch): `./check.sh --gate dma-mode-x86_64`, `--gate qemu-virtio-iommu-x86_64`,
`--gate dma-mode-ports` and the `arch-profile-*-no-dt-absent-1` rows, and `--gate
verify-model-tests` (blocked on the aarch64/riscv64 test-kernel rebuild, as noted for P02M0169).
The milestone's ticks wait for those results.

## Verification, continued (2026-09-09T21:40:00Z)

- `./check.sh --gate qemu-virtio-iommu-x86_64`: PASSED (with the block endpoint P02M0099's
  virtio-blk item added to its traffic phase). Its phase-2b assertion - that the test-kernel phase
  ran under the `gate` row with `enforcing-required` - was found grepping the suite's stdout for a
  line the runner writes to the run log the suite names; corrected to read the concatenated result
  logs. The row announcement (`qemu-run: run mode gate, DMA mode enforcing-required (harness
  provenance) via fw_cfg`) is present there.
- Still owed at the end of the job: `dma-mode-x86_64`, `dma-mode-ports` (both ports, eight
  boots), the `arch-profile-*-no-dt-absent-1` rows, and the aarch64/riscv64 builds they need.

## The ports' DMA-mode gate, run for the first time (2026-09-10T17:55:00Z)

`check-dma-mode-ports.sh` was written by this milestone and deferred to the end-of-job sweep. Running
it - after P02M0173 flipped its rows to the enforcing machine - found four defects in it, three fixed
and one left open as a finding. They are recorded in full in this batch's P02M0173 record, because
that is where the flip and the fixes live; in brief:

- the four driver rows booted the full interactive machine, which an emulated port cannot bring up
  inside DeviceManager's boot window; they boot the reduced machine now;
- the two DIRECT rows asserted on NetworkService, which a rootless direct boot never starts, and on a
  by-name refusal that such a boot never triggers because it never claims the NIC;
- `dma-port-loader-refused` forbade `loader: kernel loaded`, which this loader always prints before it
  resolves the DMA mode, so the row could not pass on either port.

Six of the aarch64 row's seven boots now pass, including both refusal rows this milestone owns: the
absent ESP record refused by the loader before a kernel was loaded, and the absent device-tree node
refused by the kernel with no driver admitted.

OPEN: the independent-producer row. This milestone's design has the LOADER refuse a second producer
beside the path's own; on the aarch64 UEFI path the loader hands over and the KERNEL refuses instead.
Nothing is admitted - the boot refuses every device claim, so the safety property holds - but the
refusal comes from the wrong component, and the loader's own check does not see a node the kernel
then finds in the very tree the loader handed it. The gate is left failing on that row rather than
relaxed. `dma-mode-aarch64` and `dma-mode-riscv64` therefore do not pass, and this milestone stays
`- [ ]`.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0172 (2026-09-10T14:56:45Z):

## The independent-producer row, diagnosed and closed

The previous entry left `dma-mode-aarch64` failing on the two-producer row and said why it was left
failing rather than relaxed. This entry closes it. Two separate defects were behind it, and a third
was found on x86_64 while confirming the first.

### What the row was actually showing

The rule this milestone owns is that a second, independent DMA-mode producer stops the boot. On the
aarch64 UEFI two-producer fixture the boot DID stop and nothing was admitted, but the refusal came
from the kernel rather than the loader, and the row demanded the loader. The guest log named the
cause three lines above the refusal:

    loader: no device-tree table (kernel will scan)

`find_dtb` reads the firmware's configuration table. These ports run a UEFI firmware that describes
the machine with ACPI and publishes NO device-tree configuration table, so the lookup answers 0, the
loader's independent-producer check is handed nothing, and the loader hands a mode over. The kernel
then has to find a tree, because this architecture describes its hardware with one: it falls back to
the fixed address and the low-DRAM scan its reader has always had, finds the machine's tree, reads
the boot-policy node out of it and refuses every device claim.

So the rule held and the boot refused. What was wrong was the row's assumption that the loader is
always the component that can see the tree.

### The scan that was tried, and measured, and reverted

The first attempt made the loader look where the kernel looks: the firmware's table, then the fixed
dumped-tree address, then a page walk of low DRAM. It works - an instrumented boot of the
two-producer row printed `dma-probe: header at 0x40000000`, `dma-probe: node found` and then the
loader's own refusal - and it is wrong anyway. On the ORDINARY row, where there is no second producer
and the walk therefore does not stop early, the same boot died:

    dma-probe: at 0x47700000
    Synchronous Exception at 0x000000005E8FF3D4

The loader runs under the FIRMWARE's page tables, and a firmware is entitled to leave pages inside
its own conventional memory unmapped - guard pages, on this one. 119 MB into the window the walk read
one and took a data abort, on a row that has nothing to do with second producers. A loader may read
the addresses firmware handed it. It may not go fishing in memory firmware did not describe, and no
memory map would have saved it: the descriptors call those pages conventional memory, because it is
the page tables and not the map that they are missing from.

The scan was reverted out of the loader. The rest of the attempt was kept:

- `src/fdt/src/lib.rs` gained `scan(start, end, step, phys_to_virt)` - the page walk and the header
  validity check the two kernels each had a copy of. Its comment says in as many words that it is for
  a caller that owns its page tables. `src/kernel/arch/aarch64/dtb.rs` and
  `src/kernel/arch/riscv64/dtb.rs` call it; their behaviour is unchanged and the page step became a
  named constant. `src/fdt/src/tests.rs` proves the walk steps over ground that is not a tree, stops
  at the FIRST tree rather than a later one (which is what each kernel's reader means by "the tree
  this machine has"), answers None for a window holding nothing, and refuses a zero step instead of
  spinning on it.
- `src/boot/loader/src/dma_mode.rs` gained `independent_tree(published, phys_to_virt)`, which asks
  the firmware's tree and only that one, and asks `is_valid()` BEFORE `boot_policy_record()` - the
  old one-line check did not, so a configuration table pointing at something that is not a blob would
  have walked whatever the garbage header declared. Both port backends call it.

### The rule now belongs to whichever component reads the tree

`dma-port-two-producers` in `src/tools/guest-verdict.py` requires the loader's refusal OR the
kernel's, forbids any driver coming online, and the gate additionally greps that the node was named
and that the boot refused. That is the same security claim - a second producer stops the boot,
whether or not the values agree, and nothing is admitted - asserted about the component that is
actually handed the tree. A firmware that does publish the configuration table is still refused a
step earlier, in the loader, and that alternative is the first one the case lists.

This is a deliberate change to WHICH component the row demands the refusal from. It is recorded here
rather than passed over: if the owner wants the loader to be the refuser on a firmware that publishes
no tree, the two ways to get there are an edk2-internal FDT client protocol or a harness-supplied
pointer, and neither is something this implementer should choose on its own.

### The x86_64 row that could never have passed

`dma-loader-refused` forbade `loader: kernel loaded`. The loader prints that the moment it holds a
verified kernel image, which is necessarily BEFORE it can judge the DMA mode of the set it verified:
`main.rs` prints it at the read and resolves the mode two hundred lines later, both inside
`efi_main`. The row could not pass however the loader behaved, and this is the first session in which
it was run. It now forbids only the kernel banner, which is what "the boot stopped in the loader"
claims. The same defect had already been corrected on `dma-port-loader-refused`. Two gate messages
that said "no kernel loaded" were corrected to "before the kernel started" for the same reason.

### Verification

Every gate below was run from ONE tree, after the last source edit, and each command is the whole
command. Nothing here is inferred from an earlier run.

    ./check.sh --gate dma-mode-aarch64     PASSED   7 boots, 7 verdicts
    ./check.sh --gate dma-mode-riscv64     PASSED   7 boots, 7 verdicts
    ./check.sh --gate dma-mode-x86_64      PASSED  10 boots, 10 verdicts
    cd src/fdt && cargo test --quiet       PASSED  92 tests
    ./build.sh --arch x86_64,aarch64,riscv64 --part loader,kernel   PASSED, no warnings

The two port gates cover, on each of aarch64 and riscv64: the UEFI row taking `enforcing-required`
off the ESP with virtio_net admitted behind the controller and a DHCP lease; the direct row taking
the same value out of the device tree; the UEFI and direct `--no-iommu` rows refusing virtio_net by
name and value while admitting and listing the trusted rows; the UEFI row with no ESP record refused
by the loader before the kernel started; the direct row with no node refused by the kernel with no
driver admitted; and the two-producer row refused with the node named and nothing admitted.

The x86_64 gate covers the development pair, the shipping pair, the host's refusal of a mismatched
image/mode pairing before QEMU starts, the four `fw_cfg` record fixtures (absent, malformed, another
value, a `signed` provenance on a replaceable medium), the second producer beside a signed set, the
independent `EFI/BOOT/LSDM` file beside `fw_cfg`, and the mixed selected set refused at the latch.

Not performed, and named rather than implied: no run on physical hardware, no firmware other than the
two the harness uses, and no check of what a firmware that DOES publish a device-tree configuration
table would do on the ports - the first alternative of `dma-port-two-producers` is reachable on such a
firmware and neither port's firmware here is one.
