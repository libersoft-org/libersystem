IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0202 (2026-09-29T13:36:19Z):

## Starting point

Taken in the owner's agreed order after P02M0201. The plan is four parts:
- P02M0202a, the processes and the vocabulary: the `typec-connector` provider kind, the `liber:typec@1` package,
  the `usb-c` power source kind with its `usbc` adapter and validation, TypeCService, the `typec` tool and the
  `typeccheck` probe;
- P02M0202b, UCSI over ACPI: the `ucsi-acpi` driver - the shared mailbox, `_DSM` functions 1 and 2, `Notify(0x80)`,
  the command discipline, the commands, recovery with ten-second bounds;
- P02M0202c, a port controller: the `tcpci` child-binding driver, the Type-C sink state machine and a sink-only
  Power Delivery engine with its safety invariants and timing budget;
- P02M0202d, the verification: the UCSI fixture with a harness PPM in four profiles, the `tcpc-partner` model with
  a Power Delivery source partner, host suites, the sleep case (carried with P02M0197) and a hardware run (the
  owner's board).

What each part consumes was surveyed against the tree before any code (the `acpi-node` channel already carries
`dsm` and `notifications`; the claimed node's own SystemMemory region inside its `_CRS` range is admitted; the
`power-provider` contract and PowerService are the templates for the typec provider and service).

THE OWNER'S DECISIONS THE PLAN ASKS WHEN THE PART STARTS, built with the plan's proposed values and asked in the
job's summary: the UCSI configuration default (accept both swap directions, leave the operation modes as the PPM
reports them); the TCPCI sink selection rule; whether the shipping `typec` tool also receives the operator grant.

## P02M0202a - the processes and the vocabulary (2026-09-29)

- `src/idl/typec.lsidl` (new package `liber:typec@1`): the connector record (capabilities, partner, orientation,
  roles, operation mode, contract, Type-C current, offers <= 11 as records, cable, modes <= 16, DisplayPort, last
  refusal, answering), the provider contract `typec-provider` (an `updates` stream shaped as `power-provider`'s, and
  `request`), the read interface `typec` (connectors, subscribe) and the operator interface `typec-control` (the
  four requests), answers `done`/`refused` with a reason/`indeterminate`. `liber:device@1` gains `typec-connector = 27`,
  `liber:power@1` gains `usb-c = 6`, `liber:security@1` gains `typec` and `typec-control`. gen.sh: package `typec`
  (base only); the `typec-proto` crate; `src/proto` features and dependency; the manifest's sources and library rows.
  The offers are a list of records, not of `u32`: a `Vec<u32>` push in a new shared proto crate is a generic another
  library may import (the lesson of `sel-page.refused`).
- The provider kind wired where `ipmi`'s was: `driver_protocol::provider::TYPEC_CONNECTOR` (+ `TYPEC_NAME`,
  `TYPEC_POWER_NAME`), system-manifest `ProviderKindName::TypecConnector`, DeviceManager's two kind mappings.
- `power-model`: `canon::validate` refuses a `usb-c` record carrying a field the kind lacks (a capacity, charge state,
  runtime, load, temperature, trip, control, or an alarm other than a reported source fault) or a known voltage above
  60 V (`USB_C_MAX_MICROVOLTS`); the new `usbc` adapter publishes only what is measured, unknown while no partner is
  attached, a reading past 60 V as invalid (range), a VBUS fault as a reported source fault. Host tests: the 48 V
  contract reading 50.9 V known, past 60 V invalid and refused when claimed known, a lacking field refused, an
  unplugged connector and a detach with VBUS about 0 V canonical with no known value (6 tests; power-model 50).
- `service_logic::typec_requests` (5 host tests): one request outstanding per connector, fifteen seconds, completed
  once - indeterminate at the deadline or when the provider goes, a late answer dropped.
- `src/user/services/core/src/typec_service.rs` (TypeCService): PowerService's structure, its registry holding
  connectors where PowerService's holds sources (`Payload` never coalescing a partner, role, contract, mode, refusal
  or answering change away), each record checked (numbered 1-16, offers' positions, no contract without a partner),
  the operator's requests through `typec_requests`. Manifest: program (static), service (transparent, reconstructible,
  CATALOGUE `kinds = ["typec-connector"]`, SERVE, CONTROL); ServiceManager `plan_relaunchable`, `service_of_cap`,
  `serve_resolve` and PermissionManager's grants; `CAP_TYPEC`, `CAP_TYPEC_CONTROL`.
- `typec`, the shipping tool (read grant only), with `typec-client` and `typec-client-provider` (trampolines on the
  three architectures), lib.sh wave 3; `typeccheck`, the development probe and the one holder of the operator grant
  (`await`, `source`, `beside-ac`, `data`, `power`, `enter`, `exit`, `list`).

## P02M0202b - UCSI over ACPI (2026-09-29)

- `src/user/libs/driver/ucsi` (new crate, 14 host tests): the layouts by VERSION (1.x's 16-byte messages at 16/32,
  2.x's 256-byte ones at 16/272; a short range or a major outside 1-3 refused), CCI, every command the plan names with
  its fields where the specification puts them and its largest answer, the answers' decoders against hostile lengths
  (capability, connector capability, connector status per version - 2.0's orientation, 2.1's readings when ready -
  cable, alternate modes, PDOs, error status, attention VDO), and the discipline: `execute` (one command, its
  completion checked for length against MESSAGE_IN and the command's answer and for its connector, BUSY waited on,
  ERROR and NOT SUPPORTED completions still acknowledged, the acknowledgement's completion awaited, a connector change
  reported and acknowledged only beside the status that reads it, ten seconds a step) and `reset` (polled through
  function 2 alone). A scripted PPM holds the OPM to it: no second command before an acknowledgement, no refresh
  after a notification.
- `src/user/drivers/core/src/typec_ucsi.rs` (7 host tests): a connector as read (`Port`), its record and its supply,
  the charger/host/device reading of the partner, the contract from the RDO and the offer it names, DisplayPort's pin
  assignment (D, then C, then E) and configuration, and the refusals before any command.
- `src/user/drivers/core/src/ucsi_acpi.rs` (bin `ucsi_acpi`): ready first; the node; `_DSM` function 0's mask for
  functions 1 and 2; VERSION through function 2; notifications subscribed before the reset; reset, notifications,
  capability, each connector's capability, the configuration (swaps accepted both ways where the connector can swap;
  the operation modes left alone; no power command), every connector read; then `typec-connector` and, where a
  connector sinks, `power-source`. Requests refused before any command where the rules say, then the command, then
  the connector change reporting the result within five seconds. Recovery: every connector reported not answering,
  two resets at most, then a driver-reported failure. Waiting answers DeviceManager (`wait_or_answer_until`).
  Manifest: `ucsi_acpi`, platform rows `hid = "PNP0CA0"` and `cid = "PNP0CA0"`, `dma = "none"`.

## P02M0202d, the UCSI half (2026-09-29)

- `src/harness/acpi-fixture.py`: the UCSI device `\_SB.UCSI` (`USBC000`, `_CID` EISAID `PNP0CA0`, present only when
  the memory is written with `--ucsi-version`), its `_CRS` mailbox at 0x6000 past the harness's pages and its own
  `MBOX` region over it laid out for the version, `COPY`, a `_DSM` whose function 1 stages CONTROL and MESSAGE_OUT and
  rings the doorbell and whose function 2 copies the inbound staging area (counted), and line 4's `_E04` (the copy,
  counted, then `Notify(0x80)`); the staging areas and counters in the harness's pages from 0x1000. The acpi gate's
  `_AEI` expectations follow (a third line, four connections), and that gate PASSED again (390 s).
- `src/harness/ucsi-ppm.py` (new; `--self-test` passes): the PPM behind the staging areas in four profiles (v12,
  v21, slow, spoiling), two dual-role connectors (connector 2 offering DisplayPort), a control socket (attach,
  detach, ppm-enter, malformed, silent, silent-next, status, violations) and a command log with a VIOLATION line
  for every break of the discipline.
- `src/tools/check-typec-ucsi.sh` (gate `typec-ucsi`) and `ucsi-ppm` (the self-test as a gate).

VERIFICATION: `./check.sh --gate typec-ucsi` PASSED (966 s, four boots). v12: a charger's contract (offer 2,
9 V, 2000 of 3000 mA) and its four offers in `typec`, its `usb-c` source online with nothing measured, beside the
fixture's ACPI adapter as two sources of two publications; detached, the source offline; a host attached, a data-role
swap done; a power-role swap refused with the PPM's error (0x200); DisplayPort entered under the override, the PPM
asked for pin assignment D (configuration 0x00000805); a PPM gone silent from the next command, connector 2 reported
not answering, the PPM reset and read again. v21: the contract with VBUS measured at 9020000 uV and -2000000 uA;
SET_PDR refused at configuration and said; DisplayPort refused before any command (no SET_NEW_CAM in the PPM's log),
then entered by the PPM and reported; a status past its size and one naming connector 7 refused, each followed by a
reset. slow: ready in 11 ticks, the connectors published after 22670 ms. spoiling: attach and detach served. The PPM
saw no break of the discipline in any boot. Its function-1 window was first 0.1 s and caught the driver's genuine
round trip through the ACPI service as a violation - so the check was seen to fire - and is now two seconds.
Two gate-script defects found on the way: TypeCService said it was online only on its bootstrap channel (it now also
prints it), and the probe's `list` is information, not a PASS.

## P02M0202c - a port controller and the sink-only Power Delivery engine (2026-09-29)

WHAT WAS IMPLEMENTED

- `src/user/libs/driver/usb-pd/` (new crate, manifest source row `usb-pd`; host suite `host.usb-pd`, release row
  added). Pure leaves, no runtime:
  - `message.rs`: the codec - header, object count checked against the bytes, reserved types refused per class,
    extended messages refused, `Revision`, `Control`, `Data`, `decode`, `encode`.
  - `pdo.rs`: `Offer`, `SinkPdo` (`allows`, `encode`), `Sink`, `Selection` (`request()`), `select` - the proposed
    selection rule (most power among Fixed offers inside one described sink PDO, current limited to that PDO's, ties
    to the lower voltage; below `op-sink-microwatt` position 1 at 5 V with capability mismatch).
  - `timer.rs`: the timer table at the 10 ms tick (`ticks() = ceil(min/10)+1`), `late()` true for SenderResponseTimer
    alone, `HARD_RESET_COUNT = 2`.
  - `engine.rs`: the Type-C sink and the sink policy engine as `Engine::event(Event) -> Vec<Action>`; `Report` for
    the record and the supply. Actions: `SinkPath`, `Transmit`, `HardReset`, `Alarms`, `AutoDischarge`, `Arm`,
    `Cancel`, `CcOpen`, `Receive`, `Publish`.
  - `tcpci.rs`: the TCPCI 2.0 register map, alert bits, commands, `cc`, `flipped`, `vbus_millivolts`,
    `alarm_threshold`, `received`, `transmit_buffer`, `header_info`.
- `src/user/drivers/core/src/typec_tcpci.rs` (+ tests): `describe(block, acpi)` reads the `usb-c-connector` child
  from the controller's property block - a tree's raw cells and strings, or `_DSD`'s hierarchical data node in the
  node channel's encoding - into `Described::{Sink, TypeCOnly(why), Refused(role)}`; `record` and `supply` from the
  engine's `Report`; `partner` (with Power Delivery by the first offer's USB Communications Capable bit, else a
  charger).
- `src/user/drivers/core/src/tcpci_driver.rs` (bin `tcpci`; manifest program row, platform match
  `compatible = "tcpci"`, provides `typec-connector` and `power-source`, `dma = "none"`; the system-manifest DMA
  table beside `i2c_hid`). Bring-up: connections (I2C, GPIO line), description (a source or dual-role connector
  refused before READY), `ScopedBus` (plain I2C required), the alert line checked level-low, READY, identity,
  initialisation awaited at most one second, DEVICE_CAPABILITIES_1 (a controller that cannot switch the sink path
  refused; VBUS measurement bit 10), registers, the port as found fed to `Event::Bound`. Loop:
  `common::wait_providers_until` (new, bounded) over the alert, the consumers and the earliest timer; each alert
  read whole, cleared, then fed in order (CC, power, transmission, hard reset, message, fault, alarms); requests
  refused `transport-does-not`; a failed transfer waits half a second for a lost controller's stop before failing.
  Lines the gate reads: attached/detached, contract (said only when it changes), hard reset sent (n of 2), VBUS back
  after the hard reset with the count, alarms, malformed messages.
- `src/user/libs/acpi/model/src/node.rs`: `PRP0001` and `compatibles` - a node whose `_HID` or a `_CID` is `PRP0001`
  is matched by its `_DSD`'s `compatible` strings (MATCH_ID_COMPATIBLE), as the ABI's comment always said and nothing
  did. Test `a_prp0001_node_is_matched_by_its_dsd_compatible_strings`, seen failing with the matching removed.
- `src/user/drivers/core/src/common.rs`: `wait_providers_until` (and `wait_providers` through the same inner loop).
- `src/user/services/core/src/typeccheck.rs`: `steady N SECONDS` and `cycle`; grants `Device` and `DevicePolicy`
  added to the probe's row in `permission_manager.rs`.
- `src/harness/tcpc_partner.py` (new): `Tcpc` (the register file, the receive queue, GoodCRC only with RECEIVE_DETECT,
  VBUS measurement and both alarms), `Source` (the source policy engine with its timers, profiles `charger31`,
  `charger20`, `weak`, `typec15`, `typec30`, scripts, violations, response measurement, `negotiate N`), `Scheduler`.
  `vhost-i2c-gpio.py`: `--tcpc`, `--tcpc-log`, `--stretch`, the `tcpc ...` control commands, the scheduler in the
  select loop, line 5 named `tcpc-alert`, and `TcpcTest` (6 tests) in its host suite.
- `acpi-fixture.py --tcpc-out` (a `PRP0001` device `TCPC` below the virtio-i2c companion, `compatible` `tcpci`,
  `I2cSerialBusV2` 0x52, `GpioInt` line 5 level active-low, data node `CON0` as `connector`), `fdt_edit.py
  tcpc-fixture` (the `tcpc@52` node with its `connector` child), and `qemu-run.sh` `I2C_FIXTURE=tcpc` loading each.
- `src/tools/check-typec-tcpci.sh` (gate `typec-tcpci`; `check.sh`, catalog, boots-a-guest list, release row).

DECISIONS

- THE PROPOSED DEFAULTS stay the owner's to confirm: the selection rule as the plan states it.
- A BOARD WITHOUT A DESCRIPTION: the engine's `runs_pd` is false - no message received or sent, the alarms at vSafe5V;
  an alarm cannot be answered with a hard reset, so it opens CC (error recovery). A bind that finds VBUS with the path
  ON keeps it at vSafe5V's alarms; with the path OFF (the machine runs from something else) it goes through error
  recovery so the source restarts at vSafe5V - the plan says only "sinks at the Type-C current only".
- `Receive(bool)`: RECEIVE_DETECT is on only while Attached with Power Delivery running (Linux enables PD reception at
  attach too); a message GoodCRC'd before Attached would otherwise be lost and the source would hard-reset.
- A bind that found VBUS with the path OFF enables it at the first PS_RDY, once the alarms bound the new contract -
  the plan says the bind leaves the path as found; turning it on only after a contract the engine saw made keeps
  invariant 5.
- A FAULT ENDS when a contract is made anew (PS_RDY).
- A message that expected no answer and went unacknowledged sends Soft_Reset (the specification's protocol error).
- ON STOP the port is left as it is, the sink path among it: a lost controller cannot be reached anyway, and the plan's
  rebind case needs the path found on. Said in the driver's stop line.
- The partner's check "the sink path on when the hard reset takes VBUS down" is a violation under KVM and a note on the
  emulated ports, since tPSHardReset is not among the two stretched timers.
- THE REGISTER MAP was checked against Linux's `tcpci.h` where it names bits: POWER_CONTROL bit 5 disables the alarms
  and bit 6 the VBUS monitor (first written the other way round, corrected before any run). DEVICE_CAPABILITIES_1 bit 2
  (sink VBUS) and bit 10 (VBUS measurement and alarms) are from the TCPCI 2.0 table as I recall it - no copy of the
  specification is in the tree; a real controller run is the owner's.

VERIFICATION (targeted)

- `cargo test --manifest-path src/user/libs/driver/usb-pd/Cargo.toml`: 23 passed. Watched failing first: the test
  data for the 5 V fallback was wrong (5 V at 3 A is 15 W, not below it) and was corrected; then three deliberate
  engine breaks in a scratch copy - the hard reset sent before the sink path is off, the transition window narrowed at
  Accept, every fixed offer admitted - each caught (1, 1 and 7 tests failing respectively).
- `cargo test --manifest-path src/user/libs/acpi/model/Cargo.toml`: 8 passed; the new test failed with the matching
  disabled.
- `cargo test --manifest-path src/user/drivers/core/Cargo.toml --lib typec_tcpci`: 4 passed.
- `cargo test --manifest-path src/tools/system-manifest/Cargo.toml`: 28 passed.
- `./build.sh --part user` (x86_64): ok, no warning.
- Gates: `i2c-backend` (17 tests with the TCPC suite), `firmware-fixtures`, `grant-vocabulary` (52), `source-hygiene`
  - all passed; hygiene first failed on two `| head -1` pipes in `check-typec-ucsi.sh`, now `grep -m1`.

FOUND BY THE GATE (P02M0202c/d, 2026-09-29)

- Run 1 failed at `vbus-stays-off` on the gate's own defect: `probe_judged` ran inside `$(...)`, whose subshell cannot
  `wait` for the probe the gate started, so the log was judged before the probe finished (the probe itself passed,
  detached after 1580 ms). `probe_judged` now leaves its PASS line in `judged`. The probe's await line also lost a
  double space when the case takes no argument.
- Run 2 failed at `cycle`: after the rebind the driver sent Soft_Reset and only then made its two offers to
  DeviceManager; the source's capabilities waited behind them and its SenderResponseTimer (27 ms) ran out, so it sent
  a hard reset and VBUS dropped. A DEFECT OF THE DRIVER, which the partner's record caught (`marked-hard-resets 1`,
  `vbus-low 0`): the offers are now made before the engine is told of the port as found.
- Run 2 also showed, at the detach of the `no-caps` case, the model re-raising its low alarm on every register
  change while VBUS stayed below the threshold, and the driver saying alarms the engine then ignored. The model now
  raises an alarm as VBUS crosses a threshold, once; the driver says an alarm only to an attached port.
- AND A REAL-WORLD HAZARD the same trace pointed at: on an unplug VBUS decays through vSafe5V's low threshold
  (4.25 V) while CC is already open and tPDDebounce runs - above a controller's VBUS-present threshold - so the engine
  would have called every unplug a fault and sent a hard reset. An alarm while CC is open is now the detach (the sink
  path off first, no hard reset, no fault): `an_alarm_while_cc_is_open_is_the_detach_and_no_fault`, seen failing with
  the rule removed. usb-pd: 24 tests.
- Run 3 failed at `overshoot` on the gate's own check: the partner sent PS_RDY 20 ms into the overshoot and the sink's
  hard reset arrived 0.5 ms after it, so the partner's state was `ready`, not `transition`. The required order held
  (the sink path off at 12 ms, before the hard reset; source-fault reported). The gate now checks the 18 V alarm and
  exactly one more hard reset from the sink. The same trace showed the driver printing the alarm (and reading VBUS
  for it) BEFORE feeding the engine, and printing "hard reset sent" before sending it: a console line costs
  milliseconds on the path an invariant is timed on. The driver now acts, then says - the alarm, a received hard
  reset and a sent one alike.
- Runs 2 and 4 failed at `cycle` for a reason the offers' order did not explain, so it was MEASURED on a manual
  instance with the partner counting register transfers: a normal response is 5 transfers and 4-7 ms (200
  negotiations: p50 4.219 ms, p99 7.378 ms, max 7.698 ms, no timeout), and after the rebind the sink DID answer in
  2.2 ms - but its Request carried MessageID 0, the ID of its own Soft_Reset, which the source had stored and
  discarded again as a retry, as the specification's receiver must. A DEFECT OF THE ENGINE the independent partner
  caught and the host suite did not: `wait_caps` reset both message counters, which is right after an attach or a hard
  reset and wrong when the Accept of the sink's own Soft_Reset arrives or after a Reject or Wait with no contract.
  The counters now reset only at an attach, a hard reset and a Soft_Reset sent (`soft_reset`) or received; test
  `message_ids_count_on_from_a_soft_reset_and_never_repeat_one_the_source_stored`, seen failing with the old reset
  restored. usb-pd: 25 tests. The partner now logs every message each way and the transfers each response took.

VERIFICATION: `./check.sh --gate typec-tcpci` PASSED (607 s, one development instance on x86_64 under KVM, the
partner's stretch 1), its fifth run; the four before failed as above and each failure was fixed at its cause, none by
relaxing a bound. Every case the plan names, with what was measured:
- a revision 3.1 charger: the contract at offer 4, 15 V, 2000 mA of 3000 mA offered; the one request the partner saw
  was that one (no 9, 12, 20 V or PPS request); no alarm through the 5 V to 15 V transition; the tool showing the
  port controller, the contract and all six offers;
- PR_Swap, DR_Swap, VCONN_Swap and Discover_Identity answered Not_Supported and the contract kept; Get_Sink_Cap
  answered `0001912c 0004b0c8` (the board's two PDOs);
- the three malformed messages not believed (the driver named Length, Reserved and Extended) with no reset of any
  kind following;
- Wait then Accept (the request repeated after tSinkRequest); Reject with the contract kept; less power (a request at
  5 V) and back (15 V) with no alarm;
- VBUS driven to 17.5 V in the 15 V contract and a 5 V to 15 V transition overshooting to 18 V: each the sink path
  off before the hard reset (the partner checks it at the hard reset), `source-fault` reported (voltage known at
  17.5 and 18 V), the contract made anew;
- a hard reset the partner sent, VBUS to 0 and back: attached throughout (`steady`, 5 s), no detach line, the
  contract anew;
- no answer to a Request: the hard reset 47.3 ms after its GoodCRC (not before 24), "hard resets so far: 1" when VBUS
  came back - the count kept through the reset - and the contract anew;
- Accept without PS_RDY: the hard reset 467.9 ms after Accept reached the sink (not before 450);
- a hard reset after which VBUS stays off: detached 1560 ms after it (not before 1275);
- a detach mid-negotiation: detached, the supply offline; no violation;
- a revision 2.0 charger: 15 V, said as 2.0, its PR_Swap answered Reject;
- a charger below the operating power: offer 1, 5 V at 2000 mA, capability mismatch;
- non-PD chargers at 1.5 A and 3 A: both hard resets sent, then the Type-C current standing (`power by Type-C current,
  1.5 A` / `3 A`);
- a Power Delivery source sending no Source_Capabilities: the two hard resets 324.0 and 320.6 ms after the sink began
  receiving (not before 310), then the Type-C current;
- the virtio-i2c binding disabled and enabled during the 15 V contract: `tcpci` stopped as a lost dependency and
  bound again, "bound with VBUS present and the sink path on - no contract is trusted; it is negotiated anew through
  Soft_Reset", the contract made anew with no alarm; the partner recorded no hard reset, no sink path off, one
  Soft_Reset and VBUS never below 15000 mV;
- THE TIMING RUN: 200 negotiations, p50 4.499 ms, p99 7.879 ms, max 8.190 ms, no SenderResponseTimer run out, 5 to 6
  register transfers each - inside the budget (at most 15 ms at p99, never 24 ms);
- the partner's violation list empty at every check.
Not run here: the ports' tree description (aarch64 and riscv64, their emulated sweep at the job's end); the sleep case
(P02M0197); a real port controller.

RE-RUN AFTER THE TCPCI CHANGES: `typeccheck` gained grants and two cases and its await line changed, and
`common::wait_providers` - which `ucsi_acpi` waits in - was rebuilt around a bounded inner loop, so
`./check.sh --gate typec-ucsi` was run again: PASSED (970 s, four boots, every case as before, no break of the PPM's
discipline). Also re-run and passed after the last change: `usb-pd` (25), `i2c-backend` (17), `firmware-fixtures`,
`grant-vocabulary`, `source-hygiene`.

WHAT REMAINS OPEN IN P02M0202 (none of it passed, none claimed): the owner's confirmation of the proposed UCSI
configuration, TCPCI selection rule and the tool's operator grant; the sleep exchange in both drivers and the sleep
case (P02M0197 carries both, recorded there); the TCPCI tree description on aarch64 and riscv64 (their emulated sweep
at the job's end - `fdt_edit.py tcpc-fixture` and `qemu-run.sh`'s `I2C_FIXTURE=tcpc` are built and self-tested, not
booted); the three cross-builds; a hardware run.

## The sleep cases and the gates (2026-10-01)

- The exchange in `ucsi_acpi` and `tcpci` and both fixtures' sleep cases are P02M0197's (it landed second) and recorded
  in its audit; the gates carry them.
- `LIBER_DEVELOPMENT=1 ./check.sh --gate typec-ucsi` -> PASS (1245 s): suspend to idle and S3 with no command while
  asleep, the notifications enabled and both connectors read after each, the detach made in S3 reported after it.
- `typec-tcpci` -> PASS: the suspend refused with a contract standing and the connector named, a charger attached during
  a sleep finding the sink path off and contracted after it, and every earlier case.
- (2026-10-01) `./build.sh --arch aarch64` and `--arch riscv64` build whole with this milestone's code; the ports' guest runs it names are the owner's long run.


IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0202 (2026-10-08T03:26:12Z):

Continuation review (2026-10-08). Read the complete milestone plan, the previous implementation record, docs/TESTING.md and docs/ARCHITECTURES.md. Existing implementation and past results are being checked against the current tree; prior records are preserved verbatim. No new guest execution or hardware verification has passed in this continuation yet.

Actual continuation change: `src/user/libs/driver/usb-pd/src/pdo.rs`, `select`, now requires a Fixed 5 V PDO at
position 1 before using the capability-mismatch fallback. The previous pattern accepted any Fixed voltage if a
sink PDO admitted it. Added `a_capability_mismatch_falls_back_only_to_a_fixed_5v_first_offer` in the existing host
suite: a 15 V first offer is refused despite being board-supported; an empty list is refused; a valid 5 V fallback
retains the sink current bound and mismatch bit. The targeted test was run on the old implementation first and
FAILED (15 V was selected); after the fix the complete suite PASSED, 26 tests.

Reviewed the production `tcpci_driver`/`typec_tcpci` and `ucsi_acpi`/`typec_ucsi` paths, TypeCService's records and
request handling, the independent TCPC partner fixture, sleep exchange and the gate's port timer scaling. The
existing aarch64 guest attempt's logs show termination by the old runner before kernel boot, not a TCPCI verdict.
The plan header is corrected to reflect the confirmed owner policy and completed sleep/cross-build work; the
required emulated-port and hardware criteria remain open.

Resolved the old register-bit uncertainty from a primary source: USB-IF TCPCI revision 2.0 version 1.0, Table 4-28,
pages 64–65, https://www.usb.org/sites/default/files/documents/usb-port_controller_specification_rev2.0_v1.0_0.pdf .
It agrees with the existing `CAPABILITY_SINK_VBUS` bit 2 and `CAPABILITY_VBUS_MEASUREMENT` bit 10. No constants changed
and this is not hardware verification.

Fresh checks passed so far:
- `cargo test --offline --manifest-path src/user/libs/driver/usb-pd/Cargo.toml`: 26 passed after the fix.
- `cargo test --offline --manifest-path src/user/libs/driver/ucsi/Cargo.toml`: 14 passed.
- `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml typec_requests`: 5 passed.
- `python3 src/harness/vhost-i2c-gpio.py --self-test`: 18 passed, including the independent TCPC partner.
- `python3 src/harness/ucsi-ppm.py --self-test`: every answer checked.

Still unperformed in this continuation: guest TCPCI runs (including both device-tree ports), fresh cross-builds and
the real UCSI laptop/port-controller run. Hardware remains externally blocked until an owner-provided board is
available; the fixtures do not discharge that criterion. Long guest runs are deferred until the coordinated end
of the whole job, as requested.

Additional fresh verification: `cargo test --offline --manifest-path src/user/drivers/core/Cargo.toml --lib typec`
passed all 11 driver adapter/description tests; `cargo fmt --manifest-path src/user/libs/driver/usb-pd/Cargo.toml --
--check` and `git diff --check` exited 0.

The canonical USB-C power adapter also passed: `cargo test --offline --manifest-path
src/user/libs/power/model/Cargo.toml usbc` (6 tests). The parallel platform-device continuation registered the two
required guest variants in `check.sh` and the verification model: `typec-tcpci-aarch64` and `typec-tcpci-riscv64`,
each invokes the existing gate with `--arch`; executing them remains part of final verification.

Final verification commands queued after the shared builds settle, in this order (not yet executed):
1. `LIBER_DEVELOPMENT=1 ./check.sh --gate typec-tcpci` — x86_64 KVM response budget and all functional cases.
2. `LIBER_DEVELOPMENT=1 ./check.sh --gate typec-tcpci-aarch64` — the same cases over the device tree, source timer
   stretch 100, timing reported without applying the KVM response budget.
3. `LIBER_DEVELOPMENT=1 ./check.sh --gate typec-tcpci-riscv64` — the equivalent device-tree port run.
Each invocation will receive a fresh `RUN_STATUS_FILE`; terminal exit records, not quiet logs, decide the result.

Hardware limitation: the plan's final criterion asks for an owner-provided board and its final note specifies a real
UCSI laptop and a real port controller. This workspace/task provides no such machine or access path. No live host
Type-C power operation was attempted. An eventual hardware record can name the actual laptop/controller and the
revision run and retain its observed behavior; no additional hardware acceptance criteria are invented here.
Current simulated results and the specification check do not establish any physical-board observation. The
hardware checkbox and milestone completion therefore remain open even if both port fixtures pass.

Final-build gate attempt (2026-10-08): all-architecture source-frozen build passed (487 s; root log
`.build/logs/end-of-job/continuation-build-all-final.log`). The first fresh
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-x86.status ./check.sh --gate typec-tcpci`
FAILED, exit 1 after 297 s, at less-power renegotiation. Charger31 15 V/2 A, swaps, malformed messages, Wait and
Reject cases passed. The partner accepted a correct 5 V request, then the I2C and GPIO requests stopped completing;
the driver reported `write 0x1c: Controller` and its restart could not scope the alert line. This is not an expected
case or a pass. Full initial evidence is preserved in `.build/logs/end-of-job/typec-tcpci-x86-failed-less/` and
`continuation-typec-tcpci-x86.log`/`.status`. Ports have not run yet.

Diagnosis in progress: an exact extracted prefix of the gate through the less-power case is running with
`TCPC_TRACE=1` from `.build/logs/end-of-job/check-typec-tcpci-debug.sh`. Only HERE and log destination changed.
The backend's existing SIGUSR1 stack diagnostic will identify a live stall if it recurs. This subset cannot count
as full-gate or response-timing evidence; acceptance thresholds are unchanged.

Cross-page backend diagnosis and correction (2026-10-08): diagnostic subset reproduced the same failure. The
trace shows endless IOTLB misses at 0x6000, each immediately answered with a successful 0x6000+0x1000 update.
The owned backend's SIGUSR1 stack was `recv_message -> Device.ask -> Space.view -> ring -> Vring.put_used`.
The actual I2C ring uses base 0x5808; slot 254's 8-byte completion is at 0x5FFC and crosses the page boundary.
`Iotlb.translate` refuses a range spanning two entries, so the old combined access repeatedly asked for the
already-mapped second page and never returned to either controller or the partner timers. Diagnostic logs:
`.build/logs/end-of-job/typec-tcpci-debug/`; subset exited 1, not accepted as timing evidence.

The parallel platform-device implementer corrected only `Vring.put_used` in `src/harness/vhost-i2c-gpio.py`:
write head and length as two aligned 4-byte stores, then publish the used index last. Its new host regression
matches the actual ring layout with adjacent IOVAs mapped to noncontiguous host pages and checks ordering of the
published index. On the old code: 1 failure / 19 tests (other 18 passed),
`.build/logs/end-of-job/continuation-vhost-cross-page-before.log`. After the correction:
`python3 src/harness/vhost-i2c-gpio.py --self-test` passed all 19, with syntax and diff checks passing; evidence
`continuation-vhost-cross-page-after.log`. Reviewed the narrow fix and regression independently. This is a required
fixture correction exposed by exercising the specified TCPCI gate; no guest driver or timing threshold changed.

Full non-trace x86 gate restarted with fresh result path `continuation-typec-tcpci-x86-retry.status` and log
`continuation-typec-tcpci-x86-retry.log`. Runtime/Rust sources remain those in the final all-architecture build;
only the Python backend changed, so no additional all-architecture build is required for that correction.

Corrected full x86_64 TCPCI run PASSED (2026-10-08), terminal exit 0 after 496 s:
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-x86-retry.status ./check.sh --gate typec-tcpci`.
Every original case passed, including the formerly failing 15 V -> 5 V -> 15 V renegotiation, malformed messages,
Wait/Reject, both VBUS alarm paths, reset timer bounds/recovery, weak/non-PD/silent sources, controller dependency
withdrawal and rebind, refusal to sleep with an active contract, and charger attachment while asleep with the sink
path disabled until resume. Independent source model reported no protocol/power violations. The 200-negotiation
KVM timing result was p50 3.939 ms, p99 6.543 ms, maximum 6.936 ms, zero timeouts, 5–6 register transfers: passes
unchanged p99 <= 15 ms and no response >= 24 ms limits. Timing ran without competing builds/guests or tracing.
Root's post-fixture source-hygiene also passed in 93 s before this guest reached its test cases.

Evidence: `.build/logs/end-of-job/continuation-typec-tcpci-x86-retry.log` and `.status`;
`.build/logs/typec-tcpci/` detailed guest/probe/partner logs. The earlier failed result remains preserved separately.
The aarch64 device-tree gate is now running with a fresh `continuation-typec-tcpci-aarch64.status` result path.
Both port criteria and the physical-board criterion remain open until their actual results exist.

First aarch64 run FAILED, native exit 1 after 457 s:
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-aarch64.status ./check.sh --gate typec-tcpci-aarch64`.
TCPCI successfully bound through the device tree and published its connector/power source after an initial timed-out
initialization retry. TypeCService nevertheless listed zero connectors. Serial lines 384–386 identify the sequence:
opening the update stream timed out; DeviceManager refused another consumer while the previous closed connection
still counted against the limit; TypeCService abandoned the provider. Evidence preserved before any rerun in
`.build/logs/end-of-job/typec-tcpci-aarch64-failed-adoption/` plus the gate's `.log` and `.status`.

Actual service correction in `src/user/services/core/src/typec_service.rs`: catalogue-open failures now enter the
same existing seven-attempt bounded ProviderRetry as failed update-stream opens. The update-stream deadline begins
at the actual call, retry delay at the actual failure, and registry snapshot budget at actual provider admission.
Previously all three used a clock captured before the potentially slow catalogue call. No timeout was increased.
Root independently confirmed the lifecycle diagnosis and corrected the same PowerService path within P02M0181.

The plan requires bounded subscriber behavior. TypeCService still had blocking public enumeration, subscribe-open
and connection replies, despite its bounded live-stream registry. These now use the established PowerService
nonblocking send/close-on-full behavior; failed stream handoff closes both newly-created endpoints and removes its
registry subscription, failed connection handoff releases both endpoints. Subscription refusals also return send
failure so an unread refusal queue closes rather than silently retaining the client.

Added the development `typeccheck stalled` regression in `typeccheck.rs`: fill enumeration replies without reading,
then subscription replies without reading; each connection must close within the probe's existing WAIT bound,
and another reader must still enumerate. The TCPCI gate invokes it before attaching a charger. The gate now first
awaits the initial detached connector snapshot before asserting exactly one connector: service online is not a
barrier for asynchronous provider adoption. All original case/timing assertions remain unchanged.

Fast checks: `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml provider_retry` passed all 3
existing bounded-delay/exhaustion/withdrawal tests; `rustfmt --check --edition 2024` on both changed Rust files,
`bash -n src/tools/check-typec-tcpci.sh`, and `git diff --check` passed. New backpressure probe and corrected ARM
adoption are not yet guest-verified. Refreshed service builds and guest verification are pending; no fresh full-gate
success is attributed to these newest service edits yet.

Independent root review accepted the TypeC retry/clock/client cleanup and regression probe changes, and confirmed
that waiting for the initial snapshot is the proper asynchronous-adoption barrier, not a relaxed completion
criterion. The 496 s x86 pass predates these service edits; its detailed logs were preserved separately at
`.build/logs/end-of-job/typec-tcpci-x86-passed-before-service-fix/` before the current-code rerun.

Further required deadline isolation (2026-10-08): review found that synchronous catalogue/update-stream opens
still blocked the TypeC service loop while it should enforce the operator's fifteen-second bound and the shared
registry's slow-subscriber closure. Replaced `TypeC::adopt` with two nonblocking opening stages using the shared pure
`service_logic::provider_open::Opens` state machine implemented by the parallel PowerService continuation.
Catalogue requests and stream-open requests use `try_send`; replies are consumed from the ordinary wait set.
Each stage keeps the existing OPEN_TICKS bound. Active providers, pending opens and retries share the eight-provider
admission limit, with duplicate publications refused before allocating a new slot. Timeouts enter the existing
finite retry policy; a withdrawal cancels pending opens and releases their channels; a closed catalogue cancels
pending work. Unique correlations and strict reply/channel checks prevent old replies from adopting a replacement,
and unused/late transferred capabilities are closed. Only bootstrap subscription/online signaling remain synchronous,
before entering the service loop. Public-reader backpressure fixes and new probe remain in place.

Fresh checks after this asynchronous correction:
- `(cd src/user/services/core && cargo check --bin typec_service --bin typeccheck --features development)`: passed.
- `cargo test --offline --manifest-path src/user/services/logic/Cargo.toml provider_open`: all 5 helper tests passed.
- `rustfmt --check --edition 2024 src/user/services/core/src/typec_service.rs` and `git diff --check`: passed.
Runtime wrapper peer review, final updated images and current-code TCPCI guest checks are still pending.

Independent peer review of the asynchronous TypeC wrapper completed: generated catalogue reply layout, strict
byte/capability consumption, unique correlations, withdrawal cancellation, the eight-provider reservation bound,
wait-set size, stage deadlines and public subscribe failure cleanup were checked without finding a defect.
Closed catalogues now also release their original connection; CONNECT/HEARTBEAT early returns dispose of unexpected
incoming capabilities, matching the ordinary request cleanup. Final targeted checks repeated after these small
cleanup changes: TypeC/typeccheck target cargo check passed; provider_open all 6 tests and provider_retry all 4 tests
passed; Rust formatting and git diff whitespace check passed. Source is ready for refreshed guest verification;
these checks do not substitute for the pending runtime checks or physical hardware criterion.

Latest final-source cross-build: `LIBER_DEVELOPMENT=1 ./build.sh --arch all` PASS (1277 s; `.build/logs/end-of-job/continuation-build-all-async.log`), SDK, libraries, userspace, kernel, loader, packages and volumes for x86_64, aarch64 and riscv64. This supersedes the earlier build as compiled-source evidence and includes the asynchronous provider/policy IO corrections plus the final additive fixture operation. Current service-logic tests also PASS (955, one pre-existing ignored; `continuation-service-logic-async.log`); source-hygiene/model/model-tests PASS (208 s) and generation drift check PASS (19 s). Runtime gates and milestone-specific completion limitations remain separately recorded.

Final all-target source build after the asynchronous changes PASSED in 1277 s:
`LIBER_DEVELOPMENT=1 ./build.sh --arch all`, log `.build/logs/end-of-job/continuation-build-all-async.log`.
The first current-service ARM gate then FAILED natively in 864 s (exit 1), log
`.build/logs/end-of-job/continuation-typec-tcpci-aarch64-final.log` and `.status`. Importantly the previously failing
provider adoption now passed, as did the new unread-client regression, charger negotiation, swaps, malformed
messages, Wait/Reject, changed offers, and the sustained VBUS alarm. The failure was the overshoot oracle, not its
required safe recovery: the independent partner recorded an 18 V high alarm at 657.5150 s, returned to 15 V at
657.5354 s, and recorded sink-off at 657.5401 s followed by hard reset at 657.5562 s. The guest correctly consumed
the latched high alert, but its current VBUS sample was already 15 V; the gate demanded the literal guest log
`a VBUS alarm (high) at 18000 mV` and timed out. TCPCI's voltage register is a current measurement, not the alarm's
latched voltage. The plan requires the overshoot, source-fault and sink-off-before-reset/recovery, all still required.

Preserved complete failed evidence at `.build/logs/end-of-job/typec-tcpci-aarch64-failed-overshoot/` and the exact
original script at `check-typec-tcpci-before-alarm-oracle.sh` beside it. A read-only reproduction over those logs
confirmed the original literal fails while independent 18 V alarm -> sink off -> hard reset -> new contract and
recorded guest high alarm all pass. Corrected only the gate assertion: baseline and require a new guest high alarm,
and independently baseline/require a new exact 18 V alarm in the partner's log. Source-fault, one-reset count,
sink-path/model violations and new contract checks remain untouched; no timer was stretched further and no driver
behavior changed. `bash -n src/tools/check-typec-tcpci.sh` and `git diff --check` passed.

The concurrent original RISC-V gate was intentionally stopped through its private `dev.sh down` before any test
case, to avoid running the known incorrect oracle. It exited 1 after 568 s; its generic 'no shell prompt within
4000 s' text is a consequence of that intentional shutdown, not evidence of a 4000 s boot timeout. Logs retained at
`.build/logs/end-of-job/typec-tcpci-riscv64-interrupted-old-oracle/` and the original final.log/.status. Both full ports
will be restarted against the corrected oracle; no passing port result is claimed yet.

Both corrected full port runs started from clean instances using the corrected assertion above. Peer review accepted
the oracle correction and identified one shfmt spacing difference in its arithmetic expression. Preserved the exact
running script at `.build/logs/end-of-job/check-typec-tcpci-running-oracle.sh`, then changed only that whitespace via
temporary file plus atomic rename: each already-running Bash retains its original complete script inode. Final
`shfmt -d src/tools/check-typec-tcpci.sh`, `bash -n` and `git diff --check` passed. The running test logic and final
source are identical; the harmless formatting difference does not require restarting either guest.

Final check after the narrowly corrected overshoot oracle: `./check.sh --gate source-hygiene --gate verify-model` PASS (118 s; `.build/logs/end-of-job/continuation-static-oracle-final.log`), and current `shfmt -d src/tools/check-typec-tcpci.sh`, `bash -n` and `git diff --check` PASS. Production sources and their successful all-target build are unchanged by this oracle correction. Full port guest reruns are still in progress, not yet passing evidence.

Corrected complete aarch64 gate PASSED, native exit 0 after 1192 s:
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-aarch64-oracle.status ./check.sh --gate typec-tcpci-aarch64`.
Log `.build/logs/end-of-job/continuation-typec-tcpci-aarch64-oracle.log`; detailed retained logs at
`.build/logs/typec-tcpci-aarch64/`. All cases completed: asynchronous adoption, unread enumeration/subscription
clients, negotiation/refusals/malformed input, both voltage alarms, every reset/timer, weak/non-PD/silent sources,
controller dependency withdrawal/rebind without a VBUS drop, active-contract sleep refusal and charger attachment
while asleep with sink disabled until resume. The corrected transient case independently recorded its new 18 V
alarm while the later guest sample was 15 V, then proved the same required safe reset and recovery.
Device-tree binding was `dt:/pcie@10000000/i2c@15,0/i2c/tcpc@52`. The plan's partner stretch factor remained 100;
all sink timers remained unchanged. Its 200 TCG responses were p50 29.718 ms, p99 44.651 ms, maximum 47.365 ms,
zero timeouts and six register transfers each. These TCG timings are recorded only, never claimed to meet KVM's
15 ms/24 ms thresholds. RISC-V and fresh exclusive x86_64 checks still pending at this point.

Corrected complete riscv64 gate PASSED, native exit 0 after 1344 s:
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-riscv64-oracle.status ./check.sh --gate typec-tcpci-riscv64`.
Main log `.build/logs/end-of-job/continuation-typec-tcpci-riscv64-oracle.log`; details at
`.build/logs/typec-tcpci-riscv64/`. The same complete scenario set, new stalled-reader regression, corrected
independent overshoot observation and final sleep/charger case all passed through
`dt:/soc/pci@30000000/i2c@15,0/i2c/tcpc@52`. The partner's allowed stretch remained 100. Its 200 TCG responses:
p50 39.688 ms, p99 62.051 ms, maximum 74.577 ms, zero timeouts, six register transfers. These values are recorded,
not compared with the native KVM latency thresholds. Both port guests and fixtures completed normal cleanup.
The fresh exclusive x86_64 gate has now started with stretch 1; its measured current-code verdict remains pending.

Root's final source-hygiene and verification-model checks passed in 118 s after the oracle's formatting correction,
log `.build/logs/end-of-job/continuation-static-oracle-final.log`. No compiled source changed after the 1277 s
all-target build. The earlier failures/interruption and their exact artifacts remain preserved above.

FINAL CURRENT-CODE TCPCI VERIFICATION (2026-10-08):
`LIBER_DEVELOPMENT=1 RUN_STATUS_FILE=.build/logs/end-of-job/continuation-typec-tcpci-x86-async-final.status ./check.sh --gate typec-tcpci`
PASSED, native exit 0 after 501 s. Main log `.build/logs/end-of-job/continuation-typec-tcpci-x86-async-final.log`,
terminal `.status`; detailed probe, guest and independent partner logs `.build/logs/typec-tcpci/`.
Every case passed, including the new unread-client regression, corrected independent overshoot assertion,
dependency withdrawal/rebind and both final sleep cases. The final 200-response KVM measurement (stretch 1,
no concurrent guest/build/harness tracing) was p50 4.283 ms, p99 6.192 ms, maximum 7.001 ms, zero timeouts,
5–6 register transfers: both unchanged requirements, p99 <= 15 ms and no response >= 24 ms, passed.
The guest and fixture completed normal teardown; the owned processes and both prior port guests were checked gone
before the host was explicitly released for the separate final performance-accounting work.

Final status: all requested P02M0202 software and fixture work is implemented and verified. The TCPCI proof
checkbox is now checked; the plan header and TODO entry reflect all three current-code full-gate passes and the
remaining real-hardware criterion. Overall milestone remains OPEN because neither the required owner-provided
real UCSI laptop nor TCPCI board was available. No hardware result or fresh full UCSI guest run is claimed.
The existing UCSI guest results remain historical; fresh UCSI/PD/connector/power host checks and the current
common-service TCPCI integration are recorded above. No existing requirement or timing budget was weakened.
Earlier failed and intentionally interrupted runs are retained, clearly distinguished from these terminal passes.
All pre-continuation audit content is preserved; this section assigns no audit rating.

Acceptance-scope clarification in the final coordinator review: the remaining requirement is exactly the plan’s recorded real-hardware run where the owner provides a board. A suitable supplied UCSI laptop or TCPCI board and access to boot/collect the result would provide that target; this record does not add a requirement to supply both device classes. No such target or access has been established, so the existing real-hardware checkbox remains open.

Final environment availability check (2026-10-08T13:30:32Z): read-only inspection of `/sys/class/dmi/id/{sys_vendor,product_name}` reports `QEMU` / `Standard PC (i440FX + PIIX, 1996)`. `/sys/bus/acpi/devices` has no PNP0C09, PNP0CA0 or USBC000 nodes, `/sys/class/typec` is absent, and `/dev/ttyACM*` / `/dev/ttyUSB*` have no matches. This confirms no relevant local target is exposed by this session; no owner-provided remote target/access has been established either. This is an environment inventory, not a physical-hardware acceptance test. The milestone-specific remaining hardware/design requirements above stay open.


# IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0202 (2026-10-09T05:53:40Z):

Final open-item revalidation found an inconsistency in the plan's continuation status prose: it said that a UCSI laptop AND a TCPCI board were required, while the actual completion criterion requires a recorded run on supplied real hardware and the preserved final audit explicitly clarifies UCSI OR TCPCI. Corrected only that status bullet to name one suitable supported physical target. The criterion itself, implementations, tests, prior audit text and OPEN state are unchanged. A compatible standard-ACPI EC/UCSI laptop could also supply P02M0196's outstanding EC acceptance target; suitability must be established against the actual model and firmware.

The current environment still reports QEMU / Standard PC (i440FX + PIIX, 1996), with no PNP0C09, PNP0CA0 or USBC000 ACPI node, no Type-C class and no ttyACM/ttyUSB target. No remote target/access was supplied. This is a read-only availability check, not a hardware acceptance result. No production code changed and no runtime suite was repeated for this prose correction. Documentation validation follows below.

Verification PASS: `./check.sh --gate milestone-index`, exit 0 in 2 s (`system-suspend-20261009/milestone-index-typec-clarification.log`), and `git diff --check`. Original audit prefixes for all thirteen requested milestones still match commit `d0f54598` byte-for-byte. P02M0202 remains OPEN for its unperformed physical acceptance run; no required verification was waived.


## IMPLEMENTER'S INITIAL IMPLEMENTATION ON P02M0202 (2026-10-09 18:28:16 UTC):

Reviewed the physical-board acceptance clause and retained QEMU UCSI/TCPCI verification, including the required real-hardware UCSI OR TCPCI choice. The user now excludes this physical-only acceptance from Phase 2. The three-architecture TCPCI software evidence remains as recorded, including ARM/RISC-V timer scaling limitations; it is not physical USB-PD timing proof. Final integrated verification and scope reconciliation remain pending. Existing audit content is preserved; no audit rating is assigned.


IMPLEMENTER'S SCOPE/VERIFICATION UPDATE ON P02M0202 (2026-10-09 19:47:24 UTC):

The owner has explicitly limited phase 2 to QEMU and excluded requirements needing physical hardware. The one recorded owner-provided UCSI or TCPCI board run is therefore excluded from phase 2, not reported as passed. Current-code TCPCI gate results already retained for x86_64, aarch64 and riscv64 include fault, rebind and sleep cases; native timing evidence is kept distinct from emulated timing. The existing UCSI evidence is retained, and no new UCSI run is claimed. Closure awaits final shared-tree verification.

Verification continuation (2026-10-09T22:13:43Z):

The frozen profile/readback/microphone continuation was built on all three targets with `RUST_MIN_STACK=33554432 LIBER_DEVELOPMENT=1 ./build.sh --arch ARCH --part libs`: x86_64 PASS442s, aarch64 PASS443s, riscv64 PASS439s; each staged inventory matches121 providers and121 consumers. The six foreign/runtime provider SHA256 identities remain unchanged from the successful canonical foreign regeneration, so no new foreign regeneration was required. Serial `./build.sh --arch all` with the same development/stack environment PASS271.24s; `./build.sh --arch all --part volume --kernel-on-volume` PASS221.06s. Exact logs are under `.build/logs/end-of-job/qemu-only-20261009/post-join-*`; `post-join-stage-results.json` preserves every command, environment, exit and elapsed time.

`RUST_MIN_STACK=33554432 ./check.sh --refresh dynamic-report` PASS405.10s, `./check.sh --gate verify-model` PASS28.97s, and `./gen.sh --check` PASS16.04s. The registration check preceded compilation of the new microphone test and honestly reported it declared but not yet built. The subsequent actual x86_64 SMP4 kernel run compiled and ran all16 explicitly selected cases, PASS74.51s (61s guest):112 2D and222 3D conformance cases with zero failed/unsupported/untested, HDR/Extended/resize/worker equality, partial-allocation and emergency cleanup, both brightness responsiveness tests, audio routing/recovery, pointer/touch lifecycle, and separate microphone gain with legacy capture compatibility. Exact selection and command are in `post-join-stage-results.json`; authoritative suite log is `.build/logs/test/x86_64-20261009T220307Z-1439801-guest.log`. These are functional checks, not a live performance acceptance. ARM/RISC current microphone/joined-profile execution and the required full verification workflow remain pending; merge inventory preparation must refresh all target suites. No milestone completion is asserted by these common build results alone.
