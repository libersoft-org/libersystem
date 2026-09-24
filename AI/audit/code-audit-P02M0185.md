IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0185 (2026-09-23T06:28:16Z):

Implementation started; this record is updated as the work lands.

## What was implemented

### P02M0185a - vocabulary and serving arrangement

- `src/idl/event.lsidl` (`liber:event@1`, importing nothing): `event-source` (service incarnation,
  publication slot/generation/binding generation, endpoint, receiver generation), `event-header` (sequence,
  `received-ns`), `event-end-reason` (overflow, source-discontinuity, removed, revoked, exhausted, stopped,
  shutdown), `event-end` (reason, next sequence) and `generic-event` (header plus at most 64 payload bytes).
  Crate `event-proto` (depends on `wire` alone).
- `service_logic::bounded_event` (host-tested, 4 tests): `Queue<T>` with 256 events and 32 kB of encoded
  storage checked at admission, sequence numbers in push order, an overflow that discards everything queued
  and keeps the end apart from the queue, a first end that stands, pulls that deliver the end only once
  nothing precedes it, and `exhausted` instead of a wrap. The non-MIDI fixture: the tests instantiate it with
  `liber:event@1`'s `generic-event` (key presses), through `event-proto` as a dev-dependency.
- `src/idl/midi.lsidl` (`liber:midi@1`): endpoint identity and metadata (protocol, direction, cables),
  `midi-chunk` (cable 0..15, kind short / SysEx start / continue / end / raw, at most three bytes, per-cable
  SysEx message number, start/end flags - no group), `midi-abort` with reasons, `midi-fault` with codes and
  an optional cable, `midi-item`, `midi-event` (the generic header beside the item), `midi-batch` (source,
  at most 64 events, the end once reached), receiver status. Interfaces `midi` (inventory: `endpoints`,
  `open` - unsupported for transmit and UMP, denied for receive), `midi-input` (`endpoint`, pull `read(max,
  wait-ms)`, `status`, `stop`) and `midi-admin` (`mint(alias, endpoint, owner)`, `revoke(endpoint)`).
- `src/idl/midi-device.lsidl`: `open` (connection generation, bounds with at most 64 packets), `endpoints`,
  `start`/`stop` per endpoint and receiver generation, `events` (packet batches with the host receipt time,
  and input-lost reports), and the development-only `midi-fixture` control interface.
- Generation and packaging (`gen.sh`, crates `midi-proto`, `midi-device-proto`, the facade, manifest
  sources/libraries); provider kind `midi = 17` everywhere; capabilities `midi` and `midi-input`.
- MidiService (`midi_service.rs`), manifest program/service with `CATALOGUE` (`kinds = ["midi"]`), `SERVE`,
  `ADMIN`; transparent restart; rt `CAP_MIDI`/`CAP_MIDI_ADMIN`; broker arms. Limits: 4 providers, 8
  endpoints each, one receiver per endpoint (a second is refused as busy and the service says so), 32
  client connections.
- PermissionManager: `Midi` resolves the inventory root; `MidiInput` is minted in `grant_for_task` through
  `midi-admin.mint` with a `WAIT | TRANSFER` observer of the prepared task; `midi_policy` has no shipping row.
- The combined catalogue budget: 12 minting roles in the manifest now, 2 x 12 + 1 = 25 of the 32 slots the
  system-manifest check enforces (it passes).

### P02M0185b/c - time, identity, order; chunks and SysEx

- `src/user/drivers/core/src/usb_midi.rs` (host-tested, 8 tests): whole-batch alignment and cable checks
  (unattributable faults reset every cable), per-CIN status/data/padding validation with typed faults, short
  messages, SysEx start/continue/end and one-fragment SysEx under a per-cable message number, the
  single-byte form tracked by the same counters (and not assumed realtime), realtime bytes passing through
  a SysEx, interruption/restart/cap/inactivity aborts by number with the remainder discarded until an end or
  a new start, and the exact 64 kB cap across 21 846 fragments. MidiService decodes every batch with it
  (the services crate now depends on the drivers library for this).
- Every event in a batch keeps that batch's `received-ns` (sampled by the provider when the batch became
  available; a time from the future ends the provider), and the queue numbers events in decode order -
  never sorted. Source identity carries the incarnation, publication, endpoint and receiver generation; a
  late batch naming a retired generation is ignored.

### P02M0185d - queue integrity and lifetime

- Integrity records (aborts, faults) are events in the same accounting; one that cannot be queued ends the
  receiver with `overflow` like data. Overflow, lost input (`source-discontinuity`), withdrawal (`removed`),
  revocation or owner death (`revoked`) and `stop` all discard the queue and the decoder's partial state,
  free the endpoint's slot and stop the provider; the end is what the next read returns. Reads pull and may
  wait (bounded by `wait-ms`); one pending read per receiver.

### P02M0185e - verification

- Fixture `midi_fixture` (development-only, `edu` at 0:25.0): endpoints 0 (two cables) and 1 (one cable);
  delivers exactly the packets the probe scripts, floods, reports loss, withdraws/republishes.
- Probes `midicheck`, `midihold`, `midiread`, `midifail`; gate `check-midi-service.sh` registered as
  `qemu-midi-service` (`check.sh`, `GATES` 134, `GATES_THAT_BOOT_A_GUEST` 40, `release-required.toml` with
  `host.event-proto`, `host.midi-proto`, `host.midi-device-proto`).

## Material decisions

1. The decoder lives in the driver library as the plan says, so the services crate gained a dependency on
   `drivers`; MidiService uses nothing else of it.
2. Receipt times travel inside the queue with each event, so a delayed read returns the time the batch
   arrived with.
3. `Midi`/`MidiInput` sit before the modem authorities in `VOCABULARY` for the same reason as the camera
   ones: `midifail` fails on a later grant (a modem identity grant naming an unconfigured alias).
4. A busy mint cannot reach a client (PermissionManager fails the launch), so the service prints the refusal;
   the gate reads that line.
5. Revocation: `midi-admin.revoke` exists; as with the camera, nothing calls it yet, and the guest proof
   covers revocation by owner death and by an abandoned launch.

## Verification performed (2026-09-23)

- `cargo check`: services (all bins, both configurations), drivers (all bins, `development`): clean.
- Host: `bounded_event` 4, `usb_midi` 8 (drivers lib 286 in all), event-proto 5, midi-proto 11,
  midi-device-proto 7, system-manifest 24, verify-model 157.
- `./gen.sh --accept-breaking` (28 packages); `./check.sh --gate source-hygiene`: clean;
  `check-bootstrap-plan.py`: consistent apart from the pre-existing `font_catalogue` line.

## Not performed yet

- The `qemu-midi-service` gate (development image at the end of the job) and the cross-builds.

CORRECTION TO THE IMPLEMENTER'S RECORD ABOVE (2026-09-23T07:37:10Z): the combined catalogue figure was miscounted.
The manifest had 14 catalogue-minting roles after this milestone, not 12, so the demand was 2 x 14 + 1 = 29 of
the 32 slots (the system-manifest check counted correctly and passed; only the figure written here was wrong).

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

### Found by the MIDI gate's boots, and changed

- **Every receiver grant was refused** (the common CONNECT defect): MidiService accepted CONNECT only on its
  roots. The roots and the admin connections mint admin connections, and an inventory connection mints
  another, each under its existing bound.
- **The handle baseline**: `midicheck`'s `handles` phase is gone; the gate reads the service's count from the
  system graph after `midicheck receive` (PermissionManager's two resolved roots are in it).
- **`midihold` reported into the pipe**; it reports on stderr (common list).
- **The inherit pipeline raced the busy phase.** `midihold dup | midicheck inherit` ran after `midihold hold
  2 &`, whose receiver on endpoint 1 is held for its two seconds and more, so the pipeline's own receiver was
  refused as busy. It now runs before the two holders.
- **The last count was the background holder's, not a leak** (10 after the first probes, 12 at the end, every
  phase passing). A measured boot that read the graph after every step showed 12 exactly while `midihold
  hold 2 &` held its receiver - its stop landed after `midicheck unplug` because the typed lines run faster
  than its hold - and 10 once it had gone. The gate runs `fg` before its last `graph`, and boots with no NIC
  for the same reason as the camera gate.

### Verification

| What | Command | Result |
| --- | --- | --- |
| development image | `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` then `LIBER_DEVELOPMENT=1 ./image.sh` | PASS, 2026-09-23T23:19Z |
| the gate | `./check.sh --gate qemu-midi-service` | **PASS**, 400 s, 2026-09-23T23:56:17Z |

Its lines: `midiread: PASS`; `midicheck: PASS` receive, malformed, cap, unsupported, overflow, lost,
inherit, unplug, fresh; "midihold: held endpoint 1 and stopped"; "midihold: sent its receiver, and exits";
"MidiService: a second receiver on an endpoint was refused as busy - there is no fan-out"; "MidiService: a
receiver's owner ended - its receiver is retired"; "the service's handles returned to their baseline (10)".

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

Ticked in `docs/todo/P02M0185.md` (2026-09-24): twenty-five items. Open: the guest-assertion item, which also
names the three-target cross-builds, and they have not run.

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
