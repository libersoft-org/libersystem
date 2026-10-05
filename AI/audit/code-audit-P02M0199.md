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
