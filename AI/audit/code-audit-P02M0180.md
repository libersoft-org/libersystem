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
