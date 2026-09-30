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
