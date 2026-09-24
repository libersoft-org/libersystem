IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0188 (2026-09-23T08:24:59Z):

Implementation started; this record is updated as the work lands.

Update (2026-09-23T10:05:51Z) - the state of the work when the implementation was finished. What landed,
where, the decisions taken, what was verified and how, and what was not run.

## What was implemented

### Contracts (`src/idl/admin.lsidl`, `liber:admin@1`)

- `admin-request` (request, cancel), `admin-authority` (execute, no argument), `admin-factory`
  (`mint(component, scope, launch, @rights(wait) owner: handle<task>)`, using `liber:process@1.task`),
  `admin-executor` (prepare, revalidate, execute(operation, epoch), cancel), `admin-journal` (read), the
  development `admin-probe-witness` (effects, replace-target, inject) and `admin-test` (journal fault,
  path, held). Records: `admin-descriptor` (version, action, executor + epoch, target + generation,
  parameters <= 256, payload length, SHA-256), `admin-request-args` (label <= 128), `admin-scope`,
  `admin-prepared` (operation, descriptor, deadline-ms), `admin-record`, `admin-journal-page` (records <=
  16, oldest, next, evicted). Resource `admin-payload` is a `@kernel(memory-object)`.
- `display.lsidl`: `trusted-screen` and `display-trusted { lock(epoch) -> trusted-screen; present(epoch,
  @rights(read, map) pixels: handle<image-object>) -> u64; release(epoch) }`.
- `input.lsidl`: `trusted-input` + `input-trusted { events(); arm(epoch); disarm(epoch) }`.
- `security.lsidl`: capabilities `admin-request = 41`, `admin-audit = 42`, `admin-test = 43`, appended
  after `media-import`; nothing renumbered. `device.lsidl`: provider kind `admin-executor = 20`.
- Generator: package `admin` in `gen.sh`, crate `src/user/libs/protocol/admin-proto`, manifest library row,
  `proto` facade. `./gen.sh --accept-breaking` was run after each IDL change (33 packages, exit 0).

### Pure decisions (`src/user/services/logic/src/`, no `rt` / `ipc-client`)

- `admin_descriptor.rs`: actions (`FirmwareDownload`, test-image-only `ProbeWrite` - an unknown action
  does not decode), request bounds, `check_prepared` (4096-byte descriptor, version, 32-byte digest, same
  action/parameters/payload length as asked), `escape_label` (128 bytes, C0/C1/DEL and bidi characters
  written as `\u{..}`, backslash doubled), and the service's own `prompt` template.
- `admin_broker.rs`: the one state machine - `Preparing -> Prepared -> AwaitingAttention ->
  AwaitingDecision -> Approving -> Granted -> Consuming -> Consumed -> Completed | Failed |
  OutcomeUnknown`, with `Declining -> Declined | Expired | Cancelled`. Admission (16 connections, one
  outstanding request per connection, one request in the confirmation path, scope, bounds, path,
  blocked), 60 s confirmation (6000 ticks), 30 s grants (3000 ticks), 5 s per local exchange (500 ticks),
  revalidation before the approval is recorded, consumption recorded before dispatch, the owner rechecked
  after every acknowledgment (the last recheck is the execution-admission boundary), owner death and
  connection closure as independent losses, `observer_failed` treated as owner loss, executor loss,
  storage-failure blocking and recovery. Every effect is an `Effect` the service performs.
- `admin_journal.rs`: segments (1024 records or 8 MB including the 32-byte header, at most four, only
  complete unpinned ones retired), 8192-byte framed records, per-request reservations at admission (four
  records), pins, the 256-record emergency ring (ordered, head retried, oldest non-head dropped and
  counted), `discard` for an uncertain commit, `scan` for recovery (a torn tail ends the scan).
- `trusted_keys.rs`: the secure-attention chord, arming only once every key is released, Enter
  down-then-up approves, Escape declines, anything pressed between spoils an Enter, stale epochs ignored,
  and a decision only after both the display and the keyboard acknowledged the epoch.
- `display_lock.rs`: which surface may be visible while a session is held, and what comes back.

### Executor seam (`src/user/drivers/core/src/admin_operation.rs`, drivers library)

- Preparation copies the payload (1..=4096 bytes) into executor-owned storage, digests that copy, freezes
  the live target generation and parameters; `start` is the start guard (epoch, not cancelled, not
  started, generation, lifetime) and admits one attempt; `revalidate` asks the same without starting;
  cancellation after start recalls nothing. Shared with P02M0099's DFU class module. `bootproto` was added
  to the drivers crate for SHA-256 (a minimal adjacent change).

### Services

- `admin_service.rs` (new): roles `JOURNAL` (writable scoped volume client for `vol://system/admin-audit`),
  `TIME` (fresh TimeService connection), `DISPLAY` / `INPUT` (clients of the two trusted roots),
  `CATALOGUE` (kind-scoped to `admin-executor`), and serve roots `FACTORY`, `AUDIT`, `TEST` (the last
  closed on arrival in a shipping build). Every exchange with an executor, the display, the keyboard,
  TimeService and the volume is sent without waiting through a captured generated-client encoding and
  answered through the loop under a 5 s deadline. Each iteration first asks every launching task, with an
  already-reached nonzero deadline, whether it ended (`check_owners`), before any ready reply, key or
  redemption is handled. The journal writer is asynchronous: `open-writer` (replace for a new segment,
  append otherwise), 4096-byte writes (the header alone first, so the staged length says where the record
  landed), `commit`; the index takes the offset and the file length the writer reported. An explicit
  failure keeps the record at the head of the ring and retries after 1 s; a commit whose answer never came
  is dropped, not replayed, and the broker is told it failed. Recovery at start lists and reads every
  segment (each exchange bounded to 5 s, before the service is online), keeps the newest four, treats a
  torn segment as complete so nothing is appended after garbage, and starts a fresh broker epoch above
  every recorded one. The operator's page reads records back from the volume, never from memory. The
  prompt is rendered with the terminal's unscii-16 glyphs into memory this service creates and handed to
  DisplayService read-only. Diagnostic lines name every recorded event and reason.
- `display_service.rs`: the protected session no longer has a client surface. `lock` checks there is a
  scanout, takes the lock under a sentinel no channel number can equal, hides every surface, sends
  `CLEAR` focus to InputService and answers the size, pitch and a session channel whose closing ends the
  session; `present` maps the holder's pixels (object size checked), blits them whole to the scanout and
  answers only after the device flush; `release`, a reset or replaced backing, a lost scanout, a resize
  and the holder's session channel closing all end the session and restore the prior surface, the
  console, or nothing. New surfaces stay hidden while locked; `input_focus` is denied to everything.
- `input_service.rs`: the trusted sink (`TRUSTEDKEYS`) and the trusted root (`TRUSTED`). The chord from
  the trusted keyboard, with a reader present, emits `Attention` and immediately protects the ordinary
  path: application key and contact streams are closed with their held keys released, queued pointer
  events discarded, `KEYFOCUS 0` sent to ConsoleService, and nothing on the merged path, contacts or
  pointers is delivered until AdminService disarms or its stream closes (the stream's producer is waited
  on for that).
- `permission_manager.rs`: `grant_for_task` mints `admin-request` for the exact prepared task - `duplicate
  (task, WAIT | TRANSFER)`, the launch correlation read from the task handle's koid, the policy's scope,
  `admin-factory.mint` over a fresh connection to the factory resolved by name - and narrows the result
  to `GRANT_RIGHTS`. `for_capability(AdminRequest)` is 0, so the task-less dynamic path mints nothing.
  `admin_policy`: `dfu` may request firmware download on `dfu:` targets (the tool's manifest row lands
  with the DFU tool in P02M0099); development rows for `admincheck` and `adminhelper` (probe write on
  `probe-`). `AdminAudit` and `AdminTest` are resolved-by-name fresh connections.
- `service_manager.rs` + `bootstrap.rs`: broker names `ADMINFACTORY`, `ADMINAUDIT`, `ADMINTEST`
  (PermissionManager only); the journal directory is made and scoped in the first start and again in
  `relaunch_planned` for a restart; `admin_service` is plan-relaunchable; DeviceManager's `TRUSTEDKEYS`
  is taken and handed to InputService once.
- `device_manager.rs`: a second, trusted key channel; its producer is duplicated into the bind of
  `virtio_input` and `xhci` alone, and its consumer goes to the supervisor once.
- Drivers: `ResourceKind::TrustedKeys`; `virtio_input` and the xHCI HID keyboard class `try_send` every key
  to the trusted sink before the ordinary one; `keys::feed_key` produces no cooked bytes for Ctrl+Alt+F12.
- `term`: `pub fn glyph(char)` over the existing font.

### Development-only

- `admin_fixture` driver (manifest row, PCI `edu` at 0:24.0, `admin-executor` + `fixture-control`):
  target `probe-target` over generations, `deadline-ms` 1000, every `execute` counted as a dispatch, the
  write counted as an effect with the confirmed digest; faults `lose-reply` and `fail`.
- Probes `admincheck`, `adminhelper`, `adminhostile` (manifest rows, PermissionManager rows).
- `lab.py` key map: `&` and `|`, so a scenario types background jobs and pipelines.

### Gate

- `src/harness/scenarios/admin-path.toml` (150 steps, validated by the runner's own loader) and
  `src/tools/check-admin-path.sh` (`qemu-admin-path`): cold development scenario with `-device
  edu,addr=0x18`; keys through QEMU `sendkey` only; screendumps of the idle and the prompt screens checked
  for the protected field, band and text and for none of the hostile client's green; exactly four executor
  writes; three AdminService instances with rising epochs, the restarted and the rebooted one recovering
  records; input digests printed. Registered in `check.sh`, `catalog.rs` (subject `userspace.build`,
  `GATES_THAT_BOOT_A_GUEST`, gate count 137) and `release-required.toml` (+ `host.admin-proto`).

### Documentation

- `docs/THREAT_MODEL.md` 2.3: the keyboard drivers, DisplayService and its scanout provider, InputService
  and AdminService are trusted for physical approval and for nothing else; what an ordinary client can and
  cannot do; the journal's limits; the indistinguishable decline is semantic, not constant-time.
- `docs/todo/P02M0099.md`: the DFU item now states what the adapter must supply (the P02M0188 item that
  places those requirements on P02M0099).

## Decisions taken (for the owner)

1. Executors are `admin-executor` catalogue providers - drivers DeviceManager bound - reached through a
   ServiceManager-minted kind-scoped connection; nothing a client holds can publish one. The action picks
   the executor by publication name (`org.libersystem.admin-dfu`, and `org.libersystem.admin-probe` in a
   development build).
2. DisplayService owns the protected presentation: AdminService draws into memory it owns and
   DisplayService blits it whole and acknowledges after the device flush. AdminService therefore needs no
   synchronous surface protocol, and every display exchange is asynchronous.
3. The trusted display and keyboard roots are non-exclusive client roles (the supervisor keeps its end), so
   a restarted AdminService is wired again; holder death is seen through the session channel (display)
   and the event stream's producer (keyboard).
4. `restart = "transparent"` for AdminService: a restart is a fresh epoch and fresh wiring, and nothing is
   restored from the journal.
5. An uncertain journal commit (no answer to `commit` within 5 s) is dropped rather than replayed.
6. Test-only controls added to `admin-test`: `path(present)` (an unavailable path on a machine that has
   one) and `held()` (to end a requester while an acknowledgment is held).
7. The catalogue budget grows by 2 (a transparent role); the system-manifest test passes.
8. AdminService is STATICALLY linked, for the reason `font_catalogue` is: the journal records a SHA-256 of
   each canonical descriptor, SHA-256 is `bootproto`'s, and no staged library exports it. The digest first
   lived in `service_logic::admin_descriptor`, and the first development build refused it: `service-util`
   (the shared library built from `service-logic`) then imported `bootproto::sha256::digest` with no
   provider. The call moved into AdminService and `service-logic` calls no `bootproto` function again.

## Verification

Passed (commands run from `src/` unless stated):
- `cargo test --manifest-path user/services/logic/Cargo.toml --lib`: 670 passed (broker 14, journal 5,
  descriptor 3, trusted keys 3, display lock 2 among them).
- `cargo test --manifest-path user/drivers/core/Cargo.toml --lib`: 289 passed (`admin_operation` 3).
- `cargo test --manifest-path term/Cargo.toml`: 68 passed.
- `cargo test --manifest-path tools/system-manifest/Cargo.toml`: 24 passed.
- Mutation demonstration, each restored and compared byte for byte afterwards: redemption without the
  unconsumed-grant check fails 3 broker tests; scope not checked fails 1; dispatch before the consumption
  record fails 4; the executor's start guard admitting a second attempt fails 1.
- `cargo check --bins` of services and drivers, shipping and `--features development`: clean.
- Test kernel: `cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests` built a fresh artifact.
- `cargo run --manifest-path tools/verify-model/Cargo.toml -- check`: model consistent.
- `tools/check-grant-vocabulary.sh`: 44 capabilities, each walked once.
- `tools/check-source-hygiene.sh --current`: clean. `harness/check-test-tags.sh`: consistent.
- `tools/check-bootstrap-plan.py`: AdminService's restart policy and mechanism agree; the one remaining
  mismatch is `font_catalogue`, which predates this milestone.
- The scenario document validated with `scenario.load`.

NOT RUN (RULE ONE: nothing long until every goal task is done):
- the `qemu-admin-path` gate itself, and with it every guest assertion: presentation evidence, the hostile
  client, held keys, delegation, held acknowledgments, storage failure, service restart and reboot
  recovery;
- the development image build, `./verify.sh --plan`, `./verify.sh`, the aarch64/riscv64 cross-builds of
  the shared contracts, and the demonstration that bypassing protected presentation or the executor
  boundary makes the gate fail;
- the kernel suites that exercise the updated DisplayService and InputService harnesses.

## Remaining / incomplete

- Everything above marked NOT RUN. The plan items whose completion depends on the guest gate stay open in
  `docs/todo/P02M0188.md`.
- No production component holds `admin-request` yet: the DFU tool's manifest row arrives with P02M0099,
  and no operator tool is granted `admin-audit` by default.
- Bounded launches reach the same `grant_for_task` call as the other three launch paths; no test
  exercises `admin-request` through a bounded launch specifically.

## Verification pass (2026-09-24T01:28:37Z): the gates booted, what they found, and what changed

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

### Found by the administrative path's cold boots, and changed

- **This milestone had broken the physical keyboard.** The first cold run failed at its third step: nothing
  typed through QEMU's emulated keyboard reached the shell. Measured on an ordinary development boot with a
  probe that reads `graph`, types `echo keys` through the monitor's `sendkey`, and reads `graph` again:
  InputService's received count rose by the key events, and nothing appeared on the console. The cause is
  the trusted key sink this milestone hands `virtio_input` and `xhci`: a keyboard driver's bind now holds six
  resources (device, interrupt, key sink, trusted key sink, power connection, console feed), and
  `driver_binding::MAX_BIND_RESOURCES` was five. `Holdings::hold` refuses the sixth and DeviceManager's
  `holds` ignores the refusal, so the console feed - the capability that turns a keystroke into console
  input - was silently dropped from every keyboard's bind (and its duplicate never closed). The bound is
  six (`src/user/libs/driver/binding/src/lib.rs`), with the reason written beside it. After the rebuild the
  same probe printed `echo keys` and `keys`. `driver-binding`'s host suite: 85 passed.
  **For the owner:** `holds` still ignores a full ledger - which is how this stayed silent - and turning
  that into a refused bind is a change to DeviceManager's bind path that this milestone did not make.
- **Two runs in parallel collide** (common list), and a cold run REBUILDS the development image and the
  bootable volume (`--kernel-on-volume --dma-mode enforcing-required`), so it runs after every guest gate
  that boots the image `./image.sh` wrote.
- `check-admin-path.sh` runs the scenario on a persistent disk (`RUN_DISK`, created from the volume the
  medium is paired with) and fails if the system did not run from it: the "restart and reboot recovery"
  assertions are about the journal on that disk, and a system running from the medium's image in memory
  would forget it on the reboot.
- `admin_service` is static: it computes the descriptor digest itself with `bootproto`'s SHA-256, which no
  staged library publishes, and `service-logic` no longer calls anything outside the published libraries
  (found by the first development image build). AdminService accepts CONNECT on the connections it minted
  and `admincheck` reports on stderr (common list); milestone ids were removed from the fixture, service
  and kernel-suite comments.
- **The scenario waited on an order the service does not promise.** With the keyboard fixed, the second cold
  run passed its first fifteen steps (the baseline, `unavailable`, `forged`, `wrong-target`, `storage`, and
  secure attention bringing the protected screen up with nothing waiting) and failed at the sixteenth: it
  expected "the trusted keyboard is idle and the session is armed" AFTER "the protected screen is presented",
  and the log had them the other way round. The keyboard arms when the chord's keys are up and the screen is
  presented when the display has flushed it; either can come first, and `Decision::ready` - which is what
  admits a key - needs both. AdminService now prints "AdminService: the session is ready for a decision" once,
  from whichever confirmation came second (`say_ready`), and the thirteen scenario steps that waited for the
  arming line before pressing a key wait for that one. The two individual lines are unchanged (the gate
  script still requires the arming line).
- **Every `prompt` step after the first waited for a prompt that was not the last thing in the log.** A cold
  run reads readiness from the end of the serial log, and there the log ended with AdminService's own lines or
  a background job's report, printed after the prompt they followed ("no shell prompt within 30 s" at the
  step after the first protected screen closed). The scenario presses Enter through the keyboard before each
  of those seventeen steps, so the shell's fresh prompt is the last thing written.
- **The test controls admitted two connections, and the scenario needs three.** `adminhelper contend` printed
  nothing and the prompt came back: PermissionManager could not mint its `admin-test` grant, because the TEST
  root and the connections minted from it were bounded at two - PermissionManager's resolved connection and
  the contending `admincheck`'s grant - so the helper's launch was refused, and the shell reports a refused
  launch by printing nothing. `MAX_TESTERS` is four, the bound the factory and audit roots already had.
- **The owner-death step expected the decline before the closure**, and the broker takes a declined request off
  the screen (`Effect::Hide`) before it records the decline, so the log always has "the protected screen is
  closed" first. The two steps are swapped.
- **`|` could not be typed.** The key map this milestone extended types `|` as `shift-backslash`, and a chord is
  validated by its last key, which was not in the vocabulary ("no key named 'shift-backslash'" at the first
  pipeline). `backslash` is in `KEY_NAMES`; every key step of the scenario was checked to map.
- **Lines from two processes about one action came in either order.** AdminService writes its lines straight to
  the port and the probes' lines come through the console, so "request 14 declined: requester task ended" was in
  the log before `admincheck`'s "the approval's record is held, and this requester ends", which the requester
  printed before it ended. The scenario runner gained one option: an `expect` step with `unordered = true`
  searches from where the last step that did something left the log rather than from the previous match, and
  never moves the cursor back (`src/harness/scenario.py`; four tests in `src/harness/harness-test.py`,
  `UnorderedExpectTest`; `./check.sh --gate boot-harness` PASS). Four steps use it: the held approval's decline,
  the held consumption's failure, the live delegate's requester ending, and the orphan's verdict.
- **The block step asked again before the service had let go of the connection.** After the lost reply the
  requester's connection still carries the request whose unknown outcome is being tried against the failing
  journal; asked again in that window, the broker answers `again` (one outstanding request per connection), and
  the probe took that for a failure. It now asks again on `again` for up to ten seconds, and still requires
  the decline "with nobody asked" - which the host test `a_journal_that_fails_or_never_answers_issues_no_authority`
  already proves for the same state once the attempt has been reported.
- **The epoch check caught what the epoch rule does not promise.** With every scenario step passing, the gate's
  own check failed: "epoch 2 did not follow 2". The restarted instance had started epoch 2 and decided nothing;
  after the reboot the next instance read a journal whose highest epoch was still 1 and started epoch 2 again.
  AdminService starts a new epoch above every epoch the journal carries - which is what keeps any record, and
  any authority (a grant is issued only after its record commits), attributable to one instance - so an
  instance that recorded nothing leaves nothing for the next to be above. The scenario now has the restarted
  instance decide something before the reboot (`admincheck unavailable`, a recorded decline), and the gate's
  strict check stands. **For the owner:** if every incarnation must carry a distinct epoch even when it records
  nothing, the service needs a persisted per-start marker, which the plan does not specify.

### Verification

| What | Command | Result |
| --- | --- | --- |
| the gate | `./check.sh --gate qemu-admin-path` (cold: `lab.sh scenario-cold x86_64 src/harness/scenarios/admin-path.toml`, which builds the development image and the `enforcing-required` bootable volume itself, on a persistent `RUN_DISK`) | **PASS**, 406 s, 2026-09-24T01:27:30Z - the ninth run; the eight before it failed on the defects above, one at a time |
| the emulated keyboard reaching the console | a boot of the development image; `graph`, then `sendkey` of `echo keys` through QEMU's monitor, then `graph` | before the fix: InputService's count rose and nothing reached the console; after it: `echo keys` and `keys` |
| the scenario runner | `./check.sh --gate boot-harness`; `python3 src/harness/harness-test.py UnorderedExpectTest` | PASS; 4 passed |
| the bind ledger | `cargo test --manifest-path user/libs/driver/binding/Cargo.toml` | PASS, 85 |
| changed programs | `cargo check --target x86_64-unknown-none [--features development] --bin admin_service`, `--bin admincheck` | PASS, no warnings |

The gate's own lines on the passing run: the scenario passed in 255 s; "the executor performed exactly the four
writes four confirmations authorized"; instance 1 epoch 1 recovered 0 records, instance 2 epoch 2 recovered 45,
instance 3 epoch 3 recovered 46; `admin-path-idle.ppm`: field 95 %, band 40960 px, text 7556 px, hostile green 0
px; `admin-path-prompt.ppm`: field 92 %, band 40960 px, text 41532 px, hostile green 0 px.

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

Ticked in `docs/todo/P02M0188.md` (2026-09-24): seventeen items. Open: the registration item - the cross-builds of
the shared contracts, `./verify.sh --plan` and `./verify.sh`, and the demonstration that bypassing protected
presentation or the executor boundary makes the gate fail (two runs with a deliberately broken build) have not been
done. Still true from the implementation record: no production component holds `admin-request` yet (the DFU tool
arrives with P02M0099), and no test exercises `admin-request` through a bounded launch specifically - the
scenario's pipelines exercise the pipeline path.

Decisions for the owner from this pass: `MAX_BIND_RESOURCES` 5 -> 6 (and DeviceManager still ignores a full
ledger); the scenario runner's `unordered` expectation; `MAX_TESTERS` 2 -> 4; the epoch rule as stated above.

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
