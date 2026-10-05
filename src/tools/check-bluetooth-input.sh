#!/bin/bash
# A Bluetooth keyboard as an input device: it types at the shell's prompt, and its Ctrl+Alt+Delete and Power key
# act on nothing.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its keyboard is a HID device whose
# record carries a report descriptor (a keyboard report and a consumer control report, by id), which reconnects
# its control and interrupt channels itself, and which types a script on the gate's word.
#
# WHAT IS ASSERTED:
#
#   the keyboard connects      paired with its passkey and trusted for input, its page is taken, its link
#     for input                  secured with the stored key before its HID channels are answered, both
#                                channels admitted, its descriptor read from its record, and it is connected
#                                for input
#   it types at the prompt     the scenario types `btclassic input` and nothing else; `btclassic typed` runs
#                                because the Bluetooth keyboard typed it at the prompt, through InputService and
#                                the drivers' key cooking into the console
#   its chords act on nothing  after it presses Ctrl+Alt+Delete and its Power key, the line it types next still
#                                runs, and the machine booted once
#   a gamepad joins the set    `gamepad --lines | btclassic gamepad`: the fixture's HID gamepad, paired, trusted
#                                for input and reconnected, arrives in the gamepad set with the shape its
#                                descriptor gave, at rest; its button, hat and X axis show on it; and its link
#                                going is its departure

set -euo pipefail
GUEST_GATE_NAME="bluetooth-input"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs bt_fixture btclassic bluetooth_service bluetooth_bond_store input_service gamepad

export QEMU_EXTRA="-device edu,addr=0x1d"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-90}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-240}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "bluetooth-input: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'btclassic|bt-fixture|BluetoothService|InputService' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "bluetooth-input: $line"
}

guest_gate_run $'gamepad --lines | btclassic gamepad\nbtclassic input' ""
lines="$GUEST_LINES"
expect "$lines" "btclassic: PASS gamepad" "the Bluetooth gamepad must arrive in the gamepad set, show its controls and depart with its link"
expect "$lines" "btclassic: PASS input" "the keyboard must reconnect its HID channels and be connected for input"
expect "$lines" "bt-fixture: keyboard typed \"btclassic typed\\n\"" "the fixture's keyboard must type at the prompt"
expect "$lines" "btclassic: PASS typed" "a line the Bluetooth keyboard typed at the prompt must run"
expect "$lines" "bt-fixture: keyboard pressed Ctrl+Alt+Delete" "the keyboard must press Ctrl+Alt+Delete"
expect "$lines" "bt-fixture: keyboard pressed its Power key" "the keyboard must press its Power key"
after="$guest_gate_work/after-chords"
sed -n '/bt-fixture: keyboard pressed its Power key/,$p' "$lines" >"$after"
expect "$after" "btclassic: PASS alive" "the line typed after the chords must still run: neither may act"
online="$(grep -acF 'BluetoothService: online' "$lines" || true)"
[[ "$online" == 1 ]] || fail "the services came up $online times; a Bluetooth keyboard's chord must not restart the machine"

echo "bluetooth-input: PASS - a Bluetooth gamepad in the gamepad set, and a Bluetooth keyboard that typed a command at the prompt while its Ctrl+Alt+Delete and Power key acted on nothing"
