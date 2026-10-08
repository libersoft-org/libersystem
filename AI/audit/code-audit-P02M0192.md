IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0192 (2026-09-27T19:14:29Z):

Status: IN PROGRESS. This record is updated as the work proceeds; the final state is at its end.

### Progress

- `src/user/libs/driver/protocol/src/gamepad.rs` (new, `pub mod gamepad` in `lib.rs`): the wire - `ARRIVAL`/`STATE`/`DEPARTURE` tags, bounds (`MAX_BUTTONS` 32, `MAX_HATS` 2, `MAX_AXES` 8, `MAX_LABEL` 32, `CENTRED` 8, `MAX_FRAME`), `Axis` (with `midpoint`, min plus half the span in 64 bits, rounded toward the minimum), `Shape` (validated: printable-ASCII label, counts; `initial()`, `state()` matching a decoded STATE against the gamepad, `admits()`), `State`, `StateFrame`, `Frame`, `decode` (bounded; refuses length/count disagreement, a hat above 8, an unknown tag), `encode_arrival`/`encode_state`/`encode_departure`; and the PUBLISHER TABLE `Publisher<N>` with the `Sender` trait (send-or-refuse, never close): `attach` (handles never reused), `report` (owed only on change, one owed STATE per gamepad carrying the current state), `detach` (clears the unsent mark; a gamepad whose ARRIVAL is still owed departs silently), `connected` (every held gamepad owed ARRIVAL + STATE, departed ones dropped), `disconnected`, `flush` (oldest owed first until a refusal; returns whether anything is still owed - the driver's one-tick retry condition), `owes`. `no_std`, fixed arrays.
- `driver_protocol::provider::GAMEPAD = 21`.
- Host tests `gamepad/tests.rs` (9): round trip incl. the widest shape (= `MAX_FRAME`), every malformed frame refused, a STATE refused by a gamepad it does not fit, the initial state (midpoints incl. the full `i32` range), a refused STATE sent later as the current state (changes folded), a refused ARRIVAL before the first STATE and a refused DEPARTURE after the last in owed order, a DEPARTURE clearing the unsent mark and an unannounced gamepad departing silently, every connection (first and later) owed ARRIVAL + STATE and nothing sent to no consumer, no refusal closing anything and handles never reused. `cargo test --manifest-path user/libs/driver/protocol/Cargo.toml --target x86_64-unknown-linux-gnu`: 75 passed. Clippy: no finding in the new module (the crate's older test findings are untouched).

### What was implemented (all parts of the plan)

THE VOCABULARY AND THE CAPABILITY
- `src/idl/input.lsidl`: records `gamepad-axis`, `gamepad`, `gamepad-state`; variant `gamepad-event { present, arrived, state, departed(u32) }`; `input.subscribe-gamepads` (op 4, focus proof) and `input.observe-gamepads` (op 5); `input-admin.open-gamepads` (op 2, after `open-keys`); and, after a `development only` separator, the `gamepad-fixture` interface (`attach(label)`, `report(handle, buttons, hat, axes)`, `detach(handle)`). `src/idl/security.lsidl`: `input-gamepad` appended last to `capability`. `src/idl/device.lsidl`: `gamepad = 21` appended to `provider-kind`. Regenerated with `./gen.sh --accept-breaking` (the enum additions are what the generator calls breaking; the precedent is every earlier provider kind); `docs/gen/liber/{device,input,security}/v1.{abi,md}` and the three generated protocol crates changed accordingly. The version stays 1.
- `src/user/services/core/src/permission_manager.rs`: `VOCABULARY` grows to 46 with `Capability::InputGamepad` last (the compile-time exhaustiveness check passes), grant tag `INPUT_GAMEPAD`, `for_capability` answers 0 (minted per launch), `grant_for_task` mints it through `input_admin::Client::open_gamepads()`; policy rows `gamepad` -> `[InputGamepad]` only, `gamepadcheck` -> `[FixtureControl]` only.

THE PROVIDER KIND, THE WIRE AND DISCOVERY
- `driver_protocol::provider::GAMEPAD = 21`; `system-manifest` `ProviderKindName::Gamepad` (21); DeviceManager's two kind mappings; the xHCI driver's `provides` row `{ kind = "gamepad", most = 1 }` (appended last); InputService's `CATALOGUE` role kinds `["input", "pointer", "touch", "gamepad"]`.
- `src/user/libs/driver/protocol/src/gamepad.rs` (+ `gamepad/tests.rs`): the wire and the publisher table, as recorded under "Progress" above. The CONSUMER half (`Axis::midpoint`, `Shape::new/label/buttons/hats/axes/initial/button_mask/state`, `decode`) is `#[inline]`: InputService is a dynamic executable and `driver-protocol` is not a shared library, so a non-inlined call from it would be an import no provider exports (found by the x86_64 build's provider check).
- The discovery rule: InputService keeps its `gamepad` catalogue subscription open and adopts every live gamepad provider up to four (`adopt_pad_source`, `lose_pad_source`); a withdrawn provider or a closed connection departs all of its gamepads; a closed connection is never reopened. Input, pointer and touch keep their bootstrap-only discovery.

INPUTSERVICE (`src/user/services/core/src/input_service.rs`)
- `Gamepads` (catalogue subscription, sources, pads, next id, focused stream, console stream); ids are InputService's own, given at each arrival and never reused; at most eight gamepads keyed by (source, handle), an arrival past that refused with one line.
- `PadStream` with a fixed `Owed` list (one pending state per gamepad and stream; a fixed array rather than `Vec<u32>`, because the x86_64 build's provider check found InputService importing `RawVec<u32>::grow_one` from an unrelated provider, `modem_device_proto`, which is not in its provider list). `feed` never blocks; `flush` sends pending states oldest first as room allows; `offer_state` coalesces; `offer_whole` sends `present`/`arrived`/`departed` only after the pending states and closes the stream when they cannot all go; `close(release)` sends the release (no buttons, centred hats, axes as they were) for every gamepad holding anything or owed a state, then closes.
- The serve loop flushes pads at every wake and, while any state is pending, waits with `wait_any_periodic` on a one-tick retry deadline (a housekeeping wake). Waitset: the subscription, each source, each stream's producer (to notice a reader gone).
- Focus: `subscribe-gamepads` (op 4) on the key/contact focus proof, one live stream replaced by a new valid one, released and closed on focus loss (`set_focus`); `observe-gamepads` (op 5) only on the new `Scope::Gamepad` connection that `open-gamepads` returns, refused on every other scope, refused while a surface holds focus, refused while the console stream is open; a surface taking focus (`SET`) releases and closes the console stream; `protect()` (secure attention) releases and closes both, and both ops are refused while protected; state is still tracked. Gamepad input is never forwarded to ConsoleService.

THE MAPPING (`src/user/drivers/core/src/hid.rs`, `usb_hid.rs`, `usb_class.rs`, `xhci.rs`)
- `hid::parse` records per segment the application collection's usage and ordinal and the Null State bit; a Game Pad (0x05) or Joystick (0x04) application collection with at least one mapped control is one gamepad; `Layout::has_gamepad` joins `is_useful`; `input_segs()` excludes gamepad segments from `has_pointer/has_keyboard/has_consumer/has_digitizer/keys_diff/pointer_fold/contacts`.
- `Layout::gamepads() -> Vec<GamepadMap>`: axes (GD 0x30..0x38 and the Simulation page, bit order, at most eight, declared signedness and range), buttons (variable one-bit Button-page 1..32), hats (0x39, 8 or 4 positions, at most two). `GamepadMap::apply` implements the range rules: a hat out of range is centred (with or without Null State), a Null-State axis out of range keeps its value, a non-Null-State axis out of range is refused for that field while the rest applies (`Applied { refused }`), and the driver says so once per gamepad.
- The xHCI HID module binds every HID interface (up to four) with its own ring, report page, layout, previous reports and gamepad states, the boot-keyboard fallback per interface, one Configure Endpoint for all endpoints, routing by slot and DCI; `Hids` holds a device once with its interfaces; unuseful interfaces give their ring and page back. Inventory kind `gamepad` (19, after `dfu`), ranked keyboard > touch > gamepad > pointer; online line `(gamepad)`.
- `HID_INTERFACES = 4`, `HID_COST` = 4 endpoints, `4 * (RING_BYTES + 4096)` bytes, 4 in flight; `HID_LIMITS` 8 devices at eight times that; charged once per device.
- Publication: the `gamepad` provider token is appended last (token 9) and published unconditionally; offers past `MAX_INITIAL_OFFERS` (8) are sent after READY with `common::offer_named`, since DeviceManager drops more initial offers than that. ARRIVAL labelled `usb VVVV:PPPP port P if I[ pad N]`, STATE per changed report, DEPARTURE from the detach path (`depart`) for every gamepad of every interface before rings and pages are released; per-driver handle counter. A retry timer and `common::wait_providers(..., housekeeping)` (a new helper using `wait_any_periodic` while anything is owed); `check-driver-connections.py` knows the helper.

THE TOOL (`src/user/apps/tools/src/gamepad.rs`, manifest `[[programs]] gamepad`, dynamic, providers `base-proto input-proto ipc-client lico wire lsrt`; shell `InteractiveArgs`; synopsis; `lib.sh` wave 3; the kernel's dynamic batch launch list)
- Full screen by default (alternate screen, raw input, hidden cursor, `TerminalGuard` restores on every exit path; `q` or Ctrl+C leaves; every queued event read before each redraw; a status line for arrivals and departures; "No gamepad is connected." when empty); `--lines` prints `gamepad: watching`, one line per event and `gamepad: closed`, and exits when the stream closes or its reader is gone. A refused or closed stream is reported with its causes and the tool exits non-zero.

FIXTURES AND GATES
- `src/harness/usb-gadget.sh`: `hid-gamepad` gains a 4-bit Hat Switch 0..7 (physical 0..315) with Null State plus 4 bits of padding (7-byte report); new kind `hid-gamepad-pair` (`hid.usb0`, `hid.usb1`). `src/harness/gamepad-source.py` (new): device nodes by configfs `dev`, waits for the guest's configuration, two levels of 200 reports each, then unbinds the gadget. `src/harness/test-kernel.sh` starts it for `hid-gamepad-pair` and adds the kind to the root-port-3 list.
- Kernel tests: `kernel.hardware.usb_gamepads_report_which_pad_pressed_what` ([Drivers, Usb, Slow]); `kernel.services.gamepads_reach_the_focus_owner_and_the_console_watcher_by_identity` (the three existing InputService tests answer the fourth catalogue subscription with an empty snapshot). `src/kernel/Cargo.toml` gains `input-proto` for the latter.
- `gamepad_fixture` (development-only driver, `edu` at 00:17.0, `gamepad` provider + `fixture-control` serving `gamepad-fixture`) and `gamepadcheck` (development-only probe); gate `qemu-gamepad-tool` (`src/tools/check-gamepad-tool.sh`) registered in `check.sh`, the verify-model catalog (`GATES` with `userspace.build`, `GATES_THAT_BOOT_A_GUEST`) and `model/release-required.toml`.

### Verification (commands and results)

PASSED:
- `./gen.sh --check`: no drift (33 packages).
- Host: `cd src && cargo test --manifest-path user/libs/driver/protocol/Cargo.toml --target x86_64-unknown-linux-gnu` - 75 passed (the nine gamepad wire/publisher cases among them); `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib` - 378 passed (hid: the gadget's exact descriptor with its hat, a joystick with Simulation throttle and rudder, two Game Pad collections under two report ids, the hostile variants - nine axes, forty buttons, a five-position hat, a four-position hat, a Game Pad collection with nothing mapped, an out-of-range axis with and without Null State, an out-of-range hat without it - and the regression bar over the boot keyboard, a relative mouse and QEMU's tablet; usb_class: the four-interface charge, eight devices filling every dimension exactly, a ninth refused by count, the detach giving all back).
- Clippy on the drivers library and on `driver-protocol` (all targets): no finding in the changed modules.
- rustfmt `--edition 2024 --check` on every changed Rust file (xhci.rs was reformatted once: the offer array's indentation after its type changed); `shfmt -d` and `bash -n` on `check.sh`, `lib.sh`, `test-kernel.sh`, `usb-gadget.sh`, `check-gamepad-tool.sh`; `py_compile` on `gamepad-source.py` and `check-driver-connections.py`.
- Test kernel: `cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests` - builds.
- `./build.sh --arch x86_64`: ok (one rustc SIGSEGV on the first attempt, the known intermittent crash on this machine; the retry built).
- x86_64 services suite: `TEST_SELECTION=kernel.services.gamepads_reach_the_focus_owner_and_the_console_watcher_by_identity,kernel.services.input_service_streams_pointer_events,kernel.services.a_touch_surface_reports_contacts_and_not_a_cursor,kernel.services.input_service_streams_keys_only_with_display_focus ./test.sh --arch x86_64` - 4 passed. Three defects in the new test itself were found and fixed on the way: 100 driver frames sent without letting the service read (the driver's channel is 64 deep - it now pumps every 16); the slow reader's owed state goes on the service's one-tick periodic retry, which `run_until_idle` does not wait out (the test now moves the test clock with `crate::tests::advance_clock(2)` between reads); the protected-input stream (keys) was drained through the gamepad decoder; and the console watcher's RELEASE frame, queued before the close, was mistaken for the stream still being open (the test now reads the release - the second gamepad's held button let go - and then asserts the close).
- x86_64 dynamic launch: `kernel.dynamic.dynamic_process_service_loads_programs_from_system_bin` passed with `gamepad` in its batch (run x86_64-20260927T203115Z; it also passed with the tool removed from the list, as a control).
- USB proof: `USB_GADGET=hid-gamepad-pair TEST_SELECTION=kernel.hardware.usb_gamepads_report_which_pad_pressed_what ./test.sh --arch x86_64` - 1 passed, and it RAN (not `NOT RUN`): `usb-gamepad: two gamepads on interfaces 0 and 1 reported their own levels in 4 and 4 state(s), released centred, and left with the device`; the boot line `driver.xhci: online (00:05.0, 8 device(s)) (keyboard) (pointer) (storage) (network) (uas) (audio) (gamepad)`; the far end printed its two levels and the unplug. Then `src/harness/usb-gadget.sh verify`: `the host carries nothing of this harness's`.
- Development gate: `LIBER_DEVELOPMENT=1 ./image.sh --format iso`, then `./check.sh --gate qemu-gamepad-tool` - passed (`driver.gamepad-fixture: online`, `gamepadcheck: PASS which gamepad pressed what`). The first image attempt was refused by build-shared ("the staged tree ... holds a selection candidate no consumer was built against": `abiprobe`/`vkprobe` admitted an `icdprobe` of the non-development build); `foreign-audit-link.py --check` reproduced, so it was the staged tree and not the audit link, and the build's own advice (clear `.build/image/x86_64-unknown-none`, rebuild) fixed it.
- Gates: `declared-interfaces` (85 interfaces, 21 provider kinds agree between the IDL and the driver wire), `provider-routing`, `no-fixed-provider-slots`, `source-hygiene`, `verify-model` - all pass.

ADJACENT REPAIRS, BOTH TO GATES THIS PLAN NAMES AND BOTH BROKEN BEFORE THIS WORK:
- `src/tools/check-provider-catalogue.py` (part of `no-fixed-provider-slots`) did not compile since DeviceManager's catalogue connections became scoped (`Scope` from `service_logic::catalogue_scope`): the fixture now imports `Scope` (and depends on `service-logic`), its `open_subscription` stub and `CatalogueView` carry the scope, and `serve` serves the root read-only (`Scope::inventory()`, as production does) and a minted client with every kind (`Scope::unrestricted()`, which is what these delivery and publication regressions ask of it). The 8 regressions pass and all 6 mutations are rejected again.
- `source-hygiene`: seven harness commands started as `python3 path` carried a shebang without the executable bit (`midi-source.py`, `mtp-root.py`, `printer-sink.py`, `ups-sim.py`, `usb_ffs.py`, `usb_gadgetfs.py`, `usbredir_device.py`); marked executable.

FOUND, NOT THIS MILESTONE'S, NOT CHANGED (reported): an x86_64 BOOT-STACK OVERFLOW in some kernel test selections. Run first in a boot, `kernel.dynamic.dynamic_process_service_loads_programs_from_system_bin` builds the cached LiberFS fixture (`StorageHarness::build_system_fixture` -> mount -> `crc32c` copying 8 KiB blocks by value in the debug test kernel) from its very large frame, walks off the loader's 128 KiB boot stack (`STACK_PAGES = 32` in `src/boot/loader/src/arch/x86_64/mod.rs`, no guard page) and overwrites BootInfo, the region array and the module array below it; the next test that reads a boot module panics in `module_bytes`. Proved with a gdb hardware watchpoint on BootInfo's first word (the writer is that `memcpy`, rsp 0x...7c10fe00 below BootInfo at 0x...7c110000); the frame allocator never freed or handed out the page. Pre-existing and ordering-dependent: in a full run the applications suite builds the fixture first from shallow frames. aarch64 and riscv64 raised their boot stacks to 256 KiB for this same path (their linker scripts say so); x86_64 was not raised. Proposed fix for the owner: `STACK_PAGES` 32 -> 64.

NOT YET PERFORMED (end of the job, with the other cross-architecture work):
- aarch64 and riscv64 cross-builds.
- The dynamic report refresh (`./check.sh --refresh dynamic-report`): its writer needs all three targets' graphs; the x86_64 build reports `inventory_status=refresh-required` for the new tool until then.

Status: implemented; open only for the two end-of-job items above.

ADDENDUM (2026-09-28, found while working on P02M0195a): the `system-manifest` host suite was not among the checks run above, and one of its cases failed on this milestone's manifest - `the_production_manifest_classifies_every_staged_driver` lists the in-guest fixtures that master nothing (`dma = "none"`) and did not name `gamepad_fixture`. It now does (`src/tools/system-manifest/src/tests.rs`); `cargo test` in `src/tools/system-manifest`: 25 passed.

ADDENDUM (2026-09-28, found while verifying P02M0195a): `./check.sh --gate component-oracles` listed
`gamepad_fixture` - added to the manifest by this milestone - as a staged driver with neither a kernel-test
oracle nor a stated reason. Its oracle is the `qemu-gamepad-tool` gate on the development image, which the census
(it reads kernel `covers` declarations only) cannot see; `src/tools/component-oracle-exceptions.txt` now says so.
The gate still fails, for 14 other staged drivers and services that are not this milestone's.

ADDENDUM (2026-09-28, found while verifying P02M0198a): the kernel test
`kernel.applications.permission_manager_enforces_static_and_dynamic_probe_policy` pins the whole capability
vocabulary each probe is denied, and `input-gamepad` - appended to the vocabulary by this milestone - was
missing from its `later_denials!` list, so the test failed on x86_64 (`--tags process`) with the summary ending
`... admin-test=deny input-gamepad=deny` against an expectation ending at `admin-test=deny`. Fixed in
`src/kernel/test_suites/applications.rs` (`later_denials!` gains ` input-gamepad=deny`); the other probe
summaries in that file use the same macro. No other pinned copy of the vocabulary exists in the tree
(`grep admin-test=deny`). Re-verification: the same tag run, recorded in the P02M0198 record.

## The dynamic report, refreshed at last (2026-10-02)

- `./check.sh --refresh dynamic-report`, run once all three targets' graphs were current, FAILED on this milestone's
  tool: "x86_64-unknown-none gamepad has a generic transport residual
  ...ChannelTransport...wire::Transport::call=ipc-client" - `gamepad` built `input::Client::new(ChannelTransport { .. })`
  itself, which instantiates the generated client's codec in the tool, the copy the client libraries exist to avoid.
  FIXED as every other tool does it: `input-client` (the one operation, `observe-gamepads`, declared by its stable
  name) and `input-client-provider` (its trampoline to the `liber_channel_impl_*` symbol `input-proto` exports), a
  `lib/clients/input-client.lslib` library row, the tools crate's optional dependencies under `shared-image`, and
  `gamepad`'s providers what it now imports (`base-proto`, `input-client`, `input-proto` for the stream frames'
  decoder, `lico`, `lsrt`). Its regression run and the refresh's are in the batch running now.
- (2026-10-01) `./build.sh --arch aarch64`, `--arch riscv64` and `--arch x86_64` -> ok with the client library;
  `./check.sh --refresh dynamic-report` -> ok, then the gate `dynamic-report` -> PASS; `qemu-gamepad-tool` -> PASS
  (360 s). Status: COMPLETE.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0192 (2026-10-08T03:25:49Z):

Read the complete plan and prior implementation evidence, and reviewed the shared publisher, InputService stream/discovery integration and shipped tool. The feature and its prior required USB, service, tool, cross-build and dynamic-report verification are present. Found one explicit identity-contract gap: Publisher::attach and InputService::pad_arrived wrap exhausted u32 counters to 1, reusing lifetime identities. Implementation continuation addresses only that gap and its boundary verification.

Status: continuation in progress; no unrun checks are claimed.

### Identity exhaustion correction

- `driver_protocol::gamepad::Publisher::attach` now uses zero only as the exhausted-counter sentinel: it issues the last nonzero `u32` once, then refuses attachments instead of wrapping to 1. Detach, disconnect and reconnect preserve exhaustion. No wire shape or ordinary attachment behavior changed.
- `InputService::pad_arrived` applies the same rule to the service's independent id space and reports exhaustion before changing the gamepad set or emitting an arrival.
- xHCI's refused-publication diagnostic now covers exhausted slots or lifetime handles; its existing refusal behavior is retained.
- `exhausting_handles_never_reuses_a_departed_gamepads_identity` exercises a freed early handle, the final handle, refusal while slots remain, detach and reconnect, and absence of spurious owed frames.
- PASSED: `cd src && cargo test --manifest-path user/libs/driver/protocol/Cargo.toml --target x86_64-unknown-linux-gnu gamepad::tests` (10 passed, 0 failed); `rustfmt --edition 2024 --check src/user/libs/driver/protocol/src/gamepad.rs src/user/services/core/src/input_service.rs` (exit 0).
- Final cross-build and service/tool regression verification for this correction is pending the whole job's final verification stage. The prior real USB proof remains applicable to unchanged descriptor mapping and HID transport.

### Final-stage tool regression (2026-10-08 continuation)

PASSED: `./check.sh --gate qemu-gamepad-tool` on the current development ISO, with terminal `check.sh: RESULT ok exit=0 seconds=84` in `.build/logs/end-of-job/continuation-gamepad-tool.log`. The retained guest transcript is `.build/logs/end-of-job/continuation-gamepad-tool-guest.log`; it contains the fixture's online line and `gamepad --lines | gamepadcheck` followed by `gamepadcheck: PASS which gamepad pressed what`. The final-stage coordinator stopped the owned guest and its console after the scripted probe completed, before the gate's idle timeout; the gate's existing oracle ran unchanged and passed. This is new tool-path evidence for the lifetime-id correction.

The first selected kernel invocation was refused by preflight because the x86 artifact was stale after a later SSIF source fix; no guest test ran in that attempt. A refreshed x86 build is now running the six selected InputService/real-USB pair tests. Their outcome and the final all-target build outcome are still pending, so this record does not yet claim final completion.

PASSED after the fresh x86 build: the coordinator's six-test `TEST_SELECTION=... ./test.sh --arch x86_64` invocation (the exact stable IDs are listed below), using the real `hid-gamepad-pair` USB gadget. `.build/logs/end-of-job/continuation-input-kernel-current.log` ends `test.sh: RESULT ok exit=0 seconds=69`; the suite reports 6 passed in 53 s. Retained run and guest evidence: `.build/logs/test/x86_64-20261008T040252Z-317223-{run,guest}.log`. The runner records gadget setup, both held input levels, unplug and complete teardown, while the guest confirms two gamepads on interfaces 0 and 1 each reported its own levels in four states, released centred and departed with the device. All six selected IDs have explicit `[ok]` verdicts:

- `kernel.hardware.usb_gamepads_report_which_pad_pressed_what`
- `kernel.services.a_touch_surface_reports_contacts_and_not_a_cursor`
- `kernel.services.gamepads_reach_the_focus_owner_and_the_console_watcher_by_identity`
- `kernel.services.input_service_streams_keys_only_with_display_focus`
- `kernel.services.input_service_streams_pointer_events`
- `kernel.services.pointer_and_touch_providers_are_followed_as_they_are_published_and_withdrawn`

This closes the changed InputService/xHCI path's targeted service and actual USB regressions. The final all-target build verdict is still pending; the milestone's continuation status remains open until that required verification is available.

The exact successful selected-guest command, supplied by the final-stage coordinator, was:

```sh
LIBER_DEVELOPMENT=1 USB_GADGET=hid-gamepad-pair TEST_SELECTION='kernel.services.input_service_streams_pointer_events,kernel.services.a_touch_surface_reports_contacts_and_not_a_cursor,kernel.services.pointer_and_touch_providers_are_followed_as_they_are_published_and_withdrawn,kernel.services.input_service_streams_keys_only_with_display_focus,kernel.services.gamepads_reach_the_focus_owner_and_the_console_watcher_by_identity,kernel.hardware.usb_gamepads_report_which_pad_pressed_what' ./test.sh --arch x86_64 > .build/logs/end-of-job/continuation-input-kernel-current.log 2>&1
```

PASSED on the final runtime source: `LIBER_DEVELOPMENT=1 ./build.sh --arch all`, with `.build/logs/end-of-job/continuation-build-all-final.log` ending `build.sh: RESULT ok exit=0 seconds=487` and explicitly naming complete sdk/libs/user/kernel/loader/packages/volume builds for x86_64, aarch64 and riscv64. Independently read this terminal verdict. The final static verification in `.build/logs/end-of-job/continuation-static-final.log` also passed: source hygiene, model validation and all 157 model tests, ending `check.sh: RESULT ok exit=0 seconds=205`.

Final state before performance measurement: implementation, boundary host test, tool/service/real-USB regressions and three-target compilation are complete. The job will refresh and check the dynamic report after restoring ordinary x86 artifacts from the optimized account run. This final current-artifact report check is the only remaining verification tracked for this continuation; no additional implementation is outstanding.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

Final documentation consistency check (2026-10-08): the plan header and TODO correctly leave the continuation open for the final current-artifact dynamic report, but its combined Verification checkbox still appeared complete. Reopened only that checkbox, preserving all requirement text and historical DONE evidence. Its already-passing host/guest/build checks remain passed; the final report refresh/check must pass before the combined item and milestone can be checked again. No source/build input changed.

Final report attempt interrupted (2026-10-08T13:17:02Z): `./check.sh --refresh dynamic-report` was running when the active turn was externally interrupted. On continuation its unified-exec handle 79409 was missing and the recorded process IDs 855105/855106/855113/855114 no longer existed. `.build/logs/end-of-job/continuation-dynamic-refresh-final.log` contains no terminal verdict; no successful refresh is claimed. The tracked reports were not changed by that attempt. The same required refresh is being rerun in a separate log, preserving the interrupted attempt.

### Final verification and completion (2026-10-08T13:33:17Z)

After P02M0189 measurement, the ordinary x86 image was restored with `env -u CARGO_PROFILE_DEV_OPT_LEVEL LIBER_DEVELOPMENT=1 ./image.sh --format iso --dma-mode enforcing-required`; its full x86 build passed (73 s), and kernel, virtio-gpu, DisplayService and demo hashes match the ordinary measurement artifacts. No further guest runtime source changed.

PASSED: `./check.sh --refresh dynamic-report` (393 s), log `.build/logs/end-of-job/continuation-dynamic-refresh-retry.log`; then `./check.sh --gate dynamic-report` (388 s), log `.build/logs/end-of-job/continuation-dynamic-check-final.log`. Both have terminal exit 0. The check reports 92 tools on each of three targets, six waves and three whole images matching. Refreshed `docs/DYNAMIC_EXECUTABLES.tsv`, `docs/DYNAMIC_WAVES.tsv` and `docs/DYNAMIC_IMAGE.tsv`. The 276 tool rows and their import/provider mapping sets are unchanged; generated symbol ordering and affected library footprint fields are refreshed. The interrupted first refresh remains separately recorded above.

Final state: COMPLETE. Lifetime identities no longer wrap; required host boundary checks, the tool gate, six service/real-USB tests, all-target builds and current dynamic-report verification are satisfied, with commands and limitations recorded above. The original combined verification item and TODO entry are checked again only after these final results. No new full unchanged kernel or input-suite rerun is claimed, and no required implementation or verification remains for P02M0192. Prior audit content is preserved.

Final coordinator consistency checks: `./check.sh --gate milestone-index` PASS (3 s; `.build/logs/end-of-job/continuation-milestone-index-final.log`), `git diff --check` PASS. All thirteen original audit files remain exact byte prefixes from baseline `d0f54598`, each with its UTC continuation record. Plan/index review confirms ten completed requested milestones and only P02M0196, P02M0197 and P02M0202 open for their explicitly recorded hardware/design requirements. No owned QEMU, TCPCI backend or UPS simulator remains running.
