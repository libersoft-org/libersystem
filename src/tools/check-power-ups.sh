#!/usr/bin/env bash
# A REAL HID PRODUCER THROUGH POWERSERVICE: a USB UPS the host builds - `usb-gadget.sh`'s `ups` kind, the kernel's
# HID function with a Power Device report descriptor, and `ups-sim.py` as its firmware - handed to the guest's xHCI
# controller on root port 3, bound by the xHCI driver's HID Power Device class, published as a `power-source`, and
# read and driven through the real PowerService by `upscheck`, a live client holding the read and control
# authorities. The service-first `power-service` gate proves the service over fixtures; this one proves a device's
# own reports reach it, and an operator's command reaches the device.
#
#   upscheck list      the one UPS, exact units, the controls its descriptor carries and no other
#   upscheck control   subscribed: an output switch refused by the service with nothing sent; a scheduled
#                      turn-off done and the device's next report - mains gone, on battery - arriving as an update;
#                      the cancel done and the device back on mains
#
# AND WHAT THE DEVICE RECEIVED, from its firmware's own log: one turn-off and one cancel, and nothing it has no
# control for.
#
# THE GADGET IS A PERMISSION (the owner's, 2026-09-21, with the rules `usb-gadget.sh` enforces): it needs root and the
# host's `dummy_hcd`, and without them this gate FAILS with the reason rather than skipping. The teardown and the
# check that the host carries nothing of it run on every exit.
set -euo pipefail
GUEST_GATE_NAME="power-ups"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs upscheck power_service xhci

work="$(mktemp -d "${TMPDIR:-/tmp}/liber-ups.XXXXXX")"
sim=""
gadget=""
finish() {
	if [[ -n "$sim" ]]; then
		kill "$sim" 2>/dev/null || true
		wait "$sim" 2>/dev/null || true
	fi
	if [[ -n "$gadget" ]]; then
		src/harness/usb-gadget.sh teardown >>"$work/gadget.log" 2>&1 || echo "power-ups: the gadget's teardown reported a problem (see .build/logs/power-ups/gadget.log)" >&2
		src/harness/usb-gadget.sh verify >>"$work/gadget.log" 2>&1 || echo "power-ups: the host still carries something of the gadget's (see .build/logs/power-ups/gadget.log)" >&2
	fi
	mkdir -p .build/logs/power-ups
	cp -f "$work/gadget.log" "$work/ups-sim.log" .build/logs/power-ups/ 2>/dev/null || true
	if [[ -n "$GUEST_LOG" ]]; then
		cp -f "$GUEST_LOG" .build/logs/power-ups/guest.log 2>/dev/null || true
	fi
	[[ "$work" == */liber-ups.* ]] && rm -rf -- "$work"
	guest_gate_cleanup
}
trap finish EXIT

# THE DEVICE, built before QEMU starts because `usb-host` is pointed at it on the command line, and its firmware.
if ! USB_GADGET_ID="$(src/harness/usb-gadget.sh setup ups 2>>"$work/gadget.log")"; then
	cat "$work/gadget.log" >&2
	fail "the UPS gadget could not be built - it needs root and the host's dummy_hcd and usb_f_hid"
fi
gadget=1
export USB_GADGET_ID USB_GADGET_PORT=3
python3 src/harness/ups-sim.py 2>"$work/ups-sim.log" &
sim=$!
# NO NIC: nothing here needs a network, and a NIC's status line can land inside a line the gate reads.
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-300}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-480}"

guest_gate_run $'upscheck list\nupscheck control' ""

expect() {
	local line="$1" why="$2"
	grep -aqE -- "$line" "$GUEST_LINES" || {
		echo "power-ups: expected /$line/ - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'upscheck|PowerService|driver\.xhci' "$GUEST_LINES" | tail -60 >&2 || true
		echo "--- the UPS's firmware ---" >&2
		cat "$work/ups-sim.log" >&2 || true
		exit 1
	}
	echo "power-ups: $line"
}

if grep -aq 'upscheck: FAIL' "$GUEST_LINES"; then
	grep -a 'upscheck: FAIL' "$GUEST_LINES" >&2
	fail "the probe reported a failure"
fi
expect "driver\.xhci: HID power device bound - " "the xHCI driver must bind the UPS as a HID power device"
expect "upscheck: PASS list" "the device's first report must reach PowerService in exact units, with its own controls"
expect "upscheck: an output switch the UPS does not advertise was refused" "a control the descriptor lacks must be refused by the service"
expect "upscheck: updated ups online 0 discharging 1 soc 7900 bp runtime 1800 s voltage [0-9]+ uV on-battery 1" "the device's report of going onto battery must reach the live subscriber"
expect "upscheck: updated ups online 1 discharging 0 soc 7900 bp runtime 3600 s voltage [0-9]+ uV on-battery 0" "the device's report of mains coming back must reach the live subscriber"
expect "upscheck: PASS control" "the turn-off and its cancel must reach the device and its reports the subscriber"

# WHAT THE DEVICE RECEIVED: one turn-off and one cancel, and no request it has no control for.
scheduled="$(grep -ac 'a turn-off is scheduled in 60 s' "$work/ups-sim.log" || true)"
cancelled="$(grep -ac 'the turn-off is cancelled' "$work/ups-sim.log" || true)"
[[ "$scheduled" == 1 && "$cancelled" == 1 ]] || fail "the UPS received $scheduled turn-off(s) and $cancelled cancel(s); the operator sent one of each"
if grep -aq 'a SET_REPORT this UPS has no control for' "$work/ups-sim.log"; then
	fail "a request the UPS has no control for reached it - the refused output switch was sent"
fi
echo "power-ups: the UPS received one turn-off and one cancel, and nothing it has no control for"
echo "power-ups: PASS"
