#!/bin/bash
# Music and calls over Bluetooth, against the in-guest fixture's headset and phone: A2DP both ways, AVRCP's buttons and
# absolute volume, HFP's audio gateway with its voice link, and AudioService's device model carrying all of it.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its AVDTP and AVCTP, its hands-free unit
# and its SBC and mSBC reader and writer (`drivers::bt_world`'s `av` and `hf`, `drivers::bt_sbc`) are written apart from
# the host stack's `service_logic::avdtp`, `avrcp`, `hfp` and `sbc` - its own AT exchange, frame parser, CRC, bit
# allocation and dequantization - so a stream the headset reports as good is one two implementations agree on.
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
#
# AND (`btclassic voice`):
#
#   the SLC                the headset's hands-free unit connects to this host's gateway, negotiates mSBC and reports
#                            its battery, which the operator's status shows; its voice is AudioService's default voice
#                            device at 16 kHz mono
#   no call, no audio      with no session open, its answer and its request for audio are refused
#   the voice link         a voice session brings the synchronous link up, transparent for mSBC; the headset's
#                            microphone reaches the session and the session's tone reaches the headset, every CRC good
#   the call relay         a declared call rings the headset; its answer and hang-up reach the session; the indicators
#                            follow the call
#   the speaker gain       the level AudioService sets is the headset's speaker gain
#   the session's end      takes the synchronous link down with it

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
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-210}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-360}"

guest_gate_run $'btclassic audio\nbtclassic voice' ""
lines="$GUEST_LINES"
grep -qF "btclassic: PASS audio" "$lines" || {
	echo "bluetooth-audio: expected \"btclassic: PASS audio\"" >&2
	grep -aE 'btclassic|bt-fixture|BluetoothService|AudioService|PermissionManager' "$lines" >&2 || cat "$lines" >&2
	exit 1
}
grep -qF "btclassic: PASS voice" "$lines" || {
	echo "bluetooth-audio: expected \"btclassic: PASS voice\"" >&2
	grep -aE 'btclassic|bt-fixture|BluetoothService|AudioService|PermissionManager' "$lines" >&2 || cat "$lines" >&2
	exit 1
}
grep -aE '^btclassic: ' "$lines" | sed 's/^/bluetooth-audio: /'
echo "bluetooth-audio: PASS - A2DP to a headset with SBC both implementations read alike, its delay and level, its button answered, a phone's stream routed to the default output, the operator's pause; and HFP's gateway with mSBC both ways, the call relayed and the link following the session"
