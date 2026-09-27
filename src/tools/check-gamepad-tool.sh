#!/bin/bash
# The `gamepad` tool, end to end, against the in-guest gamepad fixture - WHICH GAMEPAD PRESSED WHAT. The
# fixture publishes gamepads of the harness's shape over the production `gamepad` wire, InputService gives each
# an id and delivers the console stream to the one program granted `input-gamepad`, and the tool prints it. It
# says nothing about USB - the hardware suite's gamepad proof is that half - and everything about the
# vocabulary, InputService's identities and the tool's lines.
#
# ONE TYPED LINE, `gamepad --lines | gamepadcheck`, AND THE PIPE IS THE SYNCHRONISATION: the probe reads the
# tool's own lines, waits for `gamepad: watching` before it touches the fixture, and after each step waits,
# bounded, for the line that step must produce - so nothing is scripted ahead of a tool that is not listening,
# and a line for the wrong gamepad during a step fails it.
#
#   two gamepads, two ids        they attach and arrive at rest, each under its own id
#   which pressed what           button 3 on the SECOND shows on the second and not the first; released, it clears
#   a range's ends               X on the first at 0 and at 255
#   a hat                        the second's hat east shows `E`, and back shows `centred`
#   one leaves                   the first departs, and a button on the second still shows
#   plugged back                 the first attaches again and arrives under a NEW id

set -euo pipefail
GUEST_GATE_NAME="gamepad-tool"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the tool and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs gamepad_fixture gamepadcheck input_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x17"
# NO NIC. Nothing here needs a network, and a NIC's status line written straight to the port can land inside a
# line the probe or the gate reads.
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-180}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-360}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "gamepad-tool: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'gamepad|InputService' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "gamepad-tool: $line"
}

# AN EMPTY PREFIX, so the whole log is kept: the tool's lines carry `:` inside them (`axes=X:0..255`), and a
# prefix filter stops each captured line at the first one.
guest_gate_run $'gamepad --lines | gamepadcheck' ""
lines="$GUEST_LINES"

if grep -aq 'gamepadcheck: FAIL' "$lines"; then
	grep -a 'gamepadcheck: FAIL' "$lines" >&2
	fail "the probe reported a failure"
fi
expect "$lines" "driver.gamepad-fixture: online" "the fixture must bind"
expect "$lines" "gamepadcheck: PASS which gamepad pressed what" "every step must show on the right gamepad and on no other"
