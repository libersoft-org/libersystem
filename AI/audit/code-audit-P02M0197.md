IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0197 (2026-09-29T17:38:08Z):

## Starting point

Taken in the owner's agreed order after P02M0202: P02M0196c together with P02M0197a and P02M0197b (the plan says
neither side works alone), then the rest of P02M0197, then the rest of P02M0198. P02M0198a - the tick computed from
the counter with the sleep-offset term, the suspended state and the rebase - is in and verified on x86_64, which is
what P02M0197b's ORDER line requires first.

Owed into this milestone by the ones that landed first, recorded in the plan's OWED notes: the exchange in `ipmi`,
`smbus_ich9`, `ucsi_acpi` and `tcpci`; the kernel's HOSTC claim write restored after an S3; the watchdog's sleep
notice, its drivers' steps and the watchdog-across-a-sleep case; the IPMI, UCSI and TCPCI sleep cases.

## Part b - the kernel's entry, the freeze and the clocks (x86_64 implemented; ports owe their `arch::sleep`)

- ABI (`src/abi/src/lib.rs`): `SYS_DOMAIN_FREEZE = 109`, `SYS_SYSTEM_SLEEP = 110`, `SYS_FIRMWARE_SLEEP_TYPE = 111`,
  `SYS_CLOCK_BOOT_NS = 112`; `SLEEP_STATE_*`, `WAKE_*`, `CorePark`, `SleepReport` (64 cores). `SYS_DOMAIN_CREATE`
  takes the additive parent argument (zero keeps the caller's Domain; MANAGE on the parent otherwise).
- Freeze: a frozen flag per Process and per Domain (`object/process`, `object/domain`), inherited by a child Domain
  and by a process registered into a frozen one; `Domain::set_frozen`, `quiescent()`. Threads park at the preemption
  point, at the three blocking waits in `syscall/mod.rs` and at the syscall return (`sched::park_if_frozen`, gated by
  the `FREEZES` counter so an ordinary syscall pays one relaxed load). `sys_domain_freeze` needs MANAGE, polls
  `quiescent()` for at most `FREEZE_TICKS` (100) and fails otherwise; a thaw never clears job control's `stopped`.
- `src/kernel/sleep/mod.rs` (new): the sleep-type registry (`register`, `sleep_type`), the boot-time clock
  (`SLEPT_NS`, `boot_ns`, `add_slept`), the first-wake latch (`woke_by`, `take_woke`), `sys_system_sleep`,
  `prologue`/`epilogue` (the `sleep: entered` line on the wire synchronously, `CLOCK.suspend` at the counter),
  `suspend_to_idle` (device lines masked except the wake set, every other core parked in the idle loop's sleep mode
  with no timer, the entry core's one-shot at the raw counter for the timed wake alone, `CLOCK.rebase` first on the
  way out, each core's park counts in the report) and `park_here`.
- x86_64 S3 (`arch/x86_64/sleep.rs`, new): run from the boot core's idle context once every other core is held;
  saves the IO-APIC redirections, every recorded PCI configuration header, the kernel's MSI-X entries, the claim
  writes (HOSTC), the GPE masks and the console UART; FACS waking vector at the AP trampoline page with the
  bootstrap identity map reinstated; `s3_save_and_enter` / `s3_resume_entry` / `resume_boot_core`; restore in the
  order the plan gives (UART first), IOMMU transport replay (`dma::Iommu::replay_after_reset`), then the other cores
  through INIT/SIPI into `smp::ap_resume_entry`. The CMOS alarm with RTC_EN for the timed wake (`rtc::arm_alarm`).
  `reset()` tries the FADT reset register first; `poweroff()` writes a registered `\_S5` or the fixed ports and names
  the path it took.
- Kernel tests (`src/kernel/sleep/tests.rs`), run:
  `TEST_SELECTION="kernel.sleep.the_wake_the_sleep_saw_first_is_the_one_reported,kernel.sleep.a_sleep_type_is_checked_registered_and_kept,kernel.sleep.a_freeze_holds_the_subtree_and_its_newcomers_and_a_thaw_leaves_a_stop_alone,kernel.sleep.the_freeze_syscall_needs_manage_and_answers_once_the_subtree_is_quiet,kernel.sleep.a_suspend_to_idle_parks_every_core_until_its_timed_wake_and_no_clock_jumps" ./test.sh --arch x86_64`
  -> PASS, 5 passed (33 s). The guest log shows `sleep: entered (suspend to idle)` and
  `sleep: resumed (the timed wake, after 296 ms)` for the 300 ms timed wake. Not yet watched failing (a mutation run
  is owed before this counts as evidence). S3 itself is not exercised by these tests; its gate is part d's.

## Part a - the driver wire and DeviceManager's step

- `driver-protocol`: opcodes `SUSPEND = 16`, `SUSPENDED = 17`, `RESUME = 18`, `RESUMED = 19` (no handles),
  `SleepState`, `SuspendRequest`, `SuspendOutcome`, `Suspended`, their codecs, `MAX_SUSPEND_DEADLINE = 1000`.
- `system-manifest`: `suspend-deadline` in a driver's entry (validated like `heartbeat-deadline`); the registry
  generator (`services/core/build.rs`) emits it into DeviceManager's `Entry`.
- `liber:device@1/binding-state` gains `suspended` (regenerated with `./gen.sh --accept-breaking`).
- `driver-binding`: `BindingState::Suspended` with the edges Online->Suspended, Suspended->Online,
  Suspended->Stopping; `BindingEvent::Suspended` / `Resumed` and their reduction (a refusal moves nothing; a device
  that did not come back is `DriverReported(DeviceNotResponding)`, retryable, through the teardown);
  `shutdown_step` asks a suspended binding to stop; `Heartbeat::restart` forgets a ping outstanding at the suspend
  and counts no miss. Host tests: `cargo test --manifest-path user/libs/driver/binding/Cargo.toml` (from `src/`) ->
  92 passed.
- `liber:process@1/device-sleep` reworked so a failed step names its binding in the answer (`drivers-suspended`
  carries `failed`/`why`), with `check` (the bindings the sleep cannot suspend) and `resume` answering the devices
  that did not come back.
- DeviceManager (`device_manager/sleep.rs`, new): `check`, `suspend` and `resume` on the control channel,
  hand-encoded replies correlated to the request, driven from the standing loop one binding at a time (`step`),
  each under its entry's `suspend-deadline` times `machine_scale()`; reverse bind order with consumers before
  providers and `watchdog` publishers last; resume in bind order with `watchdog` publishers first; a refusal, an
  ended driver or a missed bound fails the step, which resumes what it suspended (and the one that did not answer)
  before answering; a resume not answered in its bound is torn down (`Wedged`) and rebound. While a run exists no
  bind starts (the four `start_candidate_at` sites), and neither the device-policy endpoint nor bus arrivals are in
  the wait set. `drain_frames` takes `SUSPENDED`/`RESUMED` only where asked (`Node::sleep_asked`) and refuses the
  manager-to-driver opcodes coming back; `advance` applies the state moves and restarts the heartbeat on resume.
- Verification so far: `cargo build --bin device_manager` (x86_64) clean; `./build.sh --arch x86_64` -> ok.

## Part a - the drivers' half of the exchange

- `drivers::common` (sleep section): every driver carries the exchange. THE COMMON STEP, run inside
  `drain_control_into`/`stand` when a driver takes no step of its own: it holds the driver's work (the calling loop is
  between units of work and serves nothing until `RESUME`), leaves the device as it is for suspend to idle, stops a
  virtio device (`quiesce_virtio`) for a state that cuts the power, and answers a device that lost its state as not
  back (the manager rebinds it). A `RESUME` with nothing suspended answers back. THE OWN STEP: `takes_sleep`,
  `suspend_requested`, `suspended`, `resumed`, `hold` (the control channel alone; `CONNECT` accepted into the serving
  set, `STOP` latched), the `SleepStep` trait and `take_sleep_step`; only waits that can say "nothing ready" hand a
  `SUSPEND` back (`wait_or_answer_until`, `wait_providers_until`, the new `wait_providers_or_sleep` and
  `serve_any_or_sleep`, `wait_node_or_answer`, `answer_ping`) - any other runs the common step, so the exchange
  completes whichever wait reads the frame. `Bind` is now `Copy`.
- `driver-protocol`: `DriverFailureCode::Busy = 6` (a refused `SUSPEND` while a sink path is enabled; retryable).
- `virtio`: `Virtio::restore` (reset, the same two feature words, `FEATURES_OK`, the config vector), `Queue::restore`
  (the same ring address, emptied, interrupt flag kept, the driver's indices zeroed), `Virtio::sleep`/`wake`.
- The drivers with their own step: `virtio_blk` (flush; S3: stopped and restored under the same binding - the system
  volume stands on it), `virtio_i2c` (restored after S3), `ahci`/`nvme` (flush; not back after a power loss),
  `virtio_scsi` (per-unit SYNCHRONIZE CACHE; stopped for S3, not back), `sdhci` (no cache; not back after a power
  loss), `tpm_driver` (S3: `Shutdown(STATE)`, refused if the TPM does not take it; resume: no Startup when the
  firmware already started it - asked with a GetCapability - else `Startup(STATE)`, falling back to CLEAR and saying
  so), the three watchdog drivers through `watchdog::serve` (not armed: left alone; armed: disarmed where the device
  allows, else its longest timeout and a last pet with that timeout as the "awake by" bound unless the device stops
  counting in the state; resume re-arms with the timeout it had, or disarms one that was not armed), `ipmi` (queue
  held; the BMC watchdog disarmed and re-armed with its countdown; resume: `prepare()` and identify again),
  `smbus_ich9` (no register until `RESUME`; then decode checked and status cleared), `ucsi_acpi` (a `SUSPEND` a
  command's wait takes is held until that command completes; resume: every connector not answering, notifications
  enabled again, every connector read again), `tcpci` (refused with `busy`, naming the connector, while the sink path
  is enabled; else sink path off and alert masked; resume reads the port afresh as a bind does - `configure` and
  `bind_afresh` factored out of the bring-up), `i2c_hid` (SET_POWER sleep and `_PS3`; resume `_PS0`, SET_POWER on,
  RESET and its indication), `xhci` (active enabled ports to U3, then the controller halted; resume: RUN and every
  suspended port back to U0; after a power loss not back). `i2c_hid`'s `wait_line` and `tcpci`'s `lost` no longer
  take a handed-back `SUSPEND` for their deadline.
- Every driver entry in `src/user/services/manifest.toml` declares `suspend-deadline` (500 for the storage drivers and
  the TPM, 600 `ipmi`, 1000 `ucsi_acpi`, 300 `xhci`, 200 `i2c_hid`, 100 the rest).
- Verification: `cargo build` of the drivers (x86_64) clean; host tests
  `cargo test --manifest-path user/drivers/core/Cargo.toml --lib` -> 439 passed, among them
  `a_restored_device_gets_its_features_back_and_its_rings_emptied_at_the_same_address` (watched failing with the ring
  left unzeroed) and `a_watchdog_in_a_sleep_is_disarmed_where_it_can_be_and_otherwise_bounds_the_sleep` (watched failing
  with the stop condition inverted); `tpm` 31 passed with
  `a_sleep_saves_the_state_and_the_wake_starts_the_tpm_only_where_the_firmware_did_not`; `driver-protocol` 79 passed;
  `system-manifest` `a_suspend_deadline_of_zero_or_past_the_ceiling_is_refused_and_every_shipped_driver_declares_one`
  passed. No guest run of any driver's step yet.

## Part a - the services' half: the transaction

- IDL (`liber:process@1`): the control-channel interfaces numbered apart so one channel carries them with
  `shutdown-notice` (@op 1): `sleep-notice` 16-19, `device-sleep` 20-22, `platform-sleep` 24-25, `sleep-entry` 28,
  `application-freeze` 32-33; `sleep-reason` (requested, sleep-button, lid, idle, critical) added to `suspend`,
  `hibernate` and the record. `liber:security@1` capability `system-sleep` appended.
- `rt`: `serve_multi_rooted` (serve several roots, the handler told which root a connection descends from),
  `domain_create_in` (the parent argument), `domain_freeze`, `clock_boot_ns`. `src/tools/foreign-audit-link.py --check`
  after the change -> "the recorded pass-2 inventory reproduces" on all three targets.
- ServiceManager (`service_manager/sleep.rs`, new): `system-sleep` served on connections it mints itself
  (`SLEEP_CLIENTS`, resolved by name `SLEEP` for PermissionManager); ONE transaction at a time, driven from the
  standing loop's one wait (the step's bound is the wait's deadline): CHECK (device-sleep.check; refused before
  anything is frozen), ANNOUNCE (every Ready row declaring the notice), FREEZE (application-freeze on a connection of
  ProcessService's supervisor root), FLUSH (hold-writes to every storage instance), DRIVERS (device-sleep.suspend),
  PLATFORM (platform-sleep.prepare with the wake nodes, only where the ACPI service runs), ENTER (sleep-entry.enter
  to SystemManager, the timed wake capped 5 s under a watchdog's "awake by"). Resume and unwind run the undo stack:
  platform wake, drivers resume, writes released, resume notice, thaw LAST. Bounds scale with the boot window
  (`BOOT_WINDOW / 3000`). Inhibitors delay only an `idle` sleep, each bounded (10 min). While a transaction runs:
  suspend/hibernate answered `again`; the admin channels' `!poweroff`/`!reboot` recorded as the door and run once the
  transaction has ended (`run_power_verb`, factored out of `handle_admin`); other admin verbs answered `SLEEPING`;
  `alive` not answered (the liveness channel leaves the wait set); a crashed service's restart deferred to the end.
  The record: outcome resumed / refused / unwound with step, who, why; a door named beside any failure.
- ProcessService: two roots through `serve_multi_rooted`; the applications Domain created at start; a launch on the
  client root lands in it (per-launch Domains as its children via `domain_create_in`), a launch on `SUPERVISE` keeps
  the control plane; `application-freeze` served on `SUPERVISE` alone (`SYS_DOMAIN_FREEZE`). Manifest role
  `SUPERVISE` added; ServiceManager's own launches go through it, PermissionManager keeps the client root.
- StorageService: the control channel in its wait set; `hold-writes` flushes the device's volatile cache
  (`FileSystem::flush_device`: DiskFs and FAT through `block_flush`) and holds every mutating request (volume ops 3, 4,
  5, 7, 9, 10, 13, 15, 16, 18, 19, 20 and a writer's commit) whole in `held_requests`; `release-writes` serves them in
  arrival order before it answers. The client request handling is factored into `serve_client_request` and
  `settle_client` so the replay takes the same path. Every storage instance declares the sleep notice.
- SystemManager: `sleep-entry.enter` recognised on the branch channel and answered after `SYS_SYSTEM_SLEEP` returns
  (the timed wake turned into a boot-clock deadline).
- ACPI service: `\_S3`, `\_S4`, `\_S5` evaluated at start and registered (`SYS_FIRMWARE_SLEEP_TYPE`);
  `platform-sleep.prepare` (per wake node `_PRW` - a GPE-block-device event is refused by name - `_DSW` or `_PSW`, the
  GPE set for wake; `_PTS` for S3/S4; `\_SI._SST`) and `wake` (`_WAK`, `_SST`, wake GPEs cleared); the sleep notice
  answered; declares the notice.
- Watchdog service: the notice - announce gives every running timer its longest timeout and a last pet, nothing is
  asked or petted until `resumed`, which restores the configured timeout and restarts the question schedule.
- TimeService: the wall clock counts on `clock_boot_ns`; `resumed` reads the RTC again; a service that read no time
  reads again at each request; served with `serve_multi_rooted` over its root and control channel; declares the
  notice (and links `process-proto`).
- PermissionManager: `Capability::SystemSleep`, resolved as `SLEEP`, granted to `sleepctl` alone.
- `sleepctl` (new tool, dynamic, wave 3): `last`, `suspend [idle|ram] [SECONDS]`, `hibernate`, `inhibit SECONDS
  REASON`; after an accepted request it polls the record until the transaction ends (it is frozen meanwhile) and
  prints how it ended, the cores' parked wakeups included. `process-client` gains `SleepClient` and its provider the
  five trampolines.
- DeviceManager: a `resume` arriving while the suspend still runs is owed and taken as soon as it ends; lists encoded
  with the wire's 16-bit count (was wrongly 32-bit).
- Verification so far: `./build.sh --arch x86_64` -> RESULT ok (after the provider lists were made exact and
  `sleepctl` joined the wave table). No guest run yet.

## Part a and b in the guest (x86_64): the S3 and suspend-to-idle faults found and fixed

- DeviceManager deadlocked the resume after an S3: `persist_incidents` wrote the "did not come back" incidents through
  ConfigService while StorageService held writes, and writes are released only after the drivers' resume answers. The
  write now runs after the sleep step and not while a sleep run exists (`device_manager.rs`, the standing loop).
- The development channel did not survive an S3 (rebound, its host handshake lost): `dev_channel.rs` takes the sleep
  itself - stopped for a sleep that cuts the power, `Virtio::restore` plus `serial_port::Stream::rearm` (both queues
  restored, the receive pool posted again) at the resume, under the same binding.
- The display stopped presenting after any display-driver rebind (not S3-specific): a configuration within the same
  generation - the output replaced under an unchanged extent - made the client library drop its images and offer them
  again, which the service refuses within a generation; the console stayed `out-of-date` for good. `surface::Surface::
  rebuild` now only acknowledges such a configuration (`src/user/libs/display/surface/src/lib.rs`). Evidence: a
  screendump before and after a line typed on the emulated keyboard differed before an S3 and was identical after it,
  before the fix.
- S3's record listed every core as "cpu0 ... 0 wakeups": the entry reports no parked cores for S3 (the per-core record is
  suspend to idle's); `arch/x86_64/sleep.rs`.
- The keyboard is restored in place after an S3 and is a wake source in suspend to idle (`virtio_input.rs` `Sleep`), the
  pointer restored in place.
- A wake that lands on another core than the entry's now kicks the entry's core (`sleep::woke_by` sends a wake IPI to
  `ENTRY_CPU`); before, a fixed button routed to the boot core left an entry on another core halted until its timer.

## Part c - what wakes the machine, the devices that ask for sleep, the policy

- THE WAKE SET: `SYS_INTERRUPT_WAKE = 113` (`rt::interrupt_wake`) marks the Interrupt a driver holds as a wake source
  (WRITE, while it owns its binding; the mark goes with the binding at revoke and drop). `sleep::{mark_wake, is_wake,
  device_interrupt}`: a 32-slot set; `idle::interrupt` reports a marked device interrupt taken while sleeping as
  `WAKE_DEVICE` with its identity; `mask_claimed_lines`/`mask_msix_entries` leave marked lines live.
  `SYS_SLEEP_STATES = 114` (`rt::sleep_states`): the states the entry takes (suspend to RAM once `\_S3` is registered and
  the FACS exists) and the fixed buttons from the FADT's PWR_BUTTON/SLP_BUTTON flags (`sci::fixed_buttons`).
- `system-sleep` gains `status` (op 6: states, wake sources, inhibitors, the last sleep's watchdog cap, the scheduled
  wake) and `schedule-wake` (op 7), the latter only on a connection minted for the new `sleep-wake` capability
  (`liber:security@1`, appended; `CAP_SLEEP_WAKE`, PermissionManager's vocabulary 54, granted to `sleepctl`). A `CONNECT`
  on a wake-capable connection mints a wake-capable one. The scheduled wake becomes the next sleep's timed wake when it
  is earlier, is spent by the sleep it woke or by one that slept past it, and one already past at a request is spent
  then. ServiceManager refuses at acceptance a state the entry would not take (`Unsupported`, recorded as refused).
- `sleepctl status`, `sleepctl wake-at SECONDS`, `sleepctl wake-cancel`; the synopsis updated.
- DeviceManager's fixed sleep button: a `SLEEP` role (a `system-sleep` connection ServiceManager mints; manifest, LAST
  after `CATADMIN`) asked for a suspend - to RAM where offered, else idle - with reason `sleep-button`
  (`press_sleep_button`), answered at acceptance.
- THE CONTROL-METHOD BUTTONS AND THE LID: `acpi_button` (new driver; pure parts `drivers::acpi_button`, 3 host tests)
  bound to `PNP0C0C`/`PNP0C0E`/`PNP0C0D` (hid and cid rules): a power button's `Notify(0x80)` powers off through a
  `system-power` connection DeviceManager hands it (by driver name, as the keyboards'); a sleep button's asks for a
  suspend through a `system-sleep` connection minted from DeviceManager's door (new `ResourceKind::SysSleep = 13`);
  `Notify(0x02)` is said as the wake. The lid publishes the new `platform-switch` provider kind (28; `liber:device@1`
  `platform-switch.watch` stream of `switch-state`), `_LID` read at bind, at each `Notify` and at every resume. Wake
  armed when asked (the ACPI service arms `_PRW`).
- THE TIME AND ALARM DEVICE: `acpi_tad` (new driver; pure parts `drivers::acpi_tad`, 4 host tests: `_GCP`, `_GRT` with
  the zone, the timer value). A new privilege kind `ClockSource`, minted at boot and relayed kernel -> SystemManager
  (`CLOCKSRC`) -> ServiceManager (kept, `CLOCK_SOURCE`) -> DeviceManager (`CLOCKSRC` role, LAST) -> the `acpi_tad` binding
  (`ResourceKind::ClockSource = 14`). `SYS_CLOCK_BASE = 115` (`rt::clock_base`): the wall clock handed; `SYS_CLOCK_RTC`
  now answers `sleep::rtc_unix` - the CMOS clock where `arch::rtc_present()` (FADT `cmos_rtc_not_present`, or the
  development switch's `rtc`), else the handed base counted forward on the boot-time clock, 0 before one. An S3 with no
  RTC arms no CMOS alarm and leaves its length unknown (`sleep::slept_unknown`) until the base handed at the resume,
  which the boot-time clock takes in one step. The driver programs `_STV` on the AC and DC timers from the sleep's timed
  wake in its suspend step and reads, clears and disables them at the resume.
- THE ORDERLY POWER-OFF AND THE FORCED DEADLINE (P02M0198d's pieces the critical battery needs): `system-power` gains
  `power-off-within(seconds)` (SystemManager -> `SYS_SYSTEM_POWER` action `POWER_OFF_WITHIN = 2`). The kernel's `power`
  module (new): the earliest deadline kept, never later, never cancelled; checked in `idle::timer_interrupt` on every
  core and folded into the idle boot core's one-shot; `power::reset` powers off instead of resetting while armed (the
  recovery ladder, the lost-SystemManager path and `POWER_REBOOT`); the sleep entry refused while armed. Two kernel tests
  (the earliest kept and fired at its tick; fired from the timer interrupt while the core spins). A new `system-shutdown`
  interface served by ServiceManager (`service_manager/shutdown.rs`): answered at acceptance, then the admin door's
  sequence, or `end_by` with the door "system-shutdown's power-off" while a sleep's transaction runs.
- THE ACTIVITY SIGNAL (P02M0199c's root, built here as the plan allows): InputService serves `ACTIVITY`
  (`liber:input@1/input-activity`, `watch(idle-after-ms)` -> stream of `idle`/`active`), every input source feeding
  `Activity::seen`, the next edge a housekeeping deadline of the loop; the edges are `service_logic::activity` (3 host
  tests). DisplayService serves `OUTPUTS` (`display-outputs.outputs`: the one output, active while a scanout is
  attached, external only where a platform description says so - none is read yet).
- THE POLICY in PowerService (`power_service/policy.rs`), deciding nothing itself: `service_logic::sleep_policy` (5 host
  tests) - a closed lid suspends once per closing unless an external display is in use; idleness suspends on battery
  once per edge (or when going on battery while idle); a critical battery hibernates once, and a refused hibernation (as
  every one is until part e) powers off in order: `power-off-within(10)` then `system-shutdown.power-off`. Roles:
  CATALOGUE gains `platform-switch`; ACTIVITY, OUTPUTS; SLEEP, SYSPOWER, SHUTDOWN filled by `supervisor_role` at every
  start and relaunch. Dependencies gain input_service and display_service.
- THE DEFAULTS the plan lists are implemented as `Settings::default()` (lid suspends, idle suspends on battery after 15
  minutes); the plan says the owner confirms them when the part starts - asked in the final report.
- Verification so far: host tests - `drivers` `acpi_` 11 passed (the zone sign and the wake mapping each watched
  failing under a mutation); `service_logic` `activity::`/`sleep_policy::` 8 passed (the external-display guard and the
  idle edge each watched failing); `system-manifest` 29 passed. Kernel tests
  `kernel.sleep.a_marked_interrupt_wakes_the_sleep_and_nothing_else_does`, `the_wake_the_sleep_saw_first_is_the_one_reported`,
  `a_suspend_to_idle_parks_every_core_until_its_timed_wake_and_no_clock_jumps` -> PASS 3 passed (before the power module
  was added); the power-module tests not yet run. `foreign-audit-link.py --check` after the rt changes -> reproduces.

## Part d - the gate `check-sleep.sh` (x86_64)

Three boots, each its own instance in private state:
1. S3 offered: suspend to idle with host-stamped serial lines (the counter silent between the kernel's lines, monotonic
   against boot-time clock across the gap, a 20 s Timer firing after its remaining awake time, the parking record);
   S3 by `system_wakeup`; S3 by the RTC alarm; after each wake ping, a file read back, a frame presented (screendump
   before/after typed keys), a line typed at the serial console answered, the wall clock within 2 s, a 1 s wait on time;
   a Ctrl+Z job across a sleep and `fg`; the sleep fixture hot-plugged after the S3s (the restored Slot Control); a
   refusal unwinding the drivers' step; a shutdown typed during a held drivers' step.
2. The platform: the fixture SSDT with `--sleep` (LID0, PWRB, SLPB, TAD0 and `_E06` on GPIO line 6; `acpi-fixture.py
   --sleep-event`, `--tad-clock`, `--tad-read`), the CMOS RTC named absent: lid, TAD clock and timer, sleep button,
   policy relaunch, power button through the registered `\_S5`.
3. The soft-off fallback: the registration refused, `shutdown` through the fixed ports.
A KEY WAKE FROM S3 WAS CHECKED, NOT ASSUMED: with the guest in S3, a key through `dev.sh key` and a QMP
`input-send-event` each left QEMU `suspended` (2026-09-30) - the gate does not require it.
Runs so far: boot 1's suspend-to-idle case PASS (count 131 -> 132: monotonic +581 ms, boot-time +5498 ms; a 20 s Timer
fired after 19994 ms awake and 24911 ms since boot; 64 cores parked, none woke for anything else; 4959 ms measured
between the kernel's lines for a 5 s wake); the rest in progress.

## Part c and d in the guest: faults the gates found (2026-09-30)

- POWERSERVICE NEVER STARTED since its sleep roles were added: `supervisor_role` handed it the `SLEEP`, `SHUTDOWN` and
  `SYSPOWER` clients with every right the minted channel carries, and `rt::receive_roles` refuses a client role above
  its ceiling (`TooManyRights`), so the service failed its bootstrap - and with it every grant resolved from its roots:
  `typeccheck` could not be launched ("the launcher refused the component") in both Type-C gates. The three are now
  narrowed to the client ceiling (send, receive, wait, transfer) in `service_manager.rs` (`client_end`), at the role
  only - a connection minted for a resolved GRANT keeps its rights, because PermissionManager narrows it again for the
  component it grants it to. Evidence: `ServiceManager: power_service: FAILED to start` in the UCSI gate's serial log
  before; the UCSI gate's `typeccheck list` and every later case passing after.
- THE DISPLAY STAYED FROZEN AFTER AN S3 (the display driver torn down and bound again): the console followed a new
  configuration only on a Configure EVENT, and DisplayService drops an event a full stream cannot take; the S3 resume
  rebinding every driver prints a burst the console presents while the display provider is adopted again, the
  Configure was dropped, and every acquire after it answered `out-of-date` - which the contract says the client
  rebuilds on, and the console ignored. `console_service.rs`: an acquire answering `out-of-date` sets the presenter's
  flag, and the loop follows the configuration (`follow_configuration`, shared with the event's path) before its next
  wait. Evidence: check-sleep's idle, RAM and RTC cases each "a frame presented" after the fix.
- A KILLED THREAD IN A WAIT SET LEAKED EVERY MEMBER: `sys_waitset_wait` called `sched::exit()` with the set's `Arc` held
  in its frame, and `exit` never unwinds - so a StorageService killed at a reboot left its channels open and its peers
  never saw it go ("left behind"). `syscall/mod.rs`: the set is dropped before the exit. Kernel test
  `kernel.test_suites...a_thread_killed_in_a_wait_set_lets_go_of_every_member` (`test_suites/kernel.rs`): failed at
  its peer-closed assertion without the fix, passes with it.
- THE HARNESS'S GPIO BACKEND CRASHED ACROSS AN S3: after `GET_VRING_BASE` stopped the event queue, `vhost-i2c-gpio.py`
  still held its buffers and answered a raise into them. `GpioModel.ring_stopped` drops what the stopped queue held;
  self-test `test_a_stopped_event_queue_drops_its_held_buffers_and_a_raise_then_fires_nothing`.
- THE PLATFORM PHASE'S NEEDLES: `seen` matched a basic regular expression, so every identity with a backslash
  (`acpi:\_SB_.LID0`) could never match, and the sleep button's needle named `SLPB_:` where the driver says `SLPB:`.
  `seen` is a fixed-string match now.
- A CTRL+Z THAT STOPPED NOTHING: the stopped-job case typed `sleepcheck count 60` without `&`, and a governed tool run
  in the foreground through PermissionManager (`run_tool`) is no job the shell tracks - the terminal is never handed it
  (`SET_FG`), so Ctrl+Z reached no one and the counter ran on. The case now starts the job with `&`, brings it to the
  foreground with `fg`, and waits for the terminal's `^Z` before the sleep.
- THE SSIF BMC WAS RESUMED BEFORE ITS SMBUS CONTROLLER (the IPMI gate's sleep case: "ipmi (platform device 39) did
  not answer the resume within its bound"): DeviceManager orders a binding whose ENTRY declares `watchdog` last at the
  suspend and first at the resume, and the `ipmi` entry declares it for every form - so the SSIF child, which publishes
  none, was resumed ahead of the controller its transactions go through. `service_logic::sleep_order`: the watchdog's
  place is a leaf's - a publisher that consumes no provider; one that consumes is ordered by its depth. Host test
  `a_watchdog_publisher_that_consumes_a_controller_goes_before_it_and_comes_back_after_it`: failed against the old
  order, passes with the new.
- THE ACPI GATE'S SERIAL-BUS FIELD AFTER THE SERVICE'S RESTART ("a GenericSerialBus field names a connection this
  instance was not granted"): DeviceManager acts on the kernel's "namespace loaded" report before ServiceManager hands it
  the new instance's admin connection, so the grant went out on the ended instance's connection; the generated client
  answers a call that never reached a live server `again` (or `commit-uncertain`), which the grant took for a refusal
  and marked done - never made again. `grant_acpi_connections`: those two answers are a lost instance's, the grant is
  given up and made whole again on the hand-off.

## Part e - hibernation (x86_64 implemented; the ports owe their `arch::sleep` half)

THE OWNER'S QUESTION, asked and still open: whether a passphrase typed at resume is offered where TpmService cannot
seal. Nothing here offers one; a machine without a sealing TPM reports hibernation "not set up" and is refused.

- ABI (`src/abi/src/lib.rs`): `SYS_SNAPSHOT_INFO = 116`, `SYS_SNAPSHOT_READ = 117`, `SYS_SNAPSHOT_RELEASE = 118`,
  `SYS_RESTORE_BEGIN = 119`, `SYS_RESTORE_WRITE = 120`, `SYS_RESTORE_COMMIT = 121`, `SYS_SYSTEM_FINGERPRINT = 122`;
  `SNAPSHOT_BATCH` (256 pages a call), `SNAPSHOT_CONTEXT` (64 bytes), `SnapshotInfo`, `SystemFingerprint`;
  `SLEEP_STATE_DISK_ENTER`, `WAKE_SNAPSHOT`, `WAKE_RESTORED`. Every call needs the new `Hibernation` privilege
  (`object/privilege.rs`), which ServiceManager hands to the image component alone.
- THE SNAPSHOT (`kernel/sleep/disk.rs`, new): `prepare` counts every page in use - the pool's frames that are not
  free and the whole of the loader's, the kernel's and the firmware's ACPI tables and NVS, never reserved, device or
  framebuffer memory - and allocates the copies, their list pages and the bitmap of frames the copy skips BEFORE the
  copy, refusing with `ERR_RESOURCE_EXHAUSTED` when free memory cannot hold them; `copy_now` runs with every other core
  held and interrupts off, takes no lock and allocates nothing; `taken`, `info`, `read` and `release`. The copies and
  the bitmap are skipped; the list pages are copied, because the restored machine gives the copies back by reading
  them. The system image's digest (`digest_of`: the kernel's code and read-only data between two new linker symbols,
  each module's name and bytes in order, and the development variant) and the hardware's (the memory map by class,
  the core count and every PCI function with its identity), computed by `fingerprint`.
- THE ENTRY (`arch/x86_64/hibernate.rs`, new; `arch/x86_64/sleep.rs`): the snapshot is S3's entry up to the last step
  - the same save (`s3_save_and_enter`) with `disk::copy_now` in place of SLP_EN - and returns twice: `WAKE_SNAPSHOT`
  directly, `WAKE_RESTORED` through `s3_resume_entry` in a restored machine, which resumes as after an S3 and stirs the
  random pool (`entropy::stir`, credited nothing) so nothing drawn after the copy is drawn again. `enter_disk` writes
  the registered `\_S4` and powers off where none is registered, naming the path it took.
- THE RESTORE (`disk::begin`, `write`, `abandon`, `commit`; `hibernate::replace_memory`): the image's frames are read
  twice and held to the first reading, each a page of RAM of an image's class named once; every page is held in a
  frame outside the image, the list pages too, and a frame the allocator answers that is a target is set aside. The
  replacement runs on the boot core's idle context: every other core sent INIT, the loader's identity map reinstated,
  a trampoline in a safe frame with page tables of its own (an identity map of all RAM in 2 MiB pages) copies every
  page to its frame with global pages off, loads the image's tables and jumps to its `s3_resume_entry` on its resume
  stack. The resume context (`hibernate::context`: magic, the kernel's page tables, the entry, the resume stack, the
  direct map, the top of RAM, the kernel image's address) is checked before a restore begins.
- THE BOOT CORE'S IDLE CONTEXT, REACHED UNDER LOAD (`sched/mod.rs`): a request waiting for it - an S3, the snapshot,
  the replacement - used to wait until the boot core settled, and a thread that never blocks kept it from ever
  settling: after `stop-all` the development agent spun on its closed stream, and the replacement never ran. The tick
  now sends even a sole thread off the boot core while a request waits (`on_timer_preempt`, `reschedule_as`), and
  `run_until_idle_bounded` runs the request between two threads' turns. The agent (`dev_agent.rs`) and the console
  (`console_service.rs`) now end an attachment whose stream closed with nothing queued, which they waited on for ever.
- THE IMAGE (`service_logic::hibernation`, `cmac`): a 4 KiB header (magic, version, state, the sealed key-encryption
  key, the image key wrapped under it, the page count, the time, the system and hardware digests and the context,
  authenticated with AES-CMAC under a key derived from the KEK) and 1 MiB chunks, each a head of frame numbers and a
  CMAC tag over the index, the frames and the ciphertext, the pages AES-128-CTR under the image key. `Refusal` names
  each way an image is refused. The KEK is 32 bytes, sealed through TpmService to PCR 4. `service-logic` is optimized in
  the services' development profile, because the image is encrypted and authenticated page by page.
- THE SPACE: a GPT partition of the LiberSystem hibernation type (`4C424653-0002-4000-8000-4C6962657246`,
  `partition::HIBERNATION_TYPE_GUID`, `find_partition`, `holds_hibernation_image`). StorageService's system instance
  finds it on the system disk at mount (`find_area`) and serves `liber:storage@1/hibernation-area` - describe, read,
  write, flush and the verdict - to the image component alone; area writes are not filesystem writes and are never
  held. An image header found at mount HOLDS every write to the system volume until the verdict (`image_hold`). WHERE
  IT IS SET UP: `image.sh --format img --hibernation MIB` (`mkimage.sh img KERNEL SIZE MIB`) adds the partition at the
  end of an installed disk.
- THE IMAGE COMPONENT (`services/core/src/hibernation_service.rs`, new; restart transparent, plan-relaunchable):
  `status` (set up where the partition is at least as large as memory and TpmService's `info` says the TPM seals),
  `write-image` (fresh keys, the KEK sealed, every chunk read from the kernel, sealed and written, the header last and
  flushed, the snapshot given back), `discard-image`; at every start, the image found at mount restored - frames first,
  `restore_begin`, every chunk authenticated, decrypted and held to the frames the first pass read, the header
  invalidated, the restore's door asked to stop every binding, memory replaced - or refused, invalidated, and the boot
  let go on.
- THE TRANSACTION (`sleep_transaction`, ServiceManager `sleep.rs`, `restore.rs`; SystemManager; DeviceManager): a
  hibernation runs as a suspend up to the entry, which takes the snapshot; then `ImageDrivers` (DeviceManager's
  `resume-for-image`: every suspended binding publishing `block` or `tpm`; one that does not come back is bound again
  after the sleep, as any, and the write finds out whether the image's disk came back), `ImageWrite`, `DiskOff`
  (SystemManager's `off-hibernated`) - or, hybrid, `ImageSuspend` and `RamEnter`. An image written owes its `Discard`,
  run after the drivers are back and before the held writes go. The drivers suspend for what the machine enters: a
  hybrid sleep's for S3. The TPM driver sends nothing for hibernation - its next start is a boot - so the TPM stays
  usable for the seal after the snapshot. THE BRING-UP WAITS FOR THE VERDICT: until the image component starts only its
  dependencies start, and then ServiceManager serves the restore's door (`prepare-replacement`, DeviceManager's
  `stop-all`; `boot-continues`, LogService's journal handed over) until the verdict, bounded at fifteen minutes.
- `sleepctl hibernate [hybrid]`; `sleepctl status` says whether hibernation is set up and what became of the image found
  at this boot; PowerService's critical battery hibernates where it is set up and powers off in order where not.
- THE GATE `check-hibernate.sh` (registered in `check.sh`, the verification model's catalog and `release-required`):
  a GPT system disk with the paired volume and a hibernation partition, swtpm behind CRB, a held QMP connection for
  `SUSPEND_DISK`; the restore with the counter going on; the boot after; S4 not offered; a modified image, another
  system image (the development variant) and another machine (another core count) refused; hybrid both ways; no TPM.
- Kernel tests (`sleep/disk/tests.rs`, new): `a_snapshot_holds_every_page_in_use_as_it_was_at_the_copy_and_gives_every_frame_back`,
  `a_restore_refuses_a_foreign_context_and_frames_that_are_not_ram_or_named_twice`,
  `a_restore_keeps_every_frame_it_takes_off_the_frames_the_image_goes_to`,
  `the_system_digest_covers_the_kernel_and_every_module_by_name_bytes_and_order`: `TEST_SELECTION=<the four> ./test.sh
  --arch x86_64` -> PASS, 4 passed (37 s), after the list-page change too. EACH WATCHED FAILING against a mutation of
  its own, one run each: nothing copied (tests.rs:65, the page as it was at the copy), a frame named twice admitted
  (:94), a target handed out as a holding frame (:144), the modules' names left out of the digest (:167).

## Part e - the hibernation gate, run to the end (2026-10-01)

- `LIBER_DEVELOPMENT=1 ./check.sh --gate hibernate` (run 13, after the scheduler, agent, console, list-page, bootproto
  and harness fixes above) -> PASS, ten boots: "an image written and entered S4 (SUSPEND_DISK), restored with the counter
  going on; the boot after found none; powered off where S4 is not offered; a modified image, another system image and
  another machine each refused with the header invalidated; hybrid discarded on the S3 resume and restored after the
  power was lost; not set up without a TPM". The restore's own line: "count 79 then 80: monotonic +3035 ms, boot-time
  +197035 ms" - the monotonic clock excludes the time the machine was off, the boot-time clock includes it.
- AFTER IT, THE GATES THE HIBERNATION WORK TOUCHED, one guest at a time: `sleep` FAILED (exit 1, 866 s) - every case of
  the first boot passed (suspend to idle, S3 by `system_wakeup` and by the RTC alarm, the hot-plug after S3, the stopped
  job, the refusing fixture, the shutdown typed during a held sleep), and the PLATFORM boot (the fixture's sleep
  devices, the CMOS RTC named absent, no TPM) never reached its shell: its serial log stops at "AcpiService: online -
  instance 1, 2 table(s), 43 node(s) reported", with no "DeviceManager: the ACPI service's admin connection arrived"
  after it. Under investigation; not claimed.

## Faults found after the hibernation gate, and fixed (2026-10-01)

- THE PLATFORM BOOT'S STALL WAS A DEADLOCK, reproduced alone (`.build`-independent script: the sleep fixture SSDT,
  the TAD's clock running, `-global ICH9-LPC.disable_s3=0`; with and without the CMOS RTC, with and without swtpm - all
  stalled; the plain ACPI fixture without the sleep devices booted). QMP showed every vCPU halted - no thread runnable,
  so processes waiting on each other. DeviceManager, binding the control-method buttons (`acpi_button`, bound when the
  ACPI service reports the fixture's rows), minted each driver's `system-sleep` connection with a blocking `CONNECT` on
  the door ServiceManager handed it - and ServiceManager answers that door from its standing loop only, while during
  its bring-up it was itself waiting on DeviceManager (the next services' catalogue connections). FIXED: ServiceManager
  mints a few `system-sleep` connections ahead into a channel of their own, DeviceManager's new LAST role `SLEEPPOOL`
  (`service_manager/bootstrap.rs`, `sleep::BUTTON_POOL` = 4); DeviceManager takes one per button bound
  (`button_sleep_connection`) and asks the door only once they are gone, by when the bring-up is over. The repro then
  reached its shell.
- SYSTEMMANAGER'S POWER CONNECTIONS RAN OUT ("every power connection is taken; this caller gets none"), which failed the
  zone's binding: the control-method buttons, each thermal zone's driver and both policies now hold one.
  `MAX_POWER_CLIENTS` 8 -> 16 (`system_manager/src/main.rs`).
- THE CONSOLE'S UART ACROSS S3: `uart16550`'s comment promised that its resume reprograms the UART, and the driver took
  the common step instead - which, for a sleep that loses power, rebinds the driver: the kernel console changed hands
  twice per S3, the old instance faulted on its revoked ports ("ring-3 general protection fault ... koid=986"), and the
  serial mirror dropped output - which is how `ipmi`'s S3 case lost the KCS "the BMC answers again" line and failed.
  FIXED: the driver takes the sleep itself (`console::Sleep`: the tap drained and the UART quiet at the suspend, the UART
  programmed again whole at the resume).
- `abi`'s HOST SUITE WAS RED: syscalls 109 to 122 (this milestone's and P02M0200's), `POWER_OFF_WITHIN`, and the layouts
  of `CorePark`, `SleepReport`, `SnapshotInfo` and `SystemFingerprint` were never frozen in its snapshot. Frozen now,
  with the sleep states, the wakes and the processor tables' codes as families; `SnapshotInfo` gained `Default`.
  `driver-protocol`'s closed-set test named 13 (`SysSleep`) unknown: updated to the two kinds added (13 and 14).

## The console across a sleep, the static gates, and the ports' entry (2026-10-01)

- THE CONSOLE STOPPED ANSWERING AFTER EVERY WAKE. The gate batch of 2026-10-01 (after the fixes above): `sleep` failed
  at "idle: a line typed at the serial console was not answered after the wake (waited 20 s ...)", every suspend-to-idle
  check before it passing; `hibernate` wrote and entered its image, and the restore boot's serial log ends with
  "ServiceManager: sleep: the transaction ended - slept and woke", the drivers bound again, and no prompt ("lab: no
  shell prompt within 900 s"). CAUSE: `uart16550`'s serve loop called `session.reset()` after every sleep step it took
  itself (`console::Sleep`, added the same day), and `serial_port::Attachment::reset` closes the receive stream's
  producer and the end it granted - so ConsoleService's stream of typed input ended at every wake, though its
  connection and the driver lived on. FIXED (`src/user/drivers/core/src/uart16550.rs`): the session is kept across the
  step - the consumer's connection and its stream outlive the sleep; a new `CONNECT` or a closed consumer still resets
  it, as before. No other driver taking its own sleep resets a consumer after the step (checked: `dev_channel`,
  `watchdog`, `ucsi_acpi` and `ipmi_driver` close only on their failure arms).
- `source-hygiene`: `kernel/sleep/disk.rs` beside `disk/tests.rs` - moved to `disk/mod.rs` (a plain move).
- `kernel-allocations`: the snapshot's digests allocate nothing (`bootproto::sha256::Sha256`, streaming, fed the parts'
  digests in turn; `arch::sleep::development_variant` fills a 64-byte buffer) and the restore's list and directory
  frames are reserved before they are taken - with the rest of this goal's allocation fixes, recorded in P02M0198's
  audit ("The static gates over this goal's changes").
- THE GATES' OWN FAULT: `launch ... | grep -q` under `pipefail` fails a launch that printed after grep stopped reading.
  `check-sleep.sh` and `check-hibernate.sh` (and `check-ipmi.sh`, `check-typec-tcpci.sh`, `check-typec-ucsi.sh`) keep
  the output and search it after.

THE PORTS' ENTRY (`src/kernel/arch/aarch64/sleep.rs` and `src/kernel/arch/riscv64/sleep.rs`, new - the `arch::sleep`
half part b left owed on the ports):

- SUSPEND TO IDLE is the ports' entry: the portable entry parks every core with the timed wake as the only timer, and
  the port masks every claimed line (`interrupts::mask_claimed_lines`: the GIC distributor's enable on aarch64, the
  APLIC source on riscv64 - the port's only wired-line controller, delivering to the IMSIC) and every MSI-X entry the
  kernel programmed (`mask_msi_entries`, by the entry address each slot recorded at programming - `SLOT_TABLE`) but
  the wake set's, and puts back exactly what it masked.
  The serial window (`serial::sleep_begin`, `sleep_wake`, `sleep_end`): transmit is synchronous on both ports, so
  nothing is drained; after a sleep that lost the settings the UART is programmed again.
- WHAT THE FIRMWARE OFFERS IS SAID AT BOOT, checked rather than assumed: PSCI's SYSTEM_SUSPEND through
  `PSCI_FEATURES` (`psci::system_suspend_offered`), the SBI's System Suspend extension through the SBI's probe
  (`sbi_probe_extension(0x53555350)`). SUSPEND TO RAM AND HIBERNATION ARE NOT OFFERED ON THE PORTS
  (`offers_ram`/`offers_disk` false, `SYS_SLEEP_STATES` names neither, a request is `ERR_UNSUPPORTED`): both need a
  resume path for a machine whose cores lost their context - the warm-boot entry PSCI and the SBI jump to, and for
  hibernation the image's kernel resumed on a core a fresh boot handed over - which neither port has. Neither QEMU
  firmware here offers system suspend anyway (QEMU's TCG PSCI has no SYSTEM_SUSPEND; OpenSBI offers SUSP only with
  its `system-suspend-test` or an RPMI platform), so the gate could not show one.
- `check-tickless-idle.py` gains the ports' suspend-to-idle stage: `sleepctl suspend idle` with the timed wake, the
  serial line's timing as the host's oracle (a 100 ms counter silent between `sleep: entered` and `sleep: resumed`,
  that interval at least the wake less a margin, the monotonic clock moved by less than the sleep and the boot-time
  clock by at least it) and the parking from `sleepctl last` (no core woke for a device, cpu0 at most once for the
  timer, every other core at most once, for the IPI that ends its park).

OPEN, FOR THE OWNER: suspend to RAM and hibernation on aarch64 and riscv64 - the per-core and per-machine resume path,
and a firmware that offers system suspend to prove it on. Recorded as not done; the ports answer `ERR_UNSUPPORTED`.

## The sleep gate's platform boot, run for the first time to its checks (2026-10-01)

The platform boot (the fixture SSDT with the sleep devices, the CMOS RTC named absent) had never reached its checks:
until the deadlock fix above it never reached its shell. Run from that boot onward (a scratch copy of the gate with
boot 1 left out, so each round costs the platform boot alone; the gate itself unchanged in that respect), it found:

- THE FIXTURE'S TAD READ NOTHING THE HARNESS WROTE: in `acpi-fixture.py`'s sleep field list `E.offset_to(TAD_GCP * 8)`
  and `E.offset_to(TAD_GRT * 8)` named POSITIONS where `offset_to` is a reserved run - a LENGTH - so `TGCP` sat at
  0x86 instead of 0x44 and `TGRT` at 0xA2 instead of 0x60 (and `STVN` over FAN1's call count). `_GCP` read 0 and the
  driver said "has no clock, " with no wake timers. Shown on the host first: the fixture SSDT run through the
  interpreter (`aml` with `testing`, a stub DSDT, the fixture's pages behind BAR2) answered `_GCP` 0 and `_GRT` sixteen
  zero bytes; after the fix 7 and the clock's bytes. FIXED: the two runs are the distances from the unit before. AND A
  REGRESSION FOR EVERY UNIT: `acpi-fixture.py --self-test` (the `firmware-fixtures` gate) decodes every `HPGS` field
  list of the plain, sleep and processor tables (`page_units`) and holds each named unit to the byte offset the host's
  readers and writers use (`PAGE_UNITS`) - 129 units in place; WATCHED FAILING with the old run restored: 21
  misplacements reported (seven units in each of the three tables).
- THE WALL CLOCK WAS THE NTP SERVER'S, NOT THE TAD'S: TimeService disciplines its offset by SNTP where the network
  reaches a server, and this machine's does - so `date` read the host's time though the kernel answered the TAD's.
  `qemu-run.sh` gains `NET_RESTRICT=1` (user-mode networking `restrict=on`: DHCP and the forwarded port still work,
  nothing leaves for the outside), which the gate's fixture boots export.
- A FILE ON THE LIVE SYSTEM VOLUME CARRIES NO TIME: a development ISO runs the system from `libermemfs`, which by
  design stamps nothing (`MemFs` has no `set_clock`), and the FAT backend's listing reports no times either - checked
  on a plain development instance with the CMOS RTC: a file written to `vol://system` and to `vol://usb` both list
  `-`. The TAD check now writes its file to the run's USB stick (FAT32, stamped from `clock_rtc()` by StorageService)
  and reads the entry's date and minute on the host from the stick's private copy (`mdir`), within two minutes of the
  host's time plus the TAD's offset - an oracle outside the guest; a probe on the plain instance showed the entry
  stamped with the kernel's time to the minute.
- THE THIRD S3 OF A BOOT LEFT EVERY REBOUND DEVICE FAILED: a driver that sets its device up again only by a bind
  answers `RESUMED` "not back" after an S3 (virtio-net, -console, -scsi, -gpu, -snd, xhci, nvme, ahci, sdhci, hda, the
  `acpi-power` zone and battery, the fixture), and DeviceManager tore it down as a crash and bound it again - spending
  one of the node's three automatic attempts per boot each time. On the platform boot's third S3 (the lid's, the TAD's
  timed one, the fan's) every one of them reached "attempt 3" and "the node is failed": the network gone for the boot,
  and the zone's publication withdrawn, so ProcessorPowerService set FAN1 to 0 and the fan's check read 0, not 60.
  FIXED (`driver_binding::budget_for_a_sleep_rebind`, called from DeviceManager's `Resumed { back: false }` arm): the
  rebind a sleep caused is given the attempt it spends back first - an attempt that then fails spends as any other, and
  an operator's one attempt is left as it is. Host test
  `a_device_lost_to_a_sleep_is_bound_again_however_many_sleeps_the_boot_has` (ten sleeps after a bring-up that spent
  all three, every rebind admitted, the count unchanged; a failure after it charged), WATCHED FAILING with the refund
  removed (`sleep 0's rebind was refused`), 93 `driver-binding` tests passing restored.
- THE GATE'S OWN FAULTS: `await_end` read the transaction's last line past `baseline_lines`, a line count left by the
  FIRST boot - the later boots truncate the log, so it named no line of theirs (in a run from the platform boot it was
  unset); it now takes the last "the transaction ended" line, which the wait before it has seen arrive. And the fan's
  needles named `acpi_fan` where DeviceManager says `acpi-fan`.
- RUN SO FAR: the scratch copy (the gate from the platform boot onward, against this tree) PASSED whole on its seventh
  round - the platform boot (the TAD's clock at boot, after the lid's S3 and after its own timed S3, each with a file
  stamped with it; the power resources in each state; the TAD's timer at the sleep's 30 s; both fans across suspend to
  idle and S3 with FAN1's cleared level applied again; the control-method sleep button; the relaunched policy's lid
  suspend; the control-method power button through the registered `\_S5`), the critical battery (QEMU gone 10100 ms
  after the 10 s deadline was armed, through the relaunched policy), the soft-off fallback through the fixed ports, and
  the watchdog boot (a suspend to idle three times the i6300esb's timeout with the guest running throughout, and a
  resume made to hang ending in the watchdog's expiry after 120 s). The gate itself, boot 1 included, is in the final
  batch and is recorded when it ends.

## The last lines before a planned power-off, and the batch of 2026-10-01 (2026-10-01)

- THE GATE'S FULL RUN FAILED ON ITS LAST PLATFORM CASE, which the scratch copy had passed: "the power button's press
  asked for nothing (waited 30 s for ... PWRB: pressed - asking the power service to stop the machine)". The machine
  HAD powered off through the registered `\_S5`, but the serial log ends "Audi" + "power-off: the registered \_S5" -
  the button driver's line, the rest of an AudioService line and everything between were never on the wire. CAUSE:
  the control-method power button (as the fixed one) asks `system-power`'s `power-off`, an immediate power-off, while
  `uart16550` holds COM1; the driver had read those lines out of the console tap (up to 1024 bytes a read) and was
  putting them out when the kernel's terminal-path writer took the UART - and bytes the driver has read are on its
  side, which the writer, by design, does not wait for. FIXED (`arch::x86_64::serial::settle_driver`, called by
  `sys_system_power` before a power-off or a reset - never on a panic's or the forced deadline's path, where nothing
  may wait): while a driver holds the UART the tap is signalled and the core yielded until the ring has stayed empty
  for 100 ms (`SETTLE_QUIET_NS`: the driver's largest read is 89 ms on the wire at 115200 baud), at most 1 s in all
  (`SETTLE_BOUND_NS`); the ports have nothing to settle (`settle_driver` is empty there). An orderly power-off is
  untouched: its drivers are stopped first and the kernel holds COM1 again by the time it is asked.
  Kernel test `kernel.object.port_range.handoff.a_planned_end_waits_for_the_driver_to_take_the_ring_and_no_longer_than_its_bound`
  on the suite's second UART: a driver that reads nothing holds the end for the bound and no longer, the bytes still
  the ring's; a driver that reads 50 ms late is waited for, and the settle ends a quiet interval after its read; with
  the kernel driving the UART it returns at once.
  WATCHED FAILING: with `settle_driver` returning at once the test failed ("a driver that reads nothing holds the end
  for the bound and no longer (426 ns)"); restored, the selection of the five `kernel.processor` and seven
  `kernel.object.port_range.handoff` tests passed (12, 37 s, x86_64).
- THE DEVELOPMENT SWITCH WAS IN EVERY BUILD: `arch::absent_named` (the CMOS RTC and the sleep-type registration named
  absent over fw-cfg) was compiled into the shipping kernel too, honoured only when a boot profile is named - against
  part d's "compiled into the development build only and never into the shipping set". It is `cfg(liber_development)`
  now on all three targets, and a shipping kernel's `absent_named` answers false whatever a machine's fw-cfg holds
  (`arch/{x86_64,aarch64,riscv64}/mod.rs`). Compiled both ways on x86_64 (`LIBER_DEVELOPMENT=0` and `1`, and the test
  kernel); the ports at the end of the job.

VERIFICATION, the gate batches of 2026-10-01 (x86_64, one guest at a time, `LIBER_DEVELOPMENT=1 ./check.sh --gate`):

- `sleep` -> PASS (1912 s), boot 1 and every later boot: suspend to idle (count 84 then 85, monotonic +485 ms, boot-time
  +5403 ms; a 20 s Timer after 20002 ms awake; 64 cores parked, none woke for anything but the wake; 4940 ms between the
  kernel's lines for a 5 s wake), S3 by `system_wakeup` and by the RTC alarm, after each wake the ping, the file, a
  frame, the serial line, the wall clock within 1 s and a 1 s wait of about 1030 ms; PCRs 0-7 unchanged across S3 and
  the secret unsealed; the stopped job; the hot-plug after the S3s; the refusing fixture; the shutdown during a held
  step; the platform boot (TAD, power resources, fans, buttons, relaunched policy, `\_S5`); the critical battery (QEMU
  gone 10115 ms after the 10 s deadline was armed); the soft-off fallback; the watchdog boot (`running` through a sleep
  three times the timeout, `watchdog` 119 s after a resume made to hang). Earlier runs that day failed - on the console
  after a wake, the TAD, the rebind budget, the gate's own needles and the power button's lost line, each fixed above.
- `hibernate` -> PASS (1987 s, ten boots; the restore "count 79 then 80: monotonic +2990 ms, boot-time +199990 ms").
- The drivers' sleep cases in their own gates -> PASS: `ipmi` (KCS and SSIF across idle and S3, the BMC's watchdog
  steps in order), `typec-ucsi` (idle and S3, the detach in S3 reported), `typec-tcpci` (refused with a contract, a
  charger attached during a sleep contracted after it), `i2c-hid` (two sleeps).
- STILL TO RUN: `sleep` once more after the switch's `cfg` (in the batch running now) and, at the end of the job, the
  ports' suspend to idle in `check-tickless-idle.py aarch64 riscv64`.
- `sleep` -> PASS again (1847 s, 2026-10-01) with the development switch compiled into the development build alone and
  the last harness fixes in.
- THE SCHEDULED WAKE, run by hand on a development instance with S3 offered (2026-10-01; no gate case carries it):
  `sleepctl wake-at 15` then `sleepctl suspend idle` with no timed wake - "slept 8911 ms and was woken by the timed
  wake", ServiceManager "the scheduled wake at 1790879099 is spent" (the transaction's steps took the rest of the 15 s);
  `sleepctl wake-at 20` then `sleepctl suspend ram` - "slept 15000 ms and was woken by the RTC alarm", spent, QEMU
  `running` with no host wake. `sleepctl status` lists the wake sources, the scheduled wake and the inhibitors.

## The wake set's devices: a USB keyboard ends a suspend to idle (2026-10-01)

WHAT WAS MISSING: part c's wake set names devices a driver arms in its SUSPEND step - USB remote wakeup through the
controller, PCI PME, a network device's wake. Only `virtio-input`'s keyboard, the control-method buttons and lid, and
the TAD armed anything; `xhci` halted its controller at every sleep.

WHAT WAS IMPLEMENTED (`src/user/drivers/core/src/xhci.rs`, `usb_hid.rs`):

- A KEYBOARD WHOSE CONFIGURATION DECLARES REMOTE WAKEUP (`bmAttributes` bit 5) is sent `SET_FEATURE(DEVICE_REMOTE_WAKEUP)`
  when it is configured (`UsbDevice::remote_wakeup` records that it took it). Enabled there and not at the sleep: a
  control transfer in the SUSPEND step would race the keyboard's own reports (the wait dispatches other HID events
  through the bound devices, and the device being addressed would have to be out of that set), and the feature acts
  only while the device's port is suspended. A pointer is not enabled; a device that refuses keeps working.
- THE SLEEP STEP: a suspend to idle asked to arm wake, with such a keyboard bound, sends the ports to U3 as before but
  does NOT halt the controller; its interrupt is marked as a wake source (`interrupt_wake`) and the answer is
  `DoneWakeArmed`. A key makes the keyboard signal its resume, the controller (QEMU's too) moves the port to Resume and
  posts the port's change on its event ring, and its interrupt ends the sleep. At the resume the mark is taken off and
  the ports go back to U0. Every other sleep halts the controller as before.
- DECISION: the controller stays running in that state because a halted controller's only wake is PCI PME through the
  platform, which this machine's emulation does not raise; Linux's s2idle keeps a wake-capable controller's
  interrupt live in the same way.

VERIFIED by hand on a development instance (2026-10-01; no gate case carries it): with both `virtio-input` bindings
disabled (`lsdev --disable 8`, `9`), so the controller's interrupt is the only armed device wake, `sleepctl suspend
idle 60` and a key sent over QMP 14 s later -> "slept 8865 ms and was woken by a device"; the control - the same
sleep for 20 s and no key - "slept 19910 ms and was woken by the timed wake", cpu0 woken once for the timer and no
core for a device. "driver.xhci: suspended with a keyboard's remote wakeup armed" at each. The gate `sleep`'s
regression run (S3 still halting the controller, suspend to idle with it armed and no core woken but by the wake) is
in the batch running now.

NOT DONE, AND WHY: PCI PME (PMCSR's PME_En from a driver's SUSPEND step, the companion's `_PRW` GPE already armed by
`platform-sleep`) and a network device's wake. No device this harness can present raises either - QEMU's PCI devices
generate no PME, and the network functions this system drives (virtio-net, CDC-ECM) have no wake function at all - so
neither could be shown working, and both are left open in the milestone for hardware that has them.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate sleep` -> PASS (1871 s) with the controller armed: boot 1's suspend to idle
  still "64 cores parked, none woke for anything but the wake", every S3 case (the controller halted as before) and
  every later boot as in the runs above.

## The ports, run at the end of the job (2026-10-01)

- `./build.sh --arch aarch64` and `--arch riscv64` -> ok, with the ports' `arch::sleep` halves (P02M0198's audit
  records the fixes the first cross-build needed in other milestones' code).
- Suspend to idle on aarch64 and riscv64 - `check-tickless-idle.py` -> PASS on both: 5007 and 5009 ms between the
  kernel's lines for a 5 s wake with the counter silent, the boot-time clock taking the sleep in and the monotonic
  clock not, 4 cores parked with no wake but the wake. "PSCI SYSTEM_SUSPEND is not offered by this firmware" and "the
  SBI System Suspend extension is not offered by this firmware", said by the kernel at boot - the check part b asks
  for, written down.
- NOT DONE, reported as open: the per-core resume path on the two ports, and with it S3-like system suspend and
  hibernation there (`ERR_UNSUPPORTED`); PCI PME and a network device's wake (no emulated device raises either).

Status: implemented and verified on x86_64; suspend to idle verified on all three targets; OPEN as above, and for the
owner's decisions (the policy's defaults; a passphrase where no TPM can seal - see also the owner's note in
`NOTES.md`, "hybernation doesn't work without TPM - allow it with warning").

## The owner's decisions, built (2026-10-02)

The owner answered the open questions (2026-10-02): closing the lid only turns the display off and does not suspend;
on battery the idle timeout suspends nothing; a critical battery does not put the machine to sleep either; hibernation
without a TPM is allowed, with a warning when it is enabled, and no passphrase is asked at the resume.

WHAT WAS DONE:
- `display-outputs` gained `set-power(on)` (`src/idl/display.lsidl`, regenerated): DisplayService's `dark` state -
  the scanout zeroed and flushed, every present completed `discarded-occluded` in order while dark, `present_active_full`
  a no-op while dark and the visible surface drawn whole when the screen comes on; the protected screen is not taken
  while dark (`lock_screen` answers `unsupported`, as for a lost scanout) and a prompt held when the screen goes off is
  refused rather than shown (`show_screen` answers `unsupported`).
- `service_logic::sleep_policy` rewritten around the owner's defaults: `Settings { lid: LidAction, idle_suspends_on_battery,
  idle_after_seconds, critical: CriticalAction }`, default `ScreenOff` / false / 900 s / `PowerOff`; new actions
  `ScreenOff` and `ScreenOn`; the lid's first state counts as an edge (a relaunched instance turns on a screen an earlier
  one turned off); an external display in use keeps the lid from acting either way; `Settings::from_keys` reads
  `power.lid` (`screen-off`|`suspend`|`nothing`), `power.idle-suspend` (`on`|`off`), `power.idle-after-s` (> 0) and
  `power.critical` (`power-off`|`hibernate`|`nothing`), a refused value naming its key and keeping the default.
  13 host tests (`sleep_policy::tests`).
- PowerService: an optional CONFIG client role (`liber:config@1/config`, ConfigService added to its dependencies) read
  once at every start - the settings said on the console in one line ("closing the lid turns the screen off, idleness
  after 900 s suspends nothing, a critical battery powers off in order"); the screen actions go through its OUTPUTS
  client's `set-power`; a critical battery under the default says "a battery is critical - powers the machine off in
  order" and runs the existing orderly power-off (forced deadline, then `system-shutdown`).
- Hibernation without a sealing TPM (`service_logic::hibernation`, `hibernation_service`): the header carries a
  KEY PROTECTION word at offset 20 (1 = sealed through TpmService, 2 = in the clear), authenticated with the rest; a
  key in the clear is the key-encryption key itself where the sealed blob goes (`Header::clear_key`). The image
  component's `sealing()` answers Seals / Absent(why) / Failed(why): absent - no grant, no TPM bound, or an owner
  hierarchy that seals nothing - is SET UP WITH THE WARNING in the status's `why` ("WARNING: ..., so the image's key is
  written beside it in the clear - anyone who can read the disk reads every secret that was in memory, and anyone who
  can write it can make the machine resume what they wrote") and the image written in the clear with the warning on
  the console; a TPM that answers wrongly is still "not set up". A RESTORE of an image in the clear is refused on a
  machine whose TPM seals (`Refusal::ClearOnSealingMachine`), after giving a late TPM the same 60 s an unseal gets - so
  an image in the clear is no way around a TPM's seal. `sleepctl status` prints "hibernation: set up - WARNING: ...",
  and `sleepctl hibernate` prints the warning before it asks. No passphrase is offered. 9 host tests
  (`hibernation::tests`, one new).
- The gates follow the defaults: `sleep` - the lid closed turns the screen off (QEMU's screendump black, a line typed
  then still black, the machine running, no transaction) and opened turns it on (not black); `power.lid suspend` set,
  the power service killed and relaunched, the relaunched instance reading it and suspending to RAM on the lid with the
  device power states checked, the key given back to `screen-off` once read; the battery boot's critical battery
  powering off in order with no hibernation asked. `hibernate` - case 7 is now WITHOUT A TPM: set up with the warning,
  an image written with its key in the clear and restored on a boot still without one (the counter going on), and such
  an image refused on a boot whose TPM seals.

VERIFICATION:
- `./gen.sh` -> ok (display-outputs `set-power`; the `hibernation-status` and `sleep-status` docs).
- `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml` -> 746 passed (sleep_policy 13,
  hibernation 9).
- `LIBER_DEVELOPMENT=1 ./build.sh --arch x86_64 --part user` -> ok.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate qemu-admin-path` -> PASS (DisplayService's protected screen with the dark
  state in); `power-service` -> PASS; `source-hygiene`, `staged-consistency`, `dependency-policy` -> PASS.
- `dynamic-report` -> FAIL on the first run: only byte counts moved (powerctl, sleepctl, the providers whose protocol
  crates changed) - no import, owner or residual. `./check.sh --refresh dynamic-report` rewrote the tracked reports.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate hibernate` -> PASS in 2463 s, twelve boots (2026-10-02): every case before,
  and "no TPM: set up with the warning, the image written with its key in the clear and restored - the counter went
  on" (count 79 then 80, monotonic +3028 ms, boot-time +254028 ms) and "clear-on-tpm: refused (its key is in the
  clear, and this machine), the machine booted fresh and the header is invalidated".
- `LIBER_DEVELOPMENT=1 ./check.sh --gate sleep` -> FAIL once on the TAD stamp: `mdir` printed `5:09`, the hour under
  ten padded with a space, and the check's pattern wanted two digits (it had passed with hours past ten). The pattern
  takes one or two digits now.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate sleep` -> PASS in 1869 s (2026-10-02): "closing the lid turned the screen
  off - black, a typed line unseen - and suspended nothing; opening it turned the screen on"; "the lid set to suspend,
  the relaunched power service suspended to RAM on it, and the TAD's clock measured the sleep" with the device power
  states as before; "a critical battery powered the machine off in order through the relaunched policy, as the owner's
  default has it, QEMU gone 10127 ms after the 10 s deadline was armed"; every other case as before.

LEFT FOR THE OWNER: whether a critical battery should still power the machine off in order (the default built) or do
nothing at all - the answer said "suspend off" and did not name the power-off, which protects the filesystems.
