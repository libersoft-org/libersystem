IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0200 (2026-09-28T16:29:10Z):

Status: IN PROGRESS - started after P02M0191c/d, in the owner's agreed order. This record is updated as the work
proceeds; the final state is at its end.

OWNER DECISIONS THE PLAN DEFERS "TO WHEN THE PART STARTS", NOT YET ANSWERED: the timeout, period and deadline
(T, P, D - proposed 60 s, 15 s, 5 s), the default of a shipping image booted as one (proposed: on), the default
device order (proposed WDAT, TCO, `i6300esb`, BMC) and the shutdown-notice bound (proposed 2 s). They are built
as the config keys the plan names, with the PROPOSED values as their defaults, and are listed as open decisions
in the final report rather than taken as decided.

WHAT WAS IMPLEMENTED (2026-09-28):

P02M0200a - the contract and who keeps it fed
- `src/idl/device.lsidl`: provider kind `watchdog = 25`; record `watchdog-description` (device, min/max timeout,
  granularity, can-disarm, survives-reset, stops-in-suspend-to-idle, stops-in-s3, running-at-bind,
  last-reset-was-watchdog); interface `watchdog` (`describe`, `arm(timeout-ms) -> effective`, `pet`, `disarm`).
  `src/idl/observability.lsidl`: `supervisor-liveness` (`alive(sequence) -> sequence`). `src/idl/process.lsidl`:
  `shutdown-action` and `shutdown-notice` (`prepare(action)`), beside `system-power`. Regenerated with `./gen.sh`.
  Kind 25 mapped in `system-manifest` (`ProviderKindName::Watchdog`), `driver_protocol::provider::WATCHDOG`
  and DeviceManager's two maps.
- `src/user/services/logic/src/watchdog.rs` (+ tests): `BootMode::from_payload` (empty/unknown refused),
  `Policy::from_keys` (T >= 3P, D < P, zero refused; `enabled` defaults per mode), `Choice` (at most four, one per
  name, the named device only, default order WDAT/TCO/i6300esb/BMC, never switched once armed, disarm or feed the
  rest, nothing before the first answer), `Schedule` (a question each P, a pet only for its answer within D).
- `src/user/drivers/core/src/watchdog.rs` (+ tests): each device's division (i6300esb two preloads of 32768 x 30 ns
  units, TCO count x 1.2 s from 2 to 1023, WDAT period x count in the table's range, BMC 100 ms units), rounded
  down and refused below the minimum; and `serve` - the one `watchdog` provider every watchdog driver publishes,
  with THE BRIDGE: a timer running at bind that can reset the machine is petted at once and fed once a second
  until the consumer's first pet or successful arm/disarm, at most 120 s (`BRIDGE_TICKS`). A refused arm or
  disarm leaves the bridge running (the timer is as it was). A planned stop writes nothing to the timer.
- ServiceManager (`src/user/services/core/src/service_manager.rs`, `service_manager/bootstrap.rs`,
  `service_manager/lifecycle.rs`):
  - `supervisor_role` - THE ONE FUNCTION for the roles only ServiceManager can fill, called by `start_service`'s
    role delivery and by `relaunch_planned` alike (P02M0196b's stage paragraph names it; this milestone landed
    first, so it builds it). Rows: the watchdog service's `MODE` payload (the boot mode's bytes: `test` when the
    kernel's MODE says selftest, else `development` in a development build, else `shipping`) and its `LIVENESS`
    channel (a fresh pair; the near end REPLACES the one the standing loop answered on).
  - The standing supervise loop's wait set gains the liveness channel (kind 7): `alive` answered at once with its
    sequence (`serve_liveness`); a closed channel is dropped unless a relaunch already replaced it.
  - Development-only admin names: `!liveness-silence` (ServiceManager stops answering `alive` and nothing else) and
    `!crash <service>` (SIG_KILL a running service's process, so the standing loop restarts it as any crash).
  - THE ORDERLY SHUTDOWN NOTICE: `shutdown_all` (the one sequence `!poweroff`/`!reboot` run) now calls
    `notify_shutdown` after LogService's FLUSH and BEFORE the reverse-dependency kills: `prepare(action)` sent at
    once to every running service whose manifest row declares the notice, on its control channel, answers
    awaited under ONE bound (`service.shutdown-notice-ticks`, default 200 ticks = 2 s); an answer is reported
    ("answered/refused the shutdown notice"), silence reported and killed with the rest.
  - `watchdog_service` added to `plan_relaunchable`.
- `src/tools/system-manifest`: a service row's `notices = ["shutdown", "sleep"]` (`Notice`), refused when named
  twice, generated into ServiceManager's table as bits (`notices: u8`).
- `src/user/services/core/src/watchdog_service.rs` (new) + manifest program/service rows: roles CATALOGUE
  (kind-scoped to `watchdog`, optional), CONFIG (optional), LIVENESS (ServiceManager), MODE (payload, read by hand -
  the plan's reader drops payload bytes - and a mode with no bytes FAILS the start). Policy keys
  `watchdog.enabled`/`device`/`timeout-ms`/`period-ms`/`deadline-ms`/`boot-bound-ms`; a timing the policy refuses
  is logged and the proposed numbers stand. With the policy off a timer is disarmed as it attaches where the
  device allows it (a refused disarm leaves it to its driver's bridge until the first answer, then fed). The
  shutdown notice: each running timer reset with the platform disarmed or given its longest timeout and a last
  pet; one surviving the reset disarmed at power-off, the boot bound (120 s) at reboot. Declared
  `restart = transparent`, `notices = ["shutdown", "sleep"]`.

P02M0200b - the devices
- Declared registers, a kernel mechanism (`src/kernel/declared.rs`, `object/registers.rs`, `RESOURCE_KIND_REGISTERS`,
  `SYS_DEVICE_REGISTER_READ/WRITE`, `OBJECT_TYPE_REGISTERS`): a (vendor, device) row table beside the class
  resolver - the BAR to resolve (only when no other function's BAR shares its page), configuration registers at
  exact width (x86: CF8 then an 8/16/32-bit access at 0xCFC + (offset & 3); ECAM: an access of that width), and
  chipset memory registers through a kernel uncached window; a write touches only its mask; one capability per
  claim, derived from it, revoked by the release, which writes nothing.
- `i6300esb` row (BAR 0; 0x60 word, 0x68 byte) and driver `src/user/drivers/core/src/i6300esb.rs`.
- ICH9 LPC row: the LPC/TCO derivation row mints exactly PM base + 0x60..0x7F; GCS declared with the No-Reboot
  mask, present only while the root-complex base is enabled; a WDAT suppresses both and refuses the claim. Driver
  `tco.rs`: arms only after clearing No-Reboot AND reading it back (`unsupported` otherwise), disarm sets the halt
  bit and reads it back, last reset from TCO2_STS.
- WDAT: the platform row's I/O registers minted as port ranges, its memory registers declared, the table handed
  as the row's property block; `wdat.rs` interpreter (required actions, dropped actions, never an unminted
  register) and `wdat_driver.rs`.
- BMC's watchdog: P02M0201's transport driver (not this milestone's code; the `bmc` division is here).

Harness and gate (P02M0200c)
- `qemu-run.sh`: `-action watchdog=${WATCHDOG_ACTION:-pause}` on every boot that opens a QMP socket (x86_64's
  interactive path, aarch64/riscv64's development path); test-mode boots unchanged (no QMP, default `reset`). The
  test machines of all three ports carry an `i6300esb` after `edu` (renumbers nothing).
- `lab.py`: `dev-run-state` (`./dev.sh run-state`, `query-status`), `kill-driver VENDOR:DEVICE` kernel-console
  request, key `!`. Kernel: `DEV_CONSOLE_KILL_DRIVER` (development kernels only) kills the process that has a
  claimed PCI function's BAR mapped.
- `scenario.py`: steps `run-state` (hold or reach, the reach bound NOT time-scaled - it is the device's) and
  `kill-driver`; harness self-tests.
- `harness/wdat-table.py`: the WDAT fixture over q35's TCO (1200 ms per count, SET_REBOOT clearing No-Reboot).
- `tools/check-watchdog.sh` (gate `watchdog`), `harness/scenarios/watchdog-i6300esb.toml` (the cross-target case).

DECISIONS
- `supervisor_role` keeps what both start paths need in statics (`SELFTEST`, `LIVENESS`) - the same way the boot
  window already reaches `start_service` - rather than threading a new parameter through a sixty-argument function.
- The shutdown notice travels on the control channel as the IDL's `prepare` frame with correlation 1; ServiceManager
  encodes it without a client (a `Client` call would wait on each service in turn, and the plan asks for all at once
  under one bound). A message on a notified control channel that is not the answer is dropped for the duration of
  the sequence (the loop serves nothing else while it runs, as the plan states).
- A policy whose timing breaks T >= 3P or D < P is refused as written and the PROPOSED numbers stand (logged);
  whether to arm is still the key's. Refusing to run at all would leave a running timer unfed.
- The driver bridge ends only on a pet or a SUCCESSFUL arm/disarm: a refused one leaves the timer as it was.
- The i6300esb's 0x60 interrupt-type bits: QEMU keeps them under its own mask (0x11), so the driver writes 0x0003
  (stage-one interrupt disabled on hardware) and QEMU stores 1, which it also treats as no interrupt.
- `lsdev`-free driver kill: `DEV_CONSOLE_KILL_DRIVER` (development kernels only) finds the process whose address
  space the claimed function's BAR window is mapped in, by walking the Domain tree - the i6300esb maps no ports, so
  the console's port-range holder lookup could not serve it.
- The online report of a volume-staged service reaches ServiceManager and not the console, so the watchdog service
  also prints its policy line itself (found by the gate's first run).
- The plan's "host suites" for the LPC derivation and the declared-register checks: that code is kernel code, which
  has no host build here, so its fixture tests are kernel unit tests (`decide` in `arch/x86_64/ioports.rs` and
  `chipset_base` in `declared.rs` are pure functions of their inputs, run in the kernel suite). The WDAT
  interpreter, the service's choice and every division are host-tested as the plan says.

OBSERVED, NOT THIS MILESTONE'S
- `src/tools/check-bootstrap-plan.py` (gate `bootstrap-plan`) fails at HEAD already: `font_catalogue` is declared
  `transparent` and is in neither `relaunch_service` nor `plan_relaunchable`. Checked against a `git archive HEAD`
  copy. `watchdog_service` itself is in both lists.
- The release of a `none` claim clears the function's bus-master bit (the containment rule every release applies);
  for the ICH9 LPC bridge that changes no DECODE bit, which is what the plan forbids, and the kernel test checks the
  decode bits across claim and release.

FOUND AND FIXED ON THE WAY (the gate's orderly-reboot case)
- THE ORDERLY SHUTDOWN NEVER REACHED ITS POWER REQUEST. `shutdown_all` killed ConsoleService in the
  reverse-dependency order; the shell's attachment to the kernel console went with it, and the kernel's
  `console_shell_loop` reads a shell that attached and went away as the operator typing `exit` - it returned and the
  kernel printed `halting`. The teardown stopped at that service, the rest never stopped and `system-power` was never
  called: `reboot` left a halted guest. Shown live on a development instance with COM1 given back to the kernel
  (`lsdev --disable kernel:com1`, then `stop console_service` -> `halting`); with the UART driver holding COM1 the
  same halt left the log cut mid-line, since the kernel's ring was never drained again. FIX
  (`service_manager/lifecycle.rs`): the console host is excluded from the teardown order with the shell - both die
  with the machine (`dies_with_the_machine`, used by `shutdown_order` and `verify_shutdown_order`). Verified live:
  the orderly reboot now stops every other service, logs `power verb - shutting down` and the next boot starts.
  (`stop console_service` as an operator verb still halts the machine the same way; that verb is not this
  milestone's and is recorded here as observed.)
- A KILLED SERVICE WHOSE CHANNEL NEVER CLOSED HUNG THE SUPERVISOR: the post-kill drain in `stop_subtree` and
  `shutdown_all` waited without a bound. It is bounded now (`drain_closed_within`, 200 ticks), and a service that
  does not end within it is named ("did not end within its bound after SIG_KILL - it is left behind") and the
  sequence goes on - the shutdown's own bound is the notice bound plus the kills, as the plan states.
- The gate's first run: one `query-status` reading came back unanswered (QMP takes one client at a time). A missed
  reading loses nothing under `pause`, so the gate and the scenario runner ask again, and fail on three misses.
- The kernel test's first run: QEMU keeps the i6300esb's 0x60 interrupt-type bits under a 0x11 mask, so the
  round-trip probe uses bit 5 (reboot disabled) and the value the register held.
- THE WDAT DRIVER STARTED A COUNT NOBODY ARMED (the gate's WDAT boot never reached a prompt). It ran `SET_REBOOT`
  before reading the timer's state, so on a WDAT over q35's TCO - halt bit clear from reset, No-Reboot set - it read
  a timer "running and able to reset", and its takeover pet STARTED the TCO's count (QEMU counts from the first
  reload); the 120 s bridge ended before the development boot's watchdog service could disarm it, and the guest
  stopped in `watchdog`. FIX (`wdat_driver.rs`): the timer is read as it was FOUND - running, and able to reset -
  before anything is written; a running one that cannot reset is stopped; `SET_REBOOT` runs after the takeover, as
  the plan's "a driver's first acts are to read ... whether the timer runs" says.
- A SUPPRESSED LPC ROW WAS REPORTED AS AN IOMMU REFUSAL: the kernel refused the TCO driver's claim with the generic
  `Refused` (ERR_ACCESS_DENIED), which DeviceManager names `iommu-required`. It is now `ClaimError::FirmwareDriven`
  -> ERR_UNSUPPORTED (lasts the boot, not a DMA refusal), which DeviceManager reports as a claim refusal; the
  kernel's own line names the table.

VERIFICATION (2026-09-28, commands and results)
- `./build.sh --arch x86_64`: ok (after each change; the last at the gate's passing run).
- Kernel variants: `cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests`, `LIBER_DEVELOPMENT=1 cargo build`,
  `cargo build`: all compile, no warnings.
- `cargo test --manifest-path src/user/services/logic/Cargo.toml watchdog`: 9 passed.
- `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib`: 409 passed (the watchdog divisions 4, the WDAT
  interpreter 3, the UART 7 among them).
- `cargo test --manifest-path src/tools/system-manifest/Cargo.toml`: 28 passed (the new notices test among them).
- `cargo test --manifest-path src/abi/Cargo.toml`: 28 passed.
- `python3 src/harness/harness-test.py RunStateStepTest ScenarioValidationTest RefusedStepTest`: 12 passed; gate
  `boot-harness`: pass. Scenario `watchdog-i6300esb.toml` validates (32 steps).
- `TEST_SELECTION=<37 ids> ./test.sh --arch x86_64`: 37 passed - the declared-register suite (the i6300esb's 0x60
  word and 0x68 byte, refusals, the capability gone after release; the ICH9 LPC row: exactly the TCO block, PM1 / PM
  timer / GPE0 refused to every mint, GCS masked to No-Reboot, no decode bit changed at claim or release; the chipset
  base against fixtures), the LPC derivation against fixture machines, and every port-range, handoff, DMA-policy and
  platform-row test (the test machines now carry an `i6300esb`). First runs failed and were fixed: one selected id
  exists only on the other ports; the 0x60 probe used bits QEMU masks.
- Gate `watchdog` (`src/tools/check-watchdog.sh`): PASS on x86_64 (4 boots, ~37 min) - i6300esb armed at 14999 ms,
  TCO disarmed; 3T running; service kill -> restarted, re-armed, 3T running; driver kill -> "running at bind",
  re-armed, 3T running; orderly reboot: disarm and the notice's answer before the first stop, the next boot up;
  silence -> `watchdog` 12 s after (window 6..20 s); reset case: second boot, i6300esb "the last reset was its own",
  TCO not; TCO armed at 14400 ms, expired 11 s after the silence; WDAT: `driver.wdat` online, no TCO driver
  (claim refused, firmware-driven), armed at 14400 ms, expired 11 s after the silence. Earlier runs failed and led to
  the fixes above.
- Gates `source-hygiene` (after moving `declared.rs` to `declared/mod.rs` and replacing `| head -1` with `grep -m 1`
  in the gate), `arch-surface`, `test-tags`: pass. `./gen.sh --check`: no drift. `foreign-audit-link.py --check`:
  reproduces. `shfmt -d` on the changed scripts: clean.
- `check-bootstrap-plan.py`: FAILS, as at HEAD, on `font_catalogue` alone (not this milestone's).
- `cargo test --manifest-path src/tools/verify-model/Cargo.toml`: 76 passed, 81 failed - every failure is the model
  failing to load on the stale port test binaries (a replaced clock test), the known condition deferred to the
  job's end-of-work port rebuild; not re-verified here.

NOT RUN (end of the job, with the other cross-architecture work): the aarch64 and riscv64 builds, the kernel tests
there (the i6300esb registers on the ECAM ports), and `QEMU_EXTRA="-device i6300esb" ./lab.sh scenario-cold
src/harness/scenarios/watchdog-i6300esb.toml --target aarch64|riscv64`. Not this milestone's to run yet: the sleep
case (P02M0197) and the BMC (P02M0201).

Status: IMPLEMENTED on x86_64 and the host, the gate passing; OPEN for the cross-architecture verification at the
job's end, for the parts owed by P02M0197 and P02M0201, and for the owner's decisions listed at the top.
- Gate `capability-model`: pass (1459 s).

## The sleep case, and the gate run again (2026-10-01)

- THE SLEEP CASE (owed to P02M0197, which landed second, and carried there): the service's step at the sleep notice and
  the drivers' steps are P02M0197's audit's; the case itself is the gate `sleep`'s watchdog boot - `-device i6300esb`,
  the policy armed at 15 s - and passes (2026-10-01): `running` at every reading through a suspend to idle three times
  the timeout and after it, and `watchdog` 119 s after a resume ServiceManager's development hook made hang after the
  drivers' step, the re-arm with the bridge bound being what expires. The BMC's watchdog steps across a sleep pass in
  the gate `ipmi` (the harness BMC's record: the announcement's timeout, the disarm, the re-arm, the restore).
- THE GATE `watchdog` FAILED in the batch of 2026-10-01 after its orderly reboot: "the instance was not recorded again
  after the orderly reboot" - `dev.sh reboot` resets the guest over QMP and asks the development broker to WAIT for
  the shell's prompt, and the broker SEEDS that wait with the serial log's tail, which after a reset is the PREVIOUS
  boot's: whenever its last line was the prompt, the wait answered "prompt" a second after the reset, and the agent
  asked next (30 s) was not there for a boot that takes about two minutes on this machine (29 s of it the loader
  verifying the live volume, 41 s StorageService copying it into memory). Reproduced on a plain development instance:
  one of four hard reboots recorded. FIXED (`src/harness/lab.py`): `dev-reboot` asks `WAIT ... nudge fresh`, and a
  `fresh` wait starts from nothing; the broker's other waits are unchanged. Three of three hard reboots recorded after
  it (116.0, 116.3 and 116.4 s to a prompt).
- `LIBER_DEVELOPMENT=1 ./check.sh --gate watchdog` -> PASS (2919 s): the i6300esb, the reset, the TCO, WDAT and the BMC
  cases, the orderly reboot's notice and the boot bound among them.
- (2026-10-01) `./build.sh --arch aarch64` and `--arch riscv64` build whole with this milestone's code; the ports' guest runs it names are the owner's long run.

## The owner's decisions, recorded (2026-10-04)

The owner answered the questions this part asked when it started, on a plain account of each: T, P and D at 60 s, 15 s
and 5 s; armed by default in a shipping image; the order WDAT, TCO, `i6300esb`, BMC; a 2 s shutdown notice - all as
built - and a ten-minute boot bound.

WHAT WAS DONE:
- `watchdog_service.rs`: `DEFAULT_BOOT_BOUND_MS` 120 000 -> 600 000, its comment saying why (a short bound turns one
  slow boot into a reset loop; a long one only catches a hung boot later); `watchdog.boot-bound-ms` still sets another.
- `drivers::watchdog`: the takeover bridge stays 120 s - the time from a driver's bind to its consumer's first pet - and
  its comment no longer calls it the boot bound, which covers the reset to the bind. `RESUME_WATCH_MS` follows the
  bridge, as before, so the sleep case's 120 s and the `ipmi` sleep case's re-arm count (1200) are unchanged.
- `check-watchdog.sh`: the BMC case still sets the bound explicitly (the case is that the notice arms the CONFIGURED
  bound); its comment says the default is ten minutes now.
- `P02M0200.md` and its TODO row carry the decisions.

VERIFICATION:
- `LIBER_DEVELOPMENT=1 ./image.sh --format iso` (x86_64, builds the service and the drivers): ok; rustfmt and shfmt
  clean. No gate exercises the default itself - `check-watchdog.sh` sets the bound it tests - so the change is proved by
  the build and by reading `read_policy`, which takes `watchdog.boot-bound-ms` and falls back to the constant.
- `./check.sh --gate ipmi` (2026-10-04, PASS in 2464 s) with these drivers: the sleep case's re-arm with the bridge
  bound still 1200 counts.

BLOCKERS: none for the decisions; the ports' runs wait for the end of the job.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0200 (2026-10-08T03:26:17Z):

Continuation review: read the complete milestone plan and the preserved implementation record, including earlier incomplete verification and later completion claims. Reviewed the dependency and verification conventions in docs/TESTING.md. No previous audit text was changed.
Review in progress: completion labels are being checked against the current code and available verification artifacts; no new passing test results are claimed at this point.
