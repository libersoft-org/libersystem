# IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0182 (2026-09-23T04:40:20Z):

This record was opened after the implementation had begun in the same working session (the IDL and the
pure leaves came first); everything below describes what is in the tree at the time of writing.

## Scope

The service slice the plan defines: contact PIV cards through short-APDU-level providers, pinpad-only PIN
verification, reader-scoped grants minted through PermissionManager, exclusive expiring transactions,
removal and recovery, card events, a test-only in-guest provider, and the `smartcard-service` gate. It does
not implement the USB CCID transport (P02M0099's), character/TPDU or extended-APDU readers, or non-pinpad
secret entry.

## What was implemented

**Vocabulary: `src/idl/smartcard.lsidl`, package `liber:smartcard@1` (new crate `smartcard-proto`).**
- `smartcard` (the application's connection): `reader`, `slots`, `events` (stream: snapshot, snapshot-end,
  inserted/removed/unavailable/available), `acquire(slot, wait-ms, lease-ms)`, `exchange`,
  `verify-piv-pin(transaction, timeout-ms)` (no PIN parameter exists), `authenticate-piv(transaction,
  32-byte challenge)`, `cancel`, `release`. Typed outcomes: `card-outcome {done, card-removed, cancelled,
  timed-out, provider-error}`, `pin-result {verified, incorrect(+retries), blocked, cancelled, timed-out,
  card-removed, provider-error, trusted-input-unavailable}`.
- `smartcard-admin.mint(alias, operations{read, transact, authenticate}, owner: handle<task>) -> minted
  {connection, reader-id, name}` - PermissionManager's alone.
- `smartcard-reader` provider contract: `describe` (name, slots, exchange level, T=0/T=1 bits, pinpad
  capabilities), `open-session` (answered only once quiescent), `events` stream (presence, fault,
  quiescent), `power`, `select-protocol`, `exchange`, `secure-verify(template)`, `abort`; every request
  carries `request-header {request, slot, card-generation}` and every reply echoes it.
- Development-only `smartcard-fixture` control interface (insert, remove, pin outcome, delay, hold-abort,
  recover, withdraw, republish, operations log, now).
- `device.lsidl`: `smartcard-reader = 14`; `security.lsidl`: capability `smartcard`, and `audit-entry`
  gained `detail` (the exact reader a grant resolved to).

**Pure leaves.**
- New crate `smartcard-model` (`src/user/libs/smartcard/model`): `atr::parse` (TS, T0, the TA/TB/TC/TD
  chain, historical bytes, TCK exactly when a protocol other than T=0 is offered, 33-byte bound, every
  declared length checked against the bytes present) and `check_byte`. Used by the fixture, by the service
  (to validate the ATR a power-on returns and pick the protocol), and by future CCID code.
- `service_logic::piv`: the exact allowlist (canonical SELECT of the full PIV AID, GET DATA for the
  Discovery Object and the PIV Authentication certificate), forbidden-by-name instructions (VERIFY in any
  form, CHANGE REFERENCE DATA, RESET RETRY COUNTER, PUT DATA, GENERATE, GENERAL AUTHENTICATE raw,
  import/attestation), short-APDU grammar, the PIN-less secure-verify template, `pinpad_fits`, GENERAL
  AUTHENTICATE construction, the signature's template and DER-ECDSA validation, PIN status words, and the
  bounded GET RESPONSE continuation (64 pieces, 16 kB).
- `service_logic::card_slots`: readers (8, slots 4 - larger advertisements refused), grants (32),
  transactions + queued acquisitions (32), per-slot FIFO of 8 with deadlines, 60 s leases never renewed,
  10 s APDU / 30 s pinpad deadlines capped by the lease, the phase machine (absent, resetting, idle, owned,
  in-flight, cancelling, unavailable), a full reset (power off, power on, protocol from the ATR, SELECT) at
  every handoff, abort-then-reset on every ending, 5 s recovery caps leading to unavailable until the
  provider's quiescent event, removal as a distinct terminal result with the drain kept separate from
  absence, late-reply rejection by request id, one serialized answer per request; `EventQueue` (16, never
  coalesced, closed on overflow).

**SmartcardService: `src/user/services/core/src/smartcard_service.rs`.** Roles CATALOGUE (factory from
CATADMIN, kinds `smartcard-reader`) and ADMIN (the minting root). Adopts readers (describe, event stream,
asynchronous `open-session`; a session never proven quiescent within 10 s passes the reader over), mints
grants against an alias table that binds aliases to provider publication names (empty in a shipping
build; the two fixture readers in a development build), waits on every grant's owner task and drops the
grant when it ends, decodes client requests through the generated dispatch and answers the asynchronous
ones by hand, sends provider requests non-blocking (encoded by the generated client into a capturing
transport, correlation rewritten), validates every provider reply's echo and bounds, and fans card
events out to per-grant bounded streams.

**Grant path.** `rt::CAP_SMARTCARD_ADMIN`; ServiceManager grants PermissionManager that name, maps it to
`smartcard_service`'s ADMIN root, and lists the service as plan-relaunchable. PermissionManager: vocabulary
31, `Capability::Smartcard` minted in `grant_for_task` with the launched task (RIGHT_WAIT|RIGHT_TRANSFER),
`smartcard_policy(component) -> (alias, operations)` (development rows only: `cardcheck`/`cardhold` all
operations on `fixture-a`, `cardread` read-only on `fixture-a`, `cardb` all on `fixture-b`), and every audit
entry records the minted reader in `detail`.

**Fixture: `src/user/drivers/core/src/smartcard_fixture.rs` + `drivers::piv_card`.** Reader A (two slots,
pinpad), reader B (one slot, no pinpad); PIV cards playing SELECT, the Discovery Object, the certificate
(chained through GET RESPONSE), VERIFY outcomes and GENERAL AUTHENTICATE; request identities echoed; a
new session held until work a departed session left has drained; held replies released late ahead of an
abort's completion; a log of every operation with the fixture's clock and whether a verify template
carried anything but placeholders. The certificate and three signatures were made once with OpenSSL; the
private key was deleted (see Decisions).

**Probes and gate.** `cardcheck` (handles, insert, pin, refuse, isolation, queue, inherit, expiry,
removal, stuck, overflow, prime-slow, inflight, session, republish), `cardhold` (hold N, dup, slow),
`cardread`, `cardb`; `src/tools/check-smartcard-service.sh` (one boot with `-device edu,addr=0x1f`,
OpenSSL/xxd prerequisites, handle baseline comparison, OpenSSL verification of the signature against the
public key in the certificate the card returned). Registered in `check.sh`, the verify-model catalogue
(`userspace.build`) and the release-required set (`gate.smartcard-service`, `host.smartcard-model`,
`host.smartcard-proto`). Manifest: sources/libraries for the new crates, program and service rows,
the fixture registry entry (development, 0:31.0, quirk, dma none), probe rows.

## Decisions not fixed by the plan

- THE FIXTURE CARD HOLDS PRECOMPUTED SIGNATURES. This tree's rule (stated in `bootsig`) is to implement no
  curve arithmetic, and adding a third-party P-256 crate is a dependency decision for the owner under
  `docs/DEPENDENCY_POLICY.md`. The card therefore answers the three challenges it knows with signatures
  made once by OpenSSL, and refuses others with 6A80. The service's command construction, response parsing
  and byte path are what the gate proves, and OpenSSL verifies the result against the card's certificate.
  If the owner wants the card to sign arbitrary challenges, vendoring a P-256 crate is the change.
- PermissionManager reaches the ADMIN root through the broker (as for Bluetooth and power): a bootstrap
  role would make PermissionManager depend on SmartcardService.
- "Bootstrap policy binds approved aliases to provider metadata" is a compiled alias table in the service
  (alias -> publication name), empty in a shipping build; policy rows name an alias and an operation set.
- A reader whose advertised secure-verification format cannot fill the PIV template is treated as having
  no pinpad (verification answers `trusted-input-unavailable`); it is not refused outright, so its public
  data remains readable.
- `cancel` and `release` both end the transaction (stopping admission, aborting anything in flight and
  resetting); an operation cut short by either answers `cancelled`.
- A timed-out operation or a provider fault ends its transaction, because the abort and reset that follow
  leave nothing of the card's state to continue on.
- `exchange` needs the `transact` operation (it only exists inside a transaction); `read` covers the
  reader's description, slots and events.
- "Buffers returning to baseline" is observed as the service's handle count from ProcessService's
  accounting; memory is not compared, because allocator retention would make it noise.

## Verification at this point

| what | command | result |
| --- | --- | --- |
| ATR leaf | `cargo test --manifest-path src/user/libs/smartcard/model/Cargo.toml` | PASS, 6 |
| service-logic (piv 10, card_slots 15) | `cargo test --manifest-path src/user/services/logic/Cargo.toml` | PASS, 569 |
| drivers library (piv_card 4) | `cargo test --manifest-path src/user/drivers/core/Cargo.toml` | PASS, 262 |
| smartcard-proto / security-proto | `cargo test --manifest-path ...` | PASS, 27 / 9 |
| manifest tool | `cargo test` in `src/tools/system-manifest` | PASS, 24 |
| generation | `./gen.sh --accept-breaking` | PASS, 21 packages |
| services and fixture type-check, both configurations | `cargo check --bins [--features development]` | PASS |
| verification model | `verify-model -- check` | PASS ("model is consistent") |
| declared interfaces | `./check.sh --gate declared-interfaces` | PASS |
| source hygiene | `./check.sh --gate source-hygiene` | PASS |
| development-gate manifest checks | `bash src/tools/check-development-gate.sh --manifest-only` | PASS, 15 programs |
| gate script syntax | `bash -n src/tools/check-smartcard-service.sh` | PASS |
| bootstrap plan | `python3 src/tools/check-bootstrap-plan.py` | FAIL on the pre-existing `font_catalogue` mismatch only |

**NOT PERFORMED**: the `smartcard-service` gate (needs `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`,
deferred to the end of the job per the owner's instruction), the target build of the new programs, and
the aarch64/riscv64 cross-builds. The plan's "guest-evidence profiles" do not exist as an artifact here;
selection is the catalogue's `userspace.build` key.

## Remaining / blockers

- The gate has not run, so no item that needs it is ticked.
- The real USB CCID adaptation belongs to P02M0099.

IMPLEMENTER'S CORRECTION ON P02M0182 (2026-09-23T06:02:00Z):

- **The fixture's PCI slot was unusable.** The smart-card fixture was pinned to 0:31.0 (`addr=0x1f`), which on
  the q35 machine every x86_64 gate boots is ICH9-LPC's: QEMU refuses `-device edu,addr=0x1f` before the
  guest starts ("slot 31 function 0 not available for edu, in use by ICH9-LPC"), so the gate as written
  could never have run. Found while choosing P02M0183's fixture slot, by starting QEMU with no guest
  (`-S -nodefaults`) for slots 0x18-0x1f: 0x18-0x1e were accepted, 0x1f refused. The registry entry now
  pins 0:27.0 and `check-smartcard-service.sh` adds `-device edu,addr=0x1b`. The gate itself is still NOT
  PERFORMED; this changes where the fixture sits, not what it proves.
- The gate is also registered in verify-model's `GATES_THAT_BOOT_A_GUEST` (since the P02M0181 work), so its
  run carries guest-log evidence and a guest slot.

Update (2026-09-23T10:12:00Z) - found by the first development image build of the job:

- The image build refused `service-util` (the shared library built from `service-logic`): `card_slots`
  called `smartcard_model::atr::parse`, and no staged library publishes that symbol. The decisions no
  longer parse an ATR. `card_slots::Answer` carries an `offer: Option<Offer>` (the protocols the ATR
  offers and the one it starts in), SmartcardService reads the ATR with `smartcard_model::atr::parse`
  and fills it in, and the suites do the same through a test helper. `smartcard-model` moved from
  `service-logic`'s dependencies to its dev-dependencies and was added to the `services` crate, which
  links it statically. Behaviour is unchanged: the same parser, the same protocol choice.
- Verified: `service-logic` host suite (670 passed), `services` `cargo check --bins` shipping and
  development.

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

### Found by the smart-card gate's boots, and changed

- **The operation log answered `again`.** The fixture's reply buffer was 4 kB and the log holds up to 256
  records of about forty bytes; the buffer is 16 kB (`smartcard_fixture.rs`).
- **`cardcheck queue` misread the log** once it was longer than one window: it now reads the whole log
  backwards - its own first exchange after the holder's release, the holder's last verification before it,
  and power-off, power-on, protocol and SELECT in that order between them.
- **`cardcheck inherit` treated "nothing yet" as failure.** It waits while the transferred endpoint's
  channel is empty and fails only when it is closed.
- **The probes' `handles` phase is gone** (the baseline is the system graph's, above); `cardcheck` no longer
  holds `process`.
- **The restart phase could never be observed from the shell.** After `stop` and `start`, the replacement
  admits reader A only once the fixture has answered its new session, which it does only after the old
  session's held exchange has drained (about four seconds, by design). Every probe needing a reader-A grant
  typed in that window was refused at launch - PermissionManager treats a grant it cannot mint as a failed
  launch, and the service's `mint` refuses a reader whose session is still opening - and the shell reports
  a refused launch by printing nothing: `cardcheck session` and `cardcheck republish` printed nothing and the
  gate ran out of console time. Diagnosed with a reduced boot of the restart alone (the refused launches,
  and `graph` showing the pre-restart process). The restart is now the probe's: `cardcheck restart` (policy
  row gains `Capability::Supervisor`, development-only program) stops and starts the service through the
  supervisor's admin channel, as `stop` and `start` do and as `btcheck refund` does, with the exchange in
  flight, and waits on the fixture's log until the replacement's session is answered; `cardcheck session`
  then checks the order (drained before session) and acquires on the replacement. The gate expects
  `cardcheck: PASS restart` and reads its second handle count before the restart.

### Verification

| What | Command | Result |
| --- | --- | --- |
| development image | `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64` then `LIBER_DEVELOPMENT=1 ./image.sh` | PASS (build RESULT ok, 167 s; image exit 0), 2026-09-23T22:24Z |
| the gate | `./check.sh --gate smartcard-service` | **PASS**, 481 s, 2026-09-23T22:32:56Z |
| changed probes and PermissionManager | `cargo check --target x86_64-unknown-none --features development --bin btcheck --bin cardcheck --bin permission_manager`, and `--bin permission_manager` without the feature | PASS |
| source hygiene | `./check.sh --gate source-hygiene` | PASS |

The gate's own lines: `cardread: PASS`; `cardcheck: PASS` insert, pin, refuse, isolation, queue, inherit,
expiry, removal, stuck, overflow, restart, session, republish; `cardb: PASS`; "SmartcardService: an event
stream fell sixteen behind and is closed"; "cardcheck: an exchange the fixture holds is in flight"; "the
service's handles returned to their baseline (10)"; "OpenSSL verified the PIV authentication signature
against the card's certificate".

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

Ticked in `docs/todo/P02M0182.md` (2026-09-24): twenty items, everything but registration. Open: the
registration item (cross-builds not run). For the owner: `cardcheck` holds the supervisor's admin channel.

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

### P02M0182 in this continuation

No code of this milestone changed in this continuation. Its registration item - the only one open - waits only on
the cross-builds, which run last (below).

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

### P02M0182 at the end

Ticked at the end: the registration item. P02M0182 is COMPLETE: its plan's status line says so, and its row in
`docs/todo/TODO.md` is checked.

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0182 (2026-09-24T06:54:16Z):

Rating: 9/10

The supported slice is implemented completely and correctly. No defect was found in the command policy, the slot and
transaction state machine, removal and recovery, or the grant model. One deviation from the plan's wording (how
reader aliases are configured) keeps this from 10.

## Findings

### 1. Reader aliases are compiled into the service, not delivered as bootstrap policy (minor deviation)

The plan says: "Policy selects an explicit configured reader alias ... Bootstrap policy binds approved aliases to
provider metadata." In `smartcard_service.rs`, the alias table `ALIASES` is a compile-time constant:
- a development build binds `fixture-a` and `fixture-b` to the fixture's publication names;
- a shipping build binds none.

The component-to-alias half lives in PermissionManager (`smartcard_policy`), which is the manifest grant path.
The alias-to-provider-metadata half, however, cannot be configured for a shipping image without a code change,
so there is no deployment-time path to grant a real reader.

This satisfies "the default is no reader grant" and the fixture-proven slice, but not the plan's "bootstrap
policy" wording. No real CCID provider exists yet (it is P02M0099's), so nothing is broken today.

## Verified

- **Command allowlist (`service_logic::piv`).**
  - Only byte-exact canonical SELECT of the full PIV AID and the two canonical GET DATA commands pass.
  - The short-APDU length grammar is checked first, including the 261-byte cap.
  - The instruction is judged before the class byte, so VERIFY (including the status-only 4-byte form),
    CHANGE REFERENCE DATA, RESET RETRY COUNTER, PUT DATA, key generation/import and GENERAL AUTHENTICATE are
    refused as forbidden behind any class byte. A client's GET RESPONSE is refused.
- **PIN and authentication.**
  - The PIN never exists in the service: `verify_template` carries eight 0xFF placeholders for the reader to fill.
  - The secure-verify request carries block and digit bounds only. A reader whose advertised format cannot fill the
    template, or which has no pinpad, answers `trusted-input-unavailable`.
  - `pin_status` maps 63Cx, 6983, 6400 and 6401, and nothing is retried.
  - `authenticate` requires a verification that succeeded in the same transaction. It builds the P-256
    GENERAL AUTHENTICATE itself, continues the answer within 64 pieces and 16 kB, and accepts only
    `7C {82 DER-ECDSA}` with minimal positive integers. The card's 6A8x answers are `unsupported`, and 6982 is
    `denied`.
- **Slots and transactions (`service_logic::card_slots`).**
  - Limits: one operation in flight per slot, eight FIFO waiters with deadlines, a 60 s lease cap, 10 s per APDU and
    30 s for the pinpad (all bounded by the remaining lease). Queued waiters count against the 32-transaction bound.
  - Operations are checked against grant, operation mask, card generation, reset epoch, presence and phase before
    dispatch.
  - Release, cancel, lease expiry and owner death all end the transaction:
    - an operation in flight is answered once and aborted;
    - the slot is not reset until the provider reports the abort complete;
    - the reset is power off, power on, protocol selection from the ATR (read through the shared
      `smartcard_model::atr` parser) and SELECT;
    - only after that can another client acquire the slot;
    - verification state goes with the transaction.
  - Abort and reset are capped at five seconds; past that the slot is unavailable and its waiters fail, until the
    provider reports quiescence.
  - Removal ends every transaction on the slot. A reinsertion is a new generation, and a draining slot stays draining
    until its abort completes.
  - Replies are accepted only for the slot's current request, and the service additionally requires the echoed
    request, slot and card generation.
- **Grants.**
  - The mint resolves an alias to exactly one current publication, refusing absence and ambiguity, and never takes
    the first reader.
  - The grant is bound to one reader key and an operation mask, and the public connection has no reader selector.
  - The grant holds the owner task observer; its termination drops the grant whatever copies of the endpoint
    exist.
  - Withdrawal closes every grant to that reader. Only PermissionManager may resolve the minting root: the
    broker's `cap_grants` lists `CAP_SMARTCARD_ADMIN` for `permission_manager` alone.
- **Bounds.** Eight readers and four slots are enforced; a larger advertisement is refused, not truncated. There
  are 32 grants, an ATR of at most 33 bytes and a response of at most 258 bytes.
- **Events.** Event streams start with an atomic snapshot, charged before admission, and hold at most 16 pending
  events. Removal and reinsertion are separate events, and an overflowed stream is closed without holding up any
  transaction.
- **Provider session.** A provider session is admitted only on the `open_session` answer, the provider's proof it
  drained a previous session, within 10 s; otherwise the reader is passed over.
- **Registration.** The gate is registered in `check.sh`, the verify-model catalogue (including the
  guest-booting list) and `release-required.toml`.

## Checks performed

Code reading only; nothing was built or run for this review. Files read: `smartcard_service.rs` (whole),
`service_logic/src/piv.rs` (whole), `service_logic/src/card_slots.rs` (state machine, 255-1044), the broker's
`cap_grants`/`service_of_cap` in `service_manager.rs`, and PermissionManager's smart-card mint path.

---

IMPLEMENTER'S RESPONSE ON P02M0182 (2026-09-24T19:39:29Z):

## Finding 1 - Reader aliases are compiled into the service, not delivered as bootstrap policy: REJECTED

The observation is accurate: `ALIASES` in `smartcard_service.rs` is a compiled table. It is empty in a shipping
build and holds the two fixture readers in a development build. It is rejected as a change, for four reasons:

- **The whole policy is compiled.** The review accepts the other half of the same policy, the component-to-alias
  half, as "the manifest grant path". That half is `permission_manager.rs::smartcard_policy`, which is also a
  compiled table. So is every other device-policy row PermissionManager holds. Nothing in this tree configures
  either half at deployment time: the image is the unit of configuration.
- **A real reader needs a code change either way.** Granting one takes a policy row naming an alias in
  PermissionManager and the alias's publication name here. Both land in the same change, in the same way.
  Delivering only the second half through bootstrap would not make a real reader grantable without a code
  change.
- **Delivering it would be new infrastructure.** No service receives an alias table at bootstrap. Building that
  would mean a manifest schema, validation, bootstrap delivery and parsing in SmartcardService, and in the modem,
  camera and MIDI services, which have the same shape. The milestone does not require it, and there is no real
  CCID provider to bind yet: that is P02M0099's.
- **The properties the plan cares about are met.** As the review verified:
  - the default is no reader grant;
  - an alias resolves to exactly one current publication, or the mint fails;
  - the grant is bound to one reader key and an operation mask.

  The decision is already recorded in the milestone: "THE ALIAS POLICY IS A COMPILED TABLE".

No code change for this milestone. Nothing this round changed touches SmartcardService or its gate.

---

AUDITOR'S RE-AUDIT ON P02M0182 (2026-09-25T00:27:22Z):

Rating: 10/10

The rejection of Finding 1 is justified. Verified:
- **The other half is compiled too.** The component-to-alias half, which the first review accepted as the
  manifest grant path, is itself a compiled table: `permission_manager.rs::smartcard_policy`.
- **Nothing configures policy after the build.** The manifest ServiceManager bootstraps from is generated into the
  build as well, and nothing in the tree configures either half after the build.
- **Moving the table would not help.** It would not create the deployment-time path the finding asked for.
- **No existing mechanism is bypassed.** Nothing in the tree called "bootstrap policy" is bypassed by the table.
  The phrase appears elsewhere only for P02M0120's allocator bootstrap.
- **The security properties hold:**
  - there is no default grant;
  - an alias resolves to exactly one publication;
  - each grant is bound to one reader key and one operation mask.

No code of this milestone changed in this round, and there is no regression. The milestone is COMPLETE.
