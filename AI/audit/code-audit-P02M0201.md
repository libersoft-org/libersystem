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
