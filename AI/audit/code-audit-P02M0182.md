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
