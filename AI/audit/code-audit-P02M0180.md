# P02M0180 implementer notes

## The shared catalogue stage (2026-09-22)

This is the milestone's second contract item and it is deliberately first in the work: it is the one
piece P02M0181 through P02M0187 all need, and none of them should wait for a Bluetooth stack to get
it.

**What the shape was.** `provider-catalogue`'s root answers the reserved CONNECT opcode. That
opcode takes no arguments, so the connection it mints cannot be narrowed - every consumer got the
whole closed vocabulary, and the only thing between that authority and its use was the consumer's
own restraint. Nine manifest rows mint one on an ordinary boot.

**What it is now.** A second serve root, `provider-catalogue-admin`, whose one operation takes the
kind subset. Its root is delivered to ServiceManager alone, because the supervisor is the only
program that knows which service was declared to need which kinds - the manifest row is that
declaration - so the minting authority never reaches a service.

**Three things were worth more than the feature.**

1. THE ENFORCEMENT MUST READ THE SERVER'S RECORD, NOT THE REQUEST. `open` takes a `provider-info`
   whose `kind` field the caller filled in. The lookup already requires that field to match the
   entry at that slot and generation, so checking the caller's field would have been correct today
   and silently wrong the first time that lookup was relaxed. It reads the entry.

2. THE REFUSAL HAS TO NAME THE KIND AND THE SUBSET. The first enforced boot printed one refusal and
   nothing else, and nothing in the tree could say which of nine consumers had produced it - six
   manifest rows were candidates and all six looked right. Adding the kind narrowed it to `usb-bus`
   and adding the subset (`0`, the inventory connection) said it was not a manifest row at all: it
   was a hand-written bootstrap branch minting through the catalogue's own root. A manifest change
   could not have reached it and no test would have noticed, because until the subset was
   enforceable an unrestricted connection behaved exactly like a correct one.

3. A FIXED BUFFER IS A PANIC AND NOT A TRUNCATION. The diagnostic above was written into a 96-byte
   array whose two literals are 113 bytes between them. The index panicked, DeviceManager died
   mid-bring-up, and the boot stopped with every driver bound and no error anywhere - which reads
   exactly like the hang the previous mistake produced, from an entirely different cause. Both were
   found by reading the log rather than by a test, which is worth remembering about this program:
   its failures are silences.

**What is not covered.** The in-guest denial test runs in the development configuration, like the
three tests beside it in that module; the shipping boot image does not run any of them. The
`development-build` gate compiles that configuration and the scenarios harness replays it.

## The host-testable leaf (2026-09-22)

Six modules, 39 tests. The order they were written in is the order they depend on each other, and
each was checked against a document before the next was started.

**Where the vectors came from.** FIPS-197 for AES, RFC 4493 for CMAC, Core Appendix D for f4/f5/f6/g2
and for the Core's own `e` sample. The owner approved fetching the specification for the Appendix D
sample data; the fetch was a read of a public document and nothing left this machine.

**One vector was written from memory and was wrong.** A second Core `e` sample, typed before the
document was fetched, failed - while FIPS-197 Appendix B, FIPS-197 C.1, the all-zero known answer
and the first Core sample all passed, so the cipher was right and the expectation was not. It was
removed rather than corrected to whatever the code produced. That is the whole argument for the
rule the test files state: an expected value that cannot be sourced proves only that the
implementation agrees with whoever typed it.

**What the document gave that memory could not.**

- The f5 salt could be CONFIRMED rather than asserted, because D.3 prints the intermediate `T`.
  Nothing else in the derivation would have caught a wrong salt - every downstream value would have
  been wrong together and consistently.
- Which of the two f5 counters is the MacKey and which is the LTK. The flattened table is ambiguous
  about which label belongs to which block; D.4 resolves it, because f6's sample is keyed with the
  number counter zero produces. A guess here is the defect that still completes a pairing.
- `keyID` is `62746c65`, which is ASCII `btle` - one letter away from what memory offered.

**What is deliberately absent.** No P-256: the controller owns it, and the milestone requires a
controller that does. No inverse AES: CMAC is forward-only, and code that exists and is never
exercised is code nothing would notice breaking. No constant-time claim: the S-box is a table, the
module says so in its own header, and the threat this service faces is a peer on a radio rather
than a process that can measure a cache.

---

# IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0180 (2026-09-23T01:35:41Z):

The sections above this line were written while the work was in progress and are preserved as they
stand. This section is the implementation record proper and is updated as the work continues.

## What is implemented and verified

**The shared catalogue stage (milestone section "Provider and client contracts", item 2).** DONE.

- `src/idl/device.lsidl`: `provider-catalogue-admin` with `open-consumer(kinds)`; `provider-kind`
  gained `bluetooth-hci = 11`.
- `src/user/services/logic/src/catalogue_scope.rs`: the scope decision (`Scope::of`, `admits`,
  `inventory`, `bits`/`from_bits`), 5 host tests.
- `src/user/services/core/src/device_manager.rs`: `CatalogueClients` carries a scope per slot and
  moves it on `retire`; `channel_pair_for_catalogue(clients, scope)`; `CatalogueAdminView` serving
  `open_consumer`; `serve_catalogue_admin_once`; the `CATADMIN` serve root in the one wait;
  enforcement in `open_subscription` and `CatalogueView::open`; `MAX_CATALOGUE_CLIENTS` 8 to 32.
- `src/user/services/core/src/service_manager/bootstrap.rs`: `mint_scoped_consumer`, used by the
  `RoleKind::Factory` arm when the role declares kinds, and by the hand-written PermissionManager
  branch.
- `src/tools/system-manifest/src/lib.rs`: `kinds` on a role, validated (factory-only, bounded at
  `MAX_ROLE_KINDS` = 16, duplicates refused), emitted into the generated plan. 3 host tests.
- `src/user/services/manifest.toml`: the `CATADMIN` serve root on device_manager; six consumer rows
  moved to it with their kinds.
- `src/user/services/core/src/device_manager/tests.rs`: `catalogue_scope_denial`, an in-guest test
  (development configuration).

**The host-testable leaf (milestone section "Verification and completion", item 1, partially).**
Six modules in `src/user/services/logic/src/`, 39 host tests, none depending on `rt` or
`ipc-client`:

| module | what it holds | vectors |
| --- | --- | --- |
| `aes` | AES-128, encryption only | FIPS-197 Appendix B and C.1, Core `e` sample |
| `cmac` | AES-CMAC | all four RFC 4493 examples, both subkeys |
| `smp` | f4, f5, f6, g2 | Core Appendix D.2-D.5 |
| `hci` | packet kind, direction, ceilings, three credit counters, session epoch | none needed |
| `l2cap` | ACL fragment reassembly, bounded, with an assembly deadline | none needed |
| `hogp` | boot mouse report, protocol mode, CCCD value | none needed |

**The client contracts (milestone section "Provider and client contracts", items 4 and 6, and the
scopes item).** `src/idl/bluetooth.lsidl` defines `bluetooth`, `bluetooth-operator`,
`bluetooth-profile` and `bluetooth-bond-store`; `src/idl/device.lsidl` defines `hci-transport`
with its packet kinds, attachment record, control events and reset. A 19th protocol package
(`bluetooth-proto`) was added, wired into `gen.sh`, the aggregate `proto` crate and its facade.

## Verification performed so far

| what | command | result |
| --- | --- | --- |
| driver mutation gate | `./check.sh --gate driver-mutations` | PASS, 89 of 89 |
| service-logic host tests | `cargo test --manifest-path src/user/services/logic/Cargo.toml --lib` | PASS, 496 |
| manifest tool host tests | `cargo test --manifest-path src/tools/system-manifest/Cargo.toml --lib` | PASS, 22 |
| interface declarations | `./check.sh --gate declared-interfaces` | PASS, 41 interfaces, 11 kinds |
| source hygiene | `./check.sh --gate source-hygiene` | PASS |
| milestone index | `./check.sh --gate milestone-index` | PASS |
| development configuration | `./check.sh --gate development-build` | PASS |
| x86_64 image | `./build.sh --arch x86_64` | PASS |
| x86_64 boot | `./test.sh --arch x86_64 --tags boot` | PASS, 16 tests |
| every rescoped consumer | `./test.sh --arch x86_64 --tags usb,storage,input,display,audio,audio-service,network,permission-service` | PASS, 91 tests |

NOT YET PERFORMED: the milestone's own `bluetooth-service` guest gate, which does not exist yet;
aarch64 and riscv64 cross-builds.

## Remaining work on P02M0180

BluetoothService itself, the bond store service and its storage backing, the ProcessService bounded
resource limits, the InputService additional source slot, the in-guest controller/peer fixture, the
guest gate and its registration in the verification model.

## Progress update (2026-09-23T02:13:11Z)

### Implemented since the record was started

**The rest of the host-testable leaf.** Five more modules in `src/user/services/logic/src/`:

| module | what it holds | tests |
| --- | --- | --- |
| `bond_store` | the on-disk record, its FNV-1a corruption check (explicitly not a MAC), version and address-kind refusals, the 64-record / 64 kB bounds, replace-not-append | 10 |
| `hci_codec` | command encoding, event decoding with length validation, advertising report walk, the two byte-order boundaries (address, public key) | 8 |
| `smp_pairing` | the initiator state machine: Just Works, LE Secure Connections, no downgrade, confirm and check verification, reflected-key refusal, out-of-order refusal | 8 |
| `gatt_mouse` | the boot mouse discovery: HID service, protocol mode, boot report, its own CCCD, boot mode then notifications | 6 |
| `hogp` (extended) | `Pointer`, the relative-to-absolute fold with the drivers' own range | +1 |

`att` gained `Entries::with_entry` for Find Information's format byte.

**ProcessService** (`src/idl/process.lsidl`, `src/user/services/core/src/process_service.rs`):
`resource-limits` record (six fields) and `launch-prepared-limited` (@op 11). `limited_domain`
refuses `u64::MAX` in any field and zero in the five that must admit something; DMA may be zero.
Three limits are the Domain's birth (`domain_create`) and three are properties
(`domain_set_limit`); a refused assignment closes the Domain and fails the launch. No fallback.

**The bond store service** (`src/user/services/core/src/bluetooth_bond_store.rs`): serves
`bluetooth-bond-store`; reads the file at start (a missing file is an empty store; an unreadable one
is said out loud); every mutation replaces the whole file through `open-writer(replace)` + chunked
`write` + `commit`, and rolls the in-memory table back if the commit fails. `list` never carries
key material. Restart policy `escalate` - see decisions below.

**BluetoothService** (`src/user/services/core/src/bluetooth_service.rs`, ~1500 lines): the IO shell
around the leaf. Controller adoption from the kind-scoped catalogue subscription (at most two);
the HCI initialisation sequence one command at a time; a bounded command queue (16) with at most one
command outstanding and credits returned only on the controller's own completion events; ACL/L2CAP
reassembly per link; SMP over CID 6 driving `Initiator`; GATT over CID 4 driving `Discovery`;
notifications decoded and written to report streams; the three interfaces served from three roots
with the root deciding the interface; `open-mouse` as a stream whose producer is kept on the link;
reconnect to an enabled bonded peer after init, encrypting with the stored LTK and proving
encryption before discovery; scan (capped 10 s, 64 deduplicated results), pairing (capped 60 s, one
live attempt), reassembly deadlines; epoch filtering; reset/fault/removal handling.

**InputService** (`src/user/services/core/src/input_service.rs`): a `Bluetooth` source slot. It
reads the `BLUETOOTH` role last, asks `enabled()`, opens `open-mouse` for the first enabled
peer, folds each report through `hogp::Pointer` into the same five-byte record every driver
pointer produces and sends it down the one existing path (`map_event` + `state.record` + forward to
ConsoleService). A closed stream retries on a two-second deadline; a dead profile connection is
re-resolved by name through the broker.

**Kernel harness callers** (`src/kernel/test_suites/services.rs`): the three tests that bootstrap
InputService by hand now send `BLUETOOTH` with no handle. (A first pass also added it to the
DisplayService and NetworkService harnesses, which also send `CATALOGUE`; that was caught and
reverted before anything ran - it would have broken both.)

**ServiceManager restart ladder and broker** (`service_manager.rs`, `service_manager/bootstrap.rs`):
`Kept` moves into `Broker` after bring-up; `plan_relaunchable` + `relaunch_planned` re-run a
plan-driven service's bootstrap (closing the dead instance's serve roots first via
`Kept::release_serve_roots`); `launch_service_from_volume` is the single launch entry point so a
relaunch cannot drop a service's limits; three resolve names (`BTREAD`, `BTOPERATOR`,
`BLUETOOTH`) served from `broker.kept`; grants for PermissionManager (read, operator) and
InputService (profile). `src/tools/check-bootstrap-plan.py` reads `plan_relaunchable` too.

**Manifest**: programs and services for `bluetooth_service` and `bluetooth_bond_store`; the
`bluetooth-proto` library and source rows; InputService's `BLUETOOTH` role and dependency.

### Material decisions, stated for the audit

1. **The platform radio allow/deny setting the plan names does not exist in this tree.** Nothing
   configures one, and inventing one would be the invented radio policy the plan forbids. The
   existing platform-level allow/deny for a device is DeviceManager's persistent device policy
   (disable/enable): a disabled controller has no binding and no provider, so it is not in
   `controllers` and a `power` naming it is `not-found`. `power` itself controls whether this
   host uses a controller it has. If the owner means a separate radio setting, it is a new decision.

2. **InputService's bootstrap role is a hard dependency, and that has a cost.** The manifest
   validator makes every role from a service a dependency on it, with no optional escape; so
   InputService now starts after BluetoothService, and a Bluetooth stack that never starts is an
   input service that never starts. The plan asks for a bootstrap client explicitly ("update every
   bootstrap caller, including kernel harness callers"), so this was implemented as specified and the
   coupling is recorded rather than worked around. BluetoothService starts with no controller and no
   bonds for that reason.

3. **PermissionManager reaches BluetoothService through the broker, not through bootstrap roles.**
   The same validator rule would make PermissionManager - which launches every tool - wait for the
   radio stack. Resolving by name is the existing pattern for restartable services and keeps grants
   working across a BluetoothService restart.

4. **The bond store is `escalate`, not `transparent`.** Its one client holds a connection handed
   over at bring-up and has no resolve for it, so a restarted store would serve a root nobody holds.

5. **In-guest fixture crypto.** The plan's sentence "cryptographic expected transcripts/keys are
   independent fixtures, not expectations generated by the code under test" is satisfied for the host
   tests (Core Appendix D, RFC 4493, FIPS-197). A peer emulator must compute its half of a pairing
   against a host nonce that comes from system randomness, so it cannot be a static transcript; the
   in-guest gate's assertions are about observable effects (bonded state, encryption, cursor motion,
   bond reuse, forget), not key values.

### Pre-existing defect found, not fixed (out of scope)

`./check.sh --gate bootstrap-plan` fails on `font_catalogue`: the manifest declares it
`transparent` and `relaunch_service` has never been able to re-run its bootstrap. Both facts are in
HEAD's sources (`git show HEAD:src/user/services/manifest.toml` and HEAD's `relaunch_service`), so
the gate was red before this work - reasoned from HEAD's sources, not by running the gate at HEAD.
After this work the gate's only mismatch is that one; `bluetooth_service` is on both sides.

### Verification since the last update

| what | command | result |
| --- | --- | --- |
| service-logic host tests | `cargo test --manifest-path src/user/services/logic/Cargo.toml --lib` | PASS (see final count below) |
| every service binary | `cargo check --manifest-path src/user/services/core/Cargo.toml` (from `src/user/services`) | PASS, warnings denied |
| test kernel | `cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests` | PASS |
| bootstrap plan | `./check.sh --gate bootstrap-plan` | FAIL, only on the pre-existing `font_catalogue` mismatch |

NOT PERFORMED YET: any image build or guest run with these services in it (deferred to the end of the
job per the owner's instruction); the fixture and the gate do not exist yet.

## Progress update (2026-09-23T02:36:29Z) - the fixture, the probes, the gate

### Implemented

**The in-guest fixture.** `src/user/drivers/core/src/bt_peer.rs` (pure, host-tested) and
`src/user/drivers/core/src/bt_fixture.rs` (the driver binary, `required-features = ["development"]`).
The peer's AES-128 (with an S-box GENERATED from the field rather than typed), CMAC and f4/f5/f6 are
a separate implementation from the host's, held to FIPS-197, RFC 4493 and Core Appendix D by 6 host
tests of its own. The driver binds QEMU's `edu` test function at 0:29.0 (registry: plain-pci,
class 0xff/0x00/0x00, address-pinned, `quirk`, `dma = "none"`, `development = true`), publishes
`bluetooth-hci`, emulates the controller (init commands, P-256 key, DHKey, scan/advertising,
connect, disconnect, encryption) and the LE boot mouse (SMP responder, the HOGP GATT table, CCCD
write refused over an unencrypted link, a 34-step report script with eight full-scale steps a leg
so a cell-grid consumer sees motion). It prints pairing and encryption events with a KEY FINGERPRINT
(4 bytes of a CMAC under the key), never the key.

**The probes** (static, services crate, `required-features = ["development"]`, manifest
`development = true`): `btcheck` (pair, reuse, forget, limits, refund, exhaust, loss) and
`btread` (read authority only). Policy rows in PermissionManager: `btcheck` - Device,
DevicePolicy, Input, Process, Bluetooth, BluetoothOperator; `btread` - Bluetooth.

**The gate** `src/tools/check-bluetooth-service.sh`, registered in `check.sh`,
`verify-model/src/catalog.rs` (`userspace.build`) and `release-required.toml`: two boots on one
disk (`RUN_DISK` in the gate's work directory), `QEMU_EXTRA="-device edu,addr=0x1d"`.

**Harness**: `qemu_run_system_disk` / `RUN_DISK` in `src/harness/qemu-run.sh`, applied to the
system disk only (a first version applied it inside `qemu_run_disk`, which the USB medium also
uses - two disks resolving to one file - and was corrected before anything ran).

**`Capability::Input` now delivers a connection.** PermissionManager's table held zero for it, so
no component granted it could subscribe to a pointer; it is resolved through the broker like the
Bluetooth names (`CAP_INPUT` -> InputService's SERVE root). The gate's "live pointer client"
needs it, and it is a pre-existing gap: the capability was in the vocabulary with nothing behind it.

### Decisions changed since the last update

**InputService no longer takes a bootstrap role; it resolves the profile through the broker,
asynchronously.** The earlier implementation followed the plan's words ("the InputService bootstrap
client"). It was reversed on two measured facts: (1) `stop_subtree` stops the whole reverse-
dependency closure, so with the role's mandatory dependency, `stop bluetooth_service` would have
stopped InputService, DisplayService and the console above it - the plan's own "restart the service"
step would have taken the input path down; (2) the plan's other requirement for this slot is
"reconnects after service restart", which only a by-name resolve can satisfy. The resolve is
asynchronous because the broker (the supervisor) answers only once it supervises, and a kernel
harness has no broker at all: a blocking resolve would park the pointer path. The three kernel
harness callers were reverted with it (an unread tag on the broker channel would have been taken
as the answer to a resolve). **This is a deviation from the plan's wording and is reported as one.**

**A second deviation, for the same reason**: PermissionManager reaches BluetoothService through the
broker too (unchanged since the last update, restated because it is the same rule).

### Observations for the audit, not fixed (out of scope)

- A plain-PCI function the kernel does not resolve is inventoried with `bar_phys: 0, bar_len: 0`,
  and `sys_device_claim` still makes a `DeviceMemory` for it. A driver that then called
  `SYS_DEVICE_MEMORY_MAP` would map physical page 0. The fixture never maps; the kernel path is
  worth a look.
- `./check.sh --gate bootstrap-plan`: the pre-existing `font_catalogue` mismatch (see above).
- The plan asks for registration in "guest-evidence profiles". No artifact by that name exists in
  this tree; selection is the catalog's `userspace.build` key, which is what was registered.

### Verification at this point

| what | command | result |
| --- | --- | --- |
| driver host tests (incl. the peer's own vectors) | `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` | PASS, 258 |
| service-logic host tests | as before | PASS, 529 |
| development configuration builds all development-only programs | `./check.sh --gate development-build` | PASS, 5 programs |
| development-gate manifest checks | `bash src/tools/check-development-gate.sh --manifest-only` | PASS, 7 programs |
| development-gate full | `./check.sh --gate development-gate` | FAIL at step 4 only: the last built volume predates the new services - an ordering artefact that the final build clears |
| verification model | `./check.sh --gate verify-model` | PASS |
| declared interfaces | `./check.sh --gate declared-interfaces` | PASS, 41 interfaces, 11 kinds |
| gate script syntax | `bash -n src/tools/check-bluetooth-service.sh` | PASS |

**NOT PERFORMED**: the gate itself (needs `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` and two
boots - deferred to the end of the job per the owner's instruction); aarch64/riscv64 cross-builds
(deferred, slow architectures). No P02M0180 item beyond the catalogue stage is ticked, because its
required verification has not run.

### Remaining clause the gate does not cover

"pointer input from an unrelated source remains usable": the console driver types keys and does
not move a pointer, so the gate proves the pointer SERVICE still answers (a subscription opens
after the stack is exhausted and restarted) and not that a second pointing device's motion still
arrives. Closing it needs a way to move another pointer from the gate (a QEMU monitor
`mouse_move` through the harness).

### Corrections found while implementing P02M0181 (2026-09-23)

- `device_manager::provider_kind_from_wire` had no arm for `BLUETOOTH_HCI`: a `bluetooth-hci`
  publication's catalogue subscription frames were typed `block`. Fixed (with the two new kinds).
- The catalogue item was ticked without its "check the combined manifest needs against that budget"
  and "refusal at the cap" parts. Added: `system-manifest` budget check + tests, and the development
  self-test `device_manager::tests::catalogue_cap_refusal`.
- `system-manifest`'s `the_production_manifest_classifies_every_staged_driver` failed on
  `bt_fixture` (`dma = "none"`); the test now names the in-guest fixtures.
- `check-bluetooth-service.sh` piped into `head` under `pipefail`, which `source-hygiene` refuses; now
  `grep -m 1`. `./check.sh --gate source-hygiene` passes.

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

### Found by the Bluetooth gate's boots, and changed

- **The bond store could not start.** ServiceManager handed `bluetooth_bond_store` its minted `STORAGE`
  directory connection with every right its pair was made with, and the store refuses a role carrying
  more than its kind allows, so BluetoothService waited for it for ever. The bond branch of
  `service_manager/bootstrap.rs` narrows it to SEND|RECEIVE|WAIT|TRANSFER, like the font and journal
  branches.
- **The second scan found nothing.** `bt_fixture` advertised once per epoch; it now advertises whenever a
  scan is enabled and nothing is connected to it, as a peripheral does.
- **The pairing line never printed.** The fixture printed `paired;` when the long-term key first appeared
  at a DHKey check, and its peer derives the key at the random exchange; it now prints it when the host's
  DHKey check is answered with the peer's own.
- **The cursor watch ended at once.** InputService's `subscribe` is a bounded SNAPSHOT of a 32-event ring,
  not a live stream; `btcheck`'s `watch` takes snapshots through the window and counts only what each
  added past the longest overlap with the one before.
- **Budget names.** ProcessService names a budget by its artifact (`bluetooth_service.lsexe`); `btcheck`
  strips `abi::EXECUTABLE_SUFFIX`.
- **An enable right after a disable answers `busy`** while the teardown lands, which the policy interface
  documents as the cue to ask again; `btcheck loss` asks again for up to ten seconds.
- **The refund could not be observed from the gate.** With BluetoothService stopped PermissionManager
  cannot mint `btcheck`'s Bluetooth grants, so any probe launched then is refused. `btcheck refund` makes
  the stop itself through the supervisor's admin channel, as `stop` does, and reads the accounting at once;
  its policy row gained `Capability::Supervisor` (development-only program). The gate runs `start` after it.
- **InputService never reconnected after a restart.** Its retry took `None` from `enabled()` for a dead
  connection, and generated clients answer a failed transport with `Some(Err(..))` and record it in
  `last_error()`; it now checks `last_error()`.
- **A cold reboot proved nothing.** A medium carrying `system-volume.img` ran the system from a copy of
  that image in memory even when the loader had chosen the paired disk: the kernel sent `LIVEVOL` whenever
  the module existed. The kernel now hands the image over only when the loader did not choose a block
  volume (`kernel/main.rs`), which is the documented rule and what the kernel's own "the system volume is a
  paired block volume" line already said. `RUN_DISK` is created from the volume the medium is paired with
  (`qemu_run_system_disk`, x86_64), and the gate fails if the first boot did not run from that disk.
  **Decision for the owner: this is a change to the boot chain.**
- **The last uncovered clause is now covered: pointer input from an unrelated source.** The record above
  left "pointer input from an unrelated source remains usable" open. `btcheck refund`, with the service
  stopped, prints that another pointer may move and watches a live pointer subscription; the gate script
  waits for that line and moves and clicks the machine's own virtio tablet through QEMU's monitor
  (`mouse_move`, `mouse_button`) - nothing Bluetooth can be the source while the service is stopped - and
  the phase fails unless motion, a press and a release arrive within ten seconds.

Temporary diagnostic prints added to BluetoothService and InputService to find the two input defects were
removed before the final build.

### Verification

| What | Command | Result |
| --- | --- | --- |
| the gate, first pass (before the unrelated-pointer check existed) | `./check.sh --gate bluetooth-service` on a development image | **PASS**, 841 s, 2026-09-23T16:29Z |
| the gate, on the job's final image (built 2026-09-23T23:17Z, imaged 23:19Z) | `./check.sh --gate bluetooth-service` | **PASS**, 841 s, 2026-09-24T00:10:18Z |
| changed probe | `cargo check --target x86_64-unknown-none --features development --bin btcheck` | PASS |

Its lines on the final image: `btread: PASS`; `btcheck: PASS` pair, limits (twice: before the stop and after
the start), exhaust, refund - which now includes the host moving and clicking the machine's own tablet while
the service is stopped - loss, reuse (after the restart and after the transport came back); "the fixture
paired, key fingerprint 7a344632"; "bt-fixture: encryption with a remembered key 7a344632"; on the second
boot of the same disk "the stack presented the key the first boot's pairing produced (7a344632)", `btcheck:
PASS reuse` and `btcheck: PASS forget`.

### What the gate still does not assert

The plan's gate item also asks the gate to "assert key/store-capability denial and stale-handle rejection".
Neither is a step of this gate. What exists: no capability in the grant vocabulary names the bond store, whose
endpoint the manifest hands to BluetoothService alone, and no record of any Bluetooth interface carries key
material (by construction, not asserted in a guest); stale controller-session epochs are refused in the
host-tested leaf. An in-guest assertion of each is still to be written, so that item stays open.

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

Ticked in `docs/todo/P02M0180.md` (2026-09-24): every item the gate and the host suites now prove - eighteen.
Open: the operator item (the plan's "configured platform allow/deny setting" for a radio does not exist; the
refusal a disabled controller gets is DeviceManager's device policy) and the InputService item (the slot
reaches the profile through the broker by name, not through a bootstrap role, so that stopping Bluetooth does
not stop InputService and the display above it) - both departures were reported when they were made and are
the owner's to accept; the gate item (the gate does not assert key/store-capability denial or stale-handle
rejection in a guest); and the registration item (cross-builds not run).

Decisions for the owner from this pass: the kernel's `LIVEVOL` rule (a boot-chain change); `btcheck` and
`cardcheck` holding the supervisor's admin channel (development-only programs); PermissionManager's probe rows
and fixture-control path no longer `cfg`-gated; `bootproto::manifest::MAX_ROWS` 256 -> 512.

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

### P02M0180: key/store-capability denial and stale-handle rejection, asserted in the guest

The gate item's last sentence - "Assert key/store-capability denial and stale-handle rejection" - was the one
thing it did not do. Now it does, in the existing first boot, with the existing grants (`btcheck` holds the
read and the operator authority, and no bond-store capability exists in the grant vocabulary at all):

- `btcheck deny` (`deny` in `btcheck.rs`). With the radio powered, it encodes three requests with the generated
  clients over a capturing transport (`Capture`, which records the frame and sends nothing): the operator
  interface's `power(0, false)`, and the private bond store's `lookup(local controller, the mouse)` - the one
  call that returns key material. It mints a fresh connection per case and sends the operator `power` on a
  READ connection, the bond-store `lookup` on a READ connection, and the same `lookup` on an OPERATOR
  connection. The oracle for each is that nothing answers within one second (`wait(connection, clock() +
  TICKS) < 0`) AND that the same connection then answers its own interface - an operator connection lists the
  bonded mouse through `bonded(0)`, a read connection lists a controller that is still powered - so a
  connection that had died, or a radio the forged `power` had switched off, cannot pass as a denial. Why
  "nothing answers" is the right oracle here: op numbers are per interface and all three forged calls are
  op 1 (`power`, `lookup`, `controllers`), so a service that dispatched on the op alone would answer them; the
  generated dispatch requires the whole frame to decode (`finish()`), and BluetoothService sends nothing for a
  frame its interface does not decode and keeps the connection (`bluetooth_service.rs`, the connection loop).
  PASS line: "PASS deny: a read connection powered nothing, and neither a read nor an operator connection
  answered the bond store's lookup".
- Stale handles, in `refund` (after the probe itself stops `bluetooth_service` through the supervisor's admin
  channel): the read grant the probe was launched with - itself a connection to the instance just stopped -
  must not answer `controllers()`. And in `loss` (the fixture's device is disabled mid-scan through
  DeviceManager's policy, so the transport goes with it, then enabled again and the controller republished and
  initialised afresh): the scan id begun before the loss must be refused by both `results` and `scanning` - it
  belongs to a controller session that ended, which the new one must not honour.

Run: `./check.sh --gate bluetooth-service` - **PASS**, 841 s, finished 2026-09-24T02:15:21Z, on the development
image built at 01:59Z from this tree. Its lines: btread PASS; btcheck PASS pair, deny, limits, exhaust, refund,
loss, reuse (after the restart); the fixture's key fingerprint f762b764, encryption with the remembered key;
after the cold reboot the stack presented the same key; reuse, forget; "PASS - read denial, pairing, cursor,
restart reuse, cold-reboot reuse and forget".

Ticked in `docs/todo/P02M0180.md`: the gate item. Still open: the operator item and the InputService item
(departures stated in the plan, the owner's to accept - unchanged by this continuation), and the registration
item until the cross-builds have run.

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

### P02M0180 at the end

Ticked at the end: the registration item, its cross-builds now built. P02M0180 stays OPEN on the operator item and
the InputService item, the two stated departures that are the owner's to accept.

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0180 (2026-09-24T06:39:11Z):

Rating: 7/10

The host stack, the bond store, the bounded launch, the catalogue scoping and the gate are real and largely
correct. Two defects need a code change inside this milestone - key material is not actually cleared, and a
power-off can leave the radio working - and the operator item is not met in a shipping configuration.

## Findings

### 1. Transient key material is not cleared (defect)

The plan requires: "Clear transient key buffers on completion/failure and exclude key bytes from logs and events."
The code says it does this in several places but does not:

- `service_logic::smp_pairing::Initiator` "clears" the Diffie-Hellman key by assigning `None` (`fail`, the
  `PAIRING_FAILED` branch, and the DHKey-check success branch: `self.dhkey = None`). Assigning `None` to an
  `Option<[u8; 32]>` rewrites the discriminant, not the 32 payload bytes, so the key stays in the struct. The LTK
  stays in `keys.ltk` by design. `BluetoothService` then drops the initiator with `link.pairing = None`
  (`on_encryption`, `encryption_failed`, `on_disconnected`, `OperatorView::cancel`), which again rewrites only the
  discriminant of the `Option<Initiator>` held inside `Link`. The key bytes stay in `controller.link` for the whole
  life of the connection.
- `bluetooth_service.rs::Stack::on_encryption` (pairing path): the `BondRecord` built for `store` carries
  `key: ltk.to_vec()` and is dropped without zeroing. Only the local `ltk` array is filled.
- `bluetooth_service.rs::Stack::on_connected` (reconnect path): the `ltk` array copied from the looked-up record
  is never cleared, and neither is `record.key`. The `LE_ENABLE_ENCRYPTION` parameters built by
  `hci_codec::enable_encryption` sit in `Controller::pending` and are dropped after `pump()` sends them, again
  unzeroed. The same holds for the command built from `Step::Encrypt` in `run_smp`.
- `Stack::bond()` calls the store's `lookup`, which returns the key. It is used by `ProfileView::open_mouse` and by
  `on_encryption` only to read `record.enabled`, so every open and every encryption pulls an LTK into this process
  and drops it uncleared. The store's `list` already returns the same records without keys.

Why it matters: the milestone's containment argument is that the process parsing a stranger's radio packets
holds as little key material as possible, for as short a time as possible. As written, several copies of the LTK
(and the DHKey) persist in that process, including in memory that is freed and reused. `enable()` shows the
intended pattern (`record.key.fill(0)` after the store). It needs applying, together with an explicit overwrite
before an `Option` is set to `None`, in the places above.

### 2. Power-off can leave the link and the scan running at the controller (defect)

`OperatorView::power(false)` does the following, in order:
1. Queues `DISCONNECT` for a live link and `LE_SET_SCAN_ENABLE(false)` for a running scan.
2. Calls `controller.pump()` once. `pump` sends at most one command, and none while another command is
   outstanding.
3. Calls `end_session()`, which clears `pending`, resets `outstanding` and the credits, and takes the link.

The effects:
- With both a link and a scan, the scan-disable is discarded.
- With any command in flight (during a pairing's DHKey generation, for example), the `DISCONNECT` is discarded
  too.
- The host has already forgotten the link, but the controller keeps the connection and keeps scanning, and the
  operator is answered `Ok`.

The next power-on sends HCI Reset, which cleans up, but until then the radio stays active against the operator's
explicit request. Power-off needs to either deliver the commands it queued, or reset the controller, before the
host forgets the session.

### 3. The operator item is not met in a shipping configuration (open item - confirms the implementer's record)

- **No shipping operator.** No shipping component holds `bluetooth-operator`. The only holder is the
  development probe `btcheck`. Its PermissionManager row is compiled into the shipping build as well, since
  these rows are no longer `cfg`-gated (a documented decision), so it is inert there only while nothing named
  `btcheck` is launchable. A shipping image therefore has no path at all to pair or enable a mouse. That
  contradicts the plan's "done when the supported mouse path scans, pairs, durably bonds and supplies useful
  input".
- **Any address can be paired.** `OperatorView::pair` accepts any address. The plan says "the operator selects a
  current scan identity", and nothing checks that the address came from a current scan.
- **The platform allow/deny setting.** It does not exist, and the implementation maps it to DeviceManager's
  device policy: a disabled controller is absent, so `power` answers `not-found`. That is a reasonable reading,
  but it is a departure the plan leaves to the owner.
- **Power-on (minor).** If initialisation stopped because the controller refused an init command
  (`on_complete` leaves `init` short of `Ready` with `powered = false`), `power(true)` returns `Ok` without doing
  anything. The request is not refused, and it cannot be retried.

### 4. InputService reaches the profile through the broker, not a bootstrap role (open item - justified departure)

Verified against the code:
- `system-manifest` requires a role's provider to be a declared dependency ("supplies this role but is not a
  declared dependency").
- ServiceManager stops the reverse-dependency closure.

So the bootstrap client the plan names would have made `stop bluetooth_service` stop InputService and the display
above it, contradicting the plan's own restart/refund proof.

The slot in `input_service.rs` (`Bluetooth`) meets the functional clauses:
- **Bounded:** one profile connection and one stream.
- **Recovers:** it re-resolves after a dead profile connection (`retry`, via `client.last_error()`) and reopens
  after a closed stream.
- **Same path as other pointers:** it forwards the normalised record through the existing forward path, and not
  while the protected session holds the keyboard.
- **No pretend provider:** it publishes no driver provider.

This is not a code defect, but the item is correctly left open for the owner.

### Optional (not required for the milestone)

- `Controller::pump` bounds a command only by `hci_codec::command` (255 parameter bytes). It never checks
  `limits.ceiling(Kind::Command)`, so a transport advertising a smaller command maximum would refuse the send, and
  the command would be dropped with a message. The plan asks to "honor smaller advertised/controller limits". The
  only transport today (the fixture) advertises 258, so nothing is affected yet. `hci::check_outbound` exists and
  is unused on this path.

## Verified

- **Catalogue scoping:** DeviceManager's `open` finds the provider by kind, slot, publication generation and
  binding generation, and checks the minted scope against the publication's kind. `subscribe` is refused
  before the snapshot, and the root's CONNECT answers an inventory-only connection.
- **Bounded launch:**
  - `service_manager/bootstrap.rs::service_limits` states exactly the plan's figures for both services.
  - `launch_limited_from_volume` prepares, releases, and cancels on a failed release, with no fallback.
  - ProcessService's `limited_domain` rejects zero or unlimited values, creates the Domain and sets the IPC
    queue, stack and DMA limits before the process can run, and closes the Domain on any failure.
- **Bond store:**
  - The file is replaced whole through `open_writer(Replace)` and `writer.commit`, answered only after the
    commit, and the in-memory table is rolled back on a failed write.
  - 64 records, 64 kB, unsupported versions and wrong key lengths are all refused.
  - `list` carries no key, and a record that does not parse is unavailable rather than an empty peer.
- **Bonded only after the durable commit:** a failed store ends the attempt with a disconnect. `forget` deletes
  durably before it answers, then disconnects the live link.
- **Scan:** the deadline is capped at 10 s, with 64 deduplicated results and explicit cancel/complete. Pairing
  is capped at 60 s, with one live attempt per controller.
- **Pairing security:** no downgrade (a response without Secure Connections is refused), healthy randomness or
  no pairing, and the reflected public key is refused.
- **Controller sessions and bounds:**
  - Reset advances the session epoch.
  - Stale-epoch packets are discarded.
  - Loss releases held buttons before the report streams close.
  - A withdrawn controller is dropped and its replacement is initialised from scratch.
  - Scan handles from a previous session answer `not-found`.
  - Bounds: two controllers, 16 clients, 16 queued commands, four report streams per link, SDU 512 with a
    1-second assembly deadline.
- **Fixture:** `drivers::bt_peer` implements AES, CMAC and f4/f5/f6 independently of `service_logic::smp` (the
  S-box is generated, not copied).
- **Gate registration:** the gate is registered in `check.sh`, the verify-model catalogue (including the
  guest-booting list) and `release-required.toml`. The implementer's records show it passing, including the
  `deny` and stale-handle assertions.

## Checks performed

Code reading only; nothing was built or run for this review. Files read: `bluetooth_service.rs` (whole),
`bluetooth_bond_store.rs` (whole), `service_logic::{smp_pairing, hci, l2cap (bounds), bond_store (bounds)}`,
the Bluetooth parts of `input_service.rs` and `permission_manager.rs`, `service_manager/bootstrap.rs` (limited
launch), `process_service.rs` (`limited_domain`, prepared launch), `device_manager.rs` (scoped `open`),
`drivers/core/src/bt_peer.rs` (provenance), and the registration lists.

---

IMPLEMENTER'S RESPONSE ON P02M0180 (2026-09-24T19:39:29Z):

Each finding was checked against the code before anything was changed. Findings 1 and 2, the pairing and power-on
parts of Finding 3, and the optional point were confirmed and fixed. The rest of Finding 3 and Finding 4 describe
the two open items correctly, and they need no code change. Both items stay open for the owner.

## Finding 1 - Transient key material is not cleared: ACCEPTED

Confirmed. Setting an `Option<[u8; N]>` to `None` rewrites only the tag, so the DHKey and the LTK stayed in the
`Initiator` inside `controller.link`. Several other copies were also dropped without being zeroed. Changes:

- **`service_logic::smp_pairing` (the initiator):**
  - The new private `Initiator::forget_keys` overwrites the DHKey, the MacKey and the LTK where they are stored,
    and only then sets the two `Option`s to `None`. `fail` and the peer's `PAIRING_FAILED` branch now call it.
  - The success branch overwrites the DHKey and the MacKey in place and keeps only the LTK.
  - A new `impl Drop for Initiator` calls `forget_keys`. Every `link.pairing = None` and every dropped link
    therefore clears what the initiator held: `on_encryption`, `encryption_failed`, `on_disconnected`,
    `OperatorView::cancel`, `end_session` and `withdraw`.
  - The DHKey-check branch and `check_value` now borrow the keys (`as_ref`) instead of copying them into locals.
  - The overwrite is `fill(0)` followed by `core::hint::black_box`, which keeps the stores from being removed as
    dead. A volatile write needs `unsafe`, and this crate holds no `unsafe`.
  - New host test: `only_the_ltk_outlives_a_completed_exchange_and_nothing_a_failed_one`. After completion the
    DHKey is gone, the MacKey is zero and the LTK is kept. After a failed check, neither key is kept.
- **`bluetooth_service.rs`:**
  - A new `scrub` does volatile zeroing, the same helper ModemService uses. It now clears:
    - `Stack::on_encryption`: the `BondRecord` built for `store` (its `key`) and the local LTK, after the store
      answers. The LTK is taken with `and_then(Initiator::ltk)`, without the extra `Option` copy there was.
    - `Stack::on_connected` (reconnect path): the looked-up `record.key` and the local `ltk`, once the
      encryption command is queued.
    - The new `Controller::encrypt`: it builds the `LE Enable Encryption` parameters, queues them and clears the
      local array. The reconnect path and `run_smp` now both use it.
    - `Controller::pump`: the dequeued parameters and the encoded packet, after the send or after a length
      refusal.
    - The new `Controller::clear_pending`: every queued command, before `start_init` and `end_session` drop the
      queue. A queued encryption carries the LTK.
    - `run_smp`: the LTK inside `Step::Encrypt`. The loop now iterates the step list mutably, because that heap
      list is freed right after the loop.
    - `Stack::bond()`: a record it refuses for its key length.
    - `OperatorView::enable`: the record's key, which was cleared with `fill(0)` before and now goes through
      `scrub`.
  - **LTK lookups:** the new `Stack::enabled_peer` reads `enabled` from the store's keyless `list`.
    `ProfileView::open_mouse` and the end of `on_encryption` use it. Only two callers of `lookup` remain, and
    both need the key: the reconnect path and `enable`, which has to write the whole record back.
- **Not changed:** the IPC frames that carry the key between BluetoothService and the bond store in
  `store`/`lookup`. The plan prescribes that exchange, and the generated client and `ChannelTransport` own its
  request and reply buffers. Clearing them would mean writing the bond-store exchange by hand. The finding does
  not name them, and this response does not claim them.

## Finding 2 - Power-off can leave the link and the scan running: ACCEPTED

Confirmed. `pump()` sends at most one command, and `end_session()` then discarded whatever was still queued.
Changes:

- `OperatorView::power(false)` now works in this order:
  1. It ends the host session.
  2. It queues `HCI_Reset` as the only command and pumps it. `end_session` has already cleared the outstanding
     slot and the credits.
- HCI Reset ends every connection and the scan inside the controller itself, so no queued disconnect or
  scan-disable can be lost. The disconnect/scan-disable queueing was removed.
- Power-off also leaves the controller ready and off (`init = Init::Ready`, `powered = false`). The next
  `power(true)` then runs initialisation again from the reset, even when the power-off arrived during
  initialisation. Without that, `init` would have stayed at a middle step and power-on would have done nothing.
- `drivers/core/src/bt_fixture.rs` changes, because the fix needs it: the fixture's `HCI_Reset` now drops its
  connection and scan, as a real LE link layer does. Before, it only completed the command, so a power cycle
  against the fixture could not have reconnected. The gate's boots are unaffected: the fixture already dropped
  its link when the service's connection closed.

## Finding 3 - The operator item is not met in a shipping configuration

- **No shipping operator: ACCEPTED as a description of the open item; no code change.** Correct. The only holder
  of `bluetooth-operator` is the development probe. A shipping operator would be a new trusted component, such as
  a settings UI, and this milestone cannot invent it. The PermissionManager grant path is ready for it. The item
  stays open for the owner.
- **Any address can be paired: ACCEPTED.**
  - `OperatorView::pair` now answers `not-found` for an address that is not among the controller's current scan
    results.
  - To make "current" mean this session, `Controller::end_session` now drops the scan (`self.scan = None`)
    instead of only stopping it. After a reset, a fault or a power-off, the old scan handle answers `not-found`,
    and nothing it found can be paired.
  - This also makes the plan's "a reset ... invalidates ... scans" true for a reset. Before, only a withdrawal
    removed the scan.
  - `btcheck pair` pairs the address its scan returned, and `btcheck loss` already expected the old scan to be
    refused, so the gate's path is unchanged.
- **The platform allow/deny setting: ACCEPTED as stated; no code change.** Mapping it to DeviceManager's
  persistent device policy is the recorded departure. Accepting it is the owner's decision.
- **Power-on after a refused initialisation: ACCEPTED.** In `Stack::on_complete`, a refused init command now
  leaves the controller ready and off (`init = Init::Ready`, `powered = false`) instead of stuck at the middle
  step. `power(true)` then restarts initialisation from the reset instead of answering `Ok` for nothing.

## Finding 4 - InputService reaches the profile through the broker: ACCEPTED as stated; no code change

This agrees with the implementer's recorded reason: a role is a dependency, and a stop takes its reverse closure.
The item stays open for the owner to accept the departure.

## Optional - `Controller::pump` ignores the advertised command maximum: ACCEPTED

`Controller::command` now calls `hci::check_outbound` with the controller's `Limits` before a command is queued.
A command longer than the advertised maximum is refused with a message, and its caller sees the same `false` it
already handles for a full queue. This matches the plan's "validate ... declared length before enqueueing".

## Milestone document

`docs/todo/P02M0180.md` records each change in one clause, at the item it belongs to: the packet-ceiling item,
the reset item, the bond item and the open operator item. The operator item stays unticked.

## Verification

- **Specific to this milestone:**
  - `smp_pairing`: 9/9, including the new key-clearing test.
  - `bluetooth-service`: every probe row passed - `btread`, `pair`, `deny`, `limits`, `exhaust`, `refund`, `loss`, `reuse` after the restart and after the cold reboot, and `forget`. That covers the scan-bound `pair`, encryption with the cleared LTK and parameters, reconnect encryption from the bond store, and `enabled` read from the listing.
  - Power-off is not in the gate. It was checked by reading the code, and the fixture now models the reset it relies on.
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

