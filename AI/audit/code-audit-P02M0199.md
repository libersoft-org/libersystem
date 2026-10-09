IMPLEMENTER'S IMPLEMENTATION ON P02M0199 (2026-10-04 to 2026-10-05):

Scope: `docs/todo/P02M0199.md`, both halves - the contract, the join and the floor (0199a), the USB monitor class,
the ACPI video backlight, the ambient-light sensors and the sleep (0199b), the key path, the policy and the tool
(0199c), and the verification (0199d). DDC/CI stays open: it waits for a display controller that can reach a bus.

## What was implemented

**The contract.** `display-device.lsidl` gained `backlight` and `ambient-light` (the device side) and `display.lsidl`
`display-brightness` (the read), `display-brightness-control` (the set, the policy's alone) and `brightness-policy`
(the policy's control). Provider kinds `backlight` (31) and `ambient-light` (32) in every spelling; capabilities
`brightness` and `brightness-control` with three permission rows (`brightness`, `brightcheck`, `brightread`).

**DisplayService** (`display_service/brightness.rs`): the catalogue's backlights opened and described, the join
(`service_logic::brightness::join`, host-tested), the floor (5 % of the range), steps, the set with its explicit zero,
the subscription with a serial per change, the system keys from InputService's new `SYSKEYS` root, firmware hotkeys,
one press one step across both sources, and `touched` - whether anything moved a level since it appeared.

**The brightness policy** (`brightness_policy.rs`, decisions in `service_logic::brightness::Policy`, host-tested):
stored levels after a two-second settle, never its own changes (serials); restore for a backlight nothing has moved;
firmware defaults by power source; idle dim to 30 % and its undoing; automatic brightness through `_ALR`'s curve or a
default one with hysteresis, paused by any change not its own until the next idle.

**The USB half.** `drivers::hid_display` maps the VESA Brightness, the EDID Information bytes and the Sensors page's
Illuminance from the shared field table; `class_display_hid.rs` in xhci publishes a backlight and, through a second
publication `classes::Module` gained, an ambient-light provider from one interface; the sensor's reporting and power
states are set at bind (one-element feature arrays the table now records). The HID field table admits a feature report
up to 256 bytes. The `monitor` and `consumer-keys` gadget kinds, `monitor-sim.py` and `keys-sim.py`.

**The ACPI half.** The kernel's boot-framebuffer decoder (`firmware::decoder_of` over the boot scan's BAR record, in
`abi::Framebuffer`); `acpi_backlight` (the `video-output` class, `_BCL`/`_BCM`/`_BQC`, `^_DOS` 0x04 at bind and resume,
the notification map, the firmware that steps itself) and `acpi_als` (`ACPI0008`: `_ALI`, `_ALR`, `_ALP`, 0x80/0x82),
their pure parts in `drivers::acpi_video`, host-tested. The fixture's `--brightness` devices on a ninth GPIO line, and
`qemu-run.sh`'s `VGA=std`.

**The tool and the probes.** `brightness` over the new `display-client` library; `brightcheck` and `brightread`.

## Found and fixed on the way

- The catalogue scope's mask was 32 bits and could not express kind 32: widened to 64 (`catalogue_scope`, DeviceManager).
- A `kind = "client"` role is a duplicate of the provider's root, shared with the supervisor's mints; PowerService answers
  nothing on its root but a mint. The policy's display, power and activity roles are `factory`.
- bash's printf hands its output over at each 0x0a byte and each write of a configfs `report_desc` replaces the
  descriptor: the monitor's descriptor arrived as its last eighteen bytes. `usb-gadget.sh` writes it through `dd`.
- The dev instance boots from the medium's own volume and keeps nothing across a reset: the ACPI gate runs with a
  `RUN_DISK`.

## Verified

- `brightness-usb` PASS (2026-10-05), `brightness-acpi` PASS (2026-10-05, 318 s): every check the plan names.
- Host suites: `service_logic` brightness 22 and catalogue_scope 5; `drivers` 495 (hid_display 8, acpi_video 6, keys);
  the kernel's firmware tests (decoder, DMAR identity) and the `boot` tag (16) with the policy online; the twelve
  DisplayService and InputService kernel tests whose harnesses hand the new roles.
- Host gates scoped to the change: bootstrap-plan, boot-manifest, dependency-policy, declared-interfaces,
  provider-routing, verify-model, verify-model-tests, guest-verdict, source-hygiene, test-tags. `capability-model` ran
  past the 20-minute bound this batch gave each gate and is to be run alone.

## Departures, kept and stated in the milestone

- A backlight call is bounded (a tenth of a second), not asynchronous: the generated clients are synchronous.
- A shadowed backlight answers `denied`: the error vocabulary has no `busy`.
- The policy's roles are factory roles, not plain clients (above).
- The gadget's EDID is its first thirty-two bytes: `usb_f_hid` answers GET_REPORT from at most sixty-four.

## Owner questions, collected for the end of the job

The defaults - floor 5 %, automatic brightness off, idle 300 s to 30 %, the default curve - and `_ALR`'s percent of
normal taken as percent of the range.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-09 18:19:13 UTC):

Read the complete plan, its existing implementation record, DisplayService brightness handling, the USB monitor HID mapping/driver, and the monitor fixture/gate. The user's Phase 2 scope excludes physical-only hardware acceptance, while retaining QEMU/software requirements. Existing implementation has synchronous backlight RPCs in DisplayService, does not consume monitor brightness Input reports, and has no recorded no-`_DOS` adapter test. These are software verification/implementation gaps, not physical-hardware exclusions. EDID integration is being implemented with P02M0099; EDID access alone does not provide a DDC/CI brightness transport. No new tests have been run for this continuation yet. Existing audit content is preserved.

Progress (2026-10-09 18:30:33 UTC): implemented asynchronous backlight catalogue open, describe, initial get, events open and set in `display_service/brightness.rs`. Each stage uses a fresh non-reused correlation, an absolute deadline and nonblocking sends/receives; withdrawal cancels pending work and closes acquired endpoints, late transferred capabilities are closed, failed writers leave the join until their event stream recovers. Keys/events remain queued while a write is outstanding so repeated steps use the acknowledged current level. Root replies/subscriptions are nonblocking. A separate Backlight-only `BRIGHTCAT` factory role was minimally necessary: DisplayService's existing Display-only `CATALOGUE` performs synchronous RPCs and cannot safely share asynchronous replies. This changes connection separation, not granted device kinds. All positional test bootstraps were updated. The original single-CATALOGUE specification and synchronous-departure historical notes are retained and superseded by this documented integration decision.

The HID monitor mapper/driver now consumes a Monitor Control Brightness Input field independently of its writable Feature field, opens the interrupt pipe even without an ambient sensor, and publishes level changes with sequence numbers. Host tests cover monitor-only input, bounds/short bodies, and refusal of VESA brightness outside a Monitor application. The monitor gadget adds report 4; its firmware can make a controlled device-originated change from55 to63. The USB gate requires that public level and rejects an echoed SET63. The ACPI fixture can omit only `_DOS`, retaining `_DOD` and the panel methods; `BRIGHTNESS_NO_DOS=1 src/tools/check-brightness-acpi.sh` verifies this mode through bind, reboot, idle and S3 resume. These guest changes are not yet run. EDID propagation from P02M0099's display metadata now feeds the brightness output/join.

Independent review identified a pre-existing128-byte public snapshot ceiling and a new128-byte admission-reply regression. Both are fixed locally: variable-sized ready messages use channel peek, fallible allocation and nonblocking receive; public lists and change frames use the generated record writers with a VecWriter. The new kernel fixture `kernel.services.display_service_keeps_serving_while_backlights_stall` exercises silent providers, late/withdrawn replies, recovery,101-level descriptions and multiple snapshot rows; its kernel compile passed, runtime pending.

Verification: PASS `RUST_MIN_STACK=33554432 cargo check --offline --bin display_service --target x86_64-unknown-none` from `src/user/services/core` (latest1.35s, log `display-brightness-variable-check.log`); PASS `RUST_MIN_STACK=33554432 cargo test --offline --manifest-path src/user/drivers/core/Cargo.toml --lib hid_display -- --nocapture` from repository root (10tests; log `hid-display-host-tests.log`); PASS `python3 src/harness/acpi-fixture.py --self-test`, Python syntax compilation, shell syntax checks and `git diff --check`. Logs under `.build/logs/end-of-job/qemu-only-20261009/`. Earlier compilation FAILED on VecWriter::tag and the wrong identity type, both corrected. An accidental root-cwd cross-target invocation failed because that toolchain lacks the target; a drivers-cwd host-test invocation selected the configured bare-metal target and failed to find std. Neither is counted as a pass; corrected invocations above are the evidence. Long tests/guests and final integrated verification remain unperformed for this continuation.

Progress (2026-10-09 18:45:54 UTC): completed the plan's firmware-backlight selection rule. The additive optional `backlight.firmware-display-id` operation carries ACPI `_ADR` without changing the existing description record; ACPI evaluates it, USB returns None, and DisplayService admits older providers via Unsupported/metadata-timeout fallback. Firmware candidates prefer internal panels, then their stable ACPI namespace keys. A withdrawn provider cannot be resurrected by that fallback. Added exact host cases for internal/external priority, equal-priority namespace order and absent IDs. PASS: brightness logic 23 tests, `./gen.sh` (38 packages and aggregate/docs, 28 s), focused development service checks (2.07 s), and `RUST_MIN_STACK=33554432 cargo check --offline --bin acpi_backlight --bin xhci --target x86_64-unknown-none` from drivers/core (1.42 s). Logs remain under `.build/logs/end-of-job/qemu-only-20261009/`. Registered `brightness-acpi-no-dos`; neither ACPI variant nor the changed USB/kernel fixtures has yet run in a guest for this continuation.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-09T19:03:34Z):

Independent continuation read the complete 460-line plan, DisplayService's asynchronous brightness provider and client paths, the new FirmwareDisplayId contract/ACPI implementation and node admission rules, tie-break logic, channel ownership rules, and the actual DisplayService fixture. No additional production defect was established in those paths. The existing asynchronous fixture uses only Native providers, so it does not exercise successful, refused or unanswered optional firmware metadata. Extending that same live-service fixture to cover these required compatibility/lifecycle paths before final guest verification; no guest result is claimed.

Implementation/verification update (2026-10-09T19:05:06Z): added `kernel.services.display_service_admits_optional_firmware_metadata_without_stalling` in `src/kernel/test_suites/display_brightness.rs`. It drives actual DisplayService through the dedicated catalogue, first an old firmware provider that ignores FirmwareDisplayId, then an explicit Unsupported response, then two internal-panel identities. It requires display heartbeat/read progress while metadata is held; a fresh GET correlation after its deadline; late metadata rejection and closure of a transferred handle; fallback namespace ordering, internal-panel precedence and namespace ties in the public active/shadowed snapshot; and closure of driver/event streams, including withdrawal during the metadata request. This complements the earlier native-provider asynchronous fixture rather than replacing it.

PASS: `RUST_MIN_STACK=33554432 cargo check --tests` from `src/kernel` (3.27 seconds); `RUST_MIN_STACK=33554432 cargo test --manifest-path src/user/services/logic/Cargo.toml --lib brightness::tests::firmware_ties_use_internal_panel_then_namespace_not_publication_order -- --nocapture` (one host test); targeted rustfmt and `git diff --check`. UNPERFORMED: both kernel brightness fixtures and USB/ACPI guest gates. Source inspection found no further definite production defect: _ADR is admitted on the existing node channel; firmware metadata is optional and asynchronous; pending correlations are distinct; withdrawals retire in-progress admission and writes; raw handle generations prevent closed-channel aliasing. This is an implementation record, not an independent audit rating.


IMPLEMENTER'S SCOPE/VERIFICATION UPDATE ON P02M0199 (2026-10-09 19:47:24 UTC):

The owner has explicitly limited phase 2 to QEMU. DDC/CI over a native display-controller DDC/AUX bus and native-driver backlight registers remain outside the available phase-2 model/driver path; no physical display is available. These are excluded from current phase-2 acceptance, not implemented or tested here. QEMU virtio-GPU EDID discovery, asynchronous backlight admission/operations, monitor-originated changes, ACPI behavior without _DOS, and existing brightness integrations remain required and await final guest verification.


Verification continuation (2026-10-09 20:37:42 UTC).
The frozen-source x86 functional restage passed (`RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64`, 75 s). The following actual guest command then passed all 15 selected tests in 62 s overall (49 s guest):
`TEST_SELECTION="$(paste -sd, .build/logs/end-of-job/qemu-only-20261009/final-targeted-kernel-selection.txt)" SMP=4 RUST_MIN_STACK=33554432 ./test.sh --arch x86_64`.
The wrapper log is `.build/logs/end-of-job/qemu-only-20261009/final-kernel-x86_64-retry2.log`; its published guest log is `.build/logs/test/x86_64-20261009T203553Z-799584-guest.log`.
Both new production-service fixtures passed: display_service_keeps_serving_while_backlights_stall and display_service_admits_optional_firmware_metadata_without_stalling. These prove service progress/metadata compatibility in a guest; external USB/ACPI/no-_DOS driver gates remain pending.
This is functional x86 evidence. RISC-V compilation overlapped the run, so its timing rows are not quiet performance acceptance. Corresponding ARM/RISC-V guest execution and final shared-tree verification remain pending. Optional foreign-audit executables were absent from this intermediate image and are being regenerated before final staging; none of these selected tests depends on them.


Verification continuation (2026-10-09 20:54:46 UTC): the same comma-separated 15-test selection passed on aarch64 (`SMP=4 RUST_MIN_STACK=33554432 ./test.sh --arch aarch64` with `TEST_SELECTION` loaded from the recorded selection file), 386 s overall. Evidence: `.build/logs/end-of-job/qemu-only-20261009/final-kernel-aarch64.log` and its published `.build/logs/test/aarch64-20261009T204629Z-894988-guest.log`. Both graphics profiles (112 + 222 cases), Extended/HDR/resize/worker equality, both cleanup and both brightness fixtures, AudioService routing/recovery and pointer/touch lifecycle passed. Zero pages were reported lost or permanently retired. This run used the frozen artifacts preceding the newly approved independent microphone-volume API; it is not verification of that subsequent change. Its emulated timing rows are retained as measurements, not native performance acceptance. RISC-V and final integrated verification remain pending.


Verification continuation (2026-10-09 20:55:57 UTC): the same 15 selected tests also passed on riscv64 (`TEST_SELECTION` from `final-targeted-kernel-selection.txt` joined by commas, `SMP=4 RUST_MIN_STACK=33554432 ./test.sh --arch riscv64`), 525 s overall / 512 s guest. Evidence: `.build/logs/end-of-job/qemu-only-20261009/final-kernel-riscv64.log` and `.build/logs/test/riscv64-20261009T204629Z-895023-guest.log`. All selected graphics/profile/HDR/cleanup/brightness/audio/pointer tests passed; zero pages lost, permanently retired or refused. Thus this focused selection has actual PASS evidence on all three architectures. These are the retained pre-microphone-API artifacts, not verification of the newly authorized audio extension. Native performance, external-device scenarios and final shared verification remain separate outstanding work.

Verification continuation (2026-10-09T22:13:43Z):

The frozen profile/readback/microphone continuation was built on all three targets with `RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch ARCH --part libs`: x86_64 PASS442s, aarch64 PASS443s, riscv64 PASS439s; each staged inventory matches121 providers and121 consumers. The six foreign/runtime provider SHA256 identities remain unchanged from the successful canonical foreign regeneration, so no new foreign regeneration was required. Serial `./build.sh --arch all` with the same development/stack environment PASS271.24s; `./build.sh --arch all --part volume --kernel-on-volume` PASS221.06s. Exact logs are under `.build/logs/end-of-job/qemu-only-20261009/post-join-*`; `post-join-stage-results.json` preserves every command, environment, exit and elapsed time.

`RUST_MIN_STACK=33554432 ./check.sh --refresh dynamic-report` PASS405.10s, `./check.sh --gate verify-model` PASS28.97s, and `./gen.sh --check` PASS16.04s. The registration check preceded compilation of the new microphone test and honestly reported it declared but not yet built. The subsequent actual x86_64 SMP4 kernel run compiled and ran all16 explicitly selected cases, PASS74.51s (61s guest):112 2D and222 3D conformance cases with zero failed/unsupported/untested, HDR/Extended/resize/worker equality, partial-allocation and emergency cleanup, both brightness responsiveness tests, audio routing/recovery, pointer/touch lifecycle, and separate microphone gain with legacy capture compatibility. Exact selection and command are in `post-join-stage-results.json`; authoritative suite log is `.build/logs/test/x86_64-20261009T220307Z-1439801-guest.log`. These are functional checks, not a live performance acceptance. ARM/RISC current microphone/joined-profile execution and the required full verification workflow remain pending; merge inventory preparation must refresh all target suites. No milestone completion is asserted by these common build results alone.
