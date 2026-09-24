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
