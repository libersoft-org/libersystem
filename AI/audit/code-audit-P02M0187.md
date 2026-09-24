IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0187 (2026-09-23T07:44:22Z):

(The implementation began at the time in the title - the first files of this milestone carry it; this record was
written at 2026-09-23T08:21:41Z, once the first pass was in the tree, and is updated as the work lands.)

### A blocker found and how it was resolved (for the owner to confirm)

The combined catalogue budget from P02M0180 counted two client slots for EVERY catalogue-minting role ("a
restart mints the replacement before the manager has seen the old channel close"). With this milestone's
`CATALOGUE` role the manifest has 16 such roles, which that count puts at 33 of the 32 slots - so the plan's own
instruction ("reuse the shared finite 32-client/subscriber bounds and combined manifest-budget check") could not
be met as the check stood. ServiceManager relaunches ONLY `restart = "transparent"` services - after a crash and
on an operator's start alike (`restartable` and `start_stopped_service`) - so a role of an `escalate` service is
one channel for the life of the boot. `system-manifest` now counts two slots for roles of transparent services
and one for the rest; `MAX_CATALOGUE_CLIENTS` stays 32. With both new services (both `escalate`) the demand is
25 of 32. The test `the_manifests_catalogue_connections_fit_the_catalogues_client_table` now checks the bound and
one past it for both restart classes. DeviceManager's tables are unchanged.

### What was implemented

**Interfaces and authority.**
- `src/idl/import.lsidl` (`liber:import@1`, crate `import-proto`): device, storage, object and parent identities
  scoped to the publication, attachment epoch, PTP session, content epoch, service incarnation and the CLIENT
  CONTEXT they were issued to; objects also carry the revision (FNV-1a) of their ObjectInfo as fetched.
  `import-limits`, `import-device`, `import-storage`, `import-time`, `import-object` (format and whether the
  standard defines it, optional size, capture time and parent, filename <= 255, interpretation complete/partial,
  the original dataset <= 4096 as opaque bytes), `import-entry` (object | unrepresentable), `import-page` (<= 2
  entries), `import-enumeration` (opened cursor grant with count | over-limit count), transfer state/cause/status,
  `import-chunk` and `import-read` (chunk | end). Interfaces `media-import` {limits, devices, storages,
  open-enumeration, open-read}, `import-cursor` {next, close}, `import-transfer` {read, status, cancel}. No
  command, catalogue, backend, writer, delete or capture operation. The header states the consumer obligation:
  publish only after `complete`, commit a `volume.open-writer` only then and abort otherwise, within the writer's
  own cap.
- `src/idl/ptp-transport.lsidl` (`liber:ptp-transport@1`, crate `ptp-transport-proto`): attach(version), one
  command container <= 32 bytes, pull <= 4096 bytes with `again` for none, an event stream of containers <= 32
  bytes or an overflow notice, cancel and reset (a new attachment).
- Provider kind `ptp-transport = 19` in `device.lsidl`, `driver-protocol`, `system-manifest` and DeviceManager's
  two conversions; `security.lsidl` capability `media-import`; rt `CAP_MEDIA_IMPORT` (`IMPORT`); ServiceManager
  broker `cap_grants`/`service_of_cap`/`serve_resolve`; `gen.sh` packages, externals and aggregate; `proto`
  facade features, dependencies and re-exports; manifest `sources` and `libraries` rows.

**The production leaves** (`src/user/services/logic`, no `rt` or IPC dependency):
- `ptp.rs` (+ `ptp/tests.rs`, 10 tests): container constants and the read-only operation allowlist; command
  encoding (<= 5 parameters, <= 32 bytes); `Inbound`, the incremental bulk-IN parser of one transaction -
  fragmented headers, coalesced data and response, checks of transaction ID, operation, container type and
  length, a data container over the transaction's limit refused FROM ITS HEADER (`TooLarge`), the four-gigabyte
  length refused (`Unrepresentable`), a missing, second or unexpected data phase and anything after the final
  response refused (`Sequence`); event containers; datasets read fallibly (`Reader` with checked arrays and
  UCS-2 strings, `time` for PTP DateTime strings, `device_info` with `usable()`, `ids` bounded, `storage_info`,
  `object_info` read as far as it can be with a partial flag); `format_known`; `revision`.
- `media_import.rs` (+ `media_import/tests.rs`, 13 tests): every limit (8 providers, 16 clients, 8 cursors, 8
  transfers, one of each per client and per device, 4 MB budget with at most 2 MB of snapshots, 65 536 handles,
  32 storages, 8192/8192/4096 dataset caps, 2 records and 8192 bytes a page, 4096 a record and a chunk, 60 s
  cursor idle, 30 s transfer idle, 2 s recovery); `Budget`; `admit`; `Epoch`, `scoped`, `Named` and `resolve`
  (another publication is not found, another context denied, anything older stale); `change` (events that move
  the content epoch, lose the session, or cancel the transaction); `Session` transaction IDs; `Snapshot` (the
  count checked against the container's own length and the bound before the IDs are charged and reserved);
  `Cursor` and `fits`; `Transfer` (opening, reading, complete only with every byte, one data container of exactly
  the expected length and the matching success - a zero-byte object needing the response too - and every other
  ending partial with its count and cause); `Link`, the per-device phase machine (attach with a new session ID,
  open, survey, one transaction at a time, cancel, reset that stales everything, unavailable, the 2 s deadline).

**The service** `src/user/services/core/src/media_import_service.rs`:
- Roles in declared order: `SERVE`, `CATALOGUE` (`kinds = ["ptp-transport"]`, optional); starts with no device.
- Every transport request is sent without blocking (the generated client encodes into a capture transport), one
  outstanding per device, answered in the loop, with a 2 s answer deadline. The survey after attach or reset:
  the event stream, OpenSession as transaction 0 under a session ID the device has not had, GetDeviceInfo (a
  device without the whole subset is listed and not usable), GetStorageIDs, GetStorageInfo for each. A reported
  change or an event overflow moves the content epoch, ends a transfer in progress `stale` and cancels its
  transaction, and re-reads the storages when the device is free; a device reset or cancelled transaction event
  resets the device.
- Client operations: `limits`, `devices` (with `busy`) and `storages` answered at once; `open-enumeration`,
  `next`, `open-read` and `read` wait for the device - one transaction at a time, `again` for a device that is
  completing another, one bounded pending reply per device. Enumeration takes the whole snapshot in one
  GetObjectHandles, pulled 4096 bytes at a time through `Inbound` into `Snapshot`, answers `over-limit` with the
  count for a container past 65 536 handles and cancels the transaction; a page is at most two serialized
  GetObjectInfo reads, each dataset charged as declared, and records that do not fit the record budget - or whose
  dataset is past 4096 bytes - are `unrepresentable` rather than truncated; a missing object is `stale` and moves
  the epoch. A read revalidates the object's metadata first (GetObjectInfo; a different revision is `stale`),
  charges the chunk, and runs GetObject as its own transaction, pulled only when a read is waiting.
- Endings and recovery: a client cancel, a closed transfer channel, a 30 s idle transfer, an over-limit list or
  an oversized dataset CANCELS the transaction; a malformed stream, an uncertain command send, a failed pull or
  cancel, or a transaction the device stopped moving on for 30 s RESETS the device; recovery that has not
  finished in 2 s, or a reset that fails, leaves it unavailable. Withdrawal ends every transfer on the device
  `removed` with its count and closes its cursors.
- Buffers sized for the largest frames with envelopes (a pull answer, a page, the 32-storage list),
  compile-time asserted; per-device buffers, pages, datasets, snapshots and chunks all charged to the budget.

**Deployment and grants.** Manifest program and service `media_import_service` (`restart = "escalate"`,
`state_class = "ephemeral"`, dependencies `log_service`, `device_manager`, `process_service`); program
`import_probe` (shipping, in the test image). PermissionManager: `Capability::MediaImport` appended to
`VOCABULARY` (41), tag `IMPORT`, stored client `media_import` resolved by name through the broker, a fresh
connection per grant; policy row `import_probe` -> [`storage`, `media-import`]. DECISION: the root is reached
through the broker rather than a bootstrap role, as for every other destination service. Kernel summaries:
`later_denials!` ends in ` media-import=deny`.

**Tests.**
- `src/user/services/core/src/import_probe.rs`: modes `browse`, `stale`, `import`, `removal`, printing
  `import-probe: PASS <mode>` or `FAIL`, and the `paging`/`first chunk` markers the harness acts on.
- `src/kernel/test_suites/services.rs`, module `import_rig` and test
  `kernel.services.media_import_service_reads_snapshots_and_whole_objects_and_nothing_else`: the harness is the
  catalogue and a PTP responder served through the generated `ptp_transport::dispatch` (the event stream by hand):
  one consumer, 40 000 handles and a 65 537-handle storage generated as they are pulled, exact ObjectInfo, a
  12 kB photo and a zero-byte object, recording every operation; a real StorageService volume
  (`StorageHarness::start_empty`) is the destination. The steps: exact pages, the over-limit storage, another
  context's identity denied, an unknown opcode ending the connection; a reported change staling the cursor and
  its objects; the photo and the empty object committed and read back; the camera withdrawn after one chunk
  with the partial count and the destination unchanged; only the read-only subset ever reached the responder.
  The kernel crate gained `ptp-transport-proto`.
- `src/kernel/tests.rs` `run_permission_resolved_grant_scenario` (it replaces the spool-only scenario) and
  `kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone`: both roots resolved
  by name once, a fresh connection per launch for two `spool_probe` and two `import_probe` launches, neither
  probe receiving the other's authority, `date` receiving neither.
- Gate `media-import-service` (`src/tools/check-media-import-service.sh`), registered in `check.sh`,
  verify-model `GATES` (136) and `GATES_THAT_BOOT_A_GUEST` (42), and `release-required.toml` (+
  `host.import-proto`, `host.ptp-transport-proto`).

### Verification (targeted; RULE ONE defers every boot to the end of the job)

- PASSED: `service-logic` host tests (643, including the 23 new); `import-proto` (17) and `ptp-transport-proto`
  (3); `system-manifest` (24, with the restart-aware budget test); `verify-model` (157); `cargo check --bins` of
  the services crate with and without `development`, the drivers crate (`development`) and the tools crate
  (`shared-image`); the test kernel build; `./gen.sh --accept-breaking`; rustfmt on every changed Rust file;
  `./check.sh --gate source-hygiene`, `--gate grant-vocabulary` (41), `--gate test-tags`.
- `check-bootstrap-plan.py`: only the pre-existing `font_catalogue` mismatch.
- NOT RUN: the `media-import-service` gate (both kernel tests), the PermissionManager summary test with
  `media-import=deny`, dynamic-report, and the AArch64/RISC-V cross-builds - all deferred to the end of the job.

## Verification pass (2026-09-24T01:28:37Z): the gates booted, what they found, and what changed

### Common to the service gates of P02M0180-P02M0188 (recorded in each of the nine files)

Each item below was found by booting a gate, fixed, and booted again. None of the gates had run before
this pass.

- **The first development image did not build.** Dynamic programs import every non-inlined function
  they call from a staged library and must declare each one; services calling the decisions crate now
  declare `service-util`, and a service calling a crate no staged library publishes is STATIC, as
  `font_catalogue` is (`power_service`, `smartcard_service`, `midi_service`, `admin_service`). The generated
  packages that use `liber:process@1.{task}` only as a handle no longer declare `process-proto`.
  `bootproto::manifest::MAX_ROWS` went from 256 to 512: the development volume's signed manifest carried
  291 rows (the row count is a `u16` on the wire; 512 rows of the longest staged path fit the 64 kB bound
  - a boot-protocol bound, flagged for the owner). The capability names the broker resolves for the new
  services live in `services::capability_names`, so `rt` stays byte-for-byte its committed state.
- **No fixture ever bound.** QEMU's `edu` function is class 0x00, subclass 0xff (`lspci`: `class 00ff`);
  all seven fixture rows in `manifest.toml` asked for 0xff/0x00. Corrected in all seven.
- **PermissionManager could launch none of the new probes.** Their policy rows were
  `#[cfg(feature = "development")]`, and PermissionManager is a dynamic program built once, into the shared
  image both configurations stage, without that feature - so the rows were absent from the development
  image too, and the shell reports a refused launch by printing nothing. The rows, the fixture-control
  grant path and the per-probe policy tables are unconditional, as the older probe rows (`btcheck`,
  `sandbox_probe`) already were; what keeps them inert in the shipping configuration is that none of their
  programs is staged there and that its supervisor mints no catalogue connection admitting a fixture's
  kind. PermissionManager declares the `smartcard-proto`, `modem-proto`, `camera-proto` and `midi-proto`
  it now calls.
- **No grant could be minted from a resolved root.** `rt::connect_or_resolve` keeps the connection the
  broker minted from a service's root and mints its own from it; SmartcardService, ModemService,
  CameraService, MidiService and AdminService accepted CONNECT only on their roots, so every mint was
  refused. A connection now mints another of its kind under the same bound, as PowerService's and
  BluetoothService's already did.
- **Handle baselines are read from the system graph** (`graph`), which holds each service's process and
  reads the kernel's own count: ProcessService's accounting lists only launches in a Domain of their own,
  and these services run in the supervisor's. SystemGraphService's 4 kB reply buffer had been outgrown by
  the graph (every `graph` answered `query error`); it is 64 kB, on the heap. Two measured properties of
  the graph decide WHEN a gate reads it: PermissionManager resolves each service root the first time it
  mints from it and keeps that connection (+1 handle per root, once), so the baseline is read after the
  first probes; and the graph keeps the process handle it was given at its own start, so after a service
  restart it reads the instance that ended (state `failed`, no handles) - no baseline is read across a
  restart.
- **A pipeline stage's diagnostics never reached the terminal** (older than this job; the gates exposed
  it). The shell handed the pipeline broker its terminal without DUPLICATE, so each per-stage stderr
  `duplicate` failed silently and `eprint` fell back to stdout - the pipe, for every stage but the last.
  The shell now hands the broker a duplicable endpoint, the broker gives the last stage a narrowed copy
  (send, wait, transfer) and closes the duplicable one once every stage's error endpoint exists. The
  holders that are first stages (`cardhold`, `modemhold`, `camhold`, `midihold`, `admincheck`) report on
  stderr.
- **Three fixtures spun a processor.** The camera, smart-card and modem fixtures armed their timer only
  when something was due; a kernel timer stays expired until armed again, so after the last deadline
  every wait returned at once and the fixture starved the probes. The timer is armed on every pass, to
  `u64::MAX` when nothing is due.
- **Gate scripts**: three were not executable (modem, camera, MIDI) and four copied their log onto itself
  (`cp "$GUEST_LINES" "$lines"` naming one file) and exited under `set -e`; fixed.
- **Two runs in parallel collide.** Every x86_64 guest forwards host port 5555; a second guest with a NIC
  is refused before it boots. Gates are run one at a time.

### Found by the first runs of the kernel scenarios, and changed

Neither kernel scenario had run before this pass (`./check.sh --gate media-import-service` boots the test
kernel with `TEST_SELECTION` naming the responder scenario and the grant scenario). Defects in the scenario
and its probe, none in the service:

- **The grant scenario read PermissionManager's start-up report as broker traffic** - the same scenario the
  spool gate runs; `run_permission_resolved_grant_scenario_in` now drains the nine start-up messages behind
  "online" before it plays the broker.
- **The destination was named with bare file names.** The scenario seeded `keep.jpg` through the storage
  harness and `import_probe` opened writers on `photo.jpg`, `empty.bin` and `keep.jpg`; a volume client takes
  whole `vol://` paths and refused every one ("the destination was seeded" failed first). The probe now names
  its destination files in the launch's working directory (`vol://system` in the scenario), and the scenario
  seeds and reads back `vol://system/...`.
- **The import rig handed MediaImportService its SERVE role with every right**, which the service refuses; it
  now carries SEND|RECEIVE|WAIT|TRANSFER, as ServiceManager's does.
- **The probe raced its own closes.** `browse` closed its cursor and at once asked for the over-limit
  enumeration, and was answered `exhausted` ("the over-limit storage was not refused with its count"): closing a
  cursor is its channel going away, which MediaImportService sees on its next pass, and until then the
  one-cursor-per-client bound still counts it. `stale` (a second enumeration) and `import` (a second transfer)
  had the same shape. The probe's `patiently` now asks again on `exhausted` as it did on `again`, inside the same
  ten-second bound - a slot that is never given back still fails the step.

### Verification

| What | Command | Result |
| --- | --- | --- |
| the gate | `./check.sh --gate media-import-service` (boots the test kernel with `TEST_SELECTION=kernel.services.media_import_service_reads_snapshots_and_whole_objects_and_nothing_else,kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone`) | **PASS**, 40 s, 2026-09-24T01:00:45Z |
| the changed probe | `cargo check --target x86_64-unknown-none --features development --bin import_probe` | PASS |

Its lines: browse, stale, import and removal passed; both selected tests ran and none failed.

### Host-side checks run in this pass (2026-09-23/24, x86_64 host)

| What | Command (from `src/` unless it says `./`) | Result |
| --- | --- | --- |
| decisions crate | `cargo test --manifest-path user/services/logic/Cargo.toml` | PASS, 670 |
| drivers library (fixtures' pure halves, decoders) | `cargo test --manifest-path user/drivers/core/Cargo.toml --lib` | PASS, 289 |
| manifest model | `cargo test --manifest-path tools/system-manifest/Cargo.toml` | PASS, 24 |
| terminal | `cargo test --manifest-path term/Cargo.toml` | PASS, 68 |
| bind ledger | `cargo test --manifest-path user/libs/driver/binding/Cargo.toml` | PASS, 85 |
| changed programs | `cargo check --target x86_64-unknown-none [--features development] --bin <name>` for `btcheck`, `cardcheck`, `permission_manager`, `admin_service`, `import_probe` | PASS |
| verification model | `./check.sh --gate verify-model` | PASS |
| declared interfaces | `./check.sh --gate declared-interfaces` | PASS |
| grant vocabulary | `./check.sh --gate grant-vocabulary` | PASS |
| milestone index | `./check.sh --gate milestone-index` | PASS |
| source hygiene | `./check.sh --gate source-hygiene` | PASS |
| bootstrap plan | `./check.sh --gate bootstrap-plan` | FAIL, the pre-existing `font_catalogue` mismatch only |
| dynamic report | `./check.sh --gate dynamic-report` | FAIL: "missing aarch64-unknown-none provider admin-proto" - the tracked report covers all three targets and cannot be checked or refreshed without their builds |

### Not performed in this pass, and why

- **The AArch64 and RISC-V cross-builds** that every registration item names. The owner's standing rule is
  that nothing slow runs while a task of the job is open, and that big runs are theirs to start. Commands:
  `./build.sh --arch aarch64`, `./build.sh --arch riscv64` (and with `LIBER_DEVELOPMENT=1` for the development
  programs), then `./check.sh --refresh dynamic-report` and `./check.sh --gate dynamic-report`.
- **`./verify.sh --plan` and `./verify.sh`**, for the same reason.
- No gate was run on AArch64 or RISC-V; every gate here is x86_64-only by its own statement.

### Where it stands, and what is the owner's

Ticked in `docs/todo/P02M0187.md` (2026-09-24): sixteen items. Open: the registration item (the cross-builds and
the dynamic graph check that needs them have not run).

## Final state of this pass (2026-09-24T01:32:00Z)

All nine service gates pass on x86_64, each run by itself (every x86_64 guest forwards host port 5555, so two
at once collide):

| Gate | Result | Finished (UTC) | Tree it ran on |
| --- | --- | --- | --- |
| `bluetooth-service` | PASS, 841 s | 2026-09-24T00:10:18Z | development image built 2026-09-23T23:17Z |
| `power-service` | PASS, 360 s | 2026-09-24T00:16:18Z | the same image |
| `smartcard-service` | PASS, 481 s | 2026-09-23T22:32:56Z | development image built 2026-09-23T22:22Z |
| `qemu-modem-service` | PASS, 800 s | 2026-09-23T22:46:16Z | the same 22:22Z image |
| `qemu-camera-service` | PASS, 400 s | 2026-09-23T23:49:37Z | the 23:17Z image |
| `qemu-midi-service` | PASS, 400 s | 2026-09-23T23:56:17Z | the 23:17Z image |
| `spool-service` | PASS, 40 s | 2026-09-24T01:00:05Z | the test kernel over the tree as it then stood |
| `media-import-service` | PASS, 40 s | 2026-09-24T01:00:45Z | the same |
| `qemu-admin-path` | PASS, 406 s | 2026-09-24T01:27:30Z | its own cold development build of the final tree |

What changed after a gate's pass, and why it does not reach that gate's path: after the smart-card and modem
passes, the bind ledger's bound (keyboard drivers only), the Bluetooth probe's pointer check, and the
AdminService, admin probe, import probe, kernel scenario and harness changes; after the Bluetooth, power, camera
and MIDI passes, the latter group only. None of these is on the path of a gate that passed before it. After the
last gate, `permission_manager.rs` was run through `rustfmt` with the tree's `rustfmt.toml` - three hunks this
job's edits had left unformatted, formatting only - and `cargo check --bin permission_manager` passed after it.

Host checks at the end: `./check.sh --gate grant-vocabulary`, `milestone-index`, `source-hygiene`,
`verify-model` and `declared-interfaces` - all PASS. `bootstrap-plan` fails only on the pre-existing
`font_catalogue` mismatch; `dynamic-report` cannot be checked without the AArch64 and RISC-V builds.

Not performed (the owner runs the long and cross-architecture work): `./build.sh --arch aarch64` and
`./build.sh --arch riscv64` (also with `LIBER_DEVELOPMENT=1`), `./check.sh --refresh dynamic-report` then
`./check.sh --gate dynamic-report`, `./verify.sh --plan` and `./verify.sh`.
