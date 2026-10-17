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


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0190 (2026-10-08T03:26:17Z):

Continuation review: read the complete milestone plan and the preserved implementation record, including earlier incomplete verification and later completion claims. Reviewed the dependency and verification conventions in docs/TESTING.md. No previous audit text was changed.
Review in progress: completion labels are being checked against the current code and available verification artifacts; no new passing test results are claimed at this point.

Continuation result (2026-10-08): no missing implementation was identified in the reviewed TPM path. Checked the locality-page platform binding, startup/self-test/leftover recovery sequence, fresh provider describe, sequential provider dispatch, service grant/queue/component-tag decisions, the manifest's optional TPM catalogue/admin-only root, and the P02M0197 sleep exchange (Shutdown(STATE), firmware-start detection and bounded fallback). The earlier audit's stale open cross-target items are closed by the retained 2026-10-07 artifacts: `.build/logs/end-of-job/tpm-aarch64/gate.log` and `tpm-riscv64/gate.log` each contain the scenario PASS and the final `qemu-tpm-tool: PASS` with host quote verification. These are prior runs, not new runs of this continuation.

Fresh verification: `cargo test --manifest-path src/tpm/Cargo.toml` PASS (31 unit tests and 3 against swtpm); `cargo test --manifest-path src/user/services/logic/Cargo.toml tpm::` PASS (9). No TPM production code was changed, so the costly CRB/TIS/port scenarios were not repeated here. The plan remains COMPLETE; no new incomplete TPM requirement found.

Coordinated final checks at the frozen continuation source (2026-10-08), run by the root implementer: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (487 s; `.build/logs/end-of-job/continuation-build-all-final.log`) on x86_64, aarch64 and riscv64. `./check.sh --gate source-hygiene --gate verify-model --gate verify-model-tests` also PASS (205 s; `.build/logs/end-of-job/continuation-static-final.log`): source hygiene, model consistency, and all 157 model tests. This supersedes the earlier global-hygiene failures; the narrow diagnostic-script fixes are recorded by the root implementer. Runtime gates are reported separately below.

Continuation final state for this milestone: no missing TPM implementation requirement was found after source and prior guest-evidence review. Fresh TPM/library/service tests and all three compiled targets passed; the preserved x86_64 and port guest results remain the required runtime evidence. No new TPM guest rerun was performed or claimed. The plan remains COMPLETE.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

AUDITOR'S REVIEW ON P02M0190 (2026-10-15T04:29:29Z):

**Rating: 8/10.** The TPM is a usable, typed, split-authority resource, as the milestone asks. The parts and what was checked:
- **The library:** `GetCapability` is bounded and checks for hostile answers, and `flush_leftovers` and the hierarchy state are in place.
- **The driver:** it follows the required start order (Startup, SelfTest, the leftovers flushed, the hierarchy and identity read, one line, one provider), runs one operation at a time, and `describe` reads afresh.
- **The platform row:** one locality-0 page, with no interrupt and no DMA.
- **The `tpm` provider kind and the three capabilities:** each was appended, not inserted.
- **TpmService:** its decisions live in `service_logic::tpm`. Its queue answers `busy` past one call per connection or sixteen waiting, `interrupted` (never replayed) for the in-flight call, and `unavailable` for waiting calls.
- **The grant split:** enforced in the service.
- **The component tag:** prepended and checked by the service.
- **PermissionManager:** a per-launch, task-bound mint.
- **The rest:** `tpm-client`, the tool, the probe and the gate.

Two defects remain. Both are in how TpmService answers, not in what it enforces:
- With no provider present, `seal`, `unseal` and `quote` answer `owner-hierarchy-unavailable` instead of `unavailable`. That outcome is the one the contract and the documentation reserve for a provisioned TPM whose only remedy is a TPM clear.
- A secret, nonce or sealed object past its bound closes the grant's connection instead of answering `bounds`.

## Findings

1. **Medium, Defect - With no TPM provider, `seal`, `unseal` and `quote` answer `owner-hierarchy-unavailable` instead of the contract's `unavailable`.**
   - **The code.** `owner_usable()` is false whenever no provider is held (`/data/yellow/libersystem/src/user/services/core/src/tpm_service.rs:242-244`: `self.provider.as_ref().is_some_and(...)`). `client()` runs `tl::admit(grant, &op, self.owner_usable())` before the queue (:399-404). `admit` refuses an operation that needs the owner with `OwnerHierarchyUnavailable` (`/data/yellow/libersystem/src/user/services/logic/src/tpm.rs:123-125`). The queue's `Refusal::Unavailable` for a missing provider (tpm.rs:212-214) is therefore reachable only by `random`, `pcr-read` and `pcr-extend`.
   - **What it contradicts:**
     - the milestone: "calls made while no provider is present, answer `unavailable`" (`docs/todo/P02M0190.md:146-147`);
     - the contract: `unavailable` = "No TPM is being served: none was found, or its driver is restarting" (`/data/yellow/libersystem/src/idl/tpm.lsidl:41-42`);
     - `docs/TPM.md:96-97`.
   - **Why it matters.** The same docs say `owner-hierarchy-unavailable` means a provisioned owner hierarchy whose remedy is a TPM clear that makes every sealed object unopenable (`docs/TPM.md:79-83`). That is what a program is told:
     - on every machine without a TPM;
     - before the driver's first publication is adopted;
     - throughout every driver restart.
     
     It also defeats a real consumer. `hibernation_service`'s restore retries `unseal` while the answer is `Unavailable` (`/data/yellow/libersystem/src/user/services/core/src/hibernation_service.rs:300-310`). It receives `OwnerHierarchyUnavailable` instead, stops waiting at once, and fails with "its key did not unseal" (:315) whenever TpmService has not yet adopted the TPM.
   - **Why nothing caught it.** The host tests cover `Unavailable` only with `Op::Info` on the bare queue (`tpm/tests.rs:100, 132`). The gate's driver-down window exercises only `random` (`src/harness/scenarios/tpm-tool.toml:192-224`).
   - **Fix.** Answer `unavailable` before the owner check when no provider is held. One way is to treat "no provider" as owner-usable in `admit`, so that the queue answers.

2. **Low, Defect - A secret, nonce, sealed object or digest past its bound closes the application's grant connection instead of answering `bounds`.**
   - **The code path.** The contract's bounds are IDL `@bound`s. The generated server dispatch returns `None` for a list past them (`/data/yellow/libersystem/src/user/libs/protocol/tpm-proto/src/generated/liber/tpm/v1.rs`): digest ≤ 32 at :823, secret ≤ 96 at :870, sealed ≤ 1024 at :916, nonce ≤ 64 at :963. On that `None`, `client()` returns false (`tpm_service.rs:379`) and `serve()` drops the whole grant (:499-506, "A REQUEST THIS CONNECTION CANNOT CARRY CLOSES IT").
   - **Nothing stops an over-long argument before it reaches the server.** The generated client checks only `len > u16::MAX` (v1.rs:1235-1237), and `tpm-client` adds no check (`/data/yellow/libersystem/src/user/libs/clients/tpm-client/src/lib.rs:105-128`). As a result, the `MAX_SECRET`/`MAX_SEALED`/`MAX_NONCE` checks in `admit` (logic `tpm.rs:107, 112, 118`) can never fire for an over-long input.
   - **Why it matters.**
     - The contract documents `bounds` as "A count, secret or nonce past its bound" (`tpm.lsidl:28-29`).
     - The tool is required to print "every refusal by its name" (`P02M0190.md:209-210`). Yet `tpm seal 16 <a 97-byte text> FILE`, `tpm quote 16 <a 65-byte nonce> FILE` and `tpm unseal <a 1025-4096-byte file>` print "tpm: TpmService did not answer" (`/data/yellow/libersystem/src/user/apps/tools/src/tpm.rs:149-151`).
     - A third-party program that passes one over-long argument loses that grant's connection until its next launch.
   - The bound itself is enforced, so this is not a security issue.
   - **Fix.** Either of:
     - reject past-bound lengths in `tpm-client` with `bounds` (and have the tool check them);
     - raise the wire bounds above the semantic ones, so that `admit` answers `bounds` itself.

## Verification performed

- **Static review only.** Nothing was built, run or tested for this review.
- **Read:**
  - `tpm_service.rs` (whole), `service_logic::tpm` and its tests;
  - `tpm.lsidl`;
  - `tpm_driver.rs`: start order, `describe`, serving loop;
  - `src/tpm/src/ops.rs`: `properties`, `handles`, `identity`, `hierarchy`, `flush_leftovers`;
  - `tpm-client`, and the `tpm` tool;
  - the manifest rows for `tpm_service` (transparent, reconstructible, optional `tpm` catalogue role, ADMIN root only), `tpm_driver` (two match rules, `dma = "none"`, one `tpm` provider), the dynamic `tpm` tool and the static development-only `tpmprobe`;
  - PermissionManager's TPM arm: the task duplicated `WAIT|TRANSFER`, `mint`, and the connection narrowed;
  - the gate `check-tpm-tool.sh` and the outline of the scenario.
- **Independent cross-check.** A separate read-only pass covered the remaining items, and I re-checked each item it reported against the code above:
  - the platform rows: one page, with no IRQ;
  - the appended `tpm` kind in all three closed mappings;
  - the appended capabilities;
  - `TPMADMIN` in ServiceManager's three rows.
- **Both findings were confirmed in the code.** Finding 1 through the `owner_usable` → `admit` → `submit` order, and the hibernation consumer's wait loop. Finding 2 through the generated decode and the `serve()` drop path.
- **Not counted against this milestone.** "Nothing else holds any of them" no longer holds: P02M0197 later gave `hibernation_service` a ServiceManager-minted `TPMSEAL` role (manifest `[[services]] hibernation_service`). That is P02M0197's extension, not a defect here.

IMPLEMENTER'S RESPONSE ON P02M0190 (2026-10-17T01:44:22Z):

Both findings were verified and accepted.

**Finding 1 - with no TPM provider, `seal`/`unseal`/`quote` answer `owner-hierarchy-unavailable` instead of `unavailable` (Medium): ACCEPTED.**
- `src/user/services/logic/src/tpm.rs`: the owner hierarchy's state is now `Owner::{Unknown, Usable, Unusable}`, and `admit(.., owner)` refuses on that ground only for `Unusable`.
- `src/user/services/core/src/tpm_service.rs`: `owner()` answers `Unknown` when no provider is served; the sealing `info` maps to `Usable`.
- New host test `tpm/tests.rs::with_no_tpm_served_a_call_that_needs_the_owner_is_unavailable_and_not_told_to_clear_the_tpm`, watched failing; the existing tests now use `Owner::`.
- `src/harness/scenarios/tpm-tool.toml` seals while the driver is down and expects `tpm: seal refused: unavailable`.

**Finding 2 - an over-bound secret, nonce, sealed object or digest closes the grant instead of answering `bounds` (Low): ACCEPTED, the client half.**
- `src/user/libs/clients/tpm-client/src/lib.rs` checks `DIGEST_LEN`, `MAX_SECRET`, `MAX_SEALED` and `MAX_NONCE` before sending, and answers `Outcome::Bounds`.
- `tpm-tool.toml` seals a 97-byte secret and expects `tpm: seal refused: bounds`.
- An over-bound request sent raw on the wire still closes the connection, as any request the generated dispatch cannot decode does. That is the transport's rule for every interface, and changing it for one would be a protocol change outside this milestone.
- Both scenario steps run in the `qemu-tpm-tool` gate.

The milestone records both under "Corrections of 2026-10-15".

**Verification:** `qemu-tpm-tool` passed on x86_64, both steps among its runs, and so did `host-tests`, with the TPM logic and client tests.
