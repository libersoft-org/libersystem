IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0190 (2026-09-28T12:32:38Z):

Status: IN PROGRESS - started after P02M0196a (the platform claim it requires), in the owner's agreed order.
This record is updated as the work proceeds; the final state is at its end.

## What was implemented (2026-09-28)

### The library's additions (`src/tpm`)
- `ops.rs`: `Tpm::properties(first, count)` and `Tpm::handles(first)` over `TPM2_GetCapability` - typed and bounded:
  at most 16 properties and 64 handles asked for, `moreData` followed at most 4 times and only while the range asked
  for is not covered, an answer listing more than was asked, a property outside the range or out of order, a handle
  of another range, another capability's answer or a `moreData` byte that is neither 0 nor 1 refused as
  `Malformed`, and a TPM that never stops saying more answered `Bounds`. `Tpm::identity()` (family, manufacturer,
  vendor string, firmware version from the fixed properties 0x100-0x10C) and `Tpm::hierarchy()` (`ownerAuthSet`
  from `TPM_PT_PERMANENT`, `shEnable` from `TPM_PT_STARTUP_CLEAR`; `Hierarchy::owner_usable`). `Tpm::flush_leftovers()`
  lists transient objects (0x80xxxxxx) and loaded sessions (0x02/0x03xxxxxx) and flushes each, answering how many.
  `Sealed::encode`/`decode` - a sealed object as bytes a caller keeps (version, PCR, the two halves by length),
  refusing any other shape. Constants for the command codes, handle ranges, properties and the two memory warnings.
  Still no passthrough.
- Host tests: `src/tpm/src/tests.rs` against scripted TPMs (every hostile GetCapability answer above, the leftover
  flush, the identity and hierarchy words, the sealed-object shape); `src/tpm/tests/swtpm.rs` against `swtpm`:
  a killed predecessor's leftovers - transient objects and sessions created until the TPM itself refuses the next -
  make an unseal fail with `TPM_RC_OBJECT_MEMORY` and after `flush_leftovers` the same unseal returns the secret;
  an owner authorization set with `HierarchyChangeAuth`, and the owner hierarchy disabled with `HierarchyControl`
  under the platform hierarchy's empty authorization, are each reported by `hierarchy()` while random, PCR read and
  extend keep working. (Those two commands are built in the test, not offered by the crate.)

### The driver and its provider
- `src/user/drivers/core/src/tpm_driver.rs` (`tpm_driver`): bound through the platform claim, its one resource the
  locality-0 page mapped from the claim's `DeviceMemory`; the interface read from the interface-identifier register
  (CRB or FIFO); at every start Startup(CLEAR), SelfTest, `flush_leftovers`, the hierarchy and identity read and one
  boot-log line (interface, manufacturer, firmware version, owner hierarchy, leftovers flushed), then ONE `tpm`
  provider; any failure before it a failed bind naming the step. It serves `tpm-device` one operation at a time,
  whole, in the order received (single-threaded; each request runs to its end before the next is read), mapping the
  library's errors to the contract's outcomes; a planned stop certifies quiet (nothing in flight, no DMA).
- The `tpm` provider kind appended in all three closed mappings: `provider-kind` in `src/idl/device.lsidl` (24),
  `ProviderKindName::Tpm` in `src/tools/system-manifest`, `driver_protocol::provider::TPM`, and DeviceManager's wire
  mapping both ways.
- Manifest: `tpm_driver`, `lifecycle = "function"`, `dma = "none"`, two platform rules - `table = "TPM2"` and
  `compatible = "tcg,tpm-tis-mmio"` (a rule names one id) - one `tpm` provider, one consumer.

### The service, the contract and the authority
- `src/idl/tpm.lsidl` (`liber:tpm@1`, new package `tpm-proto`, wired into `gen.sh`, the aggregate `proto` and the
  manifest's sources and libraries): `tpm` (info, random, pcr-read, pcr-extend, seal, unseal, quote), `grant`
  (`tpm`, `tpm-measure`, `tpm-seal`), `tpm-admin.mint(kind, component, owner)`, and the provider contract
  `tpm-device`. Every answer carries an `outcome` - `done`, `not-granted`, `pcr-not-allowed`, `bounds`, `busy`,
  `policy-refused`, `owner-hierarchy-unavailable`, `other-component`, `interrupted`, `unavailable`, `tpm` (with the
  TPM's code) and `fault` - since an IDL `result` carries the base error only. `fault` is the one outcome the plan's
  list does not name: a transport or response the library could not read has no TPM response code to report.
- `src/user/services/logic/src/tpm.rs` (`service_logic::tpm`, host-tested): which operations each grant carries,
  `admit` (grant, then PCR - extend 16 and 23 only, everything else 0-23 - then the bounds, then the owner
  hierarchy), the component tag (SHA-256 of the component's name, first 32 bytes of every sealed secret; 96 bytes
  left for the application), `untagged` (`other-component`), and the `Queue`: one request at the driver, one call
  per connection (`busy`), sixteen waiting (`busy`), `lost()` answering the call in flight `interrupted` - never
  replayed - and those waiting `unavailable`, `unavailable` while no provider is present, a closed connection's
  waiting call dropped. `service_logic::sha256` - the loader's algorithm in this crate, because the crate is also the
  staged library `service-util`, which may import nothing but the runtime; a test holds it to `bootproto`'s.
- `src/user/services/core/src/tpm_service.rs` (`tpm_service`): a CATALOGUE role for `tpm` alone (presence optional)
  and one ADMIN root; no public root, no claim, no storage; `restart = "transparent"`,
  `state_class = "reconstructible"`. A publication is described within a bound and served; a second is logged and
  not used. `info` is answered from the description; every other call is admitted by `service_logic::tpm`, queued,
  and sent to the driver one at a time with a seal's secret tagged; an unseal's answer is untagged for the caller or
  refused `other-component`. The driver's withdrawal or a closed provider channel is the queue's `lost()`. A closed
  admin root is given up, not waited on.
- `src/idl/security.lsidl`: `tpm`, `tpm-measure`, `tpm-seal` appended after `input-gamepad`. PermissionManager:
  the vocabulary (49), the tags `TPM`/`TPMMEASURE`/`TPMSEAL`, one `grant_for_task` arm (the admin root resolved over
  the broker as `TPMADMIN`, the task duplicated with wait and transfer, `mint` with the grant and the component, the
  minted connection narrowed to the grant rights), and the rows: `tpm` holds all three and `volumes`, `tpmprobe`
  holds `tpm`, `tpm-seal` and `volumes`. ServiceManager's three tables (`CAP_TPM_ADMIN` for `permission_manager`, the
  service serving it, the root it resolves to) and `plan_relaunchable`. The kernel's permission audit string gained
  the three `=deny`s.
- `src/user/libs/clients/tpm-client` + `tpm-client-provider`: `TpmClient::new(tpm, measure, seal)` sends each call
  on the connection whose grant carries it and answers `not-granted` itself for a call none carries; every method
  `#[inline(always)]` so a dynamic program imports only the trampolines, which the provider declares for the three
  architectures.

### The tool, the probe and the gate
- `src/user/apps/tools/src/tpm.rs` (`tpm`, shipping, dynamic): `info`, `random N`, `pcr N`, `extend N TEXT`
  (SHA-256 of TEXT), `seal N TEXT FILE`, `unseal FILE`, `quote N NONCE FILE` - printing what the TPM answered (the
  quote's parts in hex and in FILE) and every refusal by name. Shell word and synopsis added; dynamic wave 3.
- `src/user/apps/tools/src/tpmprobe.rs` (`tpmprobe`, development-only): `extend23` (refused `not-granted` by the
  library in-process and by TpmService on the observation connection, PCR 23 unchanged), `other FILE`
  (`other-component`), `own`, and `hold` (one connection held, each change of answer printed).
- `lsdev` verbs accept a platform device's stable identity as well as a row number (`lsdev --disable table:TPM2#0`),
  resolved through the device service - the scenario cannot know a row number in advance.
- `src/harness/scenarios/tpm-tool.toml` and `src/tools/check-tpm-tool.sh` (gate `qemu-tpm-tool`, registered in
  `check.sh`, the verify-model catalog as a guest-booting gate, and `release-required.toml`): the scenario in the
  plan's order behind `swtpm` over CRB, then over TIS; the host checks the two draws differ, the PCR 16 chain from the
  typed text, PCR 8 unchanged, and OpenSSL's verification of the quote (and its refusal of a one-bit-different
  attestation), over this nonce and the digest of PCR 16 as read.
- `src/harness/qemu-run.sh`: `tpm-tis-device` on the aarch64 and riscv64 `virt` machines when a run asks for TIS.
- `docs/TPM.md`: the page for third-party developers - the grants and calls, the bounds and costs, the PCR sets and
  why, PCRs shared by the machine, S3 resetting 16 and 23, what a sealed object opens for, the owner hierarchy limit,
  the uncertified quote key, which TPMs are driven, and what this TPM is not.

### Not in this milestone's hands
- ACROSS A SLEEP (`SUSPEND`/`RESUME`, `TPM2_Shutdown(TPM_SU_STATE)`): the plan gives it to whichever of this
  milestone and P02M0197 lands second - P02M0197, later in the agreed order.
- The `MSFT0101` node resolving to the table's row: P02M0196b's namespace.

## Found and fixed on the way to a passing gate (2026-09-28)
- The scenario runner had no key for `#`, which a platform identity carries (`table:TPM2#0`): `src/harness/lab.py`
  `KEYMAP` gains `'#': 'shift-3'` (the guest's US map gives `#` for shift-3).
- The tool and the probe read their TPM grants as the first messages after the volume bundle, which PermissionManager
  ends with the bootstrap terminator - so `tpm info` answered `not-granted` with all three grants minted. The grants
  are handed in the vocabulary's order (volumes first), and `tools::VolumeSet::recv_grant` reads a grant after the
  bundle by its exact tag, passing over the terminator. (The same shape meets `touch`, which reads `TIME` with
  `recv_tagged` after the bundle and so gets the terminator instead; not changed here - outside this milestone.)
- The shell routed `lsdev` only bare or with `json`/`json-min`, so the operator's verbs (`--disable`, `--enable`,
  `--retry`, `--select`, `--incident`) could not be typed at a prompt at all: `lsdev` takes the `Rest` shape, a
  superset of the old one.
- The two tables that enumerate the drivers' DMA policies (`system-manifest`'s test and the kernel's
  `the_kernel_registry_is_the_manifest_migration_table`) classify `tpm_driver` as `none` - the first platform
  row's driver, and a `none` row the registry had not had since `sdhci` moved to ADMA2.

## Verification (2026-09-28)
- `./check.sh --gate qemu-tpm-tool`: PASS (exit 0, 559 s) - the scenario's 61 steps behind `swtpm`'s CRB front-end,
  then again behind TIS, each followed by the host's checks: two draws differ, PCR 16 is SHA-256(old || SHA-256
  of the typed text), PCR 8 unchanged after its refused extend, OpenSSL verifies the quote by the point it carries
  over an attestation carrying `liber-nonce` and the digest of PCR 16 as read, and refuses it over an attestation
  one bit different; the driver's online line names CRB, then FIFO, and appears again after the enable. Three
  earlier runs of the same gate failed - on the `#` key, on the grants read after the bundle, and on the shell's
  `lsdev` routing - each fixed above; the gate is what found them.
- Host: `cargo test --manifest-path src/tpm/Cargo.toml` 30 + 3 (`swtpm`) pass; `src/user/services/logic` 681 pass
  (the TPM module's 9 among them); `src/user/libs/driver/protocol` 76; `src/user/libs/driver/binding` 89;
  `src/tools/system-manifest` 27 (after the classification above; it failed on `tpm_driver` before it).
- WATCHED FAILING: `EXTENDABLE` changed from [16, 23] to [8, 23] makes three of `service_logic::tpm`'s tests fail
  (`every_grant_carries_exactly_its_operations`, `only_pcrs_16_and_23_...`, `a_provisioned_owner_hierarchy_...`);
  restored, 9 pass.
- Kernel, x86_64: `./build.sh --arch x86_64` ok; `TEST_SELECTION=kernel.dma_policy.the_kernel_registry_is_the_manifest_migration_table,kernel.applications.powerbox_grants_a_picked_file_to_a_component ./test.sh --arch x86_64` - 2 passed.
- `./gen.sh --check` exit 0; `./check.sh --gate source-hygiene` clean; `python3 src/tools/foreign-audit-link.py
  --check` - the recorded inventory reproduces.
- NOT RUN (the job's cross-architecture work runs once, at its end): the aarch64 and riscv64 builds, the
  scenario's `tpm-tis-device` run on both, and `./check.sh --refresh dynamic-report` for the new library, service
  and tool.

## Open items
- The ports' builds and `tpm-tis-device` runs and the dynamic report, above.
- ACROSS A SLEEP (`SUSPEND`/`RESUME`, `TPM2_Shutdown(TPM_SU_STATE)`) - P02M0197's, which lands second.
- The `MSFT0101` node's reconciliation onto the table's row - P02M0196b's namespace.

Status: IMPLEMENTED; verified on x86_64 and the host; the ports' runs remain.

## After P02M0196b and the static gates (2026-10-01)

- THE PROBE SHIPPED: `tpmprobe` was a DYNAMIC program of the tools crate with `development = true`, and nothing kept it
  out of a shipping image - `build-shared.sh` stages every dynamic volume row, and the manifest export is the same in
  both configurations - against this milestone's "not staged in the shipping image". `development-gate` found it. It
  is now a STATIC probe in the services crate behind `required-features = ["development"]`, as every other development
  probe is: `src/user/services/core/src/tpmprobe.rs` (moved from `src/user/apps/tools/src/tpmprobe.rs`, which is gone),
  its `[[bin]]` in `src/user/services/core/Cargo.toml` and out of `src/user/apps/tools/Cargo.toml`, its manifest row
  `owner = "services"`, static, no providers; `tpm-client`, `tpm-client-provider` (named by `extern crate`, since only
  its provider half is linked) and `tpm-proto` with `channel-client-impl` linked statically. The volume bundle, the
  grants after it and the bounded file read are written there. (`admin_fixture`, another milestone's, lacked the same
  `required-features` and has it now.)
- THE ROW IS ONE DEVICE NOW: P02M0196b's namespace merges `MSFT0101` into the `TPM2` table's row - the open item above
  ("the `MSFT0101` node's reconciliation onto the table's row") - so `lsdev` shows the row's ids as
  `[{kind=table, text=TPM2}, {kind=identity, text=acpi:\_SB_.TPM_}, {kind=hid, text=MSFT0101}]` with its one resource
  still the table's 4 KiB page. The gate's run of 2026-10-01 failed at step 3 on the old expectation
  (`identity=table:TPM2#0, ids=[{kind=table, text=TPM2}], resources=[...]` did not appear within 30 s; the serial log
  showed the merged row). `src/harness/scenarios/tpm-tool.toml` now expects the merged row - a TOML literal string, so
  the path's backslash is the line's own - for both front-ends (QEMU's `_CRS` for TIS is 0x5000 and for CRB 0x1000 at
  the same base, merged either way, the row keeping the table's page).
- AND THE PATH IS THE FRONT-END'S: with the merged expectation the CRB run passed and the TIS run failed - QEMU places
  the TIS front-end's `MSFT0101` device under the LPC bridge (`\_SB_.PCI0.SF8_.TPM_`, read in that run's serial log)
  and the CRB's at `\_SB_.TPM_`. The scenario reads the row in two parts on either side of the path (an `expect`
  moves past its match, so the second part starts where the first ended - a first try that began the second part
  with the path's dot failed on exactly that).
- `LIBER_DEVELOPMENT=1 ./check.sh --gate qemu-tpm-tool` -> PASS (519 s, 2026-10-01): "behind CRB and behind TIS, the
  TPM2 table's row is one 4 KiB page with no interrupt, the driver binds it and restarts under a held connection,
  TpmService serves the tool and refuses the probe by name, and the quote verifies"; on the host, two draws differ,
  PCR 16 is the chain of the typed text, PCR 8 did not move, and OpenSSL verifies the quote over this nonce and PCR 16.
- `development-gate` -> PASS (47 development-only programs absent from the shipping configuration, `tpmprobe` among
  them now).

## The first cross-build after the probe's move (2026-10-01)

- `./build.sh --arch aarch64` FAILED: "build-shared: Cargo image graph has no unique archive for
  user/libs/clients/tpm-client-provider". The services crate's dependency on `tpm-client-provider` (for the static
  `tpmprobe`) did not forward the image's `shared-image` feature, so the graph build compiled the provider twice -
  once with `shared-image` (the staged provider), once without (the services crate's) - and two archives are no unique
  one. x86_64 had not seen it: its graph was cached from before the move, and THE GRAPH'S CACHE KEY DID NOT COVER THE
  SERVICES CRATE, whose seed is built in that same graph - a dependency it gained changed which archives the graph
  holds without invalidating it (the first aarch64 retry hit the same stale graph). FIXED: the services crate's
  `shared-image` forwards to `tpm-client`, `tpm-client-provider` and `tpm-proto` (`services/core/Cargo.toml`), and the
  graph key digests the services crate's sources (`src/tools/build-shared.sh`, `image_graph_source_digest`). The
  rebuild then got past the graph.
- (2026-10-01) `./build.sh --arch aarch64` and `--arch riscv64` build whole with this milestone's code; the ports' guest runs it names are the owner's long run.
