#!/bin/bash
# CameraService, end to end, against the in-guest camera fixture - THE SERVICE, ITS GRANTS, ITS LEASES AND ITS
# TIMING. The fixture plays a camera whose synthetic UVC descriptors go through the staged normalizer the USB
# Video class module will use, and writes deterministic frames into buffers the CLIENTS created; success here
# establishes the service, capture policy, buffer ownership and honest timing, and says nothing about USB
# Video transport, which does not exist yet.
#
#   handles return to baseline  CameraService's handles, read from the system graph, are the same after the
#                                 first probes and after the scenario
#   inventory starts nothing    `camread`: cameras listed and formats paged; a start sent on inventory is
#                                 not an operation there, and nothing streams
#   exact negotiation and bytes `camcheck capture`: YUY2 as asked, every byte the fixture's pattern computed
#                                 independently, four buffers leased and stable while loss is counted, one
#                                 lease back and one buffer back, arrival times fixed at completion, the
#                                 device clock wrapping consistently, and a stop that released every mapping
#   encoded data                `camcheck mjpeg`: a continuous-range stream's opaque payload
#   honest timing               `camcheck timing`: no device time where none was given; a reset and an
#                                 uncounted loss each start a new device timeline
#   two clients and busy        `camhold hold` then `camcheck busy`
#   a dead owner's endpoint     `camhold dup | camcheck inherit`
#   a failed prepared launch    `camfail`: its capture grant was minted and retired with the launch
#   a kept buffer               `camhold retain` beside `camcheck again`: nothing new reaches an old client's
#                                 buffer while a new grant captures
#   quarantine and replacement  `camcheck quarantine`: an unconfirmed stop times out after two seconds and
#                                 quarantines the camera; its replacement is another camera

set -euo pipefail
GUEST_GATE_NAME="camera-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the service and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs camera_fixture camcheck camhold camread camfail camera_service

# THE FIXTURE'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x1a"
# NO NIC. Nothing here needs a network, and a NIC's IPv6 status line - written straight to the port while
# a `graph` line comes through ConsoleService's mirror - can land inside the service's line and hide its count.
export NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-240}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-400}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "camera-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'camcheck|camhold|camread|camfail|camera-fixture|CameraService' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "camera-service: $line"
}

# FROM AFTER THE FIRST PROBES, NOT FROM BOOT: PermissionManager resolves the service's roots by name the
# first time it mints a grant from them, and keeps each resolved connection - which the service counts as
# a handle. Measured: each root adds one, once, and nothing after.
# `fg` BEFORE THE LAST `graph`: the background holder, `camhold retain &`, can still hold what it was granted when
# the last probe returns, and a count read then is the holder's and not a leak. `fg` waits for it, and says
# there is no such job when it has already gone.
guest_gate_run $'camread\ncamcheck capture\ngraph\ncamcheck mjpeg\ncamcheck timing\ncamhold hold 3 &\ncamcheck busy\ncamhold dup | camcheck inherit\ncamfail\ncamhold retain &\ncamcheck again\ncamcheck quarantine\nfg\ngraph' ""
lines="$GUEST_LINES"

if grep -aq 'camcheck: FAIL\|camhold: FAIL\|camread: FAIL\|camfail: FAIL' "$lines"; then
	grep -a 'camcheck: FAIL\|camhold: FAIL\|camread: FAIL\|camfail: FAIL' "$lines" >&2
	fail "a probe reported a failure"
fi
expect "$lines" "driver.camera-fixture: online" "the fixture must bind"
expect "$lines" "CameraService: a camera was admitted" "the service must admit the fixture's camera"
expect "$lines" "camread: PASS" "inventory must list and page, and start nothing"
expect "$lines" "camcheck: PASS capture" "the YUY2 stream's bytes, leases, loss and timing must hold"
expect "$lines" "camcheck: PASS mjpeg" "the MJPEG stream must carry its opaque payload"
expect "$lines" "camcheck: PASS timing" "device time must be absent where absent, and never carried across a reset or a loss"
expect "$lines" "camcheck: PASS busy" "a second grant must be refused while another streams"
expect "$lines" "camhold: sent its endpoint while streaming, and exits" "the holder must hand its endpoint on"
expect "$lines" "camcheck: PASS inherit" "a transferred endpoint must not keep its dead owner's stream"
expect "$lines" "CameraService: a grant's owner ended - its grant is retired" "the service must observe owners, not channels"
expect "$lines" "camhold: PASS retain" "an old client's buffer must receive nothing once its stream stopped"
expect "$lines" "camcheck: PASS again" "a new grant must capture into its own buffer"
expect "$lines" "camcheck: PASS quarantine" "an unconfirmed stop must quarantine the camera, and its replacement is another"
expect "$lines" "CameraService: a stop was not confirmed in time - the camera is quarantined" "the service must say it quarantined the camera"

# THE HANDLES CAME BACK: the count after the first probes and after the scenario, from the system graph - which
# holds the service's process and reads the kernel's own count - with no probe connected either time.
counts="$(grep -aoE '\{name=camera_service, type=service, [^{]*counters=\{messages-sent=[0-9]+, messages-received=[0-9]+, handles=[0-9]+' "$lines" | grep -oE '[0-9]+$')"
[[ "$(wc -l <<<"$counts")" == 2 ]] || fail "the service's handle count was not read twice from the system graph"
[[ "$(sed -n 1p <<<"$counts")" == "$(sed -n 2p <<<"$counts")" ]] || fail "CameraService's handles did not return to their baseline ($(tr '\n' ' ' <<<"$counts"))"
echo "camera-service: the service's handles returned to their baseline ($(sed -n 1p <<<"$counts"))"

echo "camera-service: PASS - capture grants, exact negotiation, client-owned leased buffers, honest timing and loss, busy refusal, owner death, a failed launch, quarantine and replacement (the service; not USB Video transport)"
