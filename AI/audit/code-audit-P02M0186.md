IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0186 (2026-09-23T06:49:41Z):

Implementation started; this record is updated as the work lands.

### What was implemented (updated 2026-09-23T07:37:10Z)

**Public job contract.**
- `src/idl/spool.lsidl` (`liber:spool@1`, generated crate `spool-proto`): `printer-id` {slot, generation,
  binding-generation, attachment, incarnation} - the full catalogue publication identity, the attachment
  generation and the service incarnation; `document-language` {postscript, pcl, pdf}; `observation`
  {yes, no, unavailable, unsupported}; `port-status` {paper-empty, selected, error, cover-open, jam};
  `printer-info` (bounded model, languages, status, available, queued, active, max-job-bytes); `job-state`,
  `job-cause` (size-limit, unplugged, backend-error, stalled, lifetime, reset, abandoned, cancelled) and
  `job-status` (state, printer, declared, staged, acknowledged, cause, delivery-uncertain). Interfaces:
  `spool` {printers (at most 8), create(printer, language, length) -> handle<channel>} and a separate `job`
  {write (at most 4096 bytes), submit, status, cancel}. Nothing exposes a catalogue, backend, reset or raw
  endpoint.
- `src/user/services/logic/src/spool_jobs.rs` (+ `spool_jobs/tests.rs`, 8 tests): the job state machine and
  every bound - 4 MB a job, 16 MB reserved, 16 live records, 2 per grant context, 16 contexts, 8 printers,
  4096-byte frames, 30 s no-progress (3000 ticks, reset only by accepted bytes, started at activation) and
  10 min from submission (60 000 ticks). `create` checks language, length and every count before a fallible
  `try_reserve_exact` of the whole declared length; `write` takes a whole frame or none, and past the
  declaration fails the job `size-limit` taking nothing; `submit` only at exactly the declared length,
  idempotent; `close` abandons and refunds a writing job, detaches a submitted one and retires a terminal
  one; terminal jobs release their reservation at once and keep a charged record until the channel closes;
  `next_frame` promotes the oldest submission per printer with one write outstanding; `written` advances by
  exactly the acknowledged count and fails the job (uncertain) on a count past the offer; `printer_lost`,
  `printer_reset`, `tick`, `write_failed` (a no-op on a job already over) end jobs with the evidence.

**Evidence and the private backend.**
- `src/user/services/logic/src/printer_status.rs` (+ `printer_status/tests.rs`, 5 tests): the IEEE 1284 device
  ID length-checked (2-byte big-endian length including itself, at most 4096) before anything is read;
  `KEY:value;` pairs; `CMD`/`COMMAND SET` tokens with exact case-insensitive `POSTSCRIPT`, `POSTSCRIPT2`,
  `POSTSCRIPT3`; unrecognized tokens kept as bounded metadata (16 of 32 bytes); a key given twice or two
  disagreeing command sets is ambiguous; missing, malformed or ambiguous evidence means no language. Port
  bits: bit 5 paper empty, bit 4 selected, bit 3 not-error; a failed read is `unavailable` throughout; cover
  open and jam are `unsupported` unless the backend supplied evidence.
- `src/idl/printer-device.lsidl` (`liber:printer-device@1`, crate `printer-device-proto`): `printer-backend`
  {attach(version) -> {version, attachment, device-id <= 4096}, port-status(attachment) -> port-reading
  {bits, cover-open option<bool>, jam option<bool>}, write(attachment, bytes <= 4096) -> accepted count,
  reset(attachment) -> a new attachment}. No arbitrary control request.
- Provider kind `printer = 18` in `device.lsidl`, `driver-protocol`, `system-manifest` and DeviceManager's
  conversions (landed with the camera/MIDI kinds).

**The service.** `src/user/services/core/src/spool_service.rs`:
- Roles in declared order: `SERVE` (serve root), `CATALOGUE` (factory from DeviceManager's `CATADMIN`,
  `kinds = ["printer"]`, optional). Starts and serves an empty list with no printer or no catalogue.
- Every backend request goes out without blocking under the service's own correlation (the generated
  client encodes into a capture transport) and is matched to the one request outstanding per printer; late
  answers are dropped. `again` (or a zero count) accepts nothing and schedules a status read 100 ms later,
  after which the frame is offered again. A write error, or a count past the offer, fails the job
  (delivery uncertain) and resets the printer; a write unanswered for 30 s resets it; an attach or reset
  unanswered for 2 s, refused, of the wrong version or without a new attachment leaves it unavailable. A reset
  ends every job on the printer (`reset`), never replaying; a successful one restores eligibility under a new
  attachment. Withdrawal or a closed backend channel ends every job on the printer (`unplugged`); a
  replacement is a new printer and nothing is re-bound to it.
- Admission: the printer by its whole identity (unknown -> `not-found`; another attachment or incarnation ->
  `stale`), attached (`again` while attaching, `io` when unavailable), a language its device ID names
  (else `unsupported`), then `spool_jobs::create`. Each `CONNECT` on the root is a grant context (16 at
  most); a context whose connection closed is kept while anything is still charged to it.
- Job channels: an oversized frame (longer than opcode + correlation + length + 4096) is refused `invalid`
  before dispatch and leaves the job untouched; an undecodable request closes the channel exactly as the
  client closing it would. Replies to clients go out without blocking.
- Buffers sized for the largest encoded frames with envelopes: a job `write` (2 + 4 + 2 + 4096) and an
  attach/reset answer (4 + 1 + 4 + 8 + 2 + 4096), compile-time asserted against the 8192-byte buffers.

**Deployment and grants.**
- Manifest: program `spool_service` (dynamic, volume, `libexec/spool_service.lsexe`), service
  `spool_service` (`restart = "escalate"`, `state_class = "ephemeral"`, dependencies `log_service`,
  `device_manager`, `process_service`), program `spool_probe` (dynamic, volume, `libexec/spool_probe.lsexe`,
  shipping so it is in the test image); `sources` and `libraries` rows for both protocol crates; `gen.sh`
  packages and externals.
- The combined catalogue budget: 15 catalogue-minting roles with this service, 2 x 15 + 1 = 31 of the 32
  slots (system-manifest check passes). One slot of headroom is left for the rest of the job.
- `security.lsidl` capability `spool`; rt `CAP_SPOOL`; ServiceManager broker: `cap_grants` for
  `permission_manager`, `service_of_cap`, `serve_resolve` (the kept `SERVE` end). Not plan-relaunchable
  (escalate).
- PermissionManager: `Capability::Spool` appended to `VOCABULARY` (40), tag `SPOOL`, stored client
  `clients.spool` resolved by name through the broker on first use, and every grant a FRESH sub-connection
  from it (`grant_handle`); policy row `spool_probe` -> [`spool`]. DECISION: the root is reached through the
  broker, as for the camera, MIDI, modem and smart-card roots, rather than by a bootstrap role, because a
  role from a service is a manifest dependency and PermissionManager would then wait for the spooler.
- Kernel summaries: `later_denials!` in `src/kernel/test_suites/applications.rs` ends in ` spool=deny`.

**Tests.**
- `src/user/services/core/src/spool_probe.rs`: a real client holding `spool` only, one mode per step
  (`inventory`, `two`, `stall`, `abandon`, `crash`, `detach`, `unplug`, `reset`, `recovery`), printing
  `spool-probe: PASS <mode>` or `FAIL`, and the `stalled`/`queued`/`unplugged acknowledged=... uncertain=...`
  markers the harness acts on.
- `src/kernel/test_suites/services.rs`, module `spool_sink` and test
  `kernel.services.spool_service_sends_submitted_jobs_exactly_once`: the harness is the catalogue (subscribe
  with a snapshot, `open`, withdrawal and replacement frames) and three typed backends answered through the
  generated `printer_backend::dispatch` (one consumer each, bounded 128 kB recording, prefix acceptance,
  `again`, a stall, port bits, cover/jam evidence on one, a failed write, failed resets). Every byte each sink
  received is compared with the documents the steps sent. The kernel crate gained `printer-device-proto`.
- `src/kernel/tests.rs` `run_permission_spool_scenario` and
  `kernel.applications.permission_manager_grants_spool_to_its_probe_alone`: the production PermissionManager
  with the process stand-in, the broker played on its bootstrap and a played spool root - two `spool_probe`
  launches each receive their own fresh connection from a root resolved once by name; `date` receives none.
- Gate `spool-service` (`src/tools/check-spool-service.sh`: `TEST_SELECTION` of the two tests on x86_64, then
  every step's PASS line and the unplug evidence from the run's own logs), registered in `check.sh`, verify-model
  `GATES` (135) and `GATES_THAT_BOOT_A_GUEST` (41), and `release-required.toml` (+ `host.spool-proto`,
  `host.printer-device-proto`).

### Verification (targeted; RULE ONE defers every boot to the end of the job)

- PASSED: `cargo test --manifest-path user/services/logic/Cargo.toml` (620, including the 13 new);
  `spool-proto` (8) and `printer-device-proto` (2) host tests; `system-manifest` (24); `verify-model` (157,
  after `covers` was made visible to its scanner by calling the probe runner as a free function);
  `cargo check --bins` of the services crate with and without `development`; the test kernel build
  (`TEST=1 TEST_TAGS="" cargo build --tests`); `./gen.sh --accept-breaking`; rustfmt on every changed file;
  `./check.sh --gate source-hygiene`, `--gate grant-vocabulary` (40 capabilities), `--gate test-tags`.
- `check-bootstrap-plan.py`: fails only on the pre-existing `font_catalogue` mismatch; SpoolService is
  escalate and appears in neither list.
- NOT RUN: the `spool-service` gate (both kernel tests), the PermissionManager summary test with the new
  `spool=deny`, the dynamic-report check, and the AArch64/RISC-V cross-builds. All are deferred to the end
  of the whole job.

UPDATE TO THE IMPLEMENTER'S RECORD ABOVE (2026-09-23T08:21:41Z), from the work on P02M0187:

- The grant scenario was generalized for the import authority: `run_permission_spool_scenario` is now
  `run_permission_resolved_grant_scenario`, and the test is
  `kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone` (two `spool_probe`
  launches, two `import_probe` launches and `date`). `check-spool-service.sh` selects the new name.
- The catalogue figure above (31 of 32) was counted with P02M0180's rule of two slots for every minting role.
  P02M0187 made the check restart-aware - two slots only for roles of `transparent` services, which are the only
  ones ServiceManager ever relaunches - and by that count this milestone left the demand at 24 of 32.

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

Neither kernel scenario had run before this pass (`./check.sh --gate spool-service` boots the test kernel with
`TEST_SELECTION` naming the spool scenario and the grant scenario). Three defects in the scenarios themselves,
none in the service:

- **The grant scenario read PermissionManager's start-up report as broker traffic.** The harness plays the
  broker on the manager's bootstrap channel and returns from bring-up at the first message, "online"; the
  nine start-up messages behind it (the probes' reads and summaries) were still queued, and the first was
  taken for a request that was not a RESOLVE ("the manager sent its broker something other than a RESOLVE").
  `run_permission_resolved_grant_scenario_in` (`kernel/tests.rs`) now lets the manager finish and drains
  them. The scenario then passed.
- **The spool rig handed SpoolService its SERVE role with every right**, and the service refuses a role
  carrying more than a serve root may ("BOOTFAIL SERVE: role carried more rights than it is allowed") - so it
  never reported in. The rig now hands it SEND|RECEIVE|WAIT|TRANSFER, as ServiceManager does (the same fix in
  the import rig).

### Verification

| What | Command | Result |
| --- | --- | --- |
| the gate | `./check.sh --gate spool-service` (boots the test kernel with `TEST_SELECTION=kernel.services.spool_service_sends_submitted_jobs_exactly_once,kernel.applications.permission_manager_grants_spool_and_import_to_their_probes_alone`) | **PASS**, 40 s, 2026-09-24T01:00:05Z |

Its lines: inventory, two, stall, abandon, crash, detach, unplug, reset and recovery passed; "unplugged
acknowledged=9500 uncertain=true"; both selected tests ran and none failed. (`test.sh` refuses to run on a build
that does not match the sources, so the cold development build the admin-path gate makes just before it is what
it ran against.)

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

Ticked in `docs/todo/P02M0186.md` (2026-09-24): twelve items. Open: the registration item (the cross-builds and
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
