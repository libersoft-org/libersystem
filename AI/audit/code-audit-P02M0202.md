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
