IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0184 (2026-09-23T05:58:22Z):

Implementation started; this record is updated as the work lands.

## What was implemented

### P02M0184a - contract and capture grants

- `src/idl/camera.lsidl` (`liber:camera@1`): `@kernel(memory-object) resource frame-buffer` (the client's
  memory, guarded by `@rights(map, write)` on `register`), `camera-id` (publication identity plus service
  incarnation), `frame-type` (YUY2, MJPEG), colour range/matrix with `unknown`, rational `interval`,
  `intervals` (discrete list of at most 32, or a checked `interval-range`), `frame-size`, `format-info`,
  `camera-info` (no frame, no buffer; `streaming`, `available`), paged `format-page` under a snapshot
  generation, `stream-request`, `negotiated` (stream generation, exact format/size/interval, max bytes, YUY2
  stride and plane offset, range and matrix), `device-time` (raw ticks, width, optional frequency, domain,
  reset generation), `frame`, coalesced `capture-status` and `capture-event`. Interfaces `camera` (inventory:
  `cameras`, `sizes`), `camera-capture` (`camera`, `sizes`, `negotiate`, `register`, `start`, `events`,
  `release`, `stop`, `status`) and `camera-admin` (`mint(alias, owner: handle<task>)`, `revoke(camera)`).
- `src/idl/camera-device.lsidl` (`liber:camera-device@1`, importing the format records): `open`, `formats`,
  `sizes`, `negotiate(stream-generation, request)`, `register`, `queue`, `start`, `stop` (answered once every
  buffer is back and every mapping released), `events` (frame / dropped with reason / uncounted gap), and the
  development-only `camera-fixture` control interface.
- Generation and packaging: `gen.sh` (packages `camera`, `camera-device`), crates `camera-proto` and
  `camera-device-proto`, the `proto` facade, manifest `[[sources]]`/`[[libraries]]`. Names in
  `camera-device` carry a `camera-` prefix because `device-open` and `device-event` already exist in the
  facade (modem-device, display-device).
- Provider kind `camera = 16` (device IDL, `driver_protocol::provider::CAMERA`, system-manifest, DeviceManager
  both ways). Capabilities `camera` and `camera-capture` (security IDL).
- CameraService (`camera_service.rs`), manifest program/service with roles `CATALOGUE` (`kinds = ["camera"]`),
  `SERVE` (inventory root) and `ADMIN` (minting root); transparent restart via `plan_relaunchable`; rt
  `CAP_CAMERA` / `CAP_CAMERA_ADMIN`, broker `cap_grants`, `service_of_cap`, `serve_resolve`.
- PermissionManager: `Camera` is a fresh sub-connection of the inventory root; `CameraCapture` is minted in
  `grant_for_task` through `camera-admin.mint` with a `WAIT | TRANSFER` duplicate of the prepared task;
  `camera_policy` has no shipping row (no camera by default). `Camera`/`CameraCapture` sit in `VOCABULARY`
  before the modem authorities (see decision 3).
- Limits: four cameras, one stream per camera (for anybody), 32 client connections (inventory and grants);
  a live stream of another grant, or this grant's own not yet stopped, answers `again`; a quarantined camera
  answers `io`.

### P02M0184b - enumeration and producer boundary

- `src/user/drivers/core/src/uvc.rs` (host-tested, 8 tests): UVC 1.5 VS format/frame/colour descriptors
  normalized to at most 8 formats, 32 sizes and 32 intervals; YUY2 (by GUID) and MJPEG kept, anything else
  skipped and counted; malformed lengths, frames outside a format, type mismatches, duplicate indexes, count
  disagreements, zero or overflowing dimensions, a YUY2 buffer that cannot hold its frame, zero/repeated/
  disordered intervals, a continuous range whose step does not reach its maximum, and every budget refused;
  `select` answers exactly the request or nothing.
- `service_logic::camera_streams` (host-tested, 9 tests): the service's own check of a provider's
  advertisement (`validate`), bounded pages, `expect` (what a request means against the advertisement, with
  rational interval comparison and stepwise membership) and `verify` (a provider's selection must be exactly
  that - format, size, interval, stride, plane offset, and no larger frame).
- CameraService opens each provider with a bounded session (`open`, `formats`, `sizes` per format), validates
  before admitting, and refuses an advertisement that breaks its bounds. Only it opens providers; public
  clients never receive a catalogue or provider endpoint.

### P02M0184c - buffers, bounds and teardown

- `camera_streams::Stream`: at most four buffers, 8 MB each, 32 MB a stream, 128 MB in all; a buffer too
  small for the negotiated frame is refused; the same backing (by kernel object identity) once per stream;
  states `Client` / `Producer(lease)` / `Leased(lease)`; completions accepted only for the exact producer
  lease with a length that fits; releases only for the exact client lease; stop waits for every buffer with a
  two-second deadline, then quarantine; a retired stream's buffers are the client's again.
- CameraService's `register` checks the handle with `object_info` (a memory object, its real size), narrows
  it to map/read/write/transfer (duplicating down when the client handed more and can be narrowed, refusing
  otherwise) and forwards it; the service keeps identity and size, never frame storage. Completion metadata
  is typed IPC; at most four completions exist per stream; the coalesced status travels beside them and is
  retried when the client's stream is full. Stop from a client, the grant closing, the owner task ending,
  revocation and provider loss all end the stream the same way.

### P02M0184d - timing and loss

- The fixture samples `clock_ns()` when a frame is complete; the service refuses an arrival time from the
  future. Device time passes through only when present and well formed (`DeviceClock::valid`); the reported
  reset generation is the service's timeline generation, advanced on a device reset, a change of domain or
  width, and after an uncounted loss (`DeviceClock::observe`). `DeviceClock::elapsed` reads modulo the width
  and refuses across timelines.
- The observation sequence is the producer's, checked: completions and drops must continue it; an uncounted
  gap and an unexplained jump each count one unknown discontinuity, never a guessed number of frames.

### P02M0184e - verification

- Fixture `src/user/drivers/core/src/camera_fixture.rs` (development-only, `edu` at 0:26.0): synthetic
  descriptors through `uvc::normalize`, deterministic YUY2 and MJPEG payloads written into the queued client
  buffers, a 32-bit 1 MHz device clock starting 300 ms short of its wrap at each stream start, and control
  operations to drop counted frames, lose uncounted ones, drop device time, reset the clock, hold a stop,
  withdraw/republish and read its counters (including how many client buffers it holds mapped).
- Probes: `camcheck`, `camhold`, `camread`, `camfail`. Gate `src/tools/check-camera-service.sh`, registered
  as `qemu-camera-service` in `check.sh`, verify-model `GATES` (133) and `GATES_THAT_BOOT_A_GUEST` (39), and
  `release-required.toml` (`gate.qemu-camera-service`, `host.camera-proto`, `host.camera-device-proto`).

## Material decisions

1. Admin revocation is `camera-admin.revoke(camera)`, which ends every capture grant on a camera exactly as a
   closed connection would. Only PermissionManager holds the admin root and nothing in the tree calls
   `revoke` yet, so the guest proof exercises the other two revocations - the owner task ending (`camhold dup
   | camcheck inherit`) and the prepared launch being abandoned after minting (`camfail`).
2. A client must open its events stream before `start`: a client with nowhere to hear of a frame could never
   return a lease.
3. `Camera` and `CameraCapture` precede the modem authorities in PermissionManager's `VOCABULARY` so the
   camera gate's failing launch (`camfail`: capture, then a modem identity grant naming an alias nobody
   configured) fails on a LATER grant. Only development probes hold either family, so no delivery order a
   shipping component reads changed.
4. The stream generation is assigned by the service before `negotiate` is sent and carried on the provider
   wire, so a provider's answer names the stream it is for (no provider-side guessing).
5. A retired stream, whoever ran it, no longer blocks a new negotiation on that camera.
6. Pixels are never trusted input and never re-read by the service; the probe computes the fixture's
   pattern independently to check them.

## Verification performed (2026-09-23)

- `cargo check` of the services crate (all bins, with and without `development`) and the drivers crate (all
  bins, `development`): clean.
- Host suites from `src/`: `uvc` 8 (drivers lib 278 in all), `camera_streams` 9 (service-logic 603 in all),
  camera-proto 17, camera-device-proto 5, system-manifest 24, verify-model 157. The lease check was mutated
  (always accepted) and `camera_streams` failed on it before it was put back.
- `./gen.sh --accept-breaking`: 25 packages. `./check.sh --gate source-hygiene`: clean.
- `check-bootstrap-plan.py`: the camera service is consistent; the pre-existing `font_catalogue` mismatch
  remains (see the P02M0183 record).

## Not performed yet

- The `qemu-camera-service` gate (it needs a development image, built at the end of the job) and the
  aarch64/riscv64 cross-builds.

## Verification pass (2026-09-24T00:34:07Z): the gates booted, what they found, and what changed

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

### Found by the camera gate's boots, and changed

- **Every capture grant was refused** (the common CONNECT defect): CameraService accepted CONNECT only on
  its roots. The roots and the admin connections mint admin connections, and an observer connection mints
  another observer, each under its existing bound.
- **The camera fixture's alias table was `cfg`-gated** in the service, absent from the image for the
  reason the policy rows were; it is unconditional (only the fixture row exists in development alone).
- **Completions were dropped at saturation.** The service sent a status into the client's event stream
  after every frame and drop, and a frame event that found the stream full was discarded: the client never
  learned it held that buffer and never returned it. The plan bounds completion metadata to the registered
  buffers and keeps the coalesced status outside that queue. Completions that do not fit now wait on the
  grant (`GrantConn.frames`, at most one per registered buffer) and go out first (`flush`); a status is
  sent only once none is waiting (`statuses` skips a grant still holding one), and a stalled status stays
  due (`statuses_due` counts waiting frames).
- **The fixture spun** (the common timer defect): the `busy` and `inherit` phases "hung" with every call
  inside them bounded.
- **The handle baseline**: `camcheck`'s `handles` phase is gone; the gate reads the service's count from the
  system graph after `camcheck capture` (measured over a boot that read it after every step: 8 at boot, 9
  after `camread`, 10 after `camcheck capture` - PermissionManager's two resolved roots - then 10 through
  the scenario, 13 while `camhold retain &` holds its grant, and 10 again at the end).
- **The last count was missing once** ("the service's handle count was not read twice") while every
  phase passed. A measured boot of the same sequence showed the machine's IPv6 status line arriving at the
  time of the final `graph`; it is written straight to the port while `graph`'s lines come through
  ConsoleService's mirror, so it can land inside the service's line. The gate now boots with no NIC
  (`NET_NONE=1`; nothing here needs a network) and runs `fg` before its last `graph`, so the background
  `camhold retain &` has let go of its grant before the count is read.
- `camhold` reports on stderr (common list).

### Verification

| What | Command | Result |
| --- | --- | --- |
| development image | `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` then `LIBER_DEVELOPMENT=1 ./image.sh` | PASS, 2026-09-23T23:19Z |
| the gate | `./check.sh --gate qemu-camera-service` | **PASS**, 400 s, 2026-09-23T23:49:37Z |

Its lines: `camread: PASS`; `camcheck: PASS` capture, mjpeg, timing, busy, inherit, again, quarantine;
`camhold: PASS retain` and "sent its endpoint while streaming, and exits"; "CameraService: a grant's owner
ended - its grant is retired"; "CameraService: a stop was not confirmed in time - the camera is
quarantined"; "the service's handles returned to their baseline (10)".

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

Ticked in `docs/todo/P02M0184.md` (2026-09-24): twenty-three items. Open: the registration item
(cross-builds not run). Revocation is proved by owner death and by an abandoned prepared launch;
`camera-admin.revoke` has no caller yet.

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

## Continuation (2026-09-24, from 01:33Z): the verification items the first pass left open

### Common to P02M0180-P02M0188 (recorded in each of the nine files)

The first verification pass (above) ended with all nine gates passing on x86_64 and these items still open
across the nine milestones: the AArch64/RISC-V cross-builds every registration item names, and the dynamic
report that cannot be checked without them; the in-guest key/store-capability denial and stale-handle
rejection the Bluetooth gate item names (P02M0180); tests of refusal and reclamation at the modem service's
provider, client and context bounds, and the existing network guest checks that milestone names (P02M0183);
and for the administrative path (P02M0188) the demonstration that bypassing protected presentation or the
executor boundary fails its gate, `./verify.sh --plan` and `./verify.sh`. This continuation added the missing
assertions, re-ran the three gates they live in, ran the host checks and the existing x86_64 checks the job's
changes reach, and left the slow-architecture work - the cross-builds and the dynamic report - for last, once
nothing else was open.

What changed in the tree in this continuation. Development-only probes, the gate scripts, the kernel test suite
and its Cargo manifest, and `gen.sh`; no production service, driver, library, IDL or manifest row changed:

- `src/user/services/core/src/btcheck.rs` (01:36Z): the new phase `deny`, and stale-handle assertions added to
  `refund` and `loss`. `src/tools/check-bluetooth-service.sh`: runs `btcheck deny` after `btcheck pair` and
  requires its PASS line.
- `src/user/services/core/src/modemcheck.rs` (01:42Z): the new phase `context`. `src/tools/check-modem-service.sh`:
  runs `modemcheck context` after `modemcheck activate`, and a third part runs the new kernel scenario below
  through `TEST_SELECTION=... ./test.sh --arch x86_64`, reading its verdict from that run's own `RESULT-LOGS`.
- `src/kernel/test_suites/services.rs` (01:41Z): `mod modem_rig` and the scenario
  `kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back`.
  `src/kernel/Cargo.toml`: `modem-device-proto` and `modem-proto` as path dependencies, as the other harness
  wire crates (`network-proto`, `display-device-proto`, `printer-device-proto`) already are, so the rig answers
  and reads both wires through the generated code.
- After the modem gate's first run of its new part (see P02M0183's section): `src/tools/check-modem-service.sh`
  (02:29Z) runs `test.sh` without the gate's `QEMU_EXTRA`, and the scenario's wait for a new publication pumps
  the scheduler (`services.rs`, 02:37Z).
- `gen.sh` (02:46Z): the five hyphenated package keys this job added to its `EXTERNAL` table are quoted - see
  "Found in this continuation" below.
- For the P02M0188 demonstration only, two planted defects, one at a time, each in one file; each file was
  copied aside first and restored from the copy afterwards, and both restorations were checked with
  `sha256sum -c` against hashes taken before the first plant (both `OK`). Neither defect is in the tree.

### Found in this continuation, and changed (common)

- `./gen.sh --check` failed at once: "line 180: EXTERNAL[$package]: unbound variable". The five package keys
  this job added to `gen.sh`'s `EXTERNAL` table - `[modem-device]`, `[camera-device]`, `[midi-device]`,
  `[printer-device]` and `[ptp-transport]`, one each for P02M0183-P02M0187 - were written unquoted, and a shell
  formatter reads an unquoted hyphen in a subscript as arithmetic: `shfmt` (which `format.sh --changed` runs,
  and `commit.sh` runs that before a commit) had rewritten them as `[modem - device]` and so on, keys nothing
  looks up. The comment above `["display-device"]` in the same table describes exactly this. All five are now
  quoted as that one is; `shfmt -d gen.sh` reports no change, so the next format pass leaves them alone, and
  `./gen.sh --check` passes: "no drift: 33 packages, the aggregate and every profile regenerates to what is on
  disk" (20 s). The bindings on disk were not stale - only the script that checks and regenerates them was
  broken, which is why the check is the thing that found it.
- Formatting of everything the job changed, checked without writing: `rustfmt +nightly --edition 2024
  --config-path rustfmt.toml --check` over the 266 Rust files, `shfmt -d` over the 13 shell scripts and `taplo
  fmt --check` over the 25 TOML files changed since `13309309` - all three clean (the same tools and flags
  `format.sh --changed` applies).

### Verification in this continuation, before the cross-builds (x86_64 host and guests, one guest at a time)

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| Bluetooth gate | `./check.sh --gate bluetooth-service` | **PASS**, 841 s | 02:15:21Z |
| modem gate, first run | `./check.sh --gate qemu-modem-service` | **FAIL**, 821 s: both boots passed; its new third part could not start QEMU (see P02M0183) | 02:29:02Z |
| administrative path, clean tree | `./check.sh --gate qemu-admin-path` | **PASS**, 414 s | 02:35:56Z |
| modem bounds scenario alone | `env -u QEMU_EXTRA TEST_SELECTION=kernel.services.modem_service_refuses_at_its_provider_and_client_bounds_and_gives_them_back ./test.sh --arch x86_64` | **FAIL** (a scenario defect, see P02M0183), then after the fix **PASS**, 1 passed, 25 s | 02:36Z, 02:38Z |
| the same, with a bound mutated | as above, `MAX_PROVIDERS` 5, then `MAX_CLIENTS` 33 | **FAIL** each, as it must; file restored, `sha256sum -c` OK | about 02:45Z |
| generated bindings | `./gen.sh --check` | **FAIL** (the script), then after quoting **PASS**: no drift, 33 packages, 20 s | 02:46Z, 02:47Z |
| every crate host suite | `./check.sh --gate host-tests` | **FAIL** (`driver-protocol`, see P02M0188), then after the fix **PASS**, 124 suites, 104 s | 02:48Z, 03:27Z |
| verification model | `./check.sh --gate verify-model`, `--gate verify-model-tests` | **PASS**, 19 s and 53 s | 02:50Z |
| manifest-derived checks | `./check.sh --gate declared-interfaces`, `--gate grant-vocabulary`, `--gate milestone-index` | **PASS** each | 02:50Z |
| source hygiene | `./check.sh --gate source-hygiene` | **PASS**, 72 s | 02:51Z |
| bootstrap plan | `./check.sh --gate bootstrap-plan` | **FAIL**, only the pre-existing `font_catalogue` mismatch: the manifest says `restart = "transparent"` for it and `relaunch_service` has no arm for it, which is so at `a07f535b` too; every service of this job agrees with its plan | 02:50Z |
| formatting, check mode | `rustfmt +nightly --edition 2024 --config-path rustfmt.toml --check`, `shfmt -d`, `taplo fmt --check` over every file changed since `13309309` | **PASS** (266 Rust, 13 shell, 25 TOML files) | 02:20Z |
| kernel tests of the changed components | `./test.sh --arch x86_64 --tags permission-service,process-service,network,input,mouse,display,usb --timeout 1800` | **PASS**, 48 passed, 167 s | 02:59:04Z |
| the IPv6 peer gate | `./check.sh --gate ipv6-peer` | **FAIL**, 106 s, the quiet row (see P02M0183) | 03:02:56Z |
| modem gate, end to end | `./check.sh --gate qemu-modem-service` | **PASS**, 835 s | 03:18:33Z |
| the DHCP test | `TEST_SELECTION=kernel.services.dhcp_lease_renews_at_t1_and_restarts_its_clock ./test.sh --arch x86_64` | **PASS**, 1 passed, 26 s | 03:19Z |

What changed after each gate passed, and why it is not on that gate's path: after the Bluetooth pass, the modem
gate script and its kernel scenario, `gen.sh` and a host-only test file; after the administrative-path pass, the
same; after the modem pass, the host-only test file (`driver/protocol/src/tests.rs`, compiled only under
`cfg(test)`). None of them is in an image a gate boots.

Tree changes this continuation did NOT make, for the record: the user committed the tree three times while it
ran (`ea33ffd0` and `47f9544c` at 02:15-02:16Z, `0fbe2ad0` at 02:38Z); `commit.sh`'s format pass changed no source
file (no file under `src/` was newer than 02:14Z afterwards). Cargo rewrote two lock files while the host
checks built: `src/proto/Cargo.lock` gained `admin-proto` (the aggregate crate's new dependency, from P02M0188)
and `src/tools/font-gen/Cargo.lock` dropped a `smartcard-model` entry (this job made it a dev-dependency of
`service-logic`, which `font-gen` reaches as an ordinary dependency).

### P02M0184 in this continuation

No code of this milestone changed in this continuation. Its registration item - the only one open - waits only on
the cross-builds and the generated-binding and manifest checks, which run below.

The cross-builds, the dynamic report and the verification planner run last, once, and are recorded in the next section.

## Final state of the job (2026-09-24T04:30Z): the cross-builds, the dynamic report and the verification plan

### Common to P02M0180-P02M0188 (recorded in each of the nine files)

The slow-architecture work ran last, once, after every other item of the nine milestones was done or had been
reduced to a decision that is the owner's. It found one more defect of this job's, which was fixed before the run
was repeated.

### Found by the AArch64 and RISC-V builds, and changed

The first attempt at the final run failed on both ports at the same place, the build's check that every import
of a dynamic program is defined by exactly one of its declared providers:

    build-shared: media_import_service import _RNvMs3_..._5alloc7raw_vecINtB5_6RawVecmE8grow_oneCs3SQuU4xAvRw_4wasm
    has 0 declared providers (expected 1)

(`./build.sh --arch aarch64`, 433 s, and the same on the retry; `./build.sh --arch riscv64` with `wasm`'s RISC-V
hash.) `RawVec<u32>::grow_one` - `m` is `u32` in the v0 mangling - is what `Vec<u32>::push` calls when it has to
grow. With generic sharing, rustc does not instantiate it in a program if a crate the program loads already
exports it; of several exporters it takes the one with the lowest StableCrateId, and StableCrateIds differ
between targets. P02M0183's `modem-device.lsidl` has `@bound(2) dns: list<u32>`, so the generated decoder makes
`modem-device-proto` an exporter, and every program using the `proto` aggregate loads it. On x86_64 it won for
the four programs that push a `Vec<u32>` - `kill`, `traceroute`, `spool_service`, `media_import_service` - which
is why the first pass declared `modem-device-proto` as a provider of all four. But the two services also load
`wasm` (through `service-logic`), which exports the same instance, and reading the crate hashes out of the symbol
names (base 62, most significant first) shows `wasm`'s is the lower one on AArch64 (`3SQuU4...` against
`4Vf2JM...`) and on RISC-V (`4qHskh...` against `bj6yQv...`), while `modem-device-proto`'s is lower on x86_64
(`3X374D...` against `ju6TMu...`). So on the ports those two services import it from `wasm`, which they do not
declare, and no single providers list - the manifest has one per program, not one per target - is right on all
three. `kill` and `traceroute` load no other exporter; on AArch64 `kill` imports it from `modem-device-proto` as on
x86_64, and the RISC-V build below passed them too.

Changed (P02M0186's and P02M0187's services, and their manifest rows):

- `spool_service.rs`, `pump`: the printers lost in one pass are kept in a fixed `[u32; jobs::MAX_PRINTERS]` with a
  count - at most one per printer, and adoption refuses a printer beyond `MAX_PRINTERS` - instead of a `Vec<u32>`.
- `media_import_service.rs`, `tick`: the idle transfers' devices are collected (`filter`, `map`, `collect`) before
  the transfers are failed in a second loop, instead of being pushed one by one. `Transfer::timed_out` only reads
  the transfer, so both loops select the same transfers. `collect` into a `Vec<u32>` grows through the
  allocator's type-independent reserve path, not through `grow_one` - the service's existing `collect`s into
  `Vec<u32>` import nothing.
- `manifest.toml`: `modem-device-proto` and its explanatory comment are gone from both programs' providers.

Verified: `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` passed (256 s) and neither program imports the routine
any more (`llvm-readelf --dyn-syms`, 0 each); `./check.sh --gate spool-service` **PASS** (40 s, 03:53:46Z) and
`./check.sh --gate media-import-service` **PASS** (39 s, 03:54:25Z); the format check of the three files is clean;
and the ports then built (below).

### The final run

| What | Command | Result | Finished (UTC) |
| --- | --- | --- | --- |
| AArch64 build | `./build.sh --arch aarch64` | **PASS**, 326 s. The first attempt died on this machine's intermittent rustc SIGILL while compiling the kernel ("rustc interrupted by SIGILL"); the one retry built (47 s) | 03:59:51Z |
| RISC-V build | `./build.sh --arch riscv64` | **PASS**, 318 s, first attempt | 04:05:09Z |
| AArch64 development build | `LIBER_DEVELOPMENT=1 ./build.sh --arch aarch64` | **PASS**, 110 s | 04:06:59Z |
| RISC-V development build | `LIBER_DEVELOPMENT=1 ./build.sh --arch riscv64` | **PASS**, 103 s | 04:08:42Z |
| the growth routine's importers, all three targets | `llvm-readelf --dyn-syms` on the staged programs | `kill` and `traceroute` import it from `modem-device-proto` on x86_64, AArch64 and RISC-V alike; `spool_service` and `media_import_service` import it nowhere | 04:06Z |
| the dynamic reports | `./check.sh --refresh dynamic-report` | **PASS**, 335 s. Rewrote `docs/DYNAMIC_EXECUTABLES.tsv` (228 rows), `docs/DYNAMIC_WAVES.tsv` (18 rows) and `docs/DYNAMIC_IMAGE.tsv` (3 rows). Staged bytes per target: x86_64 7652888 -> 8342560, AArch64 8232032 -> 8985328, RISC-V 8402816 -> 9177000 - measured against the report as it was last refreshed (`c822f5f2`, 2026-09-19), so the difference is everything since then, not this job alone | 04:14:17Z |
| the dynamic report check | `./check.sh --gate dynamic-report` | **PASS**, 339 s | 04:19:56Z |
| the verification planner | `./verify.sh --plan` | "FULL verification", because `gen.sh` belongs to `harness.scripts` and `src/user/services/manifest.toml` to `manifest`; build and boot on all three targets; provisional estimate 8952 s against 9005 s for everything | 04:20:31Z |
| host checks on the final tree, after the last source edit | `./gen.sh --check`; `./check.sh --gate` `host-tests`, `verify-model`, `declared-interfaces`, `grant-vocabulary`, `milestone-index`, `source-hygiene`, `bootstrap-plan` | **PASS** each (no drift in 33 packages; 124 crate host suites in 106 s; hygiene 74 s), except `bootstrap-plan`: **FAIL** on the pre-existing `font_catalogue` mismatch alone | 04:24:52Z |

NOT RUN, and why: `./verify.sh` itself. Its plan is the whole suite on three architectures (about two and a half
hours, most of it emulated guests), and by the owner's standing rule the big suites are theirs to start; the job
hands them the command. No guest was booted on AArch64 or RISC-V: every gate of these milestones is x86_64-only by
its own statement, and the ports are cross-built.

### Where the nine milestones end

| Milestone | State | What keeps it open |
| --- | --- | --- |
| P02M0180 | OPEN | the operator item and the InputService item - two departures from the plan's wording, stated in the plan with their reasons, the owner's to accept |
| P02M0181 | OPEN | the three items that belong to or wait on P02M0099's real HID and ACPI producers; the service-first stage is done |
| P02M0182 | COMPLETE | - |
| P02M0183 | OPEN | the run item, on the existing `ipv6-peer` gate alone: its quiet row fails because the new services start ahead of NetworkService; the fix is the owner's decision (move the new services after the existing ones in `manifest.toml` - which this session's permission classifier refused as a change to a shared resource - or give the row's capture more room) |
| P02M0184 | COMPLETE | - |
| P02M0185 | COMPLETE | - |
| P02M0186 | COMPLETE | - |
| P02M0187 | COMPLETE | - |
| P02M0188 | OPEN | the registration item, on `./verify.sh` alone - the owner's run |

Decisions taken in this job that close nothing and block nothing, listed so the owner can overrule any of them:
the kernel's `LIVEVOL` rule - the medium's image is handed over as the live system volume only when the loader
chose no block volume (a boot-chain change, found by the Bluetooth gate's cold reboot); `bootproto::manifest::
MAX_ROWS` 256 -> 512; `MAX_BIND_RESOURCES` 5 -> 6 (the trusted key sink is a sixth keyboard resource);
PermissionManager's probe rows and fixture-control path no longer `cfg`-gated; `btcheck` and `cardcheck` holding
the supervisor's admin channel to stop and restart their service themselves (development-only programs); the
scenario runner's `unordered` expect option; AdminService's `MAX_TESTERS` of four and its epoch rule (the highest
recorded plus one); `kill` and `traceroute` declaring `modem-device-proto` for a shared generic instance.

### P02M0184 at the end

Ticked at the end: the registration item. P02M0184 is COMPLETE: its plan's status line says so, and its row in
`docs/todo/TODO.md` is checked.

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0184 (2026-09-24T07:08:32Z):

Rating: 8/10

The contract, the grant path, the descriptor normalizer, the selection and lease state machine, the stop deadline
and the timing and loss handling are sound, and they match the plan closely. Three things fall short of
what the plan asks for:
- the per-grant completion backlog is not actually bounded;
- a stream that ends before `start` leaves its buffers mapped in the provider;
- the normalizer's colour-matrix table uses the wrong code points.

## Findings

### 1. The waiting-completion backlog is not bounded to the four buffers (defect)

The plan: "Bound completion metadata to the four registered buffers". `camera_service.rs` keeps completions that
did not fit the client's event stream in `GrantConn::frames`. `Service::completion` pushes unconditionally, and
only a successful send in `flush` removes an entry. The bound depends on a comment's assumption ("at most one per
registered buffer ... the camera fills only buffers that were queued"): that a client cannot give a buffer back
before it has heard of its completion.

`Service::release` breaks that assumption. It checks only `Stream::release`, which compares the holder with
`Holder::Leased(lease)`. It neither consults nor removes the waiting entry. Leases are not secret: `Stream::lease`
numbers them 1, 2, 3, ... per stream, and the fixture fills slots in registration order. So a client that does
not read its event stream can do the following:
- fill it (eight entries);
- call `release(buffer, lease)` with predicted leases (a wrong guess costs one `stale`);
- let the provider refill the buffer.

Each round appends another completion for the same buffer, so `frames` grows at the frame rate for as long as
the client keeps this up. This is service-heap growth caused by one granted client, which the item forbids. A
service that runs out of memory takes every camera's capture down with it. While the backlog is non-empty,
`statuses_due` also keeps the loop polling every tick.

A small fix would close it:
- refuse (`again`) a release of a buffer whose completion is still waiting; or
- when a buffer is requeued, drop its waiting entry (its lease no longer matches).

Either keeps `frames` at most `MAX_BUFFERS` long.

### 2. A stream that ends before `start` leaves its buffers mapped in the provider (minor defect)

A stream can end while in `Ready`, after buffers were registered, in three ways:
- the client stops it: `Service::stop` answers at once through `Stream::stop`;
- its grant retires, through close, owner death or revocation: `Service::end_stream` sets `run = None`;
- the provider refuses `start`: `provider_answer` / `Pending::Start` calls `Stream::stopped`.

In none of these cases is the provider told anything. `camera_fixture.rs` maps each buffer in `register`, and
releases the mapping only on `stop`, on the next `negotiate` ("replaces a stream that never started") or on
departure. So up to four client objects (32 MB) stay mapped and referenced by the provider until another client
negotiates on that camera. If the client died, they are objects of a dead client.

This goes against "Release service/producer mappings and handles after confirmed stop". The service and the
contract already have what is needed: `camera-device.stop(stream-generation)` is "answered once every buffer is
back and every mapping of it released", and the fixture accepts it for a stream that never started.

The gate does not cover this path. `camcheck` checks `stats.mapped == 0` only after a started stream (`capture`,
`inherit`). No new grant is exposed: the provider writes only a running stream, and the next negotiation
replaces the slots. So the impact is a bounded retention, not a leak of pixels.

### 3. The UVC colour-matrix code points are mapped wrongly (minor defect)

`uvc::normalize` maps `bMatrixCoefficients` as `1 => Bt709, 4 | 5 | 6 => Bt601`, everything else unknown. In
the UVC colour-matching descriptor, the code points are:
- 1: BT.709;
- 2: FCC;
- 3: BT.470-2 B,G;
- 4: SMPTE 170M, i.e. BT.601, the default;
- 5: SMPTE 240M;
- 6 and above: reserved.

The Linux UVC driver maps these the same way (FCC, B,G and 170M to 601; 240M to its own encoding). The set used
here resembles H.273's numbering, where 5 and 6 are both BT.601. Common cameras (4) and the fixture's two formats
(4, 1) come out right, and 2/3 becoming "unknown" is allowed. However, an SMPTE 240M stream and any reserved value
are reported as BT.601, which contradicts "color-range/matrix metadata with unknown allowed". The later UVC
module will reuse this production helper for real cameras. The table should be `1 => Bt709, 2 | 3 | 4 => Bt601,
_ => Unknown`, and the module test should cover 5.

### Observation (not scored as a defect): the camera alias and the policy rows are compiled in

`ALIASES` (`camera_service.rs`) binds only the fixture's provider name, and PermissionManager's `camera_policy`
names only the gate's probes. Both are in every build. A shipping image binds no fixture, so nothing can be
granted there. This matches the default-deny intent, and is the same shape as the modem's (P02M0183). It means a
real camera cannot be granted without a code change, which is P02M0099's to settle.

## Verified

- **Roles and wiring.**
  - `camera` is value 16 in `device.lsidl`, `driver_protocol::provider::CAMERA` and both of DeviceManager's
    conversions.
  - The service receives a `camera`-only catalogue connection and the SERVE and ADMIN roots, and subscribes to its
    kind.
  - Withdrawal (`lose`) retires every grant bound to the camera; a republished camera is a new `CameraId`
    (slot, generations, incarnation). The gate's `quarantine` scenario proves the latter.
- **Authority.**
  - Inventory (`InventoryView`) has only `cameras` and `sizes`; any other request closes the connection.
  - `mint` resolves an alias to exactly one current publication or fails, and starts nothing.
  - PermissionManager hands the service a `RIGHT_WAIT | RIGHT_TRANSFER` duplicate of the prepared task, and the
    service waits on it. Owner termination, closure, invalid requests and `revoke` all go through `retire`, which
    closes the connection, the owner handle and the event stream, and stops the stream internally. The gate's
    `inherit` and `camfail` show that a duplicated endpoint and a failed prepared launch both end.
  - Admission of observers and grants is 32; admins are 4.
- **Negotiation.**
  - One stream per camera, for any grant; renegotiation needs a retired stream; a negotiation already in flight
    answers busy; a quarantined camera answers `io`.
  - The expected selection is computed before asking (`cs::expect`). The answer is accepted only for the same
    stream generation and when `cs::verify` matches every field, including stride and plane offset. Otherwise no
    stream exists.
- **Buffers.**
  - `register` checks the real object type, size and koid through `object_info`, the per-buffer, per-stream and
    global byte bounds (`Stream::register`) and duplicate backings.
  - Rights are narrowed to map, read, write and transfer (a handle that cannot be narrowed is refused); the IDL's
    `@rights(map, write)` already required map and write.
  - Leases move client → producer → leased on the stream's own checks; stale and double returns are refused.
  - Stop waits for the producer's confirmation. `tick` quarantines after `STOP_TICKS` (200 ticks, 2 s), and the
    stop answers `timed-out`. A late confirmation or the provider's death releases the quarantine.
- **Timing and loss.**
  - The arrival time is the provider's `clock_ns` sample, refused if from the future.
  - Device time is passed through only when `DeviceClock::valid`. Its reset generation advances on reset, domain
    change and every gap.
  - The observation sequence advances for frames, counted drops (no-buffer and device) and gaps. An unexplained
    jump counts as an unknown discontinuity, never as a number of frames.
  - The status is a coalesced snapshot outside the completion queue, sent only after the waiting completions, and
    readable through `status`.
- **The normalizer (`uvc.rs`).**
  - Descriptor lengths (27/11 for formats, 26 + 4n or 38 for frames, 6 for colour) are checked against UVC 1.5.
  - Counts and budgets are enforced: 8 formats, 32 sizes, 32 intervals, and a 64 KB graph.
  - Other checks: zero or overflowing dimensions, a YUY2 size larger than the declared buffer, zero, duplicate or
    disordered intervals, and duplicate indices. Other formats are skipped and counted.
  - `select` returns exactly what was asked for, or nothing.

## Checks performed

Code reading only; nothing was built or run for this review. Files read:
- `camera_service.rs` (whole);
- `service_logic/src/camera_streams.rs` (whole);
- `drivers/core/src/uvc.rs` (whole) and its colour-format test inputs;
- `camera_fixture.rs` (stream, register, stop and release paths);
- `camera.lsidl` and `camera-device.lsidl`;
- PermissionManager's `CameraCapture` minting and `camera_policy`;
- the kind value in `device.lsidl`, `driver_protocol` and DeviceManager;
- the gate script and `camcheck.rs`'s `busy`, `inherit`, `quarantine` and `again` scenarios.

The UVC colour-matching code points were checked against the specification's table and the Linux `uvc_driver.c`
mapping.

---

IMPLEMENTER'S RESPONSE ON P02M0184 (2026-09-24T19:39:29Z):

All three findings were confirmed against the code and fixed. The observation is not taken up as a change.

## Finding 1 - The waiting-completion backlog is not bounded to the four buffers: ACCEPTED

Confirmed as described. There is also a second route the review does not name, which a release-only fix would
have missed:
- A client that never reads its events can stop, renegotiate, register and start again.
- Each new stream then adds up to four more waiting completions, and the entries of the retired stream are
  never removed.

The fix follows the review's second option and extends it to cover that route. `Service::completion`
(`camera_service.rs`) now removes, before it pushes a new frame completion, any waiting frame completion that:
- names the same buffer: the camera refilled it, so that lease was already returned; or
- belongs to another stream generation: a retired stream, whose leases nothing can return any more.

The effect:
- Every entry left names a buffer the client holds in the running stream, so `frames` holds at most one entry per
  registered buffer (`MAX_BUFFERS`).
- The promise that matters is kept: a completion the client still needs, in order to give its buffer back, is
  never dropped.
- `statuses_due` still polls every tick while a completion waits, as before; the list it retries is now bounded.

The comments on `completion` and on the `frames` field now state this rule, instead of the assumption the review
refuted.

## Finding 2 - A stream that ends before `start` leaves its buffers mapped in the provider: ACCEPTED

Confirmed on all three paths. Changes in `camera_service.rs`:
- New `Pending::Discard` and `Service::discard(camera, generation)`. `discard` sends
  `camera-device.stop(generation)` without waiting, and the answer is ignored like `Pending::Queue`'s.
- `Service::stop`, `Ready` phase: the stream is retired and answered at once, as before (the producer never
  wrote to it), and the producer is now told to release its mappings. The `Retired` phase is only answered.
- `Service::end_stream`, `Ready` phase (grant closed, owner dead or revoked): `discard`, then the run is dropped.
  `Retired` is unchanged, because the stop that retired it already released everything.
- `provider_answer`, refused `Pending::Start`: after `stream.stopped()`, `discard` for that generation.

The fixture already accepted `stop` for a stream that never started: `release_all`, `mapped = 0`. None of the
gate's scenarios ends a stream in `Ready` between `delay_stop` and the stop it is meant for, so the quarantine
scenario is unaffected.

## Finding 3 - The UVC colour-matrix code points are mapped wrongly: ACCEPTED, with one deviation

Confirmed: the table used H.273's numbering, where 5 and 6 mean BT.601. In UVC's colour matching descriptor they
are SMPTE 240M and reserved. `drivers/core/src/uvc.rs` now maps:
- `1 => Bt709`;
- `3 | 4 => Bt601`;
- anything else => `Unknown`.

A comment in the code gives UVC's own table.

**The deviation: FCC (2) stays unknown instead of becoming BT.601.** FCC's luma coefficients (Kr 0.30, Kb 0.11)
are not BT.601's (0.299, 0.114), while BT.470-2 B,G's are. Linux substitutes 601 for FCC because a renderer needs
some matrix; this is metadata, and the plan allows "unknown". The fixture's two formats (4 and 1) come out as
before.

New module test: `the_colour_matrix_follows_the_uvc_code_points`, covering codes 0 to 6 and 255.

## Observation - The camera alias and the policy rows are compiled in: REJECTED as a change

This has the same basis as P02M0182, Finding 1:
- the component-to-alias half in PermissionManager (`camera_policy`) is compiled too, and the image is the unit
  of configuration;
- a real camera is granted by a policy row and its alias entry in one change;
- the default-deny and exact-resolution properties hold;
- the real UVC provider is P02M0099's.

## Milestone document

`docs/todo/P02M0184.md` records each fix in one clause, at its item: the frame-type metadata item, the
completion-metadata bound and the mapping-release item.

## Verification

- **Specific to this milestone:**
  - `uvc`: 9/9, including the new colour-matrix test.
  - `qemu-camera-service` passed every row: `capture`, `mjpeg`, `timing`, `busy`, `inherit`, `retain`, `again` and `quarantine`.
  - The mapped-count assertions and the quarantine's held stop are unaffected by the new discard.
- **Static checks (all pass):**
  - `cargo check` of every changed program, in both feature configurations where one is gated;
  - `rustfmt --check` of every changed file;
  - `./check.sh --gate source-hygiene`: clean.
- **Host suites:** `service-logic` 672/672 and the `drivers` library 290/290, new tests included.
- **Builds (all pass):**
  - `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`: 348 s, provider inventory `match`.
  - `./build.sh --arch aarch64`: 342 s.
  - `./build.sh --arch riscv64`: 337 s.
  - The three dynamic programs touched, `bluetooth_service`, `modem_service` and `camera_service`, link against their declared providers on all three targets.
- **Guest gates, one at a time, on a development image** (`LIBER_DEVELOPMENT=1 ./image.sh --format iso`):
  - `bluetooth-service`: PASS, 841 s.
  - `qemu-modem-service`: PASS, 839 s.
  - `qemu-camera-service`: PASS, 400 s.
  - `qemu-midi-service`: PASS, 400 s. These four ran 18:57-19:39Z.
  - `qemu-admin-path`: PASS, 465 s, 18:37-18:45Z.
- **Two earlier attempts at the four service gates tested nothing.** Their images lacked the development probes (`no artifact at vol://system/libexec/btcheck.lsexe`): first `./image.sh` had not been rerun after the build, then it rebuilt a shipping volume because it ran without `LIBER_DEVELOPMENT`. The gates were repeated as above.


---

AUDITOR'S RE-AUDIT ON P02M0184 (2026-09-25T00:27:22Z):

Rating: 10/10

All three fixes are correct and complete. The rejection of the observation is justified, on the same grounds as
P02M0182's. Nothing is unresolved.

The one subtle point was checked in the code. `completion` can drop a waiting completion in only two cases:
- **The same buffer was refilled.** That requires a successful `release`, so the client had already returned the
  lease.
- **The completion is from a replaced stream.** Its lease can no longer be returned.

A completion the client still needs is therefore never lost, and the bound holds at one entry per registered
buffer.

The camera gate passed after the change. The milestone is COMPLETE.
