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

### P02M0185 in this continuation

No code of this milestone changed in this continuation. Its guest-assertion item - the only one open - waits only
on its host tests, generated-binding and manifest checks and the cross-builds, which run below.

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

### P02M0185 at the end

Ticked at the end: the guest-assertion item. P02M0185 is COMPLETE: its plan's status line says so, and its row in
`docs/todo/TODO.md` is checked.

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0185 (2026-09-24T07:11:56Z):

Rating: 9/10

The following all do what the plan says:
- the generic queue (`service_logic::bounded_event`);
- the staged USB-MIDI 1.0 decoder (`drivers::usb_midi`);
- MidiService's receivers, admission and endings;
- the grant path.

I checked the decoder code index by code index against the USB MIDI 1.0 table, and found no misclassification.
One path does not end a receiver when the plan and the code's own comment say it should.

## Findings

### 1. A start the provider refuses leaves the receiver active and silent (minor defect)

`AdminView::mint` sends `midi-device.start(endpoint, generation)` without waiting, registers the receiver as
`active`, and answers the grant. The provider's reply arrives in `Service::on_reply`, and a refusal is handled
there as follows:

    // A start the provider refused ends that receiver: it will never deliver.
    if reader.tag() == Some(false) {
        print(b"MidiService: a MIDI device refused to start or stop an endpoint\n");
    }

It only prints. `Provider::sent` keeps bare correlations, so the reply cannot be matched to a receiver, and the
receiver is never ended. The consequences:
- The endpoint's one receiver slot stays taken. `mint` refuses a second receiver as busy while one is `active`.
- The client's reads return empty batches with no `end` until the client stops it or its owner ends.

This goes against "loss/removal cannot be silent" and "reclaiming slots after failed opens". The fixture never
refuses a `start` for an endpoint it advertises, so the gate cannot show this. A real provider (P02M0099) can
refuse, for example on a transfer setup failure.

Keeping the receiver generation with the correlation, and ending that receiver (e.g. `removed` or
`source-discontinuity`) on a refused start, would make the code do what its comment says.

## Verified

- **`bounded_event`.**
  - The 256-event and 32 kB limits are checked at admission. The caller supplies each event's encoded size.
  - An overflow terminates the stream: the queue is cleared, and the end is kept outside it and returned only
    after the events queued before it.
  - The first end stands.
  - The sequence is assigned in push order, never sorted, and `u64::MAX` ends the stream with `exhausted` instead
    of wrapping.
  - The module knows no MIDI and no `rt`. Its tests run a key-press `generic-event` fixture through it.
- **Decoder.** Every CIN is checked against the specification's table:
  - 0/1 are reserved faults.
  - 2 is F1/F3 plus one data byte, with padding checked. 3 is F2 plus two data bytes.
  - 4 is a SysEx start or continue. 5 is F7 as an end, or F6.
  - 6/7 are two- and three-byte ends, including one-fragment F0..F7.
  - 8..E require the status nibble to equal the CIN, with data bytes and padding checked per length.
  - F is the single-byte form, with SysEx tracking identical to the packet form, realtime bytes passed as short
    messages, and any other byte as `raw`.
  - SysEx is counted, never assembled.
  - The 64 kB cap includes delimiters and uses checked accumulation.
  - An abort is raised for restart, interruption by a non-realtime status, a fault on the cable, and two idle
    seconds (200 ticks).
  - After an abort, the remainder is discarded until an end or a new start, and a discarded end is never reported
    as a successful end.
  - A bad alignment or an out-of-range cable resets every cable and emits a fault with no cable. A fault on a
    known cable resets only that cable.
- **Service.**
  - At most four providers, eight endpoints each, and 1..=16 cables per endpoint. An advertisement outside these
    is refused whole.
  - One active receiver per endpoint (a second is `again`), and 32 connections.
  - Inventory's `open` answers `unsupported` for transmit or UMP, and `denied` for receive.
  - A batch is accepted only for the active receiver with the same provider, endpoint and generation, and
    refused if it is from the future.
  - Every decoder output, including aborts and faults, goes through the same bounded queue with the batch's
    receipt time. An overflow ends the receiver and stops the provider.
  - A provider's `lost` ends the receiver with `source-discontinuity`.
  - Withdrawal or provider loss ends every receiver on it with `removed`, and the connection stays readable.
  - `revoke` ends with `revoked`. The owner's end retires the receiver: its connection is closed and so is its
    observer.
  - Reads pull at most 64 events, one waiting read at a time, and replies are sent without blocking.
- **Grants.**
  - PermissionManager passes a `RIGHT_WAIT | RIGHT_TRANSFER` duplicate of the prepared task.
  - A mint resolves the alias to exactly one publication and the index to one of its endpoints.
  - The source identity carries the incarnation, the publication and binding generations, the endpoint and the
    receiver generation.

## Checks performed

Code reading only; nothing was built or run for this review. Files read:
- `midi_service.rs` (whole);
- `service_logic/src/bounded_event.rs` (whole) and its test names;
- `drivers/core/src/usb_midi.rs` (whole) and its test names;
- `event.lsidl`, `midi.lsidl` and `midi-device.lsidl`;
- `midi_fixture.rs`'s `start`, `stop` and `deliver`;
- PermissionManager's `midi_policy` and `MidiInput` minting.

I also checked where the `development` feature applies. Static services are built by `build.sh` with
`$(dev_features)`, so the gated `ALIASES` is present in a development image. Dynamic programs are built with
`shared-image` only, which is why the camera's alias is not gated.

---

IMPLEMENTER'S RESPONSE ON P02M0185 (2026-09-24T19:39:29Z):

## Finding 1 - A start the provider refuses leaves the receiver active and silent: ACCEPTED

Confirmed. `Provider::sent` kept bare correlations, so `on_reply` could not tell which receiver a refusal belonged
to. It printed the refusal, and the receiver kept its endpoint's only slot while its reader waited for nothing.
Changes in `midi_service.rs`:
- **The correlation carries the receiver.** `Provider::sent` is now `Vec<(u32, Option<(u32, u64)>)>`: the
  correlation, plus, for a `start`, the endpoint and receiver generation it was for. `Service::send` takes that
  second value. `AdminView::mint` passes `Some((endpoint, generation))`, and `Service::end`'s `stop` passes
  `None`. The 32-entry bound on `sent` is unchanged.
- **A refused start ends its receiver.** `Service::on_reply`, for a refused `start`, finds the active receiver
  with that provider, endpoint and generation, marks it inactive and ends it through `Service::end` with
  `removed`. That means:
  - the queue terminates, so a pending or next read receives the end;
  - partial SysEx is discarded;
  - the endpoint's slot is free again, because `mint` checks only active receivers.
- **No stop is sent** for a start that never took, because the receiver is marked inactive before `end` runs.
- **Why `removed`:** `event.lsidl` defines it as "the source was withdrawn or its provider failed", which is what
  a refused start is. `source-discontinuity` claims input was lost from a stream that was running.
- A refused `stop` is still only printed: its receiver has already ended.

The code now does what its comment said: "A start the provider refused ends that receiver: it will never
deliver". The fixture never refuses a start for an endpoint it advertises, so the gate still cannot trigger this
path. The code was reviewed and type-checked; the gate was run to confirm nothing it covers moved.

## Milestone document

`docs/todo/P02M0185.md` records the change at the item on terminal outcomes and freed slots.

## Verification

- **Specific to this milestone:**
  - `qemu-midi-service` passed every row: `receive`, `malformed`, `cap`, `unsupported`, `overflow`, `lost`, `inherit`, `unplug` and `fresh`.
  - The refused-start path itself is not reachable with the fixture, as stated above.
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

AUDITOR'S RE-AUDIT ON P02M0185 (2026-09-25T00:27:22Z):

Rating: 10/10

The fix is correct and complete. When a provider refuses a `start`:
1. The refusal is matched to its receiver by provider, endpoint and receiver generation.
2. The receiver is marked inactive before `end` runs, so no `stop` is sent.
3. The receiver ends with `removed`. That frees the endpoint's slot and delivers the end to a pending or next
   read.

The index shadowed inside `on_reply` is local to its block. The loop recomputes the provider index on every
iteration.

The fixture cannot exercise this path. By review the code is correct, and the MIDI gate shows no regression. The
milestone is COMPLETE.
