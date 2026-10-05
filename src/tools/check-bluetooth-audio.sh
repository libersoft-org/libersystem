#!/bin/bash
# Music over Bluetooth, against the in-guest fixture's headset and phone: A2DP both ways, AVRCP's buttons and absolute
# volume, and AudioService's device model carrying both.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its AVDTP and AVCTP and its SBC reader and
# writer (`drivers::bt_world`'s `av`, `drivers::bt_sbc`) are written apart from the host stack's `service_logic::avdtp`,
# `avrcp` and `sbc` - its own frame parser, CRC, bit allocation and dequantization - so a stream the headset reports as
# good is one two implementations agree on.
#
# WHAT IS ASSERTED (`btclassic audio`):
#
#   a2dp source            the headset, connected for audio, is configured for SBC at the best both support, reports
#                            its delay, and becomes AudioService's default output with that delay in its latency
#   the codec              a second of a 7.5 kHz tone an application plays reaches the headset as 48 kHz joint stereo
#                            SBC, bitpool 53, every CRC good, the energy in the third of eight subbands
#   absolute volume        the level AudioService sets reaches the headset as AVRCP's absolute volume, and the
#                            headset's own change comes back to AudioService
#   the target             the headset's play button is answered NOT IMPLEMENTED - there is no media session
#   a2dp sink              the phone streams to this host: a route to the default output, never an input
#   the controller         the operator's pause reaches the phone over AVRCP
#   a device leaving       the headset's link gone, its output leaves AudioService

set -euo pipefail
GUEST_GATE_NAME="bluetooth-audio"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs bt_fixture btclassic bluetooth_service bluetooth_bond_store audio_service

export QEMU_EXTRA="-device edu,addr=0x1d"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-150}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-300}"

guest_gate_run $'btclassic audio' ""
lines="$GUEST_LINES"
grep -qF "btclassic: PASS audio" "$lines" || {
	echo "bluetooth-audio: expected \"btclassic: PASS audio\"" >&2
	grep -aE 'btclassic|bt-fixture|BluetoothService|AudioService|PermissionManager' "$lines" >&2 || cat "$lines" >&2
	exit 1
}
grep -aE '^btclassic: ' "$lines" | sed 's/^/bluetooth-audio: /'
echo "bluetooth-audio: PASS - A2DP to a headset with SBC both implementations read alike, its delay and level, its button answered, a phone's stream routed to the default output, and the operator's pause"
