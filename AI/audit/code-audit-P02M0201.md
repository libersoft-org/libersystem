IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0201 (2026-09-29T06:50:37Z):

## Starting point

Taken in the owner's agreed order after P02M0181. The plan is three parts:
- P02M0201a, the transports: the kernel's resolver rows, KCS and BT, the ICH9 SMBus host driver, SSIF over
  P02M0195's bus contract as a child binding, one message layer, discovery and the manifest declarations;
- P02M0201b: the BMC service, the `liber:bmc@1` contract, the `bmc` tool and the AdminService extension;
- P02M0201c, the verification: a harness BMC behind `ipmi-bmc-extern`, the `ipmi-bmc-sim` boots and the host
  suites.

What each part consumes was checked against the tree before any code, and is recorded below as each part lands.

## What was implemented (2026-09-29)

### P02M0201a - the transports

- `src/user/libs/driver/ipmi/` (new crate `ipmi`, no_std, host-testable):
  - `lib.rs`: the message layer - `Request`/`Response`, the completion codes (`cc`), the network functions
    (`netfn`), `Failure` (Deadline, TooLong, Mismatch, Protocol, Interface, Bus, RequestTooLong), `response()`
    (the reply matched to its request by netfn|1 and command, bounded at 272 bytes), the `Registers` seam (`wait()`
    spins 64 reads, then pauses), and `Availability` (three transactions in a row unanswered = unavailable; any
    answer brings it back). TRANSACTION_MS 5000, above `ipmi-bmc-extern`'s four seconds.
  - `kcs.rs`: `transact` (WRITE_START, the bytes, WRITE_END, then READ byte by byte to idle), `abort` (GET_STATUS/
    ABORT, three tries, reading the error state's own code) and `idle`.
  - `bt.rs`: `Bt::transact` (CLR_WR_PTR, the length-prefixed message, H2B_ATN, B2H_ATN, H_BUSY toggled, the
    sequence number checked) and `read_capabilities` (Get BT Interface Capabilities, answered by QEMU's BT
    interface itself).
  - `ssif.rs`: the `Smbus` seam, `Ssif::transact` (single 0x02 or multi-part 0x06/0x07/0x08 writes, 0x03/0x09/0x0A
    reads with the 0x00 0x01 start and the 0xFF end, NACK retried to 500) and `read_capabilities` (0x57).
  - `identity.rs` (Get Device ID, Get Device GUID; the name `bmc:` + 32 hex digits, or the fallback
    `bmc:{mfg:05x}-{product:04x}-{devid:02x}@{binding}`; the 64-byte selector), `sdr.rs` (full, compact, event-only,
    FRU and MC locators; partial reads of 16 bytes under a reservation; 512 records / 256 sensors), `sel.rs`,
    `fru.rs` (typed fields: binary, BCD+, six-bit, eight-bit; 2048 bytes, 8 devices), `watchdog.rs`, `chassis.rs`,
    `lan.rs`, `event.rs` (generator 0x41: boot completed 0x1F/0x06, graceful shutdown 0x20/0x03) and `queue.rs`
    (one message at a time, the watchdog's first).
  - `tests.rs`: scripted register models of KCS, BT and SSIF shaped after QEMU's, plus the parsers against hostile
    records - 27 tests.
- `src/user/libs/power/model/src/ipmi.rs` (+ `ipmi/tests.rs`): the linear conversion (`Linear::from_record`,
  `value()` in checked i128), the temperature reading, and `thermal()` - passive/hot/critical trips from UNC/UC/UNR.
- The kernel: `src/kernel/declared/mod.rs` gains `claim_config` rows (`ClaimWrite`) with `claim_writes()` and
  `release_writes()` - the `ich9-smbus` row (8086:2930) sets HOSTC.HST_EN and clears I2C_EN at claim and restores
  HOSTC at release; `src/kernel/device.rs` calls both; a kernel test,
  `the_ich9_smbus_claim_enables_its_host_and_the_release_restores_hostc`. `src/kernel/arch/common/pci/mod.rs`:
  RESOURCED rows for the PCI IPMI KCS and BT (class 0x0C07, BAR 0). `src/abi/src/lib.rs`: PCI_SUBCLASS_IPMI,
  the prog-ifs and DEVICE_TYPE_IPMI_KCS/BT. `src/kernel/arch/common/platform.rs` and `src/kernel/firmware/mod.rs`:
  the SMBIOS type 38 cross-check, SSIF records matched by their SMBus address (`policy::smbios_ssif`).
- The drivers crate: `smbus_ich9.rs` (bin `smbus_ich9`, x86 only - the ICH9 SMBus host as an `i2c-device` provider
  with block read/write and PEC, polled, KILL on a timeout); `ipmi_bmc.rs` (the BMC operations over a `Bmc` seam:
  identify, the SDR repository with reservation retries, sensor readings, SEL info and pages, FRU, LAN channels,
  and the two administrative executors' prepare/execute); `ipmi_driver.rs` (bin `ipmi`: KCS, BT or SSIF from the
  node's `_IFT`/`_SRV` or the PCI class; the watchdog taken over at bind; publishes `ipmi`, `power-source`,
  `watchdog` and the two executors).

### P02M0201b - the service, the contract, the tool

- `src/idl/bmc.lsidl` (package `liber:bmc@1`): `ipmi-provider` (the driver's contract) and `bmc` (the tool's);
  `device.lsidl` provider kind `ipmi = 26`; `admin.lsidl` actions `bmc-sel-clear = 3` and `bmc-chassis-control = 4`;
  `security.lsidl` capability `bmc`. Generated with `./gen.sh --accept-breaking` (pre-release).
- `src/user/services/core/src/bmc_service.rs` (bin `bmc_service`): adopts every `ipmi` publication, reports a pair,
  writes the boot's event once per BMC when every row with a wanted state reads running or stopped, the orderly
  shutdown's event on the notice, and serves the tool (with `sel-follow`).
- AdminService: the executor table, the descriptor's `operation()` rendering, PermissionManager's `bmc` scope.
- `src/user/apps/tools/src/bmc.rs` (shipping tool `bmc`) and its client crates; `bmccheck` (development probe).

### P02M0201c - the verification, and what QEMU 10.0.11 does not allow

- `src/harness/ipmi-harness-bmc.py`: the SDR and FRU files, `--describe`, and the harness BMC behind
  `ipmi-bmc-extern` (OpenIPMI framing, `--guid`, `--device-id`, `--product-id`, `--malformed`, the control socket's
  `reading`, `sel-add`, `mode silent|close|oversize|normal`); its record now also holds each control command where
  it arrived, so a gate can read the order "reservation, event, refused clear".
- `src/tools/check-ipmi.sh`, rewritten onto development instances (`dev.sh up`/`down`, the shell over `lab.sh sh`):
  the first form drove the console with `guest-console.py`, which typed the next line at a prompt printed before a
  late service line and lost characters into a running command - the run stalled at `bmc fru`.
- `src/tools/check-ipmi-admin.sh` with six cold scenarios (`src/harness/scenarios/ipmi-*.toml`): the SEL clear
  declined then approved, the clear cancelled by an event the harness adds once the protected screen is ready, and
  hard reset, power down, soft shutdown and power cycle, each in a boot of its own with one BMC.
- `src/tools/check-watchdog.sh` gains case 5, the BMC's watchdog: the orderly reboot's boot bound, the same reboot
  with the notice skipped (ServiceManager's new development verb `!reboot-without-notice`), and the expiry.

FINDINGS ABOUT THE PLAN'S ASSUMPTIONS (the plan says "checked on this machine (10.0.11)"; these do not hold):
1. TWO `ipmi-bmc-sim` IN ONE BOOT ARE REFUSED: `savevm_state_handler_insert: Detected duplicate SaveStateEntry:
   id=ipmi-bmc-sim, instance_id=0x0`, and the same for two `ipmi-bmc-extern` - both call
   `vmstate_register(NULL, 0, ...)` in realize (checked in QEMU 10.0's and a later tree's sources). So "all five
   interfaces in one boot, each with a simulated BMC" and "a pair of two simulated BMCs" cannot be run. What the
   gate does instead: at most one simulated and one harness BMC a boot - the harness BMC stands in for the second
   simulated one - and the pair is a simulated BMC and the harness BMC answering one GUID.
2. THE SIMULATOR'S `device_id` PROPERTY IS OVERWRITTEN: `ipmi_sim_realize` sets `ibs->device_id = 0x20` after the
   properties are applied, so every simulated BMC answers device ID 0x20 (verified with `qom-get`: `device_id=0x21`
   reads back 32). The gate tells BMCs apart by PRODUCT ID (`product_id=`, which is kept), and the fallback name of
   a GUID-less BMC is `bmc:00000-00PP-20@...`.
3. ONE SMBIOS TYPE 38 RECORD PER BOOT: QEMU builds every type 38 with the one handle 0x3000
   (`SMBIOS_BUILD_TABLE_PRE(38, 0x3000, true)`), and OVMF installs one of them - a boot with ISA KCS and ISA BT
   showed only `smbios:38#0 - an IPMI controller over BT`, and the kernel said of the KCS node "names IPMI interface
   1 at 0xca2, which no SMBIOS record describes". So each ISA and SSIF interface's cross-check runs in a boot where
   it is the only IPMI interface.
4. QEMU'S SIMULATED WATCHDOG ANSWERS "STARTED" AFTER AN EXPIRY: Get Watchdog Timer returns the timer-use byte the
   last Set left, "don't stop" included, with a present countdown of zero once it expired. `ipmi::watchdog::state`
   now reads RUNNING as the bit AND a present countdown above zero (host test added) - otherwise the take-over at
   bind would re-arm and pet a timer that had stopped.
5. THE SIMULATOR'S SOFT SHUTDOWN IS AN ACPI POWER-BUTTON PRESS (`qemu_system_powerdown_request`), and its power
   cycle is refused with 0xD5 - both as the plan says.

DEFECTS FOUND AND FIXED ON THE WAY:
- ServiceManager's `supervisor` status answer was written into a 4096-byte buffer. One row per service, the
  canary and one per manifest driver outgrew it, the generated dispatcher answered `again`, and both `lssvc`
  ("query error") and the BMC service's settled test read nothing - so the boot's event was never written.
  `serve_stats_once` now answers from a 32 KiB heap buffer (`STATUS_REPLY_BYTES`). The BMC service now says, once,
  when the status answers an error instead of waiting silently.
- `lab.py`'s `dev-up` and `dev-reboot` waited for a prompt that is the LAST thing printed; the BMC service's
  binding line lands after the shell's first prompt, and the instance never came up. Both now ask for the boot's
  nudge, as `lab boot` already did: on a guest this command has just started, one empty line after five quiet
  seconds with the shell attached.
- The harness BMC stored a Platform Event Message's eight request bytes straight into the record, so the
  generator read 0x0441 and every field after it one byte early: the driver's own boot event, read back from
  the harness BMC's log, was not recognised as the system's. The record now takes the software ID, the channel
  byte (0 - the system interface), then the rest, as the specification lays the SEL record out and QEMU's
  simulator does; a PEM of another length is refused 0xC7. Self-test checks added.
- `lab.py`'s cold scenario runner waited for readiness as the prompt at the END of the serial log, and a cold
  guest with a BMC printed the BMC service's lines and the network's after the shell's first prompt - the run
  sat out its 1800 s. It now nudges once, as `lab boot` does: after five quiet seconds with the shell attached
  and no prompt at the end, one empty line through the channel's terminal input (a nudge the channel did not
  take is tried again after another quiet stretch). The harness host tests (86) pass.
- The gate first typed `stop '!crash bmc_service'`; the shell hands a tool the rest of its line as typed, quotes
  included, so ServiceManager answered NOTFOUND. The gate types the verb bare, and judges it by its output
  ("CRASHED"), since the restarted service's lines land after the prompt.
- AND THE COLD SCENARIO'S PROMPT WAIT: once ready, the first `prompt` step failed the same way - the watchdog
  service reports ServiceManager's first answered `alive` after the shell's prompt, on its own clock (a line
  P02M0200 added; no cold scenario had run since). `LabGuest.wait_prompt`'s cold path now asks for a buried
  prompt again, but only when the last prompt in the log came AFTER the last thing the runner sent - a key, a
  pointer event or terminal input, all of which now record the log's size (`note_input`) - so an Enter never
  reaches a command that may still be running. Two harness tests (`ColdPromptTest`); the first was run against
  the version without the nudge and fails there.
- THE PROPOSED 120 s BOOT BOUND DID NOT COVER THIS MACHINE'S DEVELOPMENT BOOT. The watchdog gate's BMC case, first
  run: the orderly reboot gave the BMC 120000 ms at the notice (correct), and the next boot - its shell not
  attached within the 30 s boot window, the drivers still binding - was reset by the BMC before the `ipmi`
  driver bound; the boot after it found "stopped at bind, initial countdown 120000 ms, the last reset was its
  own". The bound is armed at the notice, so it has to cover the shutdown sequence, the firmware and the boot
  to the IPMI bind. The case is that the notice arms the CONFIGURED bound, so the gate now sets
  `watchdog.boot-bound-ms` to 600000 for it; whether the shipping default of 120 s should be larger is added to
  the owner's pending watchdog decisions (T/P/D, the default, the order, the notice bound).
- The watchdog gate's other four cases (i6300esb, the reset, TCO, WDAT) passed in that run (2022 s in all).
- AND THE BROKER'S NUDGE (`lab boot`, and now `dev-up`/`dev-reboot`) was spent on the log's "shell attached" -
  after `dev-reboot`'s reset that is the PREVIOUS boot's line, so the one nudge went into a machine still booting
  and the real prompt, buried by the BMC service's and the watchdog service's late lines, was never asked for
  again (`dev.sh reboot` timed out at 300 s in the watchdog gate's BMC case). The broker now nudges only once
  the wait has seen the shell's prompt and it has been buried, and again after each nudge's prompt is buried;
  `shell_attached`, left unused, is removed. Harness host tests: 88 pass.

## Verification (2026-09-29, x86_64)

PASSED:
- `./check.sh --gate ipmi` - 1886 s, eight development boots (ISA KCS alone, SSIF alone, ISA BT alone, PCI KCS
  beside PCI BT, the pair, the harness BMC, malformed records, the orderly reboot).
- `./check.sh --gate qemu-ipmi-admin` - 1645 s, six cold scenarios: the SEL clear declined then approved (journal
  `requested declined requested granted consumed completed`), cancelled by an event (the harness record: Reserve
  SEL at line 47, the event at 48, Clear SEL refused 0xC5 at 54; journal `failed`), hard reset (two boots), power
  down (`shutdown`, no power-button line), soft shutdown (the power-button line after the request, `shutdown`),
  power cycle (the driver's "refused ... with 0xd5", the tool's refusal, journal `failed`, running held 10 s, the
  target `bmc:00000-0055-20@acpi:...`).
- `./check.sh --gate watchdog` - the i6300esb, reset, TCO and WDAT cases passed (the run failed at the new BMC
  case, found above); the BMC case alone, from the same script with cases 1-4 cut (a scratch copy), then PASSED
  after the boot bound, carriage return and broker nudge fixes.
- The kernel test `kernel.declared.the_ich9_smbus_claim_enables_its_host_and_the_release_restores_hostc`:
  `TEST_SELECTION=... ./test.sh --arch x86_64` after `./build.sh --arch x86_64` - 1 passed (28 s). The test
  kernel also builds (`cd src/kernel && TEST=1 TEST_TAGS="" cargo build --tests`).
- Host suites: `ipmi` 27, `power-model` 44, drivers 426, services logic 691, platform 17, system-manifest 28,
  i2c-client 6; `ipmi-harness-bmc.py --self-test`; `harness/harness-test.py` 88.
- `./gen.sh --check`, `./check.sh --gate source-hygiene` (after replacing three `said | grep -q` pipelines under
  pipefail), `--gate grant-vocabulary`; `cargo check` of `service_manager`, `bmc_service` and `bmccheck` with and
  without `development`.

FAILED, NOT THIS MILESTONE'S: `--gate bootstrap-plan` - only `font_catalogue`, as before this work; the
verify-model suite - its model load refuses a kernel test id (`the_global_clock_advances_once_per_period_however_
many_cores_tick`) that a stale aarch64/riscv64 test binary still carries; rebuilt at the end of the job with the
slow-arch builds.

NOT RUN: the aarch64 and riscv64 cross-builds (the end of the job); the out-of-band cross-check (needs the owner's
yes to install `openipmi` and `ipmitool`); the sleep case and the drivers' sleep exchange (P02M0197's).

BLOCKERS AND QUESTIONS FOR THE OWNER: installing Debian's `openipmi` and `ipmitool` for the cross-check; whether the
watchdog's 120 s boot bound should be larger (see the watchdog finding above).

## The sleep case and the gates (2026-10-01)

- The exchange in `ipmi` and `smbus_ich9` and the case A SLEEP WITH BOTH DRIVERS BOUND are P02M0197's (it landed
  second) and recorded in its audit; the gate `ipmi` carries the case: "a sleep with KCS and SSIF bound: both answered
  to idle and S3, SSIF identified again, and the BMC received the watchdog's step in order".
- `LIBER_DEVELOPMENT=1 ./check.sh --gate ipmi` -> PASS (2188 s); `qemu-ipmi-admin` -> PASS (1392 s).
- (2026-10-01) `./build.sh --arch aarch64` and `--arch riscv64` build whole with this milestone's code; the ports' guest runs it names are the owner's long run.

## The out-of-band cross-check, with OpenIPMI's simulator (2026-10-04)

The owner allowed `openipmi` and `ipmitool` on this machine (2026-10-04); both are installed (`ipmi_sim`, ipmitool
1.8.19) and `setup.sh` now lists them beside `swtpm`.

WHAT WAS DONE:
- The gate `ipmi` gained case 7 (`tools/check-ipmi.sh`): OpenIPMI's `ipmi_sim` behind QEMU's `ipmi-bmc-extern` on ISA
  KCS (`irq=0`), its VM serial side on a free loopback TCP port and its LAN side (RMCP+) on a free loopback UDP port, its
  configuration and emulator command file written by the gate (one BMC at 0x20, product 0x0f02, a SEL of 1000 entries,
  users 1 "" as user and 2 "ipmiusr" as administrator). Before the guest boots, `ipmitool -I lanplus` writes the LAN
  configuration over the LAN (static source, 192.168.77.10/24, gateway .1, MAC 52:54:00:aa:bb:cc, VLAN 7) and reads it
  back. In the guest: the boot's event written once, `bmc lan`, `bmc users`, `bmc sel`. OUTSIDE IN: the LAN
  configuration and both users as written and configured; INSIDE OUT: record 1, the boot's event the BMC service wrote
  through KCS, read over the LAN by `ipmitool sel get 1` and `sel list` with the same id, type, generator (0xf041),
  sensor type (OS Boot), sensor number, direction, data (06 ff ff) and timestamp. The simulator is the gate's helper,
  killed with the boot and in the EXIT trap; its log and the outside record are kept.
- Found, and said in the gate's head rather than claimed: `ipmi_sim` does not log a platform event its LAN side takes
  (`ipmitool event 1` answers, and the log stays as it was), so the outside-in direction is the configuration and the
  users, not an event.
- The preflight fails, rather than skips, without `ipmi_sim` or `ipmitool`.

VERIFICATION:
- `apt-get install -y openipmi ipmitool` (with the owner's leave): `ipmi_sim` and ipmitool 1.8.19 installed; `setup.sh`
  lists both. Prototyped by hand first - `ipmi_sim -c lan.conf -f bmc.emu -s <dir> -n`, `ipmitool -I lanplus ... lan
  set`/`lan print`, a development instance behind it reading `bmc lan` = "channel 1: static address 192.168.77.10
  mask 255.255.255.0 gateway 192.168.77.1 mac 52:54:00:aa:bb:cc vlan 7" and `bmc sel` = "0x0001 type 0x02 at 262178
  generator 0xf041 sensor type 0x1f ... data 06 ff ff", which `ipmitool sel list` showed as "1 | Pre-Init |0000262178|
  OS Boot | boot completed".
- `LIBER_DEVELOPMENT=1 ./image.sh --format iso`: ok (the watchdog service's ten-minute default boot bound in it).
- `./check.sh --gate ipmi` (x86_64, 2026-10-04): PASS in 2464 s - cases 1 to 6 as before and "ipmi: oob: ipmi_sim's LAN
  configuration written by ipmitool read through KCS, its users alike, and the boot's event written through KCS read
  over the LAN as the same record, field by field".
- `shfmt -d` on `check-ipmi.sh` and `setup.sh`: clean; `./check.sh --gate milestone-index`: ok with the row ticked.

BLOCKERS: none - the milestone is complete.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0201 (2026-10-08T03:26:12Z):

Continuation review (2026-10-08). Read the complete milestone plan, the previous implementation record, docs/TESTING.md and docs/ARCHITECTURES.md. Existing implementation and past results are being checked against the current tree; prior records are preserved verbatim. No new guest execution or hardware verification has passed in this continuation yet.

Current-tree review confirms the existing KCS/BT/SSIF transports and their bounds, the IPMI driver's watchdog-first
transaction seam (`Driver::ask`/`watchdog_first`), unavailable/recovery handling, child SMBus path, the suspend/resume
exchange, the BMC service and protected administrative path. The recorded completed sleep, cross-build and OpenIPMI
checks close the old record's initial pending items. No new implementation change was necessary for this milestone;
status stays COMPLETE.

Fresh targeted verification (all exited 0):
- `cargo test --offline --manifest-path src/user/libs/driver/ipmi/Cargo.toml`: 27 passed.
- `python3 src/harness/ipmi-harness-bmc.py --self-test`: every encoding checked.

Not rerun here: the long IPMI, administrative and watchdog guest gates or cross-builds; their previous passes are
recorded above and are not claimed as fresh execution. No new blocker found.

Additional fresh targeted checks passed: `cargo test --offline --manifest-path src/user/drivers/core/Cargo.toml
--lib ipmi_bmc` (7 tests, repository bounds, identity, FRU and both executors); `cargo test --offline --manifest-path
src/user/services/logic/Cargo.toml bmc` (1 protected-operation rendering test).

Further deadline review and corrective implementation (2026-10-08T03:55:39Z):
The initial continuation conclusion above is superseded by a reproduced SSIF defect. The plan requires a
five-second transaction deadline, but `ssif::transact` checked it only when a read returned NACK. A successful
late write/read, including multipart transfers, could pass the deadline; the real scoped SMBus client also used
an infinite IPC deadline. A standalone mock returned a valid successful response after a write advanced its
clock to 6000 ms with a 5000 ms transaction limit. The new repository regression failed before the fix because
an already-expired transaction still wrote to the bus.

Actual changes:
- `driver/ipmi/src/ssif.rs`: pass the same absolute millisecond deadline to each bus operation and check it both
  before and after every block write/read. Successful late blocks now return `Failure::Deadline`; multipart
  messages cannot acquire a fresh budget for each block. Existing NACK retry and malformed-message bounds remain.
- `driver/i2c-client/src/lib.rs`: expose the generated client's existing `set_deadline` through `Smbus`.
- `drivers/core/src/ipmi_driver.rs`: `SmbusLink` converts the shared deadline to runtime ticks. A local
  `SmbusTransport` bounds both request-queue backpressure and reply receipt using the runtime's existing deadline
  APIs. It discards stale correlation replies (closing their transferred handles) within the same deadline so a
  later BMC probe can recover after a timed-out operation. Initial functionality/address negotiation is also
  bounded by `NODE_TICKS`. The shared generic IPC transport is unchanged.
- `driver/ipmi/src/tests.rs`: add expired-before-write, late successful write/read, and cumulative multipart
  deadline regression cases. `driver/i2c-client/src/tests.rs`: record deadlines crossing the real generated
  codec for negotiation, both SSIF blocks, and a later transaction with a new deadline.

Fresh checks after this correction (all passed):
- `cargo test --offline --manifest-path src/user/libs/driver/ipmi/Cargo.toml`: 28 tests.
- `cargo test --offline --manifest-path src/user/libs/driver/i2c-client/Cargo.toml`: 7 tests.
- `cargo fmt --manifest-path src/user/libs/driver/ipmi/Cargo.toml -- --check` and the equivalent i2c-client command.
- `git diff --check`.

Pending final verification: peer review, refreshed all-architecture builds and the unchanged SSIF runtime case
from `src/tools/check-ipmi.sh` section 1b. Its exact helper prefix and SSIF case were copied to
`.build/logs/end-of-job/check-ipmi-ssif-deadline.sh`, with only HERE and retained log directory changed; `bash -n`
passed. Planned command: `LIBER_DEVELOPMENT=1 bash .build/logs/end-of-job/check-ipmi-ssif-deadline.sh`.
This is targeted coverage of the real scoped ICH9 connection, capabilities, GUID/product/SMBIOS, multipart
SDR/FRU/SEL reads and service restart event deduplication; it must not be reported as a fresh pass of the full
multi-boot IPMI gate. The current correction has no fresh guest result yet. Milestone reopened pending that
verification; prior completed KCS/BT/admin/watchdog/OpenIPMI runs remain historical evidence.

Final corrected-code verification (2026-10-08T04:08:04Z):
- Independent peer review of the correction found no remaining issue in the scoped changes: correlation framing,
  stale reply capability cleanup, bounded send/receive, initialization, and tick conversion were checked against
  the generated client and runtime implementations.
- `rustfmt --check --edition 2024 src/user/drivers/core/src/ipmi_driver.rs`: passed.
- Root's final `LIBER_DEVELOPMENT=1 ./build.sh --arch all`: PASSED in 487 s for x86_64, aarch64 and riscv64 after
  source freeze, including all user programs and images' staged volumes. Log:
  `.build/logs/end-of-job/continuation-build-all-final.log`.
- `LIBER_DEVELOPMENT=1 bash .build/logs/end-of-job/check-ipmi-ssif-deadline.sh`: PASSED, process exit 0. Every
  unchanged section-1b assertion passed over the real ICH9 SMBus driver: SSIF capabilities; GUID, product and SMBIOS
  agreement; sensor/SDR, FRU and SEL content; one boot event; service restart without writing it again. Main log:
  `.build/logs/end-of-job/continuation-ipmi-ssif.log`; guest and shell logs:
  `.build/logs/end-of-job/ipmi-ssif-deadline/`.

The corrected behavior is covered by the 28 IPMI and 7 client host tests plus current runtime integration and all
architecture builds. The full long multi-interface IPMI/admin/watchdog/OpenIPMI gates were not rerun: their existing
passes remain historical, and this correction changes only SSIF transport/deadline handling. No forced-controller-
stall guest case was added; deadline edge cases are the host regressions and the production IPC uses the existing
runtime deadline primitives. Completion is restored; no implementation or verification blocker remains for P02M0201.
The complete pre-continuation audit prefix from revision d0f54598 was independently verified byte-for-byte preserved.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

AUDITOR'S REVIEW ON P02M0201 (2026-10-15T04:40:15Z):

**Rating: 7/10.** The transports and the driver are complete and careful:
- The kernel's class rows (0x0C/0x07 interfaces 1 and 2, BAR 0) and the ICH9 `(8086, 2930)` row, which writes HOSTC at claim, restores it at release and replays it after S3.
- `smbus_ich9` declares only block write, block read with the device's count, and PEC.
- KCS, BT and SSIF match IPMI 2.0. That covers KCS's IBF/OBF phases and its ABORT within one second; BT's sequence, netfn and command checks with the buffer bound from Get BT Interface Capabilities; and SSIF's single and multi-part writes and reads (block numbering from 0, end at 0xFF), the NACK bound, PEC only when both sides offer it, and one absolute deadline.
- The message layer: the 272-byte bound, the 5 s deadline, three strikes on 0xC3/0xD2/deadline, the 10 s probe, a new identity read again, and the pet served before every transaction.
- Discovery by class or by `_IFT` (SMIC refused), and the SMBIOS type 38 cross-check as a match id only.
- Identity with the GUID or the binding-qualified fallback, held to the 64-byte selector.
- The SDR, SEL and FRU decoders: I checked the SDR offsets against IPMI 2.0 Tables 43-1/2/3/7/8. Also the `power_model::ipmi` linear conversion, the units and the trip points.
- The system events (generator 0x41; OS Boot 0x1F/06; OS Stop 0x20/03), the BMC watchdog encodings and sleep step, the AdminService extension (actions 3/4, two executors, the `bmc` policy row, the payload, count and reservation rules, and the `Operation:` line), and the `bmc` tool and its grants.

The defects are in the BMC service's side of each binding. It sends every call on one long-lived channel through a fresh generated client, with a per-call deadline and no disposal of late replies, so a single expired call leaves that binding answering every later call with the previous call's reply. It also never retries a provider whose `open` was refused, which a restart can hit because the provider admits one consumer. A smaller optional point: a watchdog found running at bind is taken over only after the whole SDR read.

## Findings

1. **Medium, Defect - One BMC-service call that outlives its deadline desynchronizes that binding's request channel for good.**
   - What happens: `src/user/services/core/src/bmc_service.rs:96-98` (`provider`) builds a new `bmc::ipmi_provider::Client::with_deadline(ChannelTransport { chan }, ...)` for every call, and every generated client starts at correlation 0 (`src/user/libs/protocol/bmc-proto/src/generated/liber/bmc/v1.rs:2483`). `ChannelTransport::call` returns the next message on the channel (`src/user/libs/ipc/ipc-client/src/lib.rs:38-51`), and the client only compares that message's correlation with its own (for example `v1.rs:2834` in `system_event`).
   - Nothing clears late replies: the binding's request channel (`Binding.chan`) is never drained, and only the `changes` streams are in the wait set (`bmc_service.rs:438`).
   - The deadlines: 10 s (`QUICK_TICKS`: describe, `sel_info`, chassis, identify, the boot event), 60 s (`PROVIDER_TICKS`: sensors, `sel_page`, fru, lan) and 0.5 s for the shutdown event (`:227`). Meanwhile the driver serves one request at a time and finishes a request after the service has given up on it.
   - The result: once a call expires, its reply arrives later and waits in `chan`, and the next call (again correlation 0) takes it as its own. An answer to the same operation is accepted though stale, an answer to a different operation fails to decode, and every later call reads its predecessor's reply until the binding is withdrawn or the service restarts. The `bmc` tool's view of that BMC and the service's own event writes are wrong from then on.
   - It is reachable in the milestone's own hostile mode on the fixtures. With the BMC silent, QEMU's `ipmi-bmc-extern` answers each transaction 0xC3 after four seconds. The five-second zone poll of the harness BMC's two temperatures (`src/user/drivers/core/src/ipmi_driver.rs:431`) therefore takes about 8 s before the third strike, so a 10 s call queued behind it expires (the follow poll's `sel_info` at `bmc_service.rs:245`, or `bmc chassis`).
   - `ProviderView::sensors` (`ipmi_driver.rs:682`, through `sensor()` and `read_sensor` at `:497`) has no availability short-circuit, unlike `zone_state` (`:407`). So `bmc sensors` against a silent BMC with more than fifteen readable sensors outlives 60 s.
   - The gate cannot show it: the follow probe ends before `mode silent` (`src/tools/check-ipmi.sh:394, 415-418`), and the harness SDR has four readable sensors, so no service call outlives its deadline.
   - The driver already solves the same problem for SSIF: `SmbusTransport`, `ipmi_driver.rs:133-170`, discards replies whose correlation is not the request's. Keeping one client per binding (with a monotonic correlation) and discarding late replies, or draining `chan` before each call, closes it.

2. **Low, Defect - A restarted BMC service never retries an `ipmi` provider whose `open` was refused.**
   - `Service::adopt` (`bmc_service.rs:119-130`) gives up on any failed `open`, and the provider is announced again only if it is republished.
   - The `ipmi` row declares `consumers = 1` (`src/user/services/manifest.toml:3739`), and the catalogue refuses an `open` while the previous instance's connection is still counted (`src/user/services/core/src/device_manager.rs:6351`). That count is refunded only when the driver's loop next waits and finds the closed channel (`ipmi_driver.rs:977` and `:1272`), which is after whatever request it is serving: seconds to minutes against a slow or silent BMC.
   - A replacement whose `open` lands in that window never adopts the BMC: no boot or shutdown events and no `bmc` view, for its whole life. The plan requires "restart transparent and state reconstructible (a restarted instance re-reads every `ipmi` provider)".
   - TypeCService retries exactly this case (`src/user/services/core/src/typec_service.rs:222-255`, `service_logic::provider_retry`). The gate's kill/restart case runs with an idle driver and does not reach the window.

3. **Low, Optional - A watchdog found running at bind is taken over only after identification and the whole SDR read.** In `ipmi_driver.rs`, `identify` (`:1167`) and `read_repository()` (`:1175`) both run before Get Watchdog Timer and the take-over (`:1177-1199`). The plan says such a timer is "petted at once and taken over". With the ten-minute boot bound this is harmless on the fixtures, but the delay before the first pet grows with the repository size and the interface speed. Moving the watchdog block ahead of `read_repository()` would match the requirement at no cost.

## Verification performed

- Read the whole plan, and the last sections of this audit file (the 2026-10-08 SSIF deadline continuation included).
- Kernel. `src/kernel/arch/common/pci/mod.rs:49-52` (the IPMI class rows) and `src/kernel/declared/mod.rs` (the `ich9-smbus` row, `claim_writes` and `replay_claim_writes`). The SMBIOS type 38 attachment as a `smbios:38#n` match id is in `src/kernel/firmware/mod.rs:146-190`.
- Library. `src/user/libs/driver/ipmi/src/` read in full for `lib.rs` (bounds, `Availability`), `kcs.rs`, `bt.rs`, `ssif.rs`, `queue.rs`, `watchdog.rs`, `event.rs`, `chassis.rs`, `identity.rs` and `sel.rs`. From `sdr.rs` and `fru.rs` I read the record layouts, the repository bounds and the field decoding. SDR offsets were checked against IPMI 2.0 Tables 43-1, 43-2, 43-3, 43-7 and 43-8; the Get Device ID, Set Watchdog Timer and Platform Event Message encodings were checked too. `src/user/libs/power/model/src/ipmi.rs` was read in full.
- Drivers:
  - `src/user/drivers/core/src/ipmi_driver.rs` read in full: the interface preparation, the PEC choice, `watchdog_first` and `ask`, availability and `came_back`, the watchdog view and bridge, the sleep step, the provider, power and executor views, `serve`, and the bind order and form detection;
  - `ipmi_bmc.rs` in full: the repository read, `sel_page`, `fru_bytes`, `lan_channels`, `prepare_sel_clear`/`execute_sel_clear` and the chassis rules;
  - `smbus_ich9.rs` in full.
- Services and tools:
  - `src/user/services/core/src/bmc_service.rs` in full;
  - `src/user/libs/ipc/ipc-client/src/lib.rs` and the generated `ipmi_provider` client (corr handling);
  - `src/user/services/logic/src/admin_descriptor.rs` (actions 3/4, the operation table, the `Operation:` line);
  - AdminService's executor table (`admin_service.rs:69-81, 185-196`);
  - PermissionManager's `bmc` admin scope and tool rows (`permission_manager.rs:386, 1218`);
  - the `bmc` tool's head (`src/user/apps/tools/src/bmc.rs`).
- Manifest: the `smbus_ich9`, `ipmi` and `bmc_service` rows, and the catalogue client bound of 64 in both `system-manifest` and DeviceManager.
- Gates read, not run: `src/tools/check-ipmi.sh` (the harness BMC section, the silence and close cases). From the harness BMC, `ipmi-harness-bmc.py`'s sensor list.
- Static, read-only review only. No build, test, gate or QEMU run was performed. The recorded gate passes and the 2026-10-08 continuation results were taken as recorded.
