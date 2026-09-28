IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0196 (2026-09-28T03:54:55Z):

Status: IN PROGRESS - P02M0196a (step 1: identity, the claim, and the devices static descriptions name) first,
in the owner's agreed order (after P02M0198a, before P02M0190); P02M0196b and P02M0196d follow P02M0200, and
P02M0196c comes with P02M0197a/b. This record is updated as the work proceeds; the final state is at its end.

## P02M0196a - what was implemented (2026-09-28)

STEP 1 IS IMPLEMENTED ON ALL THREE PORTS AND VERIFIED ON x86_64. The two emulated ports are compiled and run
only in the single end-of-job run (the owner's standing rule: no aarch64/riscv64 build or test while any goal
task is open), so every port-specific line below is written and NOT yet compiled or run there.

### The platform row and its publication (kernel)
- `src/abi/src/lib.rs`: `DeviceInfo` gains the appended platform part (`PlatformPart`: kind, source, state,
  flags, match ids, MMIO ranges, wired lines, connections, DMA stream id, property length) - `ROW_KIND_*`,
  `PLATFORM_SOURCE_*`, `PLATFORM_STATE_*`, `MATCH_ID_*`, `LINE_*`, `CONNECTION_*`, `PLATFORM_FLAG_*`;
  `RESOURCE_KIND_MMIO` and `RESOURCE_KIND_LINE` beside `RESOURCE_KIND_PORT_RANGE`; `SYS_DEVICE_PROPERTIES` (96)
  and the property-block record format. Nothing versioned.
- `src/platform` (new host-tested crate): `Description` (identity, match ids, MMIO, ports, lines,
  connections), `place` (the placement rule: same identity -> that row; overlap starting at the same base with
  containment -> MERGE, identity joining as an `IDENTITY` match id and the row keeping the first description's
  resources; any other overlap -> REFUSE naming the row; else a new row), `merge`, `over` (a range over RAM or
  a BAR), `identity` / `table_identity`.
- `src/kernel/device.rs`: `PlatformRow` on `DeviceEntry`, `Described`; `init` publishes each port's
  descriptions AFTER every PCI function (the functions keep their indices), through `publish_locked`: MMIO
  checked against `forbidden_ranges` (every non-reserved memory-map region and every PCI BAR), a claimable
  row's ports against the reserved set and live grants, then `platform::place`; connections joined to their
  controller's row by identity once every row exists. `has_config_space` / `is_function` keep every PCI path
  (config space, bus mastering, MSI-X, I/O decode, fault containment) off platform rows. `info`,
  `discovered` (platform ids for the matcher), `platform_mmio`, `platform_line`, `properties`.
  The claim: only a CLAIMABLE platform row is claimed (kernel-held, firmware-held and reservation refused by
  name); a row naming a DMA stream is refused by name (no IOMMU driver here serves a platform master); a
  platform claim is NON-MASTERING - an entry declaring DMA for one is refused.
- `src/kernel/syscall/mod.rs`: `SYS_DEVICE_INFO` through `device::info`; `SYS_DEVICE_RESOURCE_ACQUIRE` mints
  `RESOURCE_KIND_MMIO` (range 1.., range 0 being the claim's own `DeviceMemory`) and `RESOURCE_KIND_LINE` (an
  `Interrupt` bound through `arch::interrupts::bind_wired`), each registered as derived from the claim BEFORE
  the handle exists, so the release revokes it; `SYS_DEVICE_PROPERTIES` answers through the claim OR through
  the `DeviceMemory` minted from a claim that is still the device's current binding - the driver holds the
  latter and never the claim, whose handle cannot be transferred; `SYS_INTERRUPT_ACK` unmasks a level line
  only while the object still owns its binding; `SYS_INTERRUPT_BIND` is RETIRED (answers `ERR_UNSUPPORTED`,
  number kept).

### Claim-scoped wired interrupts on all three architectures
- x86_64 `arch/x86_64/ioapic.rs`: every I/O APIC the MADT names (up to 8), located by GSI range, all entries
  masked at boot; `route_line` with the description's trigger and polarity; `mask` / `unmask`.
  `arch/x86_64/interrupts/mod.rs`: `LINES` (the 16 legacy vectors, then a 14-vector wired pool 0xF1..0xFE for
  GSIs above 15), `bind_wired` (refuses another controller's line, a GSI no I/O APIC answers, a legacy line the
  kernel handles itself, a held line, a full pool), `signal_line` (a LEVEL line is masked at its I/O APIC
  before the EOI), `acknowledge` (unmasks it), `unbind` (masks, then frees the vector); binds are serialised
  and a slot is free only when EMPTY, so a dying `Interrupt`'s `Drop` cannot unbind its replacement. The bare
  legacy binding table (`BOUND`, `is_bindable`, `bind`) is gone.
- aarch64 `arch/aarch64/gic.rs` + `interrupts/mod.rs`: SPIs configured edge or level and enabled; a level
  line is disabled at the distributor when it fires (`disable_spi`) and enabled by the acknowledgement
  (`unmask_spi`); refused for a non-SPI INTID, the MSI frame's window, a line the kernel answers itself.
- riscv64 `arch/riscv64/aplic.rs` + `interrupts/mod.rs` + `imsic.rs`: the MSI window shrinks to EIDs 1..=54
  and EIDs 55..=62 are the claimed lines' own identities (63 stays the kernel's `WIRED_EID`); `bind_wired`
  arms the APLIC source in the domain the boot's describe pass adopted (`aplic::adopt_domain`, from the tree's
  interrupt parent) to an identity in the claiming hart's file; `signal_line` clears the source's enable for a
  level line, `acknowledge` sets it again and writes `setipnum` so a line STILL asserted is pended again (MSI
  delivery mode pends a level source on its rising edge only); `unbind` disarms the source, then disables the
  identity on its owning hart outside the slot lock, and STRANDS the slot if that hart does not answer. The
  kernel's own sources (console UART, hot-plug slots) are recorded (`hold_source`) and refused to a claim. A
  machine whose tree names no APLIC domain (no AIA) refuses wired claims by name.
- The HAL contract comment in `arch/mod.rs` lists `bind_wired` and `acknowledge`.

### Descriptions per port
- x86_64 `arch/x86_64/platform.rs`: the kernel-held set - PIC, PIT, speaker, CMOS, ISA DMA controller,
  fw_cfg, LAPIC window, every I/O APIC, the PCI host (0xCF8 and the MCFG ECAM windows), the HPET table's window,
  COM1 (`kernel:com1`, `PNP0501`, 0x3F8, IRQ 4 through the MADT override), and EVERY IOMMU UNIT the firmware
  names: DMAR hardware units and IVRS hardware definitions (`table:DMAR#0.n`, kernel-held, the unit's register
  window - `acpi::dmar_units` / `acpi::ivrs_units`, new); then the static tables - `TPM2` (one page, start
  methods 6 and 7), `SPCR`, each serial `DBG2` entry, `WDAT` (its I/O registers as ports; memory registers
  left out and said), `BGRT` as a boot fact only.
- aarch64 / riscv64 `arch/*/platform.rs` + `arch/common/platform.rs::from_tree`: every tree node with a
  `compatible` and resources, the kernel-held compatibles and the console UART marked, `reg` translated
  through `ranges`, the TPM node cut to its locality-0 page with no interrupt, I2C/SPI children as
  connections to their bus, interrupts on a GPIO controller as line connections, the property block attached
  and unresolved references flagged. `wired_line` (per port) turns a specifier into a line; riscv64's also
  adopts the APLIC domain.
- SMBIOS: the loaders pass the UEFI entry point in `BootInfo.smbios`; `src/smbios` (new, host-tested) parses
  it; `report_smbios` names the system on the boot log and lists IPMI type-38 records (the attach to
  `IPI0001` is step 2's).

### Userspace
- `driver_binding::BindingId` gains `platform: Option<u32>` - a platform device's binding is named by the
  kernel's platform number (its row) and is never the same device as any function (it used to read as
  00:00.0, the host bridge). `provider_address` orders functions, then platform devices.
- `src/idl/device.lsidl` (`./gen.sh --accept-breaking`, pre-release, nothing versioned): `device-entry`
  gains the row kind, source, state, identity, ids, resources and the unresolved flag (appended);
  `provider-info`, `binding-record` and `incident-report` gain `platform: option<u32>` (appended).
- DeviceService fills the platform fields; `lsdev` therefore lists each platform row's source, identity,
  resources and state, and resolves a platform row's stored incident by its identity.
- DeviceManager: platform nodes carry `BindingId::platform`; policy, selection and incident keys use the
  stable identity (`device.policy.platform.<identity>`), never the number; a non-claimable platform row has no
  candidate; the bind mints every further MMIO range (`ResourceKind::Mmio`) and every wired line
  (`ResourceKind::Line`) after the port ranges; the bind ledger is sized for them.
- `driver_protocol`: `ResourceKind::Mmio` (8) and `Line` (9); drivers' `handshake` keeps them in the row's
  order (`Resources::mmio`, `Resources::lines`). `rt::device_properties`.
- The manifest vocabulary (`transport = "platform"` with `hid`/`cid`/`compatible`/`table`/`class`), the
  generated registries (kernel DMA policy, DeviceManager) and `driver_binding::Match::platform` were done
  earlier in this milestone's work.

### Test machine
- `src/harness/qemu-run.sh`: every test machine that is not the DMA fixture's carries one `edu` function,
  last of the PCI devices (renumbers nothing), with `dma_mask` widened to 64 bits: the IOMMU suite's baseline
  case aims this function's DMA at a sentinel wherever the allocator put it, and `edu`'s default 28-bit mask
  sent the copy to the address's low bits instead (observed: the case failed twice with the sentinel intact).
- `src/kernel/iommu/edu.rs`: `raise_interrupt` / `acknowledge_interrupt` / `interrupt_status`; `transfer`
  waits up to a second of ticks rather than two million reads (QEMU's `edu` copies from a 100 ms timer).

### Found and fixed on the way
- QEMU's `HPET` table names its base with a register width of zero - a WINDOW, not half a register - and the
  strict GAS reader refused it, so the HPET's block was owned by no row at all. `acpi::Gas::decode_window`
  reads a window; the dev boot and the test kernel now publish `table:HPET#0` kernel-held.
- The kernel-held set named no IOMMU unit: `acpi::dmar_units` / `acpi::ivrs_units` (new, host-tested,
  bounded walks refusing a structure whose length is short or runs past the table) and the x86_64 rows.
- `hardware.rs::a_pci_function_nothing_binds_is_still_inventoried_and_holds_nothing` compared every row's
  bus address; a platform row has none (reads 0:0.0) and is excluded from that uniqueness check.
- The two services-rig hygiene changes recorded in P02M0198's work (rigs terminate their service on drop,
  closed roots are given up) are confirmed: `media_import_service_reads_snapshots...` followed by
  `modem_service_refuses_at_its_provider_and_client_bounds...` - which hung before - passes in one run.

## P02M0196a - verification (2026-09-28)

x86_64 and the host, all PASSED unless stated; aarch64 and riscv64 NOT RUN (the job's final run):
- Host suites: `src/fdt` 120, `src/smbios` 11, `src/platform` 9, `src/abi` 28, `src/acpi` 43 (the DMAR,
  IVRS and QEMU-shaped HPET cases among them), `src/tools/system-manifest` 27,
  `src/user/libs/driver/binding` 89 (a platform binding is never function 00:00.0; handoff orders functions
  then platform numbers), `src/user/libs/driver/protocol` 76 (`Mmio` and `Line` resource kinds).
- Kernel, `./test.sh --arch x86_64` with a 63-test selection (every `kernel.platform_rows.*`, the claim,
  port-range, DMA-policy and wired-registry suites, the migrated line tests, the IOMMU baseline case): 63
  passed. The platform-row suite: a row claimed with two MMIO ranges, COM2's port range and IRQ 3, its
  interrupt RAISED by the second `isa-serial`, and a release revoking every handle and masking the line; the
  `edu` function's INTx as a LEVEL line - one delivery and the line masked while asserted, delivered again
  when acknowledged still asserted, quiet after it dropped, silenced by the release; a killed holder losing
  its line, its claim and every handle; a kernel-held row (and `kernel:com1`) refused, an overlap refused, a
  same-base description merged, a range over RAM and over a BAR refused, a claimable row over COM1's ports
  refused; the property block read through the claim AND through the derived window, not after the release;
  the HPET's window a kernel-held row; every IOMMU unit a kernel-held row (vacuous on the default machine,
  and run once with `QEMU_EXTRA="-device intel-iommu"`: one DMAR unit, kernel-held, refused).
- MUTATION: the x86_64 level mask removed from `signal_line` -> the level test FAILED with 20000 deliveries
  instead of 1 (restored, and the file compared against its pre-mutation copy).
- `./test.sh --arch x86_64 --tags drivers`: 40 passed. The thirteen service and application tests touching
  the provider catalogue, DeviceService or `lsdev`, plus the modem test: 14 passed.
- Development boot (`LIBER_DEVELOPMENT=1 ./image.sh --format iso`, one-off console script typing `lsdev`):
  the boot completes, drivers bind as before, `lsdev` lists every platform row with its kind, source, state,
  identity, ids and resources (`kernel:pic` ... `kernel:com1` `ids=[{kind=hid, text=PNP0501}]`), and
  binding records carry `platform=-` for functions; SMBIOS names the system.
- Gates: `source-hygiene` clean, `arch-surface` clean, `./gen.sh --check` no drift, `no-fixed-provider-slots`
  PASS (after two host harnesses were given the appended `platform` field),
  `python3 src/tools/foreign-audit-link.py --check` PASS (rt changed). `driver-event-dispatch` FAILS - and
  fails identically on commits d1fb2eda, 4afb8288 and f28d8255, before this work: "Wedged recomputes a
  reducer decision", and its `check-device-manager-progress.py` harness does not compile (seven errors about
  `Scope`, `open_subscription` and `decimal`) on those commits too. Pre-existing, not touched here.

## P02M0196a - what is open
- aarch64 and riscv64: compile, the kernel suite (the platform-row tests run there with the `edu` INTx through
  the GIC and the APLIC), the tree enumeration on the `virt` machines and a tree-described device claimed with
  its wired line - the end-of-job run.
- `kernel:com1`'s `ConsoleTap` kind and the handoff its claim performs: P02M0191c, next in the agreed order
  after P02M0190.
- `lsdev`'s "each PCI function's companion node": the companion join is step 2's (P02M0196b); nothing is
  joined before it.
- The host test of the TPM node cut to its page lives with P02M0196d's hostile trees; the cut itself is in
  `from_tree` and runs on the ports.
