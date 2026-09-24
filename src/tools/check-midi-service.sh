#!/bin/bash
# MidiService, end to end, against the in-guest MIDI fixture - THE SERVICE, ITS RECEIVERS AND ITS BOUNDS. The
# fixture delivers raw USB-MIDI 1.0 packet batches the probes script, and MidiService decodes them with the
# driver library's decoder; success here establishes the bounded event vocabulary, the decoder over real
# packets, the receiver's queue and lifetime and the grants, and says nothing about USB MIDI transport, UMP or
# output, none of which exists yet.
#
#   handles return to baseline  MidiService's handles, read from the system graph, are the same after the
#                                 first probes and after the scenario
#   inventory                   `midiread`: endpoints with protocol, direction and cables; no receiving; no
#                                 output
#   exact order and time        `midicheck receive`: bytes, cables, kinds and SysEx fragments in packet order
#                                 at the batch's one receipt time, after a delayed read
#   typed faults                `midicheck malformed`
#   the SysEx cap               `midicheck cap`: 64 kB crossed in small fragments, aborted once by number,
#                                 never ended; recovery at a new message
#   unsupported and denied      `midicheck unsupported`
#   saturation                  `midicheck overflow`: a readable terminal overflow, nothing continuous
#   lost input                  `midicheck lost`: a readable source discontinuity
#   unplug and reattachment     `midicheck unplug`, then `midicheck fresh` on the replacement
#   two endpoints, busy         `midihold hold` on endpoint 1 beside endpoint 0; a second `midihold` refused
#   a dead owner's receiver     `midihold dup | midicheck inherit`
#   a failed prepared launch    `midifail`: its receiver was minted and retired with the launch

set -euo pipefail
GUEST_GATE_NAME="midi-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs midi_fixture midicheck midihold midiread midifail midi_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x19"
# NO NIC. Nothing here needs a network, and a NIC's IPv6 status line - written straight to the port while
# a `graph` line comes through ConsoleService's mirror - can land inside the service's line and hide its count.
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-240}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-400}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "midi-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'midicheck|midihold|midiread|midifail|midi-fixture|MidiService' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "midi-service: $line"
}

# THE DEAD OWNER'S RECEIVER BEFORE THE TWO HOLDERS: the one that holds endpoint 1 in the background may still
# hold it for its two seconds, and a pipeline started inside them would be refused as busy for that reason.
# FROM AFTER THE FIRST PROBES, NOT FROM BOOT: PermissionManager resolves the service's roots by name the
# first time it mints a grant from them, and keeps each resolved connection - which the service counts as
# a handle. Measured: each root adds one, once, and nothing after.
# `fg` BEFORE THE LAST `graph`: the background holder, `midihold hold 2 &`, can still hold what it was granted when
# the last probe returns, and a count read then is the holder's and not a leak. `fg` waits for it, and says
# there is no such job when it has already gone.
guest_gate_run $'midiread\nmidicheck receive\ngraph\nmidicheck malformed\nmidicheck cap\nmidicheck unsupported\nmidihold dup | midicheck inherit\nmidihold hold 2 &\nmidihold hold 1\nmidicheck overflow\nmidicheck lost\nmidifail\nmidicheck unplug\nmidicheck fresh\nfg\ngraph' ""
lines="$GUEST_LINES"

if grep -aq 'midicheck: FAIL\|midiread: FAIL\|midifail: FAIL' "$lines"; then
	grep -a 'midicheck: FAIL\|midiread: FAIL\|midifail: FAIL' "$lines" >&2
	fail "a probe reported a failure"
fi
expect "$lines" "driver.midi-fixture: online" "the fixture must bind"
expect "$lines" "MidiService: a MIDI device was admitted" "the service must admit the fixture's device"
expect "$lines" "midiread: PASS" "inventory must list and refuse"
expect "$lines" "midicheck: PASS receive" "bytes, cables, fragments, order and receipt time must hold"
expect "$lines" "midicheck: PASS malformed" "malformed packets must be typed faults"
expect "$lines" "midicheck: PASS cap" "the SysEx cap must hold across fragments, and recovery follow"
expect "$lines" "midicheck: PASS unsupported" "output and UMP must be unsupported, inventory must not receive"
expect "$lines" "midihold: held endpoint 1 and stopped" "a receiver on the second endpoint must work beside the first"
expect "$lines" "MidiService: a second receiver on an endpoint was refused as busy - there is no fan-out" "a second receiver on one endpoint must be refused"
expect "$lines" "midicheck: PASS overflow" "saturation must end the receiver readably"
expect "$lines" "midicheck: PASS lost" "lost input must end the receiver with a discontinuity"
expect "$lines" "midihold: sent its receiver, and exits" "the holder must hand its receiver on"
expect "$lines" "midicheck: PASS inherit" "a transferred receiver must not outlive its owner"
expect "$lines" "MidiService: a receiver's owner ended - its receiver is retired" "the service must observe owners, not channels"
expect "$lines" "midicheck: PASS unplug" "withdrawal during a SysEx must end the receiver as removed"
expect "$lines" "midicheck: PASS fresh" "the replacement's receiver must start clean"

# THE HANDLES CAME BACK: the count after the first probes and after the scenario, from the system graph - which
# holds the service's process and reads the kernel's own count - with no probe connected either time.
counts="$(grep -aoE '\{name=midi_service, type=service, [^{]*counters=\{messages-sent=[0-9]+, messages-received=[0-9]+, handles=[0-9]+' "$lines" | grep -oE '[0-9]+$')"
[[ "$(wc -l <<<"$counts")" == 2 ]] || fail "the service's handle count was not read twice from the system graph"
[[ "$(sed -n 1p <<<"$counts")" == "$(sed -n 2p <<<"$counts")" ]] || fail "MidiService's handles did not return to their baseline ($(tr '\n' ' ' <<<"$counts"))"
echo "midi-service: the service's handles returned to their baseline ($(sed -n 1p <<<"$counts"))"

echo "midi-service: PASS - the bounded event vocabulary and queue, the decoder over real packets, SysEx bounds, typed faults, saturation, loss, unplug, grants and reclamation (the service; not USB MIDI transport, UMP or output)"
