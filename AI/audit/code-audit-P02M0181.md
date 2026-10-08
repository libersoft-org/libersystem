# IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0181 (2026-09-23T02:37:05Z):

## Scope as planned, and the part that cannot close here

The plan separates a SERVICE-FIRST stage (vocabulary, provider contract, PowerService, normalization
leaf, HID/ACPI-shaped fixtures, the `power-service` gate) from FULL COMPLETION, which requires one
actual HID Power Device producer and one actual ACPI battery/AC/thermal producer to publish through
this service. Those producers belong to P02M0099 and wait there on the firmware-node identity and
AML prerequisites; the plan says in its own words that full closure "is explicitly dependent on the
open ACPI work". That item is therefore an external blocker for this milestone's final checkbox and
is reported as one; the service-first stage is what this record implements.

## Implementation record

(updated below as the work proceeds)

### What was implemented (2026-09-23)

**Vocabulary (`src/idl/power.lsidl`, package `liber:power@1`, generated into `power-proto`).**
Three interfaces for three parties: `power` (read: `sources`, `subscribe -> result<stream<power-change>>`),
`power-control` (operator: `set-output`, `schedule-output-off`, `cancel-output-off`, answering
`control-outcome {done, indeterminate}`; an unadvertised control is `error.unsupported`), and
`power-provider` (what a `power-source` publication serves: `updates` stream, `command`, `query`).
Every measurement is `{state, value, reason}` (`value-state`, `invalid-reason`); capacities carry
`quantity {energy, charge}`; temperatures carry `temperature-reference {absolute, relative}`; trips
and alarms are bounded at 16; `charge-state` has `invalid` for conflicting flags; `source-id` is
`{slot, generation, binding-generation, local}`. A development-only `power-fixture` interface and its
`fixture-field`/`fixture-command` types are in the same package, documented as such.
`src/idl/device.lsidl`: `power-source = 12`, `fixture-control = 13`. `src/idl/security.lsidl`:
capabilities `power-state`, `power-control`, `fixture-control`.

**Normalisation leaf: new crate `power-model` (`src/user/libs/power/model`).** A crate of its own
rather than a `service-logic` module because producers (drivers) and the service both link it.
- `convert.rs`: `Tagged<T>`; ACPI `acpi_quantity`, `acpi_milli` (sentinel first, `0x7FFFFFFF` range,
  checked x1000), `acpi_charge_state`, `acpi_rate` (signed by state bits, unknown without direction),
  `acpi_temperature` (absolute `raw*100-273150` checked; relative signed offset x100, every pattern an
  offset); HID `Field`, `hid_logical` (width, sign from logical minimum, null state vs invalid),
  `hid_convert` (SI-linear dimensions, one 128-bit rational, truncation once at the canonical unit,
  temperature offset inside the fraction), `hid_basis_points`, `hid_capacity` (Unit or CapacityMode,
  contradiction when they disagree).
- `canon.rs`: record constructors, `unmeasured`, `state_of_charge` (explicit first, remaining/full,
  never design, remaining > full is a contradiction), `below_threshold` (`<=`), `at_or_above` (`>=`,
  same reference), `reported`/`derived`, `alarm_transition`, `validate`.
- `acpi.rs`: `battery` (`_STA`/`_BIF`/`_BST`; rate as power or current by the power unit; reported
  critical from `_BST` bit 2; derived low/critical against the platform's warning/low levels; an
  empty slot measures nothing), `ac` (`_STA`/`_PSR`), `thermal` (`_TMP`/`_RTV`/`_CRT`/`_HOT`/`_PSV`/
  `_ACx`, derived over-temperature against the critical trip only).
- `hid.rs`: `ups` from decoded usages (capacity by CapacityMode or Unit, explicit relative state of
  charge preferred, signed current by field sign or by the charging/discharging flags, active power
  as delivered = negative, percent load, kelvin; reported alarms per status usage present; derived
  low capacity against RemainingCapacityLimit; controls from switchable outlets and
  DelayBeforeShutdown).

**Service state machine: `service_logic::power_registry`** (generic over a `Payload`), holding
sources (sorted), providers (snapshot phase, revision, live/retired/refused local masks, one control
slot), subscribers (queue of 32, per-source 100 ms window, pinned additions/removals/alarm
transitions, overflow and 5 s stall closure), and the control ladder (`control`, `control_answered`,
`query_answered`, `tick` -> `Effect::{Indeterminate, Query, ProviderFailed}`, `next_deadline`).

**PowerService: `src/user/services/core/src/power_service.rs`.** Roles CATALOGUE (factory from
DeviceManager's CATADMIN, kinds `power-source`), SERVE (`power`), CONTROL (`power-control`); no
system-power client. Adopts publications from the kind-scoped catalogue subscription (limit eight,
refusal printed), opens each provider's update stream with a bounded deadline, validates every
record with `power_model::canon::validate`, and ends a provider on any refusal. Controls are decoded
with the generated dispatch into a capture view and answered later by hand-encoded `result` frames;
requests to providers are sent non-blocking; replies are matched by correlation and late ones dropped.
`subscribe` builds every snapshot frame and a channel of snapshot + 8 before registering the
subscriber. Undecodable requests close the connection. Epoch from `random_get` per instance.

**Wiring.** Manifest: `[[sources]] power-model`; `[[programs]] power_service`; `[[services]]
power_service` (transparent, reconstructible, deps log_service/device_manager); `power_fixture`
registry entry (development, plain-pci 0:30.0, quirk, dma none, provides power-source x2 +
fixture-control x1); probes `powercheck`, `powerread` (development). `rt`: `CAP_POWER_STATE`,
`CAP_POWER_CONTROL`. ServiceManager: broker grants to PermissionManager, `service_of_cap`,
`serve_resolve` roots, `plan_relaunchable` includes `power_service`; PermissionManager's catalogue
scope is widened with `FIXTURE_CONTROL` under `cfg(feature = "development")` only. PermissionManager:
vocabulary 27 -> 30, tags, held clients, resolvable grant arms, a development-only
`FixtureControl` arm opening the fixture's endpoint through the catalogue, and development-only
policy rows `powercheck` (state, control, fixture-control) and `powerread` (state).
DeviceManager: wire<->typed mapping for the new kinds; a development self-test
`catalogue_cap_refusal`. `system-manifest`: `FixtureControl` kind, `MAX_CATALOGUE_CLIENTS = 32`,
the combined catalogue budget check (2 slots per minting role + DeviceManager's own), refusal of
`fixture-control` on any role and on any non-development driver. Drivers: `Serving::publish` for a
post-handshake replacement publication; `power-model` dependency.

**Fixture: `src/user/drivers/core/src/power_fixture.rs`.** Publishes a HID-shaped UPS provider
(local 0) and an ACPI-shaped provider (battery 0, AC 1, zone 2) from decoded data written out in
`Model::new`, every record built by `power_model`'s adapters; the control endpoint changes decoded
inputs, removes a source, withdraws and republishes a publication (new token, new generation),
withholds the next N command/query replies (logging the command as received), offers a third
`power-source` publication past its declaration, and reports the command log and outlet states.

**Probes and gate.** `powercheck` (list/control/denied/watch/alarm/coalesce/overflow/withhold/extra/
remove), `powerread` (control/publish down a read connection). `src/tools/check-power-service.sh`
(one boot with `-device edu,addr=0x1e`, the scenario typed on the console, a restart with epoch
comparison). Registered in `check.sh`, `verify-model` catalogue (`userspace.build`), and the
release-required set (`gate.power-service`, `host.power-model`).

### Decisions not fixed by the plan

- PermissionManager reaches PowerService's roots by name through the broker (as with Bluetooth): a
  bootstrap role would make PermissionManager depend on PowerService.
- No LOG/TIME role: the service needs neither beyond `print` and the kernel's monotonic clock.
- The fixture's control endpoint is a provider kind (`fixture-control`), opened by PermissionManager
  through a development-only widening of its catalogue scope; the manifest refuses the kind on any
  role and on any shipping driver. The `power-fixture` wire lives in `liber:power@1`, documented as
  development-only.
- "Unauthorized publisher" is tested as (a) the fixture offering a publication past its declaration,
  which DeviceManager must refuse, and (b) a read client sending a publication or control frame on
  its connection, which the service must close.
- "Mismatched generations": providers never carry a generation on the wire; the publication's is the
  catalogue's, `open` refuses a stale one, and a control naming a stale or forged generation is
  `denied`.
- `subscribe` has no parameters, so "clamp subscriber requests" reduces to the fixed limits.
- A source uncertain after an indeterminate control is settled ONLY by a query's answer; a control on
  an uncertain or unavailable source starts a fresh query if none is outstanding (never re-sends the
  control). Refusal mapping: denied->`denied`, not advertised->`unsupported`, range->`invalid`,
  busy/reconciling->`again`, unavailable->`io`.
- The eight-provider x sixteen-local limits make the 128-source cap reachable only when every
  provider is full; the admission check is kept and the refusal of a ninth provider is what is
  reported as exhausted.

### Defects found in P02M0180's landed work along the way (fixed here)

- `device_manager::provider_kind_from_wire` had no arm for `BLUETOOTH_HCI` (nor the new kinds), so a
  `bluetooth-hci` publication's subscription frames would have said `block`. Fixed.
- The combined-manifest catalogue budget and a refusal-at-the-cap test, both named in the catalogue
  item P02M0180 ticked, did not exist. Added now (see Wiring).
- `system-manifest`'s production DMA-policy test failed on `bt_fixture` (`none`). Fixed by naming the
  two fixtures.
- `check-bluetooth-service.sh` piped into `head` under pipefail, which `source-hygiene` refuses.
  Fixed with `grep -m 1`.

### Verification at this point

| what | command | result |
| --- | --- | --- |
| normalisation leaf | `cargo test --manifest-path src/user/libs/power/model/Cargo.toml` | PASS, 37 |
| service-logic (incl. `power_registry`, 15) | `cargo test --manifest-path src/user/services/logic/Cargo.toml` | PASS, 544 |
| manifest tool (incl. 2 new rules) | `cargo test` in `src/tools/system-manifest` | PASS, 24 |
| drivers library | `cargo test --manifest-path src/user/drivers/core/Cargo.toml` | PASS, 258 |
| power-proto | `cargo test --manifest-path src/user/libs/protocol/power-proto/Cargo.toml` | PASS, 30 |
| generation | `./gen.sh --accept-breaking` / `./gen.sh` | PASS, 20 packages |
| services type-check, both configurations | `cargo check --manifest-path core/Cargo.toml --bins [--features development]` | PASS |
| fixture type-check | `cargo check --bin power_fixture --features development` (drivers) | PASS |
| verification model | `cargo run --manifest-path tools/verify-model/Cargo.toml -- check` | PASS ("model is consistent") |
| verification model tests | `cargo test --manifest-path tools/verify-model/Cargo.toml` | PASS, 157 |
| declared interfaces | `./check.sh --gate declared-interfaces` | PASS, 45 interfaces, 13 kinds |
| source hygiene | `./check.sh --gate source-hygiene` | PASS (after the bt-gate fix) |
| development-gate manifest checks | `bash src/tools/check-development-gate.sh --manifest-only` | PASS, 10 programs |
| gate script syntax | `bash -n src/tools/check-power-service.sh` | PASS |
| bootstrap plan | `python3 src/tools/check-bootstrap-plan.py` | FAIL on the pre-existing `font_catalogue` mismatch only; `power_service` is in both the ladder and the plan |

**NOT PERFORMED**: the `power-service` gate (needs `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`,
deferred to the end of the job per the owner's instruction); the target build of the new programs;
aarch64/riscv64 cross-builds (deferred, slow architectures). The "guest-evidence profiles" the plan
names do not exist as an artifact in this tree; selection is the catalogue's `userspace.build` key.

### Blockers

- The final item (real HID and ACPI producers through this service) is blocked outside this
  milestone on P02M0099's HID Power Device driver and its firmware-node identity and AML
  prerequisites.

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

### Found by the power gate's boots, and changed

- **PowerService never started.** It is launched from the volume, and its manifest row did not declare
  `process_service`, so ServiceManager tried to launch it before ProcessService existed. The dependency is
  declared, as it now is for the smart-card, camera and MIDI services.
- **A withheld reply's source never came back.** A refused control queues the fresh query that
  reconciles an unavailable source, and the query leaves on the registry's tick; PowerService ticked before
  serving, so the query waited for the next deadline - its own - and was sent as it expired, its answer
  dropped. The loop is now wait, serve, tick and apply, then drain the subscribers.
- **The restart check read one epoch.** `powercheck` printed the service's random `u64` epoch as an `i64`,
  so half of all epochs printed negative and the gate's pattern skipped them; printed unsigned.
- The fixture row's class, the ungated policy rows and the gate's log self-copy are in the common list.

### Verification

| What | Command | Result |
| --- | --- | --- |
| the gate, first pass | `./check.sh --gate power-service` on a development image (`LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`, `LIBER_DEVELOPMENT=1 ./image.sh`) | **PASS**, 360 s, 2026-09-23T17:29Z |
| the gate, on the job's final image (built 2026-09-23T23:17Z, imaged 23:19Z) | `./check.sh --gate power-service` | **PASS**, 360 s, 2026-09-24T00:16:18Z |

Its lines: `powercheck: PASS` list (exact units for the HID-shaped UPS and the ACPI-shaped battery, adapter
and zone), control, denied, watch, alarm, coalesce, overflow, withhold, extra, remove, and list after the
restart ("the restarted service reconstructed its sources under a new epoch"); `powerread: PASS` for the read
client and for a publication sent on a read connection; "power_fixture offered more providers of one kind
than it declares in `provides`; refused".

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

Ticked in `docs/todo/P02M0181.md` (2026-09-24): the service-first stage - thirteen items. Open: the
registration item (cross-builds not run), and the three items that belong to or wait on P02M0099's real HID
Power Device and ACPI producers, which this milestone must not absorb.

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

### P02M0181 in this continuation

No code of this milestone changed in this continuation. Its registration item waits only on the cross-builds,
which run last (below). The three items that belong to or wait on P02M0099's real HID and ACPI producers stay
open, as the plan requires: the service-first gate is not full completion.

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

### P02M0181 at the end

Ticked at the end: the registration item, its cross-builds now built. P02M0181 stays OPEN on the three items that
belong to or wait on P02M0099's real HID and ACPI producers, as its plan requires.

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0181 (2026-09-24T06:46:31Z):

Rating: 9/10

The service-first stage is complete and correct as far as this review could find. No defect was found that needs a
code change in what was implemented. The point is withheld because the milestone is not complete: its three
remaining items depend on P02M0099's real HID Power Device and ACPI producers. The plan makes that explicit, and the
implementer correctly left those items open rather than redefining completion.

## Findings

No implementation defect.

Open by the plan's own terms, not a code defect of this implementation:
- P02M0099 owns HID report decoding and the ACPI namespace prerequisites.
- The final integration item (real HID and ACPI producers through this service with live client assertions) cannot
  be done until those land.

## Verified

- **Separate authorities.** `liber:power@1` has separate state, control and provider interfaces. In
  `power_service.rs`, the root a connection is minted from decides its interface. A state connection has no
  control operation, and an unknown operation closes the connection, so a reader cannot smuggle a control.
  Bootstrap roles are the `power-source`-scoped catalogue connection and the two roots; there is no system-power
  client.
- **Provider protocol (`service_logic::power_registry`).**
  - A snapshot must end before any change is accepted. Duplicate locals, locals outside 0..16, revision regressions
    and updates for unregistered or removed locals end the provider.
  - Every record is checked with `power_model::canon::validate` before the registry sees it, and a non-canonical
    record ends the provider. The same check applies to reconciliation replies.
  - Provider loss, withdrawal, protocol error or snapshot timeout (`SNAPSHOT_TICKS`) removes every source it
    published, with a removal each, and completes an outstanding control as indeterminate.
  - A replacement publication has another generation and so other source keys.
- **Bounds.** 128 sources, eight providers and 16 subscribers are enforced. A source beyond the cap is refused,
  remembered as refused and reported, not admitted by eviction. Enumeration is sorted by key.
- **Subscriptions.**
  - `subscribe` builds every snapshot frame and allocates a channel deep enough for them plus `LIVE_DEPTH`
    before registering the subscriber. Registration happens in the same loop step the snapshot was read in, so
    nothing falls between the two.
  - The per-subscriber queue holds 32 records. Ordinary measurements coalesce, and are emitted at most once per
    100 ms per source.
  - Additions, removals and alarm transitions are pinned. A removal drops only unread, unpinned measurements of its
    source.
  - Overflow, or no drain for five seconds, closes the subscription, and the reader learns that from the channel
    closing.
- **Controls.**
  - A control is checked against a live source key (slot, publication generation, binding generation, local), so
    forged or stale identities are denied. The advertised operation is checked, outlet in range, delay capped at
    86400 s.
  - One control per provider may be outstanding.
  - The command is sent without blocking. A deadline makes it indeterminate, and it is never retried.
  - The source is then uncertain until a fresh query (itself on a five-second deadline) answers. A late reply is
    dropped, not delivered twice.
  - Continued silence leaves the controls unavailable, while other providers and state clients carry on.
  - A send that failed is released as `again`, which is correct because nothing could have been delivered.
- **Normalisation (`power_model`).** Checked in `acpi.rs` and `convert`:
  - The ACPI unknown sentinel is honoured before arithmetic.
  - Capacities are scaled by 1000 in the unit's own quantity.
  - The `_BST` rate is published as power or current according to the power unit, signed by the state bits.
  - State of charge comes from last-full, never design, capacity.
  - Absolute thermal readings are `raw * 100 - 273150`, and relative ones keep their reference.
  - The one derived over-temperature alarm compares against the critical trip on the same reference.
  - Reported and derived alarms are separate entries.
- **Fixture confinement.** The fixture control authority is granted only to `powercheck`. A shipping build mints no
  catalogue connection admitting the fixture kind.
- **Registration.** The gate is registered in `check.sh`, the verify-model catalogue (including the guest-booting
  list) and `release-required.toml`.

## Checks performed

Code reading only; nothing was built or run for this review. Files read: `power_service.rs` (whole),
`service_logic/src/power_registry.rs` (whole), `libs/power/model/src/acpi.rs` (whole), the relevant parts of
`convert` and `canon`, the PermissionManager rows, and the registration lists.

---

IMPLEMENTER'S RESPONSE ON P02M0181 (2026-09-24T19:39:29Z):

## Findings: none raised, and none to accept or reject

The review reports no implementation defect, and nothing in it calls for a code change. No code change was made
for this milestone.

## The open items: agreed

The three remaining items stay open on P02M0099's producers, as the review states:
- HID Power Device report decoding;
- the ACPI namespace prerequisites;
- the final integration with real producers and live client assertions.

Completion was not redefined to close them.

The code this milestone shares with the other reviewed milestones is unchanged by this round's fixes:
- the catalogue scope;
- the bounded launch;
- `power_service.rs`;
- `service_logic::power_registry`.

---

AUDITOR'S RE-AUDIT ON P02M0181 (2026-09-25T00:27:22Z):

Rating: 9/10

No unresolved issue, incorrect fix or regression was found.

This round changed no code of this milestone. Commit `fda3d3dd` touches none of its files, so the response's
statement is accurate.

One claim the first review verified was checked again: `subscribe` does its work in this order.
1. It refuses at the subscriber bound before any other work.
2. It builds and queues the whole snapshot on a channel sized for the snapshot plus `LIVE_DEPTH`.
3. Only then does it call `registry.subscribe()`.

The point stays withheld for one reason only: the three items that wait on P02M0099's producers are still open,
as the plan's own terms require.

---

IMPLEMENTER'S RESPONSE TO RE-AUDIT ON P02M0181 (2026-09-25T01:01:42Z):

The re-audit raised no finding, so there is nothing to accept or reject, and no code of this milestone changed in
this round.

The three open items stay open until P02M0099's producers land, as the plan requires. By its own terms, the
milestone is not complete until then.

---

IMPLEMENTER'S CONTINUATION ON P02M0181 (2026-09-29T06:48:03Z):

## What this round is

The three items that waited on P02M0099's real producers. Both producers have now landed in P02M0099: the USB HID
Power Device class (2026-09-25) and the ACPI battery, AC and thermal classes (2026-09-29, the `acpi_power`
driver). This round runs each through the real PowerService with a live client and keeps the evidence in a gate.

## Implemented

- THE ACPI PRODUCER'S EVIDENCE is the `acpi` gate (`src/tools/check-acpi.sh`), extended with P02M0099's item:
  - the fixture SSDT's battery, AC adapter and thermal zone, their methods reading the harness pages, bound by
    `acpi_power` and published to PowerService;
  - `acpipower` (`src/user/services/core/src/acpipower.rs`), a client holding the read authority alone:
    - `list`: exact canonical units;
    - `watch`: subscribed while the gate changes the pages and raises the power line - a METHOD CHANGE reaching
      the service and the live client as updates;
    - `storm`: sixty-odd `Notify` events coalesced by the driver, the last reading delivered;
  - and the values read again after the ACPI service restarts. The details are in P02M0099's audit.
- THE HID PRODUCER'S EVIDENCE is a new gate, `power-ups` (`src/tools/check-power-ups.sh`):
  - The device is a USB UPS the host builds - `usb-gadget.sh setup ups` under the owner's gadget permission of
    2026-09-21 and its rules, with `ups-sim.py` as its firmware - on root port 3 of the development guest's xHCI
    controller.
  - It is bound by the xHCI driver's HID Power Device class and published to the real PowerService.
  - It is read and driven by a new probe, `upscheck` (`src/user/services/core/src/upscheck.rs`), which holds the
    read and control authorities:
    - `list`: the one UPS with the device's first report in exact units - 80 % as 8000 bp, 3600 s, 13.80 V from
      the feature report - and exactly the controls its descriptor carries;
    - `control`: subscribed, it sends three commands through PowerService's operator interface:
      - an output switch the UPS does not advertise: refused `unsupported` by the service;
      - a scheduled turn-off: `done`, and the device's next REPORT - mains gone, discharging, 79 %, 1800 s, the
        on-battery alarm - arrives at the subscriber as an update after the snapshot's revision;
      - the cancel: `done`, and the device back on mains arrives the same way.
  - The gate also reads what the DEVICE received, from its firmware's log: one turn-off and one cancel, and no
    request it has no control for (so the refused switch was never sent).
  - The gadget is torn down and `usb-gadget.sh verify` checked on every exit.
  - The gate FAILS, rather than skips, without root or the host's gadget modules.
- REGISTRATION:
  - `check.sh` `["power-ups"]`;
  - the verify-model catalogue `("power-ups", "userspace.build")` - `GATES` 151 and `GATES_THAT_BOOT_A_GUEST` 54;
  - `release-required.toml` `gate.power-ups`;
  - `upscheck` and `acpipower` in the shell table, the synopses, PermissionManager (`upscheck`:
    `PowerState` + `PowerControl`; `acpipower`: `PowerState`), the services crate (development-only bins) and
    the manifest (probe rows, development).

## Found and fixed

THE HID PRODUCER NEVER REACHED THE SERVICE ON A REAL SYSTEM:
- The first `power-ups` run failed: "PowerService: a provider is gone: it broke the provider protocol", then no UPS.
- The cause: `class_power.rs` advanced its revision on every frame, so its snapshot and the snapshot's end named
  two revisions, and the registry refuses that (`Refusal::Order`) and ends the provider.
- The kernel oracle plays PowerService itself and never read revisions.
- Fixed in P02M0099's class: a change alone advances the revision. The oracle now asserts the revisions.
- This is exactly the gap the final integration item exists to close.

## Verification (2026-09-29, x86_64; commands from the repository root)

`./check.sh --gate acpi` - PASS, 383 s:
- exact units;
- the live client's three updates after the power line;
- 61 storm events reaching the client as 7 zone updates;
- the values read again after the service restart.

`./check.sh --gate power-ups`:
- FAILED on the first run (the defect above);
- PASS after the fix, 483 s:
  - the probe's lines `updated ups online 0 discharging 1 soc 7900 bp runtime 1800 s voltage 13800000 uV
    on-battery 1` and `... online 1 discharging 0 ... runtime 3600 s ... on-battery 0`;
  - the firmware's log: one turn-off, one cancel;
  - the host left carrying nothing of the gadget's.

The UPS kernel oracle with `USB_GADGET=ups` - PASS, 1 passed (37 s), with the new revision assertions.

Host suites and checks:
- drivers 419 and system-manifest 28;
- `cargo check` of `upscheck`, `acpipower` and `xhci`;
- the test kernel's build;
- `bash -n`/`shfmt -d` of both gate scripts.

`cargo test` of verify-model:
- compiles with the new catalogue sizes;
- its tests fail as before this round, all on the model load ("kernel test
  `the_global_clock_advances_once_per_period_however_many_cores_tick` ... has no `tagged_test!` declaration"),
  which no change of this round touches.

NOT RUN:
- the aarch64 and riscv64 cross-builds of the new probes and the fixed class (the end of the job, with the
  other slow-target work);
- the dynamic report;
- `verify.sh`.

## Blockers

None for the three items.

The milestone's own text keeps no suspend or platform policy. It stays open only for the cross-builds above.

## The gates after P02M0197 and P02M0198 (2026-10-01)

- `LIBER_DEVELOPMENT=1 ./check.sh --gate power-ups` -> PASS.
- `power-service` FAILED once in the batch of 2026-10-01 with three of its fifteen lines typed: the console driver's
  budget (`GUEST_GATE_SECONDS`, 200 s) covers the boot as well, and the development boot now takes 118 s from the
  firmware to the shell on this machine (75 s from the kernel's first line, 41 s of it StorageService copying the live
  system volume into memory). The same gate run alone passed; its budgets are 300 s and 480 s now
  (`src/tools/check-power-service.sh`, the reason in its comment), and it passed again in the next batch (481 s in all).
  The boot's length is not this milestone's; it is said here because it is what the budget was paying for.
- (2026-10-01) `./build.sh --arch aarch64` and `--arch riscv64` build whole, the probes and the fixed class among
  them; `power-service`, `power-ups` and `acpi` pass on x86_64. Status: COMPLETE.


# IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0181 (2026-10-08T03:26:07Z):

Read the complete plan and traced normalization, provider streams, bounded subscriptions, control replies, and prior integration evidence. Found a real missing failure transition: a provider may explicitly return ControlOutcome::Indeterminate (the HID UPS driver does so), but PowerService currently treats every decoded reply as a definite completion and admits another control before querying fresh state. Implementing and verifying that reconciliation path; the previous completion label alone is insufficient.

Verification in this continuation: not yet run. Final results and remaining work will be appended below.

Implemented `Registry::control_indeterminate` and routed the provider's explicit uncertain outcome through it; late/duplicate replies cannot settle it, ordinary updates do not reconcile it, and only a fresh query re-enables controls. Added a host regression including silent-query expiry and recovery. Public read/subscription/connection replies now use nonblocking sends; a full read reply queue closes that client and allocated stream handles are released. Added fixture-only `power-fixture.indeterminate`, regenerated bindings, and `powercheck indeterminate` / `stalled` assertions in the existing power-service gate. Corrected the stale policy comment to the already implemented owner-approved critical-battery default.

Checks so far: `cargo test --manifest-path src/user/services/logic/Cargo.toml power_registry -- --nocapture` PASS (16); `cargo test --manifest-path src/user/libs/power/model/Cargo.toml` PASS (51); `./gen.sh` PASS (38 packages). Initial `cargo check` from `src` FAILED because it used the host target (no_std panic unwinding); rerunning from `src/user`, whose `.cargo/config.toml` selects the freestanding target, PASS for `power_service`, `powercheck`, and `power_fixture` with `--features development`. Guest regressions and final builds not yet run.

Final-stage fast checks: complete `cargo test --manifest-path src/user/services/logic/Cargo.toml` PASS (948 passed, 1 existing ignored codec probe); `./gen.sh --check` PASS; gates `verify-model` (consistent), `verify-model-tests` (157), `milestone-index`, `grant-vocabulary`, `declared-interfaces`, and `gate-oracles` PASS. Initial `source-hygiene` FAILED on the pre-existing `arch/common/console_uart.rs` plus `console_uart/tests.rs` layout; the P02M0191 continuation moves the unchanged production file to `console_uart/mod.rs`, following the checked project convention. Final hygiene and guest results remain pending.

Independent implementation review by the performance worker found no defect in the new uncertainty transition, handle cleanup, or stalled-client regression. This is a review observation, not a substitute for guest execution.

The hygiene rerun exposed five pre-existing early-closing `head` pipeline readers in three unrelated gate diagnostics (`check-bluetooth-le-audio.sh`, `check-bluetooth-transfer.sh`, `check-brightness-acpi.sh`). Minimal adjacent check repair: replace only those readers with equivalent `sed -n` selection that drains its input, preserving each diagnostic/result. This is necessary for the plan's mandatory source-hygiene check; no Bluetooth or brightness behavior changed. Shell syntax verification PASS.

First final build command `LIBER_DEVELOPMENT=1 ./build.sh --arch all` FAILED at 208 s because rustc itself received SIGSEGV compiling unchanged `lsmem`; exact retry is in progress, with x86_64 now built. The first `power-service` guest attempt used the old `libersystem.iso`: `build.sh` compiles/stages the test volume, while x86 `run.sh` deliberately boots the separately assembled ISO. The new `indeterminate` and `stalled` verbs consequently returned the old probe's usage error. This run is FAILED/invalid for the new implementation, not a pass; its guest log was saved, and the owned run was stopped after the failure was established. Assembling the current ISO with `LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode enforcing-required` before rerunning the gate. No implementation was changed to hide this artifact-selection error.

Current-ISO verification: `LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode enforcing-required` PASS; `LIBER_DEVELOPMENT=1 ./check.sh --gate power-service` PASS (153 s), including both new probes and every existing oracle. All scripted commands had completed, including the final listing after restart; the owned guest and console driver were then stopped before their idle timeout, and the unchanged gate assertions ran to successful completion. The full transcript is `.build/logs/end-of-job/continuation-power-service-current-guest.log`; the terminal result is in `continuation-power-service-current.log` beside it. `source-hygiene` also PASS after the narrowly documented layout/pipeline repairs. Real HID and ACPI integration regressions and the final cross-build are still pending at this point.

Real HID regression: `LIBER_DEVELOPMENT=1 ./check.sh --gate power-ups` PASS (129 s). The real USB HID descriptor/reports and one scheduled turn-off plus one cancel reached the device, then the changed reports reached a live PowerService subscriber. The host gadget was torn down and verified; the owned guest/console were stopped only after both scripted probes completed, before the idle timeout, and all original gate assertions passed. Logs: `.build/logs/end-of-job/continuation-power-ups.log`, `continuation-power-ups-guest.log`, and `.build/logs/power-ups/`.

The retry of `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1158 s), covering this milestone on x86_64, aarch64 and riscv64. A subsequent final cached refresh is running because the independent P02M0201 review found and corrected an SSIF deadline issue after the earlier x86/ARM stages; no PowerService code changed after its passing guest regression. Final ACPI integration verification remains pending.

### Final continuation state for P02M0181

`./check.sh --gate acpi` PASS (410 s), log `.build/logs/end-of-job/continuation-acpi-x86.log`: real AML power-method changes and the update storm reach the live power client; ACPI restart rereads the classes and keeps the namespace; the complete gate also verifies firmware/native hotplug ownership. Together with the current `power-service` and `power-ups` passes above, every required producer/service interaction is verified.

Final compiled-source `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (487 s; `.build/logs/end-of-job/continuation-build-all-final.log`). A subsequently found Python-only vhost fixture page-boundary defect was corrected and verified in P02M0195/P02M0196/P02M0202; it does not change these compiled artifacts. Final `./check.sh --gate source-hygiene` PASS (93 s; `continuation-hygiene-vhost.log`); final model validation and 157 model tests PASS in the combined 205 s static run, with generation already checked after the additive fixture operation.

Passed host verification: power registry 16, power model 51, full service logic 948 with one pre-existing ignored codec probe. Passed guest verification: `power-service`, `power-ups`, `acpi`. The first compiler crash and the invalid stale-ISO attempt are retained above as failed attempts, not passing evidence. No required implementation, verification or blocker remains for this milestone; its plan and TODO entry are COMPLETE.

### Additional adoption failure exposed by the required ARM TCPCI integration

The ARM TCPCI gate failed despite the driver publishing its connector: the initial update-stream opening timed out, the closed connection had not yet been reaped against the one-consumer limit, and the next catalogue opening failed permanently. Independent review found the same `adopt` code in PowerService. Both also measured the update-stream deadline and snapshot admission from a timestamp captured before the potentially slow catalogue request. Reopening this milestone for the minimal equivalent correction: keep the existing bounded seven-retry policy for catalogue failures too, and start each operation/registry budget from its actual stage. No deadline is increased. Implementation and fresh verification follow below; the preceding completion statement is superseded by this newly observed integration defect.

Implemented the adoption correction in `Power::adopt`: both opening stages use the existing bounded retry policy, retry delays start at actual failure, stream opening gets its own unchanged one-second budget, and snapshot admission uses the time of successful opening. The shared retry tests PASS (3), the target `cargo check` of `power_service` PASS, and rustfmt PASS. The TypeC peer review also identified an uncovered subscription-refusal case: when capacity was exhausted and the reply queue full, PowerService dropped the refusal without closing the request connection. Refusal sends now propagate failure to the existing client-close path, and `powercheck stalled` tests both unread enumeration and unread subscription replies. This extends the existing regression without changing its deadline or adding a protocol operation. All runtime sources for this correction are now frozen pending guest and target-build results.

Latest-service target verification (2026-10-08T04:49:04Z): from `src/user`, `cargo build --manifest-path services/core/Cargo.toml --target <triple> --bin power_service --bin powercheck --bin typec_service --bin typeccheck --features development` PASS for `x86_64-unknown-none`, `aarch64-unknown-none`, and `riscv64gc-unknown-none-elf`. Logs: `.build/logs/end-of-job/continuation-service-refresh-{x86_64,aarch64,riscv64}.log`. These compile the new adoption/refusal code and expanded guest probes; they do not themselves restage the boot images. Fresh image and current guest verification remain in progress.

The subsequent independent IO review found a second, pre-existing violation of deadline isolation: catalogue opening uses an unbounded synchronous transport, and update-stream opening blocks the main loop for each provider. Several silent openings can defer an already-active five-second control deadline. The source freeze is reopened for asynchronous admission using the existing captured-request/pending-reply pattern; the shared catalogue also carries platform-switch openings, so those replies must be dispatched together to preserve correlation. The latest image build PASS (301 s) and source-hygiene PASS (93 s) precede this correction and are not final verification of it. No guest pass is claimed for these intervening artifacts.

### Provider-adoption nonblocking continuation (independent review, 2026-10-08)

Source review found an additional integration failure: catalogue `open` used `Client::new` (no deadline) and `updates` used a synchronous transport, while all due retries ran before the registry's timeout tick. A silent new provider could therefore delay controls and state readers of already-adopted providers. The platform-switch policy shared this catalogue connection and performed the same synchronous open/watch handshake, so its replies cannot safely coexist with an asynchronous power-only opener. Implementing a bounded, correlation-matched opening table with per-stage deadlines and explicit late/withdrawal cleanup, shared by the two provider kinds and reused by TypeCService. The existing initial catalogue subscriptions remain startup operations; unrelated policy actions are outside this adoption change. Verification will be appended after actual execution.

IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0181 (2026-10-08T04:55:22Z):

Guest-regression support for the separately owned asynchronous adoption correction: reviewed the existing fixture's provider dispatch, publication generations, probe control deadlines, and the full service gate. The old adoption blocks the service loop in its one-second update-stream open. A fixture counter will establish that the blocking request actually arrived, and one shared 500 ms bound will cover an existing UPS control plus enumeration. This record covers fixture/probe/schema changes only; the PowerService implementation is owned and recorded separately. Existing op 9 and prior audit content are preserved. No guest result for this new regression is claimed yet.

Implemented the guest regression support: additive fixture-only `power-fixture` op 10, `defer-updates`, preserves op 9 and returns a per-publication cumulative count across generations. `power_fixture::serve` validates an actual UPDATES opening before incrementing that counter and dropping its reply; no stream is allocated for the withheld opening. `powercheck adoption` withdraws and republishes the ACPI provider, waits on that independent counter, then gives the existing UPS control and enumeration one shared 500 ms deadline. It releases the fault and requires retry recovery, then repeats with withdrawal during a counted pending opening and checks no sources return until another explicit publication. The full `power-service` gate now runs this phase and requires its PASS plus the fixture's withheld-opening marker. No PowerService source was edited by this subtask.

Passed targeted verification: `./gen.sh` (27 s, `.build/logs/end-of-job/continuation-gen-adoption.log`); from `src/user`, `cargo check --manifest-path drivers/core/Cargo.toml --bin power_fixture --features development` (1.41 s) and `cargo check --manifest-path services/core/Cargo.toml --bin powercheck --features development` (1.63 s); rustfmt, Bash syntax/shfmt and `git diff --check` of the edited fixture/probe/schema/gate. Failed-before and passed-after guest execution remain UNPERFORMED, scheduled with the root implementer against the saved old service and the asynchronous correction. Compilation alone is not regression evidence.

The admission review also exposed synchronous policy RPCs sharing PowerService's loop. Implemented a minimal adjacent correction in `power_service/policy.rs`: one outstanding policy request, nonblocking sending, existing two-second reply budget, unique non-reused correlations, late/unexpected capability disposal, and deadline/peer-close completion. Policy input streams apply backpressure while an action is pending; provider discovery and power readers/controls continue. The existing lid/external-display decisions, suspend selection, hibernation-refusal fallback, forced-power-off-before-orderly-shutdown sequence, defaults and diagnostics are retained. Startup configuration/initial subscriptions are outside this live-loop conversion. No new policy/event queue or default is introduced. This affects P02M0197 integration, so current platform/battery guest evidence and review are still required before this continuation can claim those paths verified.

### Nonblocking adoption implementation and targeted verification (2026-10-08T05:06:03Z)

Implemented `service_logic::provider_open::Opens`/`Pending`: a bounded set of catalogue and private-stream opening stages with non-reused correlations, fresh per-stage deadlines, exact channel/stage matching, explicit withdrawal/expiry/closed-channel removal and ownership returned to the runtime. Added `ProviderRetry::contains` and `due_one` so a full shared opening set leaves due retries queued without consuming an attempt. TypeCService reuses this helper; it introduces no runtime/protocol dependency into the host-testable logic crate.

`Power::adopt`, `opening_reply`, `open_failed`, `cancel_open` and the main wait/deadline loop now send both requests nonblockingly and process replies independently of already-live power clients. Power openings retain the one-second stage budget and existing seven retries; platform-switch openings retain the existing two-second budget and one-attempt behavior. Both kinds share the catalogue correlation dispatcher, preventing a synchronous switch client from consuming a power reply. Eight concurrent handshakes are bounded separately from the existing eight-power-provider admission limit (active + opening + queued retry); no new active-switch limit was added. When concurrent slots are occupied, announcement streams remain unread, and each readiness consumes one announcement, so transient congestion does not permanently discard switches. Registry deadlines and policy deadlines are included in the same wait and ticked every turn.

Catalogue success is accepted only with the matching unexpired correlation, canonical result encoding, exactly one capability and complete decoding. Stream success requires the matching private channel/correlation and exactly its single stream capability. Late, malformed, expired and withdrawn-generation replies release transferred capabilities. Withdrawal cancels an opening and its retry; peer closure cancels owned resources. Incoming unexpected capabilities on PowerService's CONNECT/HEARTBEAT early return paths are now closed as well. The separate root-owned policy-action conversion is documented in P02M0197; startup configuration and subscriptions still occur before the normal serving loop.

Verification actually executed: `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml provider_ -- --nocapture` PASS (25 tests; six opening-table regressions, four retry regressions and the matching existing provider/control cases; compilation 5.34 s). Cases cover one silent publication alongside a progressing one, both stage deadlines, incorrect channel/correlation, withdrawal and replacement generations, closed private-channel cleanup, bounded capacity/correlation exhaustion, and retry backpressure without attempt loss. `(cd src/user/services/core && cargo check --bin power_service --features development)` PASS (2.03 s). Rustfmt on changed adoption/helper files and `git diff --check` PASS. An initial root-directory `cargo test -p service-logic ...` invocation failed because the repository root has no Cargo.toml; the explicit manifest command above is the successful execution. Independent TypeC-worker review found no new wrapper, admission, correlation or main-loop defect.

Not yet performed for this new asynchronous implementation: fail-before/pass-after guest regression, rebuilt three-architecture artifacts, refreshed hygiene/model checks and final runtime guest interactions. Earlier synchronous-adoption guest/build passes above remain historical evidence and do not close these new requirements.

Final frozen-source static verification: `./check.sh --gate source-hygiene --gate verify-model --gate verify-model-tests` PASS (208 s; `.build/logs/end-of-job/continuation-static-async.log`), including consistent model validation and all 157 model tests. `./gen.sh --check` PASS (19 s; `continuation-generation-async.log`), all 38 packages and generated profiles without drift. These results include asynchronous admission, policy IO and the additive opening-failure fixture; the all-target build and guest regressions are still running/pending.

Actual failed-before verification of asynchronous adoption (2026-10-08): preserved the freshly built corrected `power_service` ELF (SHA-256 `85ea4965e8a5c82f8b9a209edb19e4902b0db4ff17c8bf9a8a0413f69e78a76c`), atomically substituted the saved pre-async ELF (`cc6ee8f51b7347cd21a6ef5f09e836b219146265fc39fd22d3978a6e9bd37b88`), and assembled a private negative ISO with the current fixture/probe. `LIBER_DEVELOPMENT=1 LIBER_KERNEL_STRIP=all ./build.sh --arch x86_64 --part packages,volume --kernel-on-volume --dma-mode enforcing-required` PASS (74 s), followed from `src` by `LIBER_DEVELOPMENT=1 LIBER_IMAGE_OUTPUT=<private.iso> LIBER_DMA_MODE=enforcing-required STRIP=all harness/mkimage.sh iso <current-kernel>` PASS. Immediately restored the corrected ELF byte-for-byte, repeated current packages/volume (PASS 72 s), and assembled the ordinary current ISO before any guest boot. Source files were never reverted; atomic replacement avoided altering Cargo's hardlinked dependency ELF. Input/image hashes and all packaging logs are in `.build/logs/end-of-job/power-adoption-regression/`.

The private negative guest ran only `powercheck adoption`. A scratch copy of `guest-gate.sh` differed solely by passing `--image` for that private ISO; it did not change the console or probe. The fixture observed an actual update-stream opening, then the old synchronous service produced the expected `powercheck: FAIL adoption: a pending provider opening blocked an existing UPS control`. The command returned to the prompt, after which only the owned QEMU and console driver were stopped; the negative assertion script confirmed the exact failure and rejected any adoption PASS. Full evidence: `negative-guest.log`, `negative-run.log`, `negative-result.log` under the directory above. This is an expected regression failure, not a passing service gate. The full ordinary `power-service` gate on the corrected current ISO is now running; its result remains pending.

Full current service-logic host regression: `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml` PASS (955 passed, 0 failed, one pre-existing ignored codec probe; tests 11.94 s). Log `.build/logs/end-of-job/continuation-service-logic-async.log`. This includes the new asynchronous-opening and retry cases and the existing policy/control suites.

Passed-after verification: `LIBER_DEVELOPMENT=1 ./check.sh --gate power-service` PASS (111 s) on the restored current ordinary ISO. All 18 scripted commands completed and the final `powercheck list` returned to its prompt. The new adoption probe observed two real withheld openings, completed the existing UPS control plus enumeration within its shared 500 ms deadline, recovered through retry, cancelled a withdrawn pending generation, and admitted a subsequent explicit fresh publication. Both stalled enumeration/subscription cases and all existing units, permissions, controls, uncertainty/reconciliation, coalescing/overflow, provider removal and service-restart assertions also passed. Full transcript and immutable image receipt are `current-guest.log` and `current-run.log`; terminal evidence is `current-power-service.log` in `.build/logs/end-of-job/power-adoption-regression/`. Only the owned QEMU and console driver were stopped, after every scripted command and the final prompt, to avoid the remaining idle timeout; the maintained gate oracle and cleanup then ran unchanged to PASS. The current ELF, packages, bootable volume and default ISO remain restored. This completes the new adoption regression's failed-before/passed-after proof; broader cross-target/static and policy verification are recorded separately by their owners.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

Current-policy guest regression PASS: `LIBER_DEVELOPMENT=1 bash .build/logs/end-of-job/check-sleep-policy-async.sh`. This retained the original `check-sleep.sh` platform and battery functions and their assertions, with the original helpers; only the scratch script location/log destination and initially absent follower were adapted. It is a selected two-boot regression, not a claim that the entire sleep gate was rerun. Evidence: `.build/logs/end-of-job/continuation-sleep-policy-async.log` and the complete `sleep-policy-async/` logs beside it.

Verified on the asynchronous policy/admission code: all firmware switch providers, default lid screen-off/on, configured lid S3 after policy restart, TAD timed S3 and D-state/resource transitions, both fan drivers across idle/S3 and commanded-level restoration, disabled/default sleep-button behavior, and orderly power-button power-off with every driver stopped before revocation. The real ACPI battery then drove the unchanged default critical-battery no-op; after configuration and PowerService restart it drove orderly power-off, QEMU gone 2388 ms after the ten-second forced deadline was armed. The copied original assertions and cleanup completed successfully. No source changed during or after this verification.

Real HID integration after asynchronous adoption: `LIBER_DEVELOPMENT=1 ./check.sh --gate power-ups` PASS (94 s), booting the same corrected ordinary ISO (SHA-256 `ddb8e63b14a3eac14a752fd0c1579ed6c3a18a2afc83513c008ca7f1776fcf81`). The real USB HID Power Device descriptor/reports reached PowerService in exact units; the unsupported outlet operation was refused without reaching the device; exactly one scheduled turn-off and one cancel reached the gadget firmware; both on-battery and restored-mains reports reached the live subscriber. Both scripted probes and the final prompt completed before only the owned QEMU/console were stopped; the unchanged gate oracle then passed. The gate stopped its simulator, tore down its gadget and verified cleanup. A separate `src/harness/usb-gadget.sh verify` also PASS: the host carries nothing of this harness. Evidence: `.build/logs/end-of-job/continuation-power-ups-async.log`, `continuation-power-ups-async-{guest,run,script,driver}.log`, `continuation-power-ups-async-cleanup.log`, and `.build/logs/power-ups/{gadget,ups-sim,guest}.log`. This supersedes the earlier HID pass for the final adoption code.

Final P02M0181 continuation state: COMPLETE. Current implementation, all-target compiled artifacts, host/static/generated checks, complete service gate and both actual producer paths are verified as recorded above. The final real ACPI path includes live battery/mains changes and policy restart through the original platform/battery scenarios; the earlier full ACPI update-storm gate remains explicitly earlier evidence. The last real HID gate passed after the asynchronous admission change, including independent host gadget cleanup. Prior failed builds, stale-image attempt and original-service regression failure remain recorded as such; no failed/unrun gate is counted as a pass. Plan and TODO status now reflect this result.
