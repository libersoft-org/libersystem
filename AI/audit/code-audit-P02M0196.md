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

P02M0196b AND P02M0196d - STARTED 2026-09-28T21:24:58Z, after P02M0200 in the owner's agreed order.

OWNER QUESTIONS THE PLAN ASKS "WHEN P02M0196b STARTS", NOT ANSWERED YET: whether `acpica-tools` may be installed
(ACPICA's AML test suite is ASL source) - built WITHOUT it, as the plan's fallback states: the interpreter's
conformance suite is the specification's constructs encoded by the tests' own encoder and the harness's emitter,
plus in-tree fixtures; and the laptop for the embedded controller's recorded run - the EC transport is a host-tested
library against a register model until then.

FIRST PIECE: `src/aml`, the interpreter (host-testable, no_std + alloc): the namespace (`namespace.rs`), names and
the search rule (`name.rs`), the object model with shared mutable objects and references (`object.rs`), the
implicit and explicit conversions (`convert.rs`), the evaluator (`interp.rs`: table load and method execution fused,
declarations, control flow, every expression opcode of the specification's table but `Unload`, which is refused;
`_OSI`/`\_OS`/`\_REV` as decided; the bounds - opcodes, time, calls, nesting, memory, loop iterations, package
depth, tables, nodes), fields (`field.rs`: region, index and bank fields, buffer fields, the access widths and
update rules, PCI_Config through `_SEG`/`_BBN`/`_ADR` and the bridges between, GeneralPurposeIo input reads only,
GenericSerialBus in its protocols), the host interface (`host.rs`), resource descriptors (`resource.rs`), `_DSD`
(`dsd.rs`) and the device helpers (`devices.rs`: the `_STA`/`_INI` walk in the specification's order, identity,
`_CRS`, `_DSD`, `_OSC`, `_DSM`, `run_reg`). `cargo test --manifest-path src/aml/Cargo.toml`: 37 passed - the
conformance suite (arithmetic, control flow, conversions, stores and CopyObject, references, method calls and
temporaries, the search rule, aliases and External, `_OSI`, memory/index/bank/buffer fields with update rules,
PCI_Config, Notify, mutexes, events, the global lock, Load/LoadTable, the walk, identity, resources, `_DSD` with a
hierarchical child, `_OSC` and `_DSM` by UUID, GenericSerialBus and GPIO fields, ConcatenateResTemplate and BCD,
`_REG`) and the hostile suite (an endless loop, a sleeping loop, deep recursion, a package past the depth bound,
allocations past the memory bound, the step bound, a region outside the policy, a field past its region, a bad
table given to Load, Unload, unknown opcodes and malformed lengths, Fatal, a name declared twice, a Wait nothing can
end). Found by the suite and fixed: the address-space descriptors' length offsets and their consumer bit (set is a
consumer).

## P02M0196b - what was implemented (2026-09-29)

THE KERNEL SIDE (`src/kernel`):
- `firmware/mod.rs` (new): the ACPI service's kernel state - the running instance (by process koid) and its event
  channel; every BAR and bridge window the bus decodes, recorded at the boot scan for EVERY function (`record_decoded`,
  from `arch::common::pci::decoded_ranges`, which now sizes a BAR with memory decode off and restores it); the
  service's SystemMemory mappings (by node, as `Weak<DeviceMemory>` - live while the service holds the handle);
  firmware-held functions; companions (a PCI function's node, with its `_AEI` lines and field lines/addresses);
  controller rows' lists; parent functions of nodes below a companion; merged descriptions and the ids they added;
  the identities the running instance reported; `_OSC` grants; SMBIOS's type-38 records. `map` (the SystemMemory
  policy through `platform::policy::system_memory`, write-back for firmware memory, uncached for MMIO, a companion's
  BAR making its function FIRMWARE-HELD and its memory decode on), `claim_refusal` (a firmware-held function; a range
  another node's live region maps - `policy::claim_blocked`), `pci` (reads of any function; writes held to
  `policy::config_write` with the function's MSI/MSI-X capability ranges walked, the driver-held test and the chipset
  rows), `attach`, `process_ended` (GPEs disabled, the channel dropped, what was published kept), `deliver` (the idle
  pass sends latched GPEs), `report` (device - reconciled through `device::publish_namespace`, IPI0001 checked against
  SMBIOS type 38 -, withdraw, companion, `_OSC` grant - applied through `arch::pci::apply_grant` and the hot-plug
  interrupts armed -, lists, and "namespace loaded" - which withdraws every namespace row, merged description and
  companion the instance did not report again, then sends `DEVICE_EVENT_NAMESPACE_LOADED`), `node` (what
  `SYS_DEVICE_NODE` answers), `tree_companion` (a device tree's PCI child node joined at the boot scan).
- `syscall/firmware.rs` (new): `SYS_FIRMWARE_TABLE` 101, `_MAP` 102, `_MEDIATED` 103, `_PCI` 104, `_REPORT` 105,
  `_EVENTS` 106, `_GPE` 107 (instance-only but the count), all behind `FirmwareInterpreter`; `SYS_DEVICE_NODE` 108.
  SystemIO regions use P02M0191's `SYS_PORT_RANGE_FIRMWARE`.
- `device.rs`: `publish_namespace` (reconcile by identity: `Same` - logged when it differs -, `Refill` with a new
  generation and an arrival, otherwise placed; a reservation a row of its own and never merged; a new row held to the
  `_CRS` check and kept out of every reservation's ranges; connections joined to a row or a companion's function),
  `withdraw_namespace`, `unmerge`, `namespace_rows`, `with_tables`; `claim` refuses a withdrawn namespace row and
  whatever `firmware::claim_refusal` says (`FirmwareDriven`); `info` shows a firmware-held function's state; the
  boot's forbidden ranges include every decoded BAR and window; a boot connection whose controller is a companion
  (or a node below one) joins to the function's row.
- `object/device_memory.rs`: `for_firmware` and `write_back`; `SYS_DEVICE_MEMORY_MAP` maps write-back where minted so.
- `arch/x86_64/sci.rs`: the GPE0/GPE1 blocks (`acpi::gpe`) initialised - enables cleared, statuses acknowledged -
  before the SCI is routed; the handler masks and latches every asserted enabled event into a bitmap (allocation-free
  `Gpes::handle_into`), decodes `GBL_STS`; the idle pass delivers; requests `gpe_request`; `gpe_instance_ended`
  (`Gpes::reset_runtime`); the storm path now MASKS the SCI's redirection entry. `arch/x86_64/firmware.rs`: tables,
  SMI, PM timer, `GBL_RLS`, CMOS NVRAM, the kernel's wired lines and the chipset rows (MCH PCIEXBAR, ICH9 PMBASE and
  ACPI_CNTL); stubs on aarch64/riscv64.
- `arch/common/pci/mod.rs`: `_OSC` gating - on an ACPI machine (not the test kernel) the scan arms slots and watches
  error reporters only on buses a grant covers (`gate_on_osc`, `apply_grant`, `controls`); `main.rs` calls
  `firmware::init` before the first scan and `firmware::deliver` on the idle pass; `arm_hot_plug_interrupts` is
  `pub(crate)` for the grant.
- `process/mod.rs`: `firmware::process_ended` beside `idle::process_ended` on both ends of a process.
- Kernel tests `firmware/tests.rs` (7): the privilege gate; tables and mediated accesses on x86_64 (DSDT, FACS, APIC
  instances, a short buffer, the CMOS clock/century/bank refusals and a written byte, the SMI disable value, the PM
  timer counting); configuration writes (header, MSI, driver-held i6300esb, q35's PCIEXBAR); regions (RAM refused, a
  hole uncached, NVS/reclaimable write-back, a BAR refused to another node and to the companion while a driver holds
  it, then admitted and the function firmware-held - in its row, its node, and refused to a claim, past the mapping);
  a claim and another node's region refusing each other; a walk reconciled across two instances (same row, differing
  report kept, reservation and the reservation rule, a merge into a static row taken out, a companion with lists,
  a controller row's lists, loaded refused for the wrong instance, arrivals then the report last, a second instance's
  withdrawal of what it did not report, refill with a new generation, an explicit withdrawal); only the running
  instance asks for GPEs.

THE ABI (`src/abi`): the eight syscall numbers, `FirmwareMapRequest`, `FirmwareNode` (with `_COMPANION`, `_PARENT`,
`_FIRMWARE_HELD`, `_LISTS`), the GPE operations, the mediated operations, the event kinds, `DEVICE_EVENT_NAMESPACE_LOADED`.

THE PLATFORM CRATE: `report` gained `LISTS` (a controller row's lines and addresses).

THE INTERPRETER (`src/aml`): `\` alone and `^` alone name a node (q35's DSDT opens `Scope (\)` - found on the first real
boot); `Host::region` - the declaring node, space, base and length announced before every SystemMemory/SystemIO access,
so the host maps a region whole and the kernel decides by node; `dsm_bytes`; the test tooling (`build`, the map-backed
host) moved to `aml::testing` behind a `testing` feature for the ACPI model's suites. 45 host tests.

THE ACPI MODEL (`src/user/libs/acpi/model`, new, host-tested): `node` - each node's role (reservation, host bridge,
companion - on a host bridge's bus or a bridge's secondary bus -, a device below an endpoint's companion carrying its
function, the `video-output` class rule, embedded controller, processor and processor container, thermal zone under
`THERMALZONE`, PCI interrupt links and everything else "not a device" with the reason) and the row it is described as
(identity `acpi:` + path, state, `_HID`/`_CID`/class ids, memory/ports/wired lines, GPIO and I2C connections by their
controllers' identities, an output `GpioIo` refused by name); `properties` - `_UID` and `_DSD` as a property block in
the node channel's value encoding; `handshake` - `_OSC` for host bridges (hot-plug, PME, AER, capability, LTR),
`\_SB._OSC` (CPPC, CPPC v2, `_OST`, `_PR3`, platform-coordinated `_LPI`), processors (`_OSC`/`_PDC` - C1 halt, MWAIT
hints, software coordination, `_PPC` notification, never the MSR P/T-state forms); `admission` - what a node channel
may evaluate (the node, its own objects, the class row's parent methods - `_DOS` for a video output -; platform methods
and other nodes refused); `events` - `_Lxx`/`_Exx`/`_EVT`. 7 host tests over scripted namespaces.

THE SERVICE (`src/user/services/core/src/acpi_service.rs`, new, static, volume-staged, transparent): roles FIRMWARE
(privilege, `supervisor_role`) and ADMIN (`liber:device@1/acpi-admin` serve root); attaches the instance; loads the
DSDT and every SSDT; `_REG` for SystemMemory, SystemIO, PCI_Config and SystemCMOS; maps the FACS for the global lock
(the FACS protocol, `GBL_RLS` through the kernel when the firmware was pending); the embedded controller from the ECDT
(early) or `PNP0C09`'s `_CRS`, `_REG`, `_GLK` honoured with a bounded global-lock take, queries drained to `_Qxx`;
companions found before the walk (a region a companion declares must name its function); `\_SB._OSC`; the `_STA`/
`_INI` walk; every node accounted for - reservations reported first, host bridges' `_OSC` reported, processors'
handshakes, companions with their lists, devices with `_CRS`/`_DSD`/`_UID` (and `_IFT` for IPI0001), controller rows'
lists; "namespace loaded"; every `\_GPE` event enabled. The loop: GPE events (edge acknowledged before, level after,
then re-enabled; the EC's by its queries), `_AEI` line events (`_Exx`/`_Lxx`/`_EVT` in the controller's scope, then the
line acknowledged), `Notify` delivered to node channels and a device/bus check or eject re-walking the subtree and
withdrawing what left, the admin root (node channels; connections), node channels (path, evaluate, `_DSD`, `_DSM`,
notifications). The host: SystemMemory mapped per region AND per declaring node (found by the gate: a mapping reused
across nodes had let another node's region over a claimed range through without asking the kernel), SystemIO through
`PortRange`s, the SMI port and PM timer through the kernel, PCI_Config and CMOS through the kernel, GenericSerialBus
through an address-scoped `i2c-device` connection, GeneralPurposeIo reads through a line-scoped `gpio-device`
connection (a write refused by name).

THE IDL (`device.lsidl`): `acpi-node`, `acpi-notification`, `acpi-connection-kind`, `acpi-admin`; `device-entry` gained
`companion` and `parent` (a pre-release record change, `gen.sh --accept-breaking`).

SERVICEMANAGER: the `acpi_service` manifest row (after LogService and ProcessService), `supervisor_role`'s FIRMWARE arm
(from P02M0200), `plan_relaunchable`, and `hand_acpi_admin` - a connection of the ADMIN root sent on DeviceManager's
control channel as `ACPI` after the first start and after every relaunch.

DEVICEMANAGER: `ACPI` hand-off; `DEVICE_EVENT_NAMESPACE_LOADED` acted on once the instance's connection is held too
(whichever arrives first); `acpi_pass` every loop: a driver's `NodeRequest` answered with `Node` (the service's node
channel) or `NodeAbsent`, at once or at the report; the lines and addresses a controller's companion or row lists minted
as scoped connections through its published `gpio-lines`/`i2c-bus` provider and handed to the service - at each
instance's report, and for a controller bound after it when it publishes.

THE DRIVER PROTOCOL: `NodeRequest` 13 (driver->manager), `Node` 14 (one channel), `NodeAbsent` 15. The wire revision
constant is NOT bumped: the owner's standing rule that nothing is versioned before the first release; every artifact of
a build agrees by construction. Drivers' `common`: `request_node`, `node`, `wait_node_or_answer`, the node frames taken
on every control drain, and a node channel whose service ended given up and asked for again. virtio-gpio and
virtio-i2c ask once online.

LSDEV: a firmware-held function's state and every row's companion/parent (DeviceService fills them from
`SYS_DEVICE_NODE`).

THE FIXTURE AND THE GATE: `harness/acpi-fixture.py` (the SSDT through the emitter - `Scope` into QEMU's `SA0`/`SA8`/`SB0`
for the ivshmem/virtio-i2c/virtio-gpio functions, since q35's DSDT already declares a node per present slot; the
ivshmem BAR below 4 GiB, because q35's DSDT is revision 1 and every integer in the namespace is 32 bits - both found on
the first fixture boot); the emitter's `qword_memory`, `crs_patched64`, `create_byte_field`, `create_qword_field`,
`shift_right`; `qemu-run.sh` `ACPI_FIXTURE`/`ACPI_FIXTURE_MEMORY` (the SSDT, `ivshmem-plain` at 0x14 over the file,
`X-PciMmio64Mb=0`) and `PCIE_HOTPLUG=acpi`; the development-only `acpi_fixture` driver (LSFX0001's probes on every
`Notify`); `tools/check-acpi.sh` (gate `acpi`); `harness/fdt_edit.py` (the shared device-tree editor,
`dma-mode-record.py` now built on it, and the ports' tree fixture); gate `firmware-fixtures`.

THE DEVICE TREE: `fdt` reads a child of a `device_type = "pci"` node as `NodeBus::Pci` with `pci_function()`; the
kernel's `from_tree` joins such a node to its function as a companion (`firmware::tree_companion`) instead of
publishing it, and a connection naming a node below a companion joins to the function's row.

DECISIONS AND WHAT IS NOT HERE:
- THE CHILD BINDING (a namespace or tree device bound through scoped connections to its controller, the controller a
  dependency, the connections as `RESOURCE` frames) is P02M0195b's, next in the agreed order; this step publishes the
  child's connections joined to the controller's row and grants the SERVICE's connections. The ports' check that a
  fixture node is bound through a line connection runs with it.
- THE SECOND FIXTURE CARVE-OUT (a processor table's registers the install check names) waits for P02M0198's install
  check, which does not exist yet; the first carve-out (a `_CRS` range in the firmware-held ivshmem BAR) is here.
- THE SLEEP-TYPE REGISTRATION the privilege admits is P02M0197b's call.
- A HOST BRIDGE'S I/O WINDOWS are not recorded, so a `_CRS` port range is checked against the reserved set, live
  grants and the functions' I/O BARs, not against a bridge's I/O window.
- HPET: q35's `\_SB.HPET._STA` reads the HPET's registers, which the kernel holds - the region is refused and the node
  "did not initialise", which is the policy working; the HPET row is the kernel's.

## P02M0196b/d - verification (2026-09-29)

ADDED AFTER THE SECTION ABOVE, before verifying: the `_CRS` port check now covers every PCI-to-PCI bridge's I/O window
as well as the functions' I/O BARs (`arch::common::pci::io_windows` - a bridge with its I/O decode off forwards no
port and is skipped, so an unprogrammed base and limit of zero cannot read as the whole legacy range; recorded at the
boot scan beside the decoded ranges by `firmware::record_decoded`, joined to the I/O BARs in `admit`). This replaces
the "A HOST BRIDGE'S I/O WINDOWS are not recorded" point in the decisions above for the bridges the scan sees. The
refusal now reads "its ports are a PCI function's I/O BAR or a bridge's I/O window". A kernel test was added for it
(below).

COMMANDS AND RESULTS (x86_64 and the host; PASSED unless said):
- Kernel, x86_64: `./build.sh --arch x86_64` (ok, 392 s), then `TEST_SELECTION=<30 ids> ./test.sh --arch x86_64
  --timeout 1800` - 30 passed (81 s): the eight `kernel.firmware.*` tests (the seven above and
  `a_namespace_row_s_ports_are_minted_from_its_claim_and_never_over_a_bar_or_a_bridge_window` - COM3's ports on a
  namespace row minted by index from its claim and revoked by the release; ports over a function's I/O BAR and inside
  the hot-plug root port's I/O window refused), the seven `kernel.platform_rows.*`, the three `kernel.declared.*` and
  the twelve x86_64 `kernel.object.port_range.*` (logs `.build/logs/test/x86_64-20260929T020010Z-392104-*`). The first
  attempt named `a_machine_with_no_port_space_mints_nothing`, which is compiled on aarch64 and riscv64 only, and was
  refused by the selection before anything ran; it is not a failure of the code.
- Test kernel: `cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests` - clean after every kernel change.
- Host suites (`cargo test --manifest-path ...`): abi 28, platform 17, aml 45 (again after the emitter's `GpioInt`
  consumer bit changed the committed sample), acpi 49, acpi-model 7, fdt 121, driver protocol 77, driver binding 89,
  system-manifest 28, smbios 11.
- Gates: `acpi` PASS (346 s: the fixture boot with every node accounted for, lsdev's rows, round 1 of the probes, two
  hot-added CPUs through `_E02`, `_OSC`, SMBIOS, the service killed and restarted with its rows kept, LSF5 withdrawn,
  the line granted again and round 2, then the firmware-hot-plug boot where `_OSC` refused native hot-plug and the
  slots stayed unarmed); `firmware-fixtures`, `aml-emitter`, `source-hygiene`, `arch-surface`, `test-tags` PASS;
  `capability-model` PASS (1434 s); `./gen.sh --check` exit 0.
- `qemu-pcie-hotplug` FAILS - AND FAILS THE SAME WAY WITHOUT THIS WORK. On the current tree: twice, and once more with
  the ACPI service stopped before the gate ("the slot was powered down at line 1, before the driver let go at line 2").
  On a tree extracted from `c19b8c1d` (`git archive`, before this step and before P02M0200), built from scratch in its
  own directory (two rustc crashes - SIGILL, then SIGSEGV compiling `core` - before the third build went through):
  the same failure, with the same sequence line for line. The kernel's `device: N released` follows the driver's
  exit, the idle pass then powers the slot down, and DeviceManager records an incident "it exited without saying
  anything" TWICE for a driver whose last frame was `STOPPED` (last opcode 10), before its `stopped cleanly` and `the
  node is removed from the bus` lines arrive - so the gate's order check reads the removal after the power-down. A
  pre-existing defect in the removal path (DeviceManager's handling of an answered stop whose exit arrives first, and
  the gate's use of a line that comes after the release), not introduced here; reported to the owner, not fixed in
  this step.
- NOT RUN: aarch64 and riscv64 (every build and test - at the end of the job, by the standing order), the device
  tree's PCI child join and the tree fixture on the ports, the embedded controller on a laptop.

## P02M0196c - the platform data of processor power (2026-10-01)

WHAT WAS IMPLEMENTED - the third item of step 3 (the first two, the platform half of sleep and the device power
states, landed with P02M0197a and b and are recorded in that milestone's audit):

- `liber:device@1/processor-firmware` (`src/idl/device.lsidl`): `processors` (every processor by namespace path, its
  UID and the kernel's core), `power(path)` (`processor-power`: the idle states - `_LPI` where the processor has one,
  `_CST` otherwise - `_PSS` with `_PCT`'s registers, `_PPC`, `_PSD`, `_CPC` with its energy-preference register,
  `_TSS` with `_PTC`'s control register, `_TPC`, `_TSD`, and each object that did not evaluate or read, by name),
  `notifications` (a stream of `Notify` 0x80, 0x81 and 0x82) and `ost(path, event, status)` (`not-found` for a
  processor without `_OST`). The records: `processor-register`, `processor-idle-state`, `processor-performance-state`,
  `processor-throttling-state`, `processor-domain`, `processor-cppc`, `processor-id`, `processor-power`,
  `processor-notification`. And `acpi-node.has(name)`, an object's presence with nothing evaluated - the zone's
  driver asks it before `_SCP`, a method whose call changes the platform.
- `acpi_model::processor` (`src/user/libs/acpi/model/src/processor.rs`): `gas`, `cst`, `lpi`, `pss`,
  `control_status`, `domain`, `cpc` (with element 19, the energy-performance preference register), `tss`, `uid_of`
  (a `Processor` object's processor ID, else `_UID` in decimal or `0x` hexadecimal) and `core_of` (the UID matched
  against the MADT's, and the APIC ID that entry gives against each core's hardware ID). `acpi::Madt::processors`
  (`src/acpi/src/lib.rs`) reads the local APIC and x2APIC entries; `CpuIdleInfo.hardware_id` (`src/abi`) carries each
  core's APIC ID out of the kernel (`idle::info`).
- THE SERVICE (`src/user/services/core/src/acpi_service.rs`): the PROCESSORS serve-root (`services/manifest.toml`,
  `liber:device@1/processor-firmware`, CONNECT minting at most two connections - an instance and a replacement's),
  each processor recorded by path and UID in the walk (`processor()`, after its `_OSC`/`_PDC` handshake), the MADT
  read once at the start (`madt_processors`), `processor_power` evaluating and reading every object
  (`processor_object`, `read_object`, `limit`), `processor_ost`, and `processor_notification` forwarding a processor's
  0x80 to 0x82 to every connection's stream from `deliver_notifications`. `ProcessorView` is the
  `processor_firmware::Service`; `serve_processors` the root's and the connections' loop.

DECISIONS:

- The processor is keyed by namespace path (`Path::text`, every segment four characters) and its UID; the core by the
  MADT's APIC ID matched against the kernel's own per-core hardware ID, not by MADT order, so a MADT listing absent or
  disabled processors before present ones still names the right core.
- An object that is absent is absent, and one that did not evaluate or is not the shape the specification gives it is
  named in `refused` and left out; the rest of the processor's power is still answered.
- `_OST` for a processor without one answers `not-found`, which the policy treats as nothing to acknowledge.
- A thermal zone's cooling methods are not served here: they reach ProcessorPowerService through the zone's own
  driver and its node channel (P02M0198's thermal-zone publication), as the item says.
- THE ROLE IS A FACTORY ROLE, NOT A `client` ROLE: a `client` role delivers a duplicate of the provider's kept root end,
  which works only for a provider serving its requests on that root. The ACPI service answers CONNECT on the
  PROCESSORS root and serves the connection it mints (as PowerService does on its root), so ProcessorPowerService's
  `processors` role (and its `power-state` role) is `factory` in `services/manifest.toml`. With the `client` role
  the first `describe` timed out. Still a manifest role, as the item asks; the mechanism is the one the service's
  CONNECT needs.
- THE GATE'S FIXTURE AND QEMU's `_OST`: q35's DSDT already declares `\_SB.CPUS.C000._OST` (CPU hot-plug), and the first
  declaration stands - the fixture SSDT's `_OST`, which wrote the shared pages, was never run. The fixture no longer
  declares one (`acpi-fixture.py`: no `_OST`, no `OSTE`/`OSTS`/`OSTN` fields, no `ost_*` keys), and the gate reads
  QEMU's own record of what C000's `_OST` was told (QMP `query-acpi-ospm-status`), checking first that it does not
  already read (0x80, 0) - so the acknowledgement the service sends after a `Notify` 0x80 is observed in QEMU, not in
  pages an SSDT writes.

VERIFICATION OF STEP 3's THIRD ITEM, so far:

- Host: `acpi-model` 22 (the processor objects, `uid_of`, `core_of`), `acpi` 51 (`Madt::processors`), `proto` 53.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate processor-power` -> PASS twice on 2026-10-01, both with the `_OST`
  change above in place: every core's `_LPI` installed and entered, `_PSS`/`_PCT`/`_PPC`/`_PSD` and CPPC with its
  preference register installed, C002's model-specific `_PCT` refused whole, the profiles' windows, `Notify` 0x80 on
  C000 (the fixture's `_E07`) read again and acknowledged - QEMU's OSPM record for C000 reads (0x80, 0) after it - and
  the zone's and the fans' cooling. A rerun after the ports' processor changes are in the tree is in the gate batch of
  2026-10-01 and is recorded below when it ends.

## The gates after the ports' processor work (2026-10-01)

- `LIBER_DEVELOPMENT=1 ./check.sh --gate processor-power` -> PASS a third time, with the device-tree ports' processor
  changes in the tree (the x86_64 half untouched by them: `map_register` answers a reason, `firmware_suspend` the halt).
- `acpi` -> PASS (416 s).
- `qemu-tpm-tool` found the merge rule's effect on the TPM row - the `TPM2` table's row now carries the namespace
  device's path and `MSFT0101` as ids, its one resource still the table's page, as step 1's merge rule states; the
  scenario's expectation is updated in P02M0190's audit.

## The first cross-build since step 2 (2026-10-01)

- `./build.sh --arch aarch64` FAILED on `acpi_service`: `rt::port` (the port instructions) is x86_64's alone, and the
  service's SystemIO accesses and its embedded-controller transport used it unconditionally - the ports had not been
  built since step 2 landed (2026-09-29). The service is built for every target and serves nothing where no ACPI table
  exists; on aarch64 and riscv64 a local `port` module answers what an undecoded port reads (all ones) and drops
  writes, behind a kernel that grants no port range there, so no access reaches it. `cargo check` of the services,
  drivers and tools crates (`--features development`, and `shared-image` for the tools) then clean on both ports.

## The platform half of sleep host-tested, and the carve-out's second admission closed (2026-10-03)

WHAT WAS DONE:
- `acpi_model::sleep` (`src/user/libs/acpi/model/src/sleep.rs`): the platform half of a sleep, pure. `Sleeper::prepare`
  lets go of what an earlier `prepare` took, then for each wake node reads `_PRW` through `Platform::prw` (absent, none,
  an event on a GPE block device, or the event's number, its deepest state and its power resources), passes a node over
  whole when it cannot wake from the state entered, holds its resources on, runs `_DSW` (or `_PSW`), sets its event for
  wake and records it only when the kernel took it; then `\_PTS` for a state the firmware enters and `\_SI._SST`
  sleeping (hibernating for S4). `Sleeper::wake`: `_SST` waking, `\_WAK` for a firmware state, every recorded event
  cleared, every holder let go, `_SST` working. `sleep_type` reads SLP_TYPa/SLP_TYPb from `\_S3`/`\_S4`/`\_S5`'s
  package (three bits each, the second defaulting to the first). The console lines are the service's, unchanged.
- The ACPI service runs `platform-sleep` through it: `SleepPlatform` implements `Platform` over its namespace
  (`node_of_identity`, `_PRW`, `resources_of`), its power resources (`power.hold`, `switch_resource`,
  `release_power`), `_DSW`/`_PSW`, the kernel's wake events (`GPE_WAKE_SET`/`CLEAR`) and `root_method`; its
  `wake_armed`/`wake_holders` fields became one `Sleeper`; `register_sleep_types` decodes through `sleep_type`. One
  difference in what is said, none in what is done: a `_PRW` element that is no power resource is now named even for a
  node that then cannot wake from the state entered.
- Six host tests against a scripted namespace (`sleep::tests`): S3 in ACPI's order and its wake undoing it; suspend to
  idle with no `_PTS`/`_WAK` and S4 with both and the hibernating `_SST`; a node passed over whole (deepest too shallow,
  no `_PRW`, a block device's event, absent); a refused event never cleared while its power is let go; a second
  `prepare` letting go of the first's; the sleep-type decoding.
- P02M0196's policy item closed on the evidence P02M0198 produced: the fixture carve-out's second admission (processor
  table registers in the firmware-held `ivshmem-plain` BAR) is exercised by the gate `processor-power`.

VERIFICATION:
- `cargo test --manifest-path user/libs/acpi/model/Cargo.toml`: 28 passed - `sleep::` 6 of them (one first run ended in
  a rustc SIGSEGV, this machine's, and passed on the retry); the suite is `host-tests`'s `acpi-model`
  (`verify-model host-suites` lists it).
- `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`: RESULT ok; rustfmt clean on the service and the model.
- `./check.sh --gate sleep` (x86_64, 2026-10-03): PASS in 2061 s - the platform boot's lid S3 with the TAD's and the
  lid's wake armed (`_PRW` resources on, `_PSW`/`_DSW`, the wake GPEs) and given back after the wake, as the power
  checks before, during and after read them; "sleep types registered" on every boot, and the fallback boot with them
  named absent.
- `./check.sh --gate processor-power` (x86_64, 2026-10-03, PASS in 614 s) for the carve-out's second admission.
- `./check.sh --gate source-hygiene`, `gate-oracles`, `milestone-index`, `verify-model`, and `dynamic-report` after
  `--refresh dynamic-report`: RESULT ok.

BLOCKERS: none for this; the ports' runs wait for the single end-of-job run, as the owner decided again (2026-10-03).


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0196 (2026-10-08T03:26:17Z):

Continuation review: read the complete milestone plan and the preserved implementation record, including earlier incomplete verification and later completion claims. Reviewed the dependency and verification conventions in docs/TESTING.md. No previous audit text was changed.
Confirmed two implementation gaps in the current tree: the dedicated device-tree fixture node has no matching driver/run despite the explicit completion criterion, and the ACPI service drains EC query commands outside its _GLK-protected ec_access path. The actual EC transport is integrated in the service; physical laptop verification remains unperformed. Work and verification results are appended below as they occur.

Implemented continuation (2026-10-08):

- The dedicated device-tree fixture now exists as a development-only arm of the existing `acpi_fixture` driver: `src/user/drivers/core/src/acpi_fixture/tree.rs`, selected by the tree source; the manifest adds the `liber,tree-fixture` compatible. The arm refuses anything but `dt:/liber-fixture` with one GPIO connection and no wired interrupt, checks the connection is line 2/rising, subscribes, and acknowledges each delivered edge. READY precedes bounded controller calls, following the existing HID child's bind pattern.
- `qemu-run.sh` accepts `I2C_FIXTURE=tree` and invokes the existing `fdt_edit.py tree-fixture` editor with GPIO slot 0x16/line 2. It preserves the existing translated vhost-user backend path and refuses conflicting tree editors.
- `check-firmware-tree.sh` boots each port, checks the PCI companion join, waits for the fixture driver's readiness, raises two independent edges through the backend control socket and requires both acknowledgements. Its log oracle first proves it rejects every omitted observation and an explicit driver refusal. Gates `firmware-tree-aarch64`/`firmware-tree-riscv64` are registered in check.sh, the verification model and the release-required set. The existing acpi fixture oracle record names both gates. At the P02M0202 owner's request the two TCPCI port gates were registered in those same tables as well.
- Actual embedded-controller service integration was checked, not assumed from the library: ECDT and PNP0C09 resource discovery, firmware-held publication, EmbeddedControl operations, _REG, _GLK, GPE delivery and _Qxx all exist. Fixed `Service::ec_queries` to call `Firmware::ec_access` for QUERY/drain, applying the same bounded ACPI global lock as region accesses and releasing it before evaluating _Qxx. Previously queries bypassed _GLK. Fixed `start_ec` failure cleanup: partially acquired/mapped EC ports are explicitly unmapped and closed before namespace fallback; metadata is appended only after both mappings succeed. Previously a failed second grant stranded the first port, so fallback could not reacquire it. Map failure diagnostics now report the map error itself.

Fresh passed checks:
- `(cd src/user && cargo check --manifest-path drivers/core/Cargo.toml --bin acpi_fixture --features development)` (x86_64); matching check for `services/core/Cargo.toml --bin acpi_service` after both EC corrections.
- `cargo test --manifest-path src/{acpi,aml,fdt,platform,smbios}/Cargo.toml` run separately: 52/45/124/17/11 passed; `src/user/libs/acpi/model/Cargo.toml`: 28 passed. The AML suite includes the EC register model's bounded input wait, read/write/burst, query flood and vanished-controller cases.
- `bash src/tools/check-firmware-tree.sh --self-test`, `python3 src/harness/fdt_edit.py --self-test`, bash syntax and rustfmt/shfmt checks: PASS.
- `./check.sh --gate verify-model,firmware-fixtures,gate-oracles,source-hygiene`: model consistent (915 checks/1951 keys), fixtures and oracle registration passed; source-hygiene failed on the pre-existing console_uart module layout, subsequently moved as recorded under P02M0191. This combined command failed overall, not a blanket pass.

Unperformed / remaining: the new `firmware-tree-aarch64` and `firmware-tree-riscv64` runtime gates and final cross-builds are queued for the coordinated end-of-job verification. The previous HID-gate substitution is no longer counted as completion of the dedicated fixture item; the plan checkbox is reopened until these run. The EC's recorded laptop acceptance and other required real-hardware runs remain unavailable; host model tests and compilation do not constitute physical hardware verification. P02M0196 remains OPEN.

Follow-up verification: the standalone `./check.sh --gate source-hygiene` after the UART move also failed overall, now on existing early-reader pipelines in the Bluetooth/brightness gate scripts. The module-layout issue no longer appears; global hygiene remains pending the root implementer's final check. No new guest execution is claimed by this check.

Coordinated final checks at the frozen continuation source (2026-10-08), run by the root implementer: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (487 s; `.build/logs/end-of-job/continuation-build-all-final.log`) on x86_64, aarch64 and riscv64. `./check.sh --gate source-hygiene --gate verify-model --gate verify-model-tests` also PASS (205 s; `.build/logs/end-of-job/continuation-static-final.log`): source hygiene, model consistency, and all 157 model tests. This supersedes the earlier global-hygiene failures; the narrow diagnostic-script fixes are recorded by the root implementer. Runtime gates are reported separately below.

Final-runtime regression investigation (2026-10-08): the P02M0202 TCPCI gate stopped after Accept because the shared vhost I2C/GPIO backend could not publish a used element spanning two IOTLB pages. A host reproduction with both pages already mapped repeatedly requested the already-cached second page and never advanced the used index. The diagnostic guest confirms I2C used base `0x5808`, so slot 254 places the eight-byte element at `0x5FFC`, crossing into `0x6000`. The existing `Iotlb.translate` deliberately returns only a single mapped span; `Space.view` therefore retries that cross-page request forever, holding the fixture's one event loop. This is a required fixture repair shared with P02M0202; a focused regression and minimal separate aligned head/length stores are being added under this implementation record. No previous verification is relabelled as passed.

Implemented the confirmed harness correction in `src/harness/vhost-i2c-gpio.py::Vring.put_used`: store the head and written length through separate aligned four-byte translated spans, then publish the used index last as before. No generic memory mapper, queue layout, guest driver, or timing allowance changed. Reviewed available-ring two-byte entries and sixteen-byte aligned descriptors: they do not have this crossing under the existing layouts; the I2C indirect tables occupy their fixed control-page slots.

Regression evidence: `python3 src/harness/vhost-i2c-gpio.py --self-test` with the new two-page test and old implementation FAILED immediately (18 passed, one failed; `.build/logs/end-of-job/continuation-vhost-cross-page-before.log`). Both pages are mapped at noncontiguous QEMU virtual addresses, and any unnecessary miss fails immediately rather than hanging. After the fix the same command PASSED all 19 tests (`continuation-vhost-cross-page-after.log`), including the element's values and used-index publication ordering. Python syntax compilation and `git diff --check -- src/harness/vhost-i2c-gpio.py` passed. Final TCPCI and dedicated tree runtime checks remain pending; historical gate evidence is preserved and is not invalidated speculatively.

The root's post-fix `./check.sh --gate source-hygiene` PASSED (93 s; `.build/logs/end-of-job/continuation-hygiene-vhost.log`). P02M0202's owner then reran the full nontrace `./check.sh --gate typec-tcpci` on x86_64: PASS in 496 s (`.build/logs/end-of-job/continuation-typec-tcpci-x86-retry.log`). The previously failing lower-power transition passed, as did all functional/sleep cases and 200 response measurements (p99 6.543 ms, maximum 6.936 ms, zero timeouts). This is fresh runtime evidence for the shared backend repair; the first failed gate and its diagnostic trace remain preserved.

Final x86_64 regression: `./check.sh --gate acpi` PASSED (410 s; `.build/logs/end-of-job/continuation-acpi-x86.log`, detailed preserved guest logs in `.build/logs/acpi/`). Both private development boots used newly assembled images. The gate verified the namespace/resource classifications, permitted and refused operation-region access, GPIO fields and notifications, two CPU hot-plug GPE deliveries, ACPI power reads/updates and coalescing, service restart preserving 50 rows while withdrawing UID 1, and both native/firmware PCI hot-plug ownership outcomes. This verifies the service remains integrated on a machine without an EC; it does not substitute for the outstanding physical EC acceptance.

Dedicated tree runtime on riscv64: `./check.sh --gate firmware-tree-riscv64` PASSED in 600 s (`.build/logs/end-of-job/continuation-firmware-tree-riscv64.log`; guest, runner, backend and host logs in `.build/logs/firmware-tree-riscv64/`). Observed the GPIO function's PCI companion join, `/liber-fixture` with a scoped rising line 2 and no wired interrupt, both independently raised and acknowledged edges, all four backend control replies, and the completed `lsdev` listing/prompt. The host helper completed successfully. Only this gate's QEMU and console-driver processes were stopped after all observations to avoid waiting out the unused upper timeout; the unchanged gate oracle and cleanup then returned PASS. The ordinary guest's kernel and fixture output appeared in its serial guest log; no transcript merging was needed. The aarch64 dedicated gate is still pending its coordinated slot.

Dedicated tree runtime on aarch64: `./check.sh --gate firmware-tree-aarch64` PASSED in 425 s (`.build/logs/end-of-job/continuation-firmware-tree-aarch64.log`; guest, runner, backend and host logs in `.build/logs/firmware-tree-aarch64/`). As on riscv64, verified the GPIO companion, scoped line 2/rising/no wired interrupt, two independent host-raised and acknowledged events, all control replies and the completed `lsdev` listing/prompt. Only this run's verified QEMU/console processes were stopped after completion; the unmodified helper wait and gate oracle returned PASS. Both dedicated fixtures therefore satisfy the previously reopened plan item, now checked.

Continuation final state: the required dedicated device-tree integration is implemented and verified on both ports; the actual ACPI EC service integration has the _GLK query and failed-start cleanup corrections, with host/compiler and the full x86_64 ACPI regression evidence above. All own source changes are finished. P02M0196 remains OPEN because its required recorded embedded-controller/real-hardware acceptance has not been performed and suitable hardware was not available to this session. No placeholder implementation or synthetic result is substituted for that acceptance. Later independently found PowerService/TypeCService adoption work is owned by P02M0181/P02M0202 and does not change the fixture or EC evidence recorded here.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

Final environment availability check (2026-10-08T13:30:32Z): read-only inspection of `/sys/class/dmi/id/{sys_vendor,product_name}` reports `QEMU` / `Standard PC (i440FX + PIIX, 1996)`. `/sys/bus/acpi/devices` has no PNP0C09, PNP0CA0 or USBC000 nodes, `/sys/class/typec` is absent, and `/dev/ttyACM*` / `/dev/ttyUSB*` have no matches. This confirms no relevant local target is exposed by this session; no owner-provided remote target/access has been established either. This is an environment inventory, not a physical-hardware acceptance test. The milestone-specific remaining hardware/design requirements above stay open.
