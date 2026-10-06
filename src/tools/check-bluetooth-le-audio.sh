#!/bin/bash
# LE Audio over the in-guest fixture's earbuds and broadcast source: a coordinated set found, bonded and streamed as one
# device in LC3 over connected isochronous streams; its level both ways; a voice session's call audio both ways with the
# call relayed through this host's telephone bearer; and an Auracast broadcast - in the clear and encrypted - found,
# joined and played on the earbuds.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its earbuds' attribute servers - PACS,
# ASCS, CSIS and Volume Control - their stream endpoint state machine, its CIG, CIS and BIG emulation, and its LC3 frame
# reader and writer (`drivers::bt_lc3`, from the specification's text) are written apart from the host stack's
# `service_logic::{bap, ascs, cap, le_audio, le_iso, gtbs, lc3}`. A tone both sides find on the same spectral line is
# one two implementations agree on; the host's codec is held to the specification's own reference frames by its suites.
#
# WHAT IS ASSERTED:
#
#   the set           the left earbud paired by the operator (Just Works) and trusted for audio; the right found by the
#                       RSI it advertises - its set's key read encrypted - bonded as the set's, trusted as it is
#   one device        AudioService lists the set once: the default output in stereo at 48 kHz, its own level, and a
#                       voice device at 16 kHz both ways
#   music             a stereo stream - 1 kHz left, 2 kHz right - configures each earbud's sink for its own channel at
#                       48 kHz / 120 octets, establishes both CISes, and each earbud reads 50 frames whole with its
#                       loudest line at its own tone
#   the level         80 sent to both members' Volume Control as setting 204; a member's own 255 taken back as 100
#   a call            a voice session winds the music's streams down and configures 16 kHz both ways; the session's
#                       tone reaches the earbuds and the microphone's tone the session; the call rung, answered from
#                       the earbud through the telephone bearer, active, hung up from it
#   a broadcast       heard by its announcement, its periodic train and BIG joined, offered as a stereo route, and
#                       played on the earbuds - its left stream on the left earbud, its right on the right
#   encrypted         no route without the Broadcast Code nor with a wrong one; joined with the right one
#   the links go      one member's link gone the device stays; both gone, it leaves AudioService
#
# THE CONTROLLER BECOMES AN LE AUDIO ONE HERE: the fixture starts as a plain LE controller - every other gate runs the
# legacy scan and connection commands - and the probe makes it one with extended advertising, periodic sync, the CIS
# central role and the synchronized receiver from its next reset, which powering the radio off sends. Like a real one it
# then refuses the legacy scan and connection commands after any extended one, so every scan and connection here runs
# the extended commands.

set -euo pipefail
GUEST_GATE_NAME="bluetooth-le-audio"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() {
	if [[ -n "${GUEST_LINES:-}" && -f "$GUEST_LINES" ]]; then
		grep -aE 'btclassic|bt-fixture|BluetoothService|AudioService|PermissionManager' "$GUEST_LINES" >&2 || cat "$GUEST_LINES" >&2
	fi
	guest_gate_fail "$@"
}

guest_gate_require_programs bt_fixture btclassic bluetooth_service bluetooth_bond_store audio_service

export QEMU_EXTRA="-device edu,addr=0x1d"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-240}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-380}"

guest_gate_run $'btclassic leaudio\nbtclassic broadcast' ""
lines="$GUEST_LINES"
for phase in leaudio broadcast; do
	grep -qF "btclassic: PASS $phase" "$lines" || fail "expected \"btclassic: PASS $phase\""
done
grep -aE '^btclassic: ' "$lines" | sed 's/^/bluetooth-le-audio: /'
echo "bluetooth-le-audio: PASS - a coordinated set bonded and streamed as one device in LC3, each earbud its own channel; its level both ways; a call's audio both ways and the call relayed through the telephone bearer; a broadcast in the clear and encrypted played on the earbuds; the set's links going"
