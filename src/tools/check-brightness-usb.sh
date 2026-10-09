#!/usr/bin/env bash
# DISPLAY BRIGHTNESS THROUGH A REAL USB MONITOR: a monitor the host builds - `usb-gadget.sh`'s `monitor` kind, the
# kernel's HID function with a Monitor Control and an Ambient Light collection, and `monitor-sim.py` as its firmware -
# handed to the guest's xHCI controller on root port 3, bound by the xHCI driver's monitor class, published as a
# `backlight` DisplayService drives and an `ambient-light` the brightness policy reads, and set through the policy by
# `brightcheck`, which holds what the `brightness` tool holds.
#
#   brightcheck idle-off   first, so no dim moves a level the checks compare, whatever idle default the owner chooses
#   brightcheck list       the backlight joined to output 0 as single-output - no ACPI backlight, no output EDID
#   set, percent, up, down an absolute level, a percent, one step each way - and the monitor received each
#   zero, zero-allowed     zero refused down to the floor without the explicit zero, and reached with it
#   brightread             the read alone: it reads, and a set sent down its connection is refused
#   auto on, level ...     automatic brightness follows the light the monitor steps between ten and two thousand lux
#   stop/start             the brightness policy, stopped with PowerService as the power-service gate stops it and
#                          started again after it - `start` brings back the one service it names - comes back with every
#                          role delivered and its stored settings, and forwards a set again
#
# AND WHAT THE DEVICE RECEIVED, from its firmware's own log: every level the checks set, and nothing it has no control
# for.
#
# THE GADGET IS A PERMISSION (the owner's, 2026-09-21, with the rules `usb-gadget.sh` enforces): it needs root and the
# host's `dummy_hcd`, and without them this gate FAILS with the reason rather than skipping. The teardown and the
# check that the host carries nothing of it run on every exit.
set -euo pipefail
GUEST_GATE_NAME="brightness-usb"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs brightcheck brightread brightness_policy display_service xhci

work="$(mktemp -d "${TMPDIR:-/tmp}/liber-brightness.XXXXXX")"
sim=""
gadget=""
finish() {
	if [[ -n "$sim" ]]; then
		kill "$sim" 2>/dev/null || true
		wait "$sim" 2>/dev/null || true
	fi
	if [[ -n "$gadget" ]]; then
		src/harness/usb-gadget.sh teardown >>"$work/gadget.log" 2>&1 || echo "brightness-usb: the gadget's teardown reported a problem (see .build/logs/brightness-usb/gadget.log)" >&2
		src/harness/usb-gadget.sh verify >>"$work/gadget.log" 2>&1 || echo "brightness-usb: the host still carries something of the gadget's (see .build/logs/brightness-usb/gadget.log)" >&2
	fi
	mkdir -p .build/logs/brightness-usb
	cp -f "$work/gadget.log" "$work/monitor-sim.log" .build/logs/brightness-usb/ 2>/dev/null || true
	if [[ -n "$GUEST_LOG" ]]; then
		cp -f "$GUEST_LOG" .build/logs/brightness-usb/guest.log 2>/dev/null || true
	fi
	[[ "$work" == */liber-brightness.* ]] && rm -rf -- "$work"
	guest_gate_cleanup
}
trap finish EXIT

# THE DEVICE, built before QEMU starts because `usb-host` is pointed at it on the command line, and its firmware.
if ! USB_GADGET_ID="$(src/harness/usb-gadget.sh setup monitor 2>>"$work/gadget.log")"; then
	cat "$work/gadget.log" >&2
	fail "the monitor gadget could not be built - it needs root and the host's dummy_hcd and usb_f_hid"
fi
gadget=1
export USB_GADGET_ID USB_GADGET_PORT=3
python3 src/harness/monitor-sim.py --button-after 55:63 2>"$work/monitor-sim.log" &
sim=$!
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-300}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-480}"

commands=(
	"brightcheck wait 60"
	"brightcheck idle-off"
	"brightcheck list"
	"brightcheck set 40"
	"brightcheck percent 70"
	"brightcheck up"
	"brightcheck down"
	"brightcheck zero"
	"brightcheck zero-allowed"
	"brightread"
	"brightcheck auto on"
	"brightcheck level 82 30"
	"brightcheck level 13 30"
	"brightcheck auto off"
	"brightcheck set 55"
	"brightcheck level 63 30"
	"brightcheck set 60"
	"stop power_service"
	"start power_service"
	"start brightness_policy"
	"brightcheck wait 30"
	"brightcheck settings"
	"brightcheck set 45"
)
guest_gate_run "$(printf '%s\n' "${commands[@]}")" ""

expect() {
	local line="$1" why="$2"
	grep -aqE -- "$line" "$GUEST_LINES" || {
		echo "brightness-usb: expected /$line/ - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'brightcheck|brightread|BrightnessPolicy|DisplayService|driver\.xhci' "$GUEST_LINES" | tail -80 >&2 || true
		echo "--- the monitor's firmware ---" >&2
		cat "$work/monitor-sim.log" >&2 || true
		exit 1
	}
	echo "brightness-usb: $line"
}

if grep -aq 'brightcheck: FAIL\|brightread: FAIL' "$GUEST_LINES"; then
	grep -a 'brightcheck: FAIL\|brightread: FAIL' "$GUEST_LINES" >&2
	fail "a probe reported a failure"
fi
expect "driver\.xhci: monitor bound - usb:1d6b:0104.*brightness 0\.\.100, its EDID read, an ambient-light sensor" "the xHCI driver must bind the monitor, read its EDID and find its sensor"
expect "brightcheck: active backlight usb:1d6b:0104[^ ]* kind=UsbMonitor output=0 reason=single-output standing=active" "the backlight must join output 0 as single-output: no ACPI backlight, no output EDID"
expect "brightcheck: idle off" "the idle timeout must be turned off before any level is compared"
expect "brightcheck: set level=40" "an absolute level"
expect "brightcheck: percent level=70" "seventy percent of 0..100"
expect "brightcheck: up level=76" "one step up is a sixteenth of the span"
expect "brightcheck: down level=70" "and one step down"
expect "brightcheck: zero level=5" "zero without the explicit zero lands on the floor"
expect "brightcheck: zero-allowed level=0" "and with it reaches zero"
expect "brightread: read 1 backlight\(s\)" "the read alone lists the backlight"
expect "brightread: set refused - the read connection carries no set" "and is refused a set"
expect "brightcheck: auto on" "automatic brightness turned on"
expect "brightcheck: level reached 82" "two thousand lux is eighty-two percent on the default curve"
expect "brightcheck: level reached 13" "ten lux is thirteen"
expect "brightcheck: auto off" "and off again"
expect "brightcheck: level reached 63" "the monitor's own button must update the public brightness state"
grep -aq 'device button changed brightness to 63' "$work/monitor-sim.log" || fail "the monitor sent no device-originated level change"
if grep -aq 'brightness set to 63$' "$work/monitor-sim.log"; then
	fail "the device-originated brightness was echoed back as a SET"
fi
expect "brightcheck: settings automatic=false idle=off" "the restarted policy must read its stored settings back"
expect "brightcheck: set level=45" "and forward a set again"
online="$(grep -ac 'BrightnessPolicy: online' "$GUEST_LINES" || true)"
[[ "$online" -ge 2 ]] || fail "the brightness policy came online $online time(s); the restart must start it again"
echo "brightness-usb: the policy came back after PowerService's restart"

# WHAT THE DEVICE RECEIVED: every level the checks set, in order, and nothing it has no control for.
all=" $(grep -aoE 'brightness set to [0-9]+' "$work/monitor-sim.log" | awk '{print $4}' | tr '\n' ' ')"
received="$all"
for level in 40 70 76 70 5 0 82 13 55 60 45; do
	case "$received" in
	*" $level "*) received=" ${received#*" $level "}" ;;
	*) fail "the monitor did not receive level $level in order (it received:$all)" ;;
	esac
done
if grep -aq 'a SET_REPORT this monitor has no control for' "$work/monitor-sim.log"; then
	fail "a request the monitor has no control for reached it"
fi
echo "brightness-usb: the monitor received every level set, in order, and nothing it has no control for"
echo "brightness-usb: PASS"
