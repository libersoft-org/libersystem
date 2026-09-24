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
