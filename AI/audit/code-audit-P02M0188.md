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

### P02M0188: bypassing protected presentation, or the executor boundary, fails the gate

The registration item asks to "prove bypassing protected presentation or the executor boundary makes the gate
fail". Both were demonstrated, one after the other, each by planting one defect in one file, running
`./check.sh --gate qemu-admin-path` (which builds its own cold development image, so each run tested the
planted tree), and restoring the file from a copy taken before the first plant:

| Planted defect | Where | Gate result | What failed it |
| --- | --- | --- | --- |
| PRESENTATION BYPASSED: when a session opens, AdminService no longer asks the display for the protected screen (`display_trusted.lock(epoch)` is not sent); it marks the session presented at once and prints the same "protected screen is presented" line | `src/user/services/core/src/admin_service.rs`, where the session is opened | **FAILED**, exit 1, 320 s (2026-09-24, about 01:50Z) | the scenario stopped at step 21 (`prompt`: no shell prompt within 30 s, after the planted session was closed); and the idle frame the scenario had captured at step 16 was not the protected screen - field 0 %, band 0 px, text 0 px - which the gate's frame check, run by hand on that frame with the gate's own thresholds, rejects (the scenario failing first, the gate never reached it) |
| EXECUTOR BOUNDARY BYPASSED: the fixture's development witness performs a write itself - `effects()` adds one to `writes` and prints "the probe write was performed" - so an effect happens that never crossed the executor's `execute` (no dispatch counted) | `src/user/drivers/core/src/admin_fixture.rs`, `effects` | **FAILED**, exit 1, 308 s (about 01:56Z) | step 3: the baseline expects "admincheck: effects writes 0 dispatches 0"; the guest printed "admincheck: effects writes 1 dispatches 0 generation 1" |

The presentation plant needed one extra line to compile under the tree's warnings-as-errors (`let _ =
DisplayCall::Lock(epoch);`, since the variant was no longer constructed); it changes nothing at run time. After
each run the file was restored from its saved copy, and `sha256sum -c` against the hashes taken before the first
plant answered `OK` for both files before anything else ran. The clean tree then passed the gate again:
`./check.sh --gate qemu-admin-path` - **PASS**, 414 s, finished 2026-09-24T02:35:56Z (its own cold development
build; the scenario, 169 steps, in 255.1 s): "the executor performed exactly the four writes four confirmations
authorized"; instances 1, 2 and 3 at epochs 1, 2 and 3 recovering 0, 45 and 46 journal records; the idle frame
field 95 %, band 40960 px, text 7556 px, hostile green 0 px; the prompt frame field 92 %, band 40960 px, text
41532 px, hostile green 0 px.

What the two failures show: the captured frames are an oracle independent of the service's own log line - the
planted service printed exactly the line a real presentation prints, and the frame was not the protected
screen; the scenario itself did not survive a session that never held the display (after it closed, the
console did not come back to its prompt within 30 s - the mechanism was not traced further, only the failure
recorded); and the effect count is read from the executor's side, so an effect that bypasses `execute` is a
write without a dispatch at the very first check.

### P02M0188: the driver protocol's host suite, found failing and changed

`./check.sh --gate host-tests` (every crate host suite, 124 of them) failed on one: `driver-protocol`,
`tests::a_field_outside_its_closed_set_is_refused_and_the_number_is_reported` - "resource kind 6: left
Ok(TrustedKeys), right Err(UnknownValue(6))". This milestone added `ResourceKind::TrustedKeys = 6` (the trusted
key sink handed to the physical keyboard drivers) to `src/user/libs/driver/protocol/src/lib.rs`, and the test
still probed 6 as the first number past BOTH closed sets. The first pass ran the crates it had changed by hand
and not this one. The test now probes each set one past its own end - 7 for the resource kinds, 6 for the
failure codes, 0 and 0xffff for both - and asserts that 6 decodes as `TrustedKeys`
(`src/user/libs/driver/protocol/src/tests.rs`). `cargo test --manifest-path ../src/user/libs/driver/protocol/Cargo.toml`
(from `src/`, as the gate runs it): 66 passed. `./check.sh --gate host-tests`: **PASS**, 124 suites, 104 s.

Ticked in `docs/todo/P02M0188.md`: nothing new - the registration item stays open on the cross-builds, `./verify.sh
--plan` and `./verify.sh`; the bypass demonstration it also names is done.

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

### P02M0188 at the end

Nothing newly ticked: the registration item stays open on `./verify.sh` alone. The owner's command: `./verify.sh`
(the plan above says FULL verification).

## Addendum (2026-09-24T05:50Z): P02M0183's `ipv6-peer`, run on the shipping image

The table above gives P02M0183's open reason as the quiet row of `ipv6-peer` missing on the development image.
The gate was then run on the shipping image, which `./verify.sh` builds for its gates: the quiet row passes there
(solicitations at 32.4, 35.9 and 43.1 s), and in two full runs nine of the ten rows pass; the tenth,
`hostile-quote`, failed both times with a different symptom each time, while six runs of that row by itself passed
all eight of its assertions. Details, commands and times are in `code-audit-P02M0183.md`. P02M0183 stays open on
`ipv6-peer` alone; nothing about this milestone changes.

---

AUDITOR'S REVIEW ON P02M0188 (2026-09-24T14:04:25Z):

Rating: 8/10

**The authorization path is sound.** I found no way to get an effect without these steps, in this order:
- a recorded request;
- a person's fresh Enter on an acknowledged protected session;
- a recorded approval;
- a recorded consumption;
- a final owner and deadline check.

The pure modules carry every decision the plan names, and the service keeps to one outstanding exchange per peer
with 5 s deadlines. The main concerns are:
- **What the person sees (defect):** the protected screen silently cuts lines, and at the harness's own
  resolution that includes the payload digest.
- **An open item:** the registration item is still unticked. What it lacks is the owner's `./verify.sh` run, not
  code.

## Findings

### 1. The protected screen silently truncates the canonical operation (defect)

The plan: "The person must invoke the chord and inspect the canonical action/target/payload there". The prompt
comes from `admin_descriptor::prompt`, and `render` (`admin_service.rs`) draws it one text line per template line.
`render` never wraps a line. When the next glyph would pass the right edge, the rest of the line is dropped
without a mark:

    if x + cell_w > width || top + cell_h > height { break; }

The geometry:
- 8x16 cells, doubled when the width is at least 1024.
- A left margin of `min(4 cells, width/8)`.
- So a line holds 76 characters at 1280 px, 60 at 1024 px and 96 at 800 px.

The two lines that matter do not fit:
- **The payload line.** `Payload:       N bytes, SHA-256 <64 hex digits>` is about 99 characters. It fits only
  on screens between 824 and 1023 px wide, or 1648 px and wider.
  - The harness gives the guest QEMU's virtio GPU and overrides no resolution, so the default 1280x800 applies.
    There, the person sees 41 of the 64 digest digits.
  - At 1024x768 they see 25.
  - Nothing on the screen says the rest is missing.
- **The label line.** `Label:         "<label>" (the requester's words, not verified)` loses its
  "not verified" marker for any label longer than about 21 characters at 1280 px, and loses part of the label
  beyond that. The label bound is 128 bytes, and escaping lengthens it further.

Why it matters: the protected screen is the only place the person can check the frozen payload's digest. As
drawn, the milestone's central evidence (the canonical descriptor) is incomplete on typical screens, with no hint
that it is.

The authorization itself is unaffected:
- the confirmed operation is still exactly the frozen one;
- 41 hex digits is still a strong prefix;
- the "Label:" prefix still sets the label apart.

The gate's frame check counts colours only (field, band, text, hostile green), so it could not see this. Wrapping
a line that does not fit onto the next line, or splitting the digest across two lines, would fix it. Choosing
the cell scale from the longest line would also work.

### 2. The registration item is still open (open item, no code change)

The gate is registered in all three places:
- `check.sh` (`qemu-admin-path`);
- `verify-model/src/catalog.rs` (the gate and its prerequisite, and the guest-booting list);
- `release-required.toml`.

The bypass demonstrations are recorded. What the item still names is the owner's `./verify.sh --plan` and
`./verify.sh` run. The item is correctly left unticked until that run, and it is not a defect of the code.

## Verified

- **Grants and owners.**
  - PermissionManager is the only place that mints `admin-request`, only through `grant_for_task` (all four
    launch paths), and only for a policy row. It passes a `RIGHT_WAIT | RIGHT_TRANSFER` duplicate of the prepared
    task and the task's koid as the launch correlation.
  - `for_capability(AdminRequest)` is 0, so there is no unbound fallback.
  - The factory refuses an owner that has ended or cannot be observed, using a wait with an already-reached
    deadline.
  - `check_owners` asks every owner explicitly before anything else that is ready is served, so termination wins
    over a key, a reply or a redemption that is ready at the same time.
  - Loss of the connection and death of the owner are independent losses. Either one cancels a pending request
    and any unconsumed grant, whoever holds the endpoints.
- **The broker.**
  - Admission declines, all as `declined` and all recorded:
    - a requester that is gone;
    - a request outside its scope or past its bounds;
    - no path to a person;
    - an unrecorded outcome;
    - contention.
  - The executor's descriptor must match the asked action, parameters and payload length, and name itself as
    published.
  - Every journal acknowledgment is correlated by request and event, and a late one changes nothing.
  - The confirmation deadline is 60 s from the request's commit and the grant deadline is 30 s.
  - Approval requires the shown session, a live owner, the deadline not passed and no `blocked`. It first
    revalidates with the executor, and only then records.
  - Redemption requires `Granted`, a live original connection and owner, and the deadline. It records consumption
    and rechecks both at the acknowledgment, which is the execution admission boundary. A lost or failed
    consumption record means no dispatch, with the grant spent.
  - A lost execution reply is `outcome-unknown` after the executor's own deadline, and is never retried.
- **Presentation and input** (apart from Finding 1).
  - DisplayService: while the lock is held, surfaces are created hidden, hidden presents complete as occluded,
    and the protected pseudo-surface holds no ordinary focus. The prior surface is restored on release. A resize,
    a replaced backing or a lost scanout ends the session.
  - InputService:
    - secure attention is taken from the trusted sink, which only keyboard drivers can feed, before any ordinary
      delivery;
    - the chord is swallowed on the ordinary path;
    - protection revokes focus, discards queued pointer events and tells the console it has lost the keyboard;
    - arming waits until nothing is held.
  - `Decision` approves only an Enter down and up after both the arm and the present acknowledged the same epoch.
    Any other key pressed in between spoils the Enter.
- **The executor seam.**
  - Executors come only from `admin-executor` publications. The probe mapping exists only in development builds,
    and the DFU slot declines until something publishes it.
  - The fixture copies at most 4096 payload bytes, and never more than the object holds. Its start guard checks
    the epoch, the target generation, cancellation, whether it has started and the lifetime, and marks it started
    once.
- **The journal.**
  - Segments: at most 1024 records and 8 MB each, at most four, and only complete ones are retired.
  - Space is reserved per request at preparation, and a segment is pinned while a request in flight has a record
    in it.
  - A record is at most 8192 bytes, and one that would be larger carries a bounded refusal instead.
  - The emergency ring is 256 entries in order, dropping from behind the head and counting what it drops.
  - An unanswered commit is discarded, never rewritten.
  - A restart reads the journal back for numbering and investigation only. A torn tail is never appended to. The
    new epoch is higher than any recorded one.
- **Records and documentation.**
  - Records carry the broker epoch, request, launch, trusted requester, action, descriptor digest, event, reason,
    monotonic time and optional UTC with its provenance, and no payload.
  - `docs/THREAT_MODEL.md` §2.3 names the trusted drivers, the display, InputService and AdminService.

## Checks performed

Code reading only; nothing was built or run for this review. Files read:
- `admin_service.rs` (whole);
- `service_logic` `admin_broker.rs`, `admin_descriptor.rs`, `admin_journal.rs`, `trusted_keys.rs` and
  `display_lock.rs` (whole), and the broker's test names;
- `drivers` `admin_operation.rs` and `admin_fixture.rs` (whole);
- `admin.lsidl`;
- DisplayService's lock, present, release and reset paths;
- InputService's trusted sink, arming and `protect`;
- PermissionManager's `AdminRequest` minting and `admin_policy`;
- the gate's frame check;
- THREAT_MODEL §2.3.

The line-width arithmetic in Finding 1 is taken from `render`'s constants and the template's line formats.

---

IMPLEMENTER'S RESPONSE ON P02M0188 (2026-09-24T19:39:29Z):

## Finding 1 - The protected screen silently truncates the canonical operation: ACCEPTED

Confirmed. `render` stopped drawing a line at the right edge without any mark. The finding's arithmetic holds.

The same cut also hit a line the finding does not name: `Parameters:`. It shows up to 32 bytes as 64 hex digits
plus `...`, about 92 characters, so it was cut as well. The fix makes the screen show the whole operation, or
nothing:

- **A pure wrap step.** The new `service_logic::admin_descriptor::wrap(lines, columns)` and `VALUE_COLUMN` (15,
  where the template's values begin):
  - A line wider than `columns` continues on the next row, indented to the value column. A digest or a label is
    shown whole, and a continuation cannot pass for a field of its own.
  - Nothing is cut or reordered. Only the indent is added.
  - It returns `None` when a continuation would have no room at all.
- **`render` picks a cell size at which the whole prompt fits.** In `admin_service.rs`, `render` tries the
  doubled cells first, only on a screen at least 1024 px wide, and uses them only if the wrapped prompt fits
  vertically. Otherwise it uses single cells. The margins are now equal on both sides; before, glyphs ran to the
  right edge.
- **If even single cells do not fit, nothing is drawn.** `render` returns `None`, and `draw`'s existing path
  refuses the request with "the protected screen could not be drawn". Refusing is safer than approving an
  operation from part of its description.

What this means at the resolutions the finding names:

| Screen | Doubled cells | Single cells |
|---|---|---|
| 1280x800 (the harness's default) | 72 columns, 15 rows | 152 columns, 31 rows |
| 1024x768 | 56 columns, 14 rows | 120 columns, 30 rows |
| 800x600 | not used | 92 columns, 23 rows |

- At 1280x800 the 99-character payload line takes two rows in doubled cells.
- With every field at its bound and a label of ordinary characters, the prompt takes 12, 12 and 16 single-cell
  rows at the three sizes, so it fits at all of them. "Every field at its bound" means:
  - 64-character executor and target names;
  - the largest epoch and generation values;
  - 32 parameter bytes shown.
- The worst case is a 128-byte label made entirely of control characters, which escapes to 768 characters. With
  every other field also at its bound, it needs:
  - 16 rows at 1280x800, which fits;
  - 18 rows at 1024x768, which fits;
  - 24 rows at 800x600, where only 23 fit. There the request is refused as undrawable instead of being shown in
    part. The requester's own label can make only the requester's own request undrawable.

New host test: `a_line_too_long_for_the_screen_continues_on_the_next_row_and_loses_nothing`. It covers 56 to
156 columns and checks:
- every row fits;
- the text without spaces is unchanged, and the digest is whole;
- continuations are indented;
- a width at the indent is refused;
- lines that fit are left alone.

The gate's frame check counts colours (field, band, text, hostile green). Wrapping changes none of them, and the
field stays well above half the frame.

## Finding 2 - The registration item is still open: ACCEPTED as stated; no code change

The gate is registered in all three places, and the item waits only for the owner's `./verify.sh --plan` and
`./verify.sh` run. That is the owner's to start, so the item stays unticked.

## Milestone document

`docs/todo/P02M0188.md` records the change at the secure-attention item, the one that requires the person to
inspect the canonical operation there.

## Verification

- **Specific to this milestone:**
  - `admin_descriptor`: 4/4, including the new wrap test.
  - `qemu-admin-path` passed with the wrapped prompt: the frame check found the field, band and text, and no hostile green.
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

