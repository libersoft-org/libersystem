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


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-09 23:55:28 UTC):

The first current ARM USB brightness integration run FAILED. Exact invocation from repository root:
`env -u BOOT_IMAGE -u LOADER_EFI SMP=4 RUST_MIN_STACK=33554432 python3 .build/logs/end-of-job/qemu-only-20261009/run-p0099-extra-guests.py --phase brightness --run --output .build/logs/end-of-job/qemu-only-20261009/final-brightness-ports`.
The wrapper selected `GUEST_GATE_SECONDS=1200 GUEST_GATE_TIMEOUT=1600 src/tools/check-brightness-usb.sh --arch aarch64`; the existing shared guest gate explicitly uses `run.sh --smp 2`, so the actual brightness guest had two CPUs despite the wrapper's inherited SMP4. This was an actual TCG guest, not a host simulation. The wrapper exited1 after520.49s; `brightness-aarch64/result.json` records FAIL. RISC-V brightness and all38 USB port cases were NOT RUN because the batch stopped at this first failure.

Evidence is retained under `.build/logs/end-of-job/qemu-only-20261009/final-brightness-ports/brightness-aarch64/`: `command.log`, `guest.log`, `monitor-sim.log`, `gadget.log`, `result.json`, and `early-stop-evidence/`. The unchanged gate accepted monitor binding/join, manual40,70,76,70, floor5 and explicit0, read-only denial, automatic82/13, and the device-originated63 change without SET echo. It then rejected the required restart assertion: `brightcheck settings` printed `automatic=false idle=300`, where `automatic=false idle=off` was required. The restarted policy logged its default idle300; its later SET45 reached the monitor, but persistence reported `could not be stored at 45`. Thus this run is not a passing brightness integration and does not prove restored settings or durable final level. The log contains no ConfigService restart or write-through-failure diagnostic during this sequence; the physical volume's actual stored tree was not independently inspected.

The complete scripted command sequence, including final `brightcheck set 45` and its answer, finished before the gate's fixed post-command recording interval. With explicit parent authorization, full current guest/run/firmware/command bytes were copied first; the owned ARM QEMU PID1708747 was matched against its private serial socket and captured process start identity, then stopped through its QMP socket. Its console reader PID1708745 continued waiting after EOF and was terminated only after its identity and full logs were retained. The unchanged gate then evaluated the complete log and returned the above failure. `early-stop-evidence/stop-reason.json`, `qmp-stop.json` and `stopped-console-helper.json` record this shortened POST-COMMAND idle wait. No PASS is inferred from an interrupted wait. Teardown reported `torn down` and `the host carries nothing of this harness's`; the owned QEMU, console reader and monitor process were gone.

Read-only source diagnosis found a concrete ownership defect: the brightness policy's CONFIG role is still `kind="client"`, so `service_manager/bootstrap.rs::deliver_roles` duplicates ConfigService's root endpoint retained by the supervisor. Both initial launch and `relaunch_planned` use this rule. The policy performs direct typed GET/SET calls on that shared receive queue; killing the old instance does not retire replies queued to the retained endpoint, and other readers share it. `brightness_policy.rs::read_settings` retains defaults when a reply is absent/malformed/mismatched. This is an unsafe source path consistent with the observed restart failure; the exact reply interleaving was not instrumented, so the log alone does not establish which reader consumed which response. ConfigService's successful SET path writes through before answering and rolls back on write failure; no change to those semantics or to policy defaults/timeouts is proposed.

A minimal review patch is staged ONLY in `.build` at `proposed-config-factory.patch`: change this CONFIG role to `factory`, using the existing `service_connect` mechanism already served by ConfigService to give every policy instance its own endpoint. No tracked source has been edited for this proposal yet. This extends the already documented factory-connection implementation decision to CONFIG without granting additional configuration authority. The existing unmodified USB gate's failed restart assertion is the behavioral regression that the corrected image must satisfy; no new test that merely mirrors the manifest row was added.

Validation of the proposed manifest in a `.build` shadow workspace PASSED without compilation or guests: the existing `.build/cargo/system-manifest/debug/system-manifest check` and unchanged `python3 <shadow>/tools/check-bootstrap-plan.py`, both exit0. The latter found two hand-written service bootstraps consistent,40 migrated, and18 declared restartable services with matching relaunch mechanisms. Exact commands, cwd, outcomes and timings are in `proposed-validation.json`; outputs in `system-manifest-proposed.log` and `bootstrap-plan-proposed.log`. These are proposal/schema/bootstrap checks, not runtime proof of the fix. Applying the reviewed patch, restaging affected artifacts, the unchanged ARM retry, RISC-V and x86 brightness integrations, the38 USB cases, and final required verification remain outstanding. Earlier audit bytes and all plan requirements remain unchanged.


Progress (2026-10-09 23:56:24 UTC): reviewed and applied the CONFIG factory-role correction to src/user/services/manifest.toml. Actual workspace checks PASS: `RUST_MIN_STACK=33554432 bash tools/system-manifest.sh check` from src (brightness-config-factory-manifest.log) and `python3 src/tools/check-bootstrap-plan.py` from repository root (brightness-config-factory-bootstrap.log), both exit0. This supersedes the proposal-only state above. Guest images have not yet been restaged and runtime regression remains unperformed; defaults, timeout and persistence semantics are unchanged.


Owner decision (2026-10-10 00:20:38 UTC):

The owner has explicitly confirmed the existing defaults: minimum5%, automatic brightness off, idle dimming after300seconds to30% of the previous level, and the default10/40/80/100% curve at0/100/1000/10000lux. These numeric settings remain unchanged. This satisfies the plan's explicit owner-confirmation condition and supersedes the earlier pending-defaults question. It does not close the still-unperformed current-image USB restart/persistence or ACPI/no-_DOS verification.


ABI verification correction (2026-10-10 00:32:05 UTC):

The complete native ABI suite exposed the existing stale Framebuffer28-byte test snapshot after the required decoder metadata had made the actual descriptor36bytes. Plan199 explicitly requires the append; reviewed current syscall refuses undersized buffers before changing display ownership. Under the documented pre-release ABI1/coherent-build policy, updated only the test to36bytes and froze decoder/decoder_present at offsets28/32, preserving every existing offset. This is not compatibility with old28-byte clients. Actual `RUST_MIN_STACK=33554432 cargo test --offline --manifest-path src/abi/Cargo.toml --target x86_64-unknown-linux-gnu`: firstFAIL27/28(exit101,1.868s), correctedPASS28/28(exit0,1.309s). Detailed source/policy review and both logs are retained in thread-placement/. No brightness runtime PASS is inferred.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 03:10:16 UTC):

The current ARM/RISC USB-brightness batch was INTERRUPTED BY A HOST KERNEL FAULT, not completed. Exact invocation with original GUEST_GATE_SECONDS1200/GUEST_GATE_TIMEOUT1600 is retained under `.build/logs/end-of-job/qemu-only-20261009/brightness-ports-20261010T024000Z`; the outer ownership-guard exec session82986 returned137. Host Linux6.12.111 reported a NULL-pointer OOPS at02:58:50UTC in guard python PID2609805, pointer/seq_printf/m_show/seq_read, and exited that task with IRQs disabled. Host cgroup OOM counters are zero. Matching upstream module-formatter source and the stack strongly implicate repeated /proc/modules reads; exact Debian debug symbols are absent, so the specific damaged object/cause is not established. No Liber guest kernel failure is inferred from this host OOPS.

The retained ACTUAL new ARM guest completed23/23 commands, restored automatic=false idle=off after the required service restart, and forwarded45 to the monitor. This is current functional observation, not a whole-gate PASS: the gate parent and firmware helper died before final assertions/teardown, so no original literal success verdict/result exists. The fixed `.build/logs/brightness-usb` copies still describe the older liber1708621 failed run and must not be confused with this new liber2609815 run. QEMU2609941 and its timeout2609940 survived independently, then ended naturally on the ORIGINAL1600s timeout; no manual shortening or host signal was used. RISC-V did not start. Logs, argv/start identities, full host OOPS, and interruption classification are retained in readonly-observer/unexpected-137 and interruption.json.

The owned liber2609815 gadget/ledger remained after the abrupt parent death. A precise cleanup and a guard reader correction are being reviewed; neither is yet claimed complete. Review also found that canonical modprobe -r can cascade into preexisting snd_pcm/snd_timer/snd/soundcore despite the harness's preservation rule. That must be avoided without forcing removal or claiming unrelated modules. All original audits and failed evidence remain preserved; required port acceptance and final integrated verification remain open.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 03:19:02 UTC):

The exact owned-gadget recovery removed only liber2609815: unbound dummy_udc.0, unlinked its HID function, and removed that gadget's function/config/string directories. Its first non-forced individual `rmmod dummy_hcd` then remained asleep inside `__do_sys_delete_module+0xbb/0x330`; sysfs still showed state=live, refcnt=0. The recovery did NOT complete, and no second module unload was attempted. This is a host-kernel/module-subsystem failure following the recorded OOPS, not a passing Liber result.

At 03:17:57UTC the parent verified exact PID/start identities using pidfds: cleanup2618783/start3349472 and rmmod2618789/start3349494. It stopped the cleanup parent, terminated only that owned rmmod, and terminated/resumed the parent to prevent another retry. Both identities disappeared within one second; wrapper status241 represents cleanup exit-15 after290.233s. No forced unload, host reboot, unrelated process signal or preexisting module removal was performed. The gadget is absent, but the seven-file ownership ledger and still-loaded test modules remain deliberately preserved. `execution.json` retains its original STARTED state; the separate `cleanup-proposal/interrupted-cleanup.json` and `execution-command.json` record the interruption honestly.

Actual host evidence and the precise cleanup are under `.build/logs/end-of-job/qemu-only-20261009/brightness-ports-20261010T024000Z/readonly-observer/unexpected-137/`. A clean host/restart preserving this workspace has been requested for remaining actual USB and final acceptance. Read-only code review and bounded host fixture tests may continue; no whole-gate brightness PASS, clean-host assertion, RISC-V run, USB38 run or final-verification PASS is claimed. Canonical cleanup and guard reader fixes remain in review.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 03:23:56 UTC):

Reviewed and applied the ignored final USB-run guard correction: `run-p0099-usb-ports.py` now reads loadable-module presence and the correctly directed holders relation from sysfs, excluding builtin entries and regular attributes such as /sys/module/compression. It reads mountpoints through /proc/self/mountinfo with one-pass escape decoding, and reports removal of any preexisting module independently of owned leftovers. Guest verdict requirements, process ownership/start checks, run_case and the module ownership closure are byte-identical. This changes verification infrastructure only; no new actual host snapshot or module operation ran.

Applied target SHA2568e1b29e1a0ac2d5757cf6f42167dd72affaef4660ae2ace2515374718ffe5da3, patch4253c95c4180b9560fa738cfa525fbeb19683478c1253e8fce20cf82d8db7935. `python3 -B .../usb38-cleanup-sysfs-v2/test_snapshot.py .../run-p0099-usb-ports.py` PASS9/9 (0.014s test time); applied `--self-test` PASS. Fixtures redirect every host path and explicitly reject /proc/modules and /proc/mounts access. Original-source negative tests detect unsafe reads and lost preexisting modules; a holders mutation fails; the first un-applied candidate's regular-file bug fails separately. Both candidate generations and all failed evidence are retained. Exact applied commands/statuses are in usb38-cleanup-sysfs-v2/application.json.

The source note module-mutex-note.md ties upstream Linux6.12.111 m_start/m_stop to module_mutex and delete_module's interruptible acquisition before state/refcount handling. Together with the retained OOPS and waiter stack, an abandoned mutex is a concrete explanation, but remains inference without matching Debian debug ELF or mutex-memory proof. It does not establish the initial invalid-pointer cause. Clean-host recovery, canonical harness cleanup correction and all remaining actual USB acceptance are still pending.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 03:29:36 UTC):

Reviewed and applied the canonical USB cleanup correction in `src/harness/usb-gadget.sh`. `loaded_modules`/`loaded` use sysfs loadable-module attributes rather than lsmod. `unload_owned_modules` combines the run's explicit ledger, newly registered absent-before USB/HID drivers, and absent-before dependencies via holders. It writes the complete owned closure to the existing ledger before any unload, so an interruption cannot lose ownership of a dependency whose holder has already disappeared. Each module is removed individually with rmmod, never cascading modprobe -r, only after its holders/refcount permit removal. Preexisting modules are never candidates and their continued presence is checked. The existing five-second transient-holder retry remains bounded; each actual removal also has a five-second TERM deadline and one-second kill grace. A timeout stops further removals. Missing/conflicting ownership, write refusal, busy modules or incomplete cleanup retain the ledger and return failure. Successful teardown remains idempotent. `cmd_verify` reads escaped mountpoints from mountinfo and fails on unreadable/malformed inventory.

The regression file `src/tools/test-usb-gadget-cleanup.py` is imported by the existing `check-guest-verdict.py` entrypoint. Its13 real-shell/private-sysfs cases cover absent-before dependency ownership and preexisting preservation, interruption followed by successful retry of an orphan dependency, ledger write refusal before any unload, new host drivers, intentional cascade mutation detecting lost preexisting modules, busy/conflicting/missing module cases, an actual timeout against a fake subprocess with no second removal, builtin/non-directory filtering, mountpoint escapes/malformed data, and idempotence. These fixtures never touch actual modules or host gadget state.

Patch SHA256ff47272f22c2b660265c279431af17359f6b81ab0bc39188e65614905a8b486f was applied only after exact before/after hashes and modes matched. Applied `bash -n src/harness/usb-gadget.sh`, `shfmt -d src/harness/usb-gadget.sh`, and `git diff --check` PASS. Applied registered `RUST_MIN_STACK=33554432 ./check.sh --gate guest-verdict` PASS87/87,21.356s test/21.680s command, literal RESULT ok exit=0 seconds=22. The earlier13-case shadow results and first malformed patch-prefix packaging failure are also retained under canonical-usb-cleanup-v1; `application.json` records the final applied commands/results. No actual module unload, clean-host assertion or hardware/guest PASS follows from these host tests. The old host OOPS and incomplete ownership ledger still require host restoration before remaining USB acceptance.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 03:55:00 UTC):

Read-only follow-up confirmed a reachable early setup ownership leak in the canonical USB fixture. cmd_setup loaded modules before writing name/modules; dummy_hcd, missing-UDC and occupied-UDC refusals occurred before those records. cmd_teardown returned success without name, and the brightness and kernel-test callers did not call teardown after setup failure because their successful-setup flags were unset. Beginning the authorized minimal correction in usb-gadget.sh and its existing private-sysfs fixture tests. No actual module operation or guest is authorized here; the host remains uncleared after the earlier OOPS. Verification and implementation results will be appended after the change.

Implementation results for the early-setup correction (2026-10-10 04:00:46 UTC):

Implemented the reachable pre-UDC failure correction in `src/harness/usb-gadget.sh`. `cmd_setup` checks existing run state and the prospective gadget-name collision before taking ownership. It validates and writes the saved module/USB-HID/rfkill inventories, plan, empty module ledger and name before its first module operation. Each absent-before module name is normalized to sysfs spelling and recorded before that modprobe attempt. A setup-owned EXIT handler is installed only after those records exist. `setup_exit` reads the persisted records rather than expired cmd_setup locals; it invokes the existing teardown in a child shell with errexit enabled, preserves the original setup error even when cleanup refuses, and retains the incomplete ownership ledger. Both successful configfs and gadgetfs return paths explicitly cancel the failure trap. No caller-success flag or caller cleanup is needed to undo a failed setup.

Extended the existing `src/tools/test-usb-gadget-cleanup.py` fixture class with seven tests, using only temporary sysfs/configfs trees and fake module commands. The four concrete failure cases are dummy_hcd load refusal after other owned modules loaded, a loaded controller without a UDC, an occupied foreign UDC that is left unchanged, and incomplete cleanup of a busy owned module. The last fixture uses original setup status73 versus teardown status1 to prove the original error survives. Additional tests prove successful configfs and gadgetfs setups cancel cleanup even if a later unrelated shell exit is29, prior foreign name/state is never claimed or cleaned, and failure to write any of six initial ownership records occurs before all module side effects. Recorded fake module calls prove ownership was already written before load attempts.

Actual commands on final source: `bash -n src/harness/usb-gadget.sh` PASS exit0; `shfmt -d src/harness/usb-gadget.sh` PASS exit0/no output; `python3 -B src/tools/test-usb-gadget-cleanup.py` PASS20/20, exit0,11.680s command (11.579s test body). The unchanged pre-fix shell was then substituted only as the fixture source in `python3 -B .build/logs/end-of-job/qemu-only-20261009/canonical-usb-early-setup/baseline-negative.py`: all four required early-failure regressions rejected it with assertion failures on leaked fake modules, expected exit1 in1.028s. This negative is retained as FAILED test output, not mislabeled a passing baseline. Full commands, statuses, original source snapshots, patch and hashes are retained under that proof directory.

The full registered guest-verdict gate was NOT rerun for this follow-up; root owns that subsequent check. No actual module, gadget, UDC, mount, service manager, guest, shared build or suspect proc formatter was touched. These tests do not repair the tainted host or discharge remaining actual USB/brightness acceptance. The earlier incomplete cleanup ledger and unperformed RISC-V/USB38 work remain open; milestone completion criteria and existing evidence are unchanged.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 04:04:12 UTC):

The applied early-setup ownership/cleanup correction now passed the registered integration command `RUST_MIN_STACK=33554432 ./check.sh --gate guest-verdict`: exit0,94/94 tests,24.937s tests/25.256s command, literal `check.sh: RESULT ok exit=0 seconds=26`. Evidence is `canonical-usb-early-setup/registered-guest-verdict.json` and its full log. Independent read-only review found no concrete issue in original exit-status preservation, cleanup errexit, before-inventory/write-ahead order, collision refusal or successful trap cancellation. These are actual host fixtures and gate integration results; no real module operation or clean-host/USB-guest claim follows. Host recovery and final actual USB/brightness verification remain required.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 04:45:47 UTC):

Continuation result: BLOCKED, NOT COMPLETE. Owner-approved brightness defaults are preserved. Corrected ARM guest completed23 commands and restored idle=off, but its host guard died and the whole gate has no PASS; RISC-V never started. Final USB and ACPI/no-_DOS checks remain unperformed. Shared94-case host USB regression does not replace these guests.

Read-only host revalidation confirms no reboot since the2026-10-10 02:58:50UTC Linux OOPS (current boot began2026-10-09 17:54:52UTC), kernel taint128, dummy_hcd still live/refcnt0/noholders, and all seven cleanup-ledger files byte-identical to the preserved ownership record. This does not retest module unloading; the prior blocked delete_module and interrupted cleanup remain the evidence. No further unload, module-list read, guest, benchmark or reboot was attempted. The same external blocker persists for the third consecutive goal turn. Independent remaining-work review found no concrete meaningful step that would discharge final proof before host recovery.

Evidence: `.build/logs/end-of-job/qemu-only-20261009/host-blocked-audit-20261010T0444.json` and HOST_RECOVERY_RESUME.md. The previous turn made actual diagnostic/correctness progress; this turn revalidated the unchanged impasse. Required next external action is a host restart/replacement preserving the workspace, followed by boot/sysfs and old-boot-ledger reconciliation. The existing owner question is unanswered. Completion flags and all original requirements are preserved; passed, failed, excluded and unperformed checks remain distinct.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0199 (2026-10-10 05:18:34 UTC):

The owner explicitly declined host restart/replacement because this is a shared development server, and instructed continuation of independent work with unavailable checks reported at the end. This supersedes the earlier blanket hold and unanswered-owner statement; it does not turn any unrun check into a pass. Read-only harness review identified module-free QEMU/userspace fixture paths. Current continuation uses one coordinated build or guest at a time, process-local four-CPU affinity, nice10 and Cargo jobs2, retaining the existing functional gate SMP2/4 and exact assertions. No host module/configfs/global gadget-ledger change, reboot, foreign-process termination or system-wide resource change is authorized or attempted.

Actual preparation and limitations are recorded in `.build/logs/end-of-job/qemu-only-20261010-shared-host/OWNER_CONSTRAINTS.md`. All-target targeted conformance linking has passed; ordinary x86 development profile/package restoration and fresh test descriptors are currently in progress. These shared preparation results are not a new milestone-specific guest PASS. Existing passed/failed evidence and unchanged completion requirements remain authoritative; final results will be appended as checks finish.
