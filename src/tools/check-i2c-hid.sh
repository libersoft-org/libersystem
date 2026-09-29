#!/usr/bin/env bash
# HID OVER I2C, end to end: two devices on the vhost-user I2C controller - a precision touchpad at 0x2C and a
# touchscreen at 0x10, `vhost-i2c-gpio.py --hid`'s models, each raising its own GPIO line, level and active low - found
# through the firmware's description (on x86_64 the HID SSDT `qemu-run.sh` builds and loads with `-acpitable`; on the
# ports the machine's tree with the `hid-over-i2c` nodes), bound as CHILDREN through the address and the line
# DeviceManager scopes on their controllers, and reaching InputService.
#
#   bound and published      each device bound, its descriptor read, the touchpad publishing `pointer` alone and the
#                            touchscreen `touch` alone, and InputService attaching both beside the xHCI tablet
#   hidcheck watch           the touchpad's moves and click arrive as pointer events, in order, and the touchscreen's
#                            two-finger contact and lift as contacts - at a probe that is a live pointer client and
#                            holds a surface with input focus, whose proof opens the contact stream
#   hidcheck storm           the control socket holds each line asserted with no report behind it: each driver resets
#                            its device once, and the moves and the contacts still arrive after
#   hidcheck cycle           the virtio-i2c binding disabled through the device policy: both HID bindings stop as lost
#                            dependencies, their providers are detached, and the tablet still moves the cursor; enabled
#                            again, both bind again on the GPIO controller that stayed bound, and deliver again
#   hidcheck malformed       the touchscreen bound again over a descriptor the control socket made malformed: its
#                            binding fails and nothing is published for it
#
# THE PROBE CUES AND THE HOST ANSWERS: every phase of `hidcheck` prints the line the helper below waits for before it
# watches, so nothing is raised ahead of a client that is not listening. ACROSS A SLEEP is P02M0197's to add, with the
# suspend and resume exchange it carries.
set -euo pipefail
GUEST_GATE_NAME="i2c-hid"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs i2c_hid hidcheck input_service virtio_i2c virtio_gpio

# THE DEVICE SIDE: the two controllers' backend with the HID models, in a directory of this run's own.
backend_dir="$(mktemp -d "${TMPDIR:-/tmp}/liber-hid.XXXXXX")"
backend=""
helper=""
finish() {
	if [[ -n "$helper" ]]; then
		kill "$helper" 2>/dev/null || true
	fi
	if [[ -n "$backend" ]]; then
		kill "$backend" 2>/dev/null || true
		wait "$backend" 2>/dev/null || true
	fi
	mkdir -p "$root/../.build/logs/i2c-hid"
	cp -f "$backend_dir/backend.log" "$backend_dir/host.log" "$root/../.build/logs/i2c-hid/" 2>/dev/null || true
	if [[ -n "$GUEST_LOG" ]]; then
		cp -f "$GUEST_LOG" "$root/../.build/logs/i2c-hid/guest.log" 2>/dev/null || true
	fi
	rm -rf "$backend_dir"
	guest_gate_cleanup
}
trap finish EXIT
python3 src/harness/vhost-i2c-gpio.py --hid --i2c "$backend_dir/i2c.sock" --gpio "$backend_dir/gpio.sock" --control "$backend_dir/control.sock" --ready "$backend_dir/ready" --trace >"$backend_dir/backend.log" 2>&1 &
backend=$!
for _ in $(seq 1 100); do
	[[ -e "$backend_dir/ready" ]] && break
	sleep 0.05
done
[[ -e "$backend_dir/ready" ]] || fail "the vhost-user backend did not start"
export I2C_FIXTURE=hid I2C_SOCKET="$backend_dir/i2c.sock" GPIO_SOCKET="$backend_dir/gpio.sock"
# NO NIC: nothing here needs a network, and a NIC's status line can land inside a line the gate reads.
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-360}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-600}"

# One command to the backend's control socket; its one-line answer.
control() {
	python3 - "$backend_dir/control.sock" "$1" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(5)
print(s.recv(4096).decode().strip())
EOF
}

# How many lines of the guest's log hold `text`, now - zero before the log exists.
seen() {
	local count
	count="$(grep -a -c -F -- "$1" "$guest_gate_work/guest" 2>/dev/null || true)"
	echo "${count:-0}"
}

# Wait, bounded, until the guest's log holds `text` more than `baseline` times; false when it never does.
wait_for() {
	local text="$1" baseline="${2:-0}" polls=0
	until (($(seen "$text") > baseline)); do
		sleep 0.2
		polls=$((polls + 1))
		((polls < 3000)) || return 1
	done
}

reports() {
	control "hid script touchpad" >>"$backend_dir/host.log"
	control "hid script touchscreen" >>"$backend_dir/host.log"
}

# THE HOST'S HALF OF EACH PHASE, answering the probe's cue.
host_side() {
	wait_for "hidcheck: watching - raise the reports now" || return 0
	reports
	wait_for "hidcheck: hold the lines now, then raise the reports" || return 0
	local resets
	resets=$(seen "is asserted with no report behind it - resetting the device, once")
	control "hid hold touchpad" >>"$backend_dir/host.log"
	control "hid hold touchscreen" >>"$backend_dir/host.log"
	wait_for "is asserted with no report behind it - resetting the device, once" "$((resets + 1))" || return 0
	sleep 1
	reports
	wait_for "hidcheck: the controller is disabled - move the tablet now" || return 0
	# THE MACHINE'S OWN TABLET, moved across the screen for a few seconds through QMP's `input-send-event`, as
	# ABSOLUTE positions: the monitor's `mouse_move` queues relative motion, which QEMU hands to the one relative
	# device - the PS/2 mouse nothing here drives - and never to a tablet. Any failure is written down rather than
	# ending this helper.
	python3 - "$root/../.build/boot/qemu-qmp.sock" >>"$backend_dir/host.log" 2>&1 <<'EOF' || echo "QMP could not be driven" >>"$backend_dir/host.log"
import json
import socket
import sys
import time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
f = s.makefile('rw')


def answer():
	while True:
		line = json.loads(f.readline())
		if 'event' not in line:
			return line


def command(execute, arguments=None):
	f.write(json.dumps({'execute': execute, 'arguments': arguments or {}}) + '\n')
	f.flush()
	return answer()


answer()
command('qmp_capabilities')
for step in range(12):
	position = 2000 + step * 2500
	command('input-send-event', {'events': [{'type': 'abs', 'data': {'axis': 'x', 'value': position}}, {'type': 'abs', 'data': {'axis': 'y', 'value': position}}]})
	time.sleep(0.3)
print('moved the tablet twelve times')
EOF
	wait_for "hidcheck: raise the reports again now" || return 0
	reports
	wait_for "hidcheck: make the touchscreen's descriptor malformed now" || return 0
	control "hid malformed touchscreen on" >>"$backend_dir/host.log"
}

expect() {
	local line="$1" why="$2"
	grep -aqE -- "$line" "$GUEST_LINES" || {
		echo "i2c-hid: expected /$line/ - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'hidcheck|i2c-hid|InputService|DeviceManager' "$GUEST_LINES" | tail -80 >&2 || true
		exit 1
	}
	echo "i2c-hid: $line"
}

refuse() {
	local line="$1" why="$2"
	if grep -aqE -- "$line" "$GUEST_LINES"; then
		echo "i2c-hid: found /$line/ - $why" >&2
		exit 1
	fi
}

host_side &
helper=$!
guest_gate_run $'hidcheck watch\nhidcheck storm\nhidcheck cycle\nhidcheck malformed' ""
wait "$helper" 2>/dev/null || true
helper=""

if grep -aq 'hidcheck: FAIL' "$GUEST_LINES"; then
	grep -a 'hidcheck: FAIL' "$GUEST_LINES" >&2
	fail "the probe reported a failure"
fi

# BOUND AS CHILDREN AND PUBLISHING BY COLLECTION.
touchpad='(TPAD|touchpad@2c)'
touchscreen='(TSCR|touchscreen@10)'
expect "driver\.i2c-hid: [^ ]*$touchpad: online" "the touchpad must bind as a child"
expect "driver\.i2c-hid: [^ ]*$touchpad: its HID descriptor is read \(address 0x2c, descriptor register 0x20" "the touchpad must read its descriptor at the register its firmware names, 0x20"
expect "driver\.i2c-hid: [^ ]*$touchscreen: online" "the touchscreen must bind as a child"
expect "driver\.i2c-hid: [^ ]*$touchscreen: its HID descriptor is read \(address 0x10, descriptor register 0x1" "the touchscreen must read its descriptor at 0x01"
expect "driver\.i2c-hid: [^ ]*$touchpad: publishes pointer" "a touchpad's mouse collection publishes pointer"
expect "driver\.i2c-hid: [^ ]*$touchscreen: publishes touch" "a touchscreen's collection publishes touch"
refuse "driver\.i2c-hid: [^ ]*$touchpad: publishes touch" "a touchpad publishes no touch - its touch pad collection is silent without a mode switch"
refuse "driver\.i2c-hid: [^ ]*$touchscreen: publishes pointer" "a touchscreen publishes no pointer"
expect "InputService: a touch provider is attached - platform device [0-9]+, .* \(1 attached\)" "InputService must attach the touchscreen"
expect "InputService: a pointer provider is attached - platform device [0-9]+, " "InputService must attach the touchpad"
expect "InputService: a pointer provider is attached - .* \(2 attached\)" "and hold it beside the tablet"

# THE PHASES.
expect "hidcheck: PASS watch" "the moves and the click must arrive as pointer events and the contacts as contacts"
expect "hidcheck: PASS storm" "each device must survive the reset its driver makes, and deliver after it"
resets="$(grep -acF 'is asserted with no report behind it - resetting the device, once' "$GUEST_LINES" || true)"
((resets >= 2)) || fail "both drivers must reset their device once when the line is held with nothing behind it (saw $resets)"
expect "DeviceManager: i2c-hid is connected through a controller whose binding ended; stopping it" "the HID bindings must stop as lost dependencies"
expect "hidcheck: both HID bindings stopped as lost dependencies" "both HID bindings must be waiting on their controller"
expect "InputService: a pointer provider is detached - platform device [0-9]+, .* \(1 attached\)" "the touchpad's pointer must be detached while the tablet's stays"
expect "InputService: a touch provider is detached - platform device [0-9]+, .* \(0 attached\)" "the touchscreen's surface must be detached"
expect "hidcheck: the tablet still moves the cursor" "the tablet must keep moving the cursor"
expect "hidcheck: PASS cycle" "both bindings must bind again and deliver"
expect "driver\.i2c-hid: [^ ]*$touchscreen: its HID descriptor at register 0x1 is refused" "a malformed descriptor must be refused by name"
expect "hidcheck: PASS malformed" "the binding must fail and publish nothing"
echo "i2c-hid: PASS"
