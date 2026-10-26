#!/usr/bin/env bash
# WHAT A USB DEVICE HELD IS GIVEN BACK WHEN IT LEAVES, on x86_64's xHCI: the development image on the ordinary machine,
# whose stick is on the controller's root port. The stick is taken out, a stick of this run's own is plugged in and read
# through `vol://usb`, and taken out again - and the DMA memory the xHCI driver's Domain holds, as `lsdev` reads it
# through DeviceManager, is the same after the second stick left as it was before that stick arrived.
#
# AND THE SECOND STICK IS READ AS ITSELF, with no request to `vol://usb` between the two. The xHCI published one block
# wire for every stick it carried and let it stand across a detach, and the USB volume lets a mount go only when its
# provider is withdrawn or a request fails - so the stick plugged second was read through the first one's mount, and
# its file was not there. A stick that leaves takes its publication with it now, and the driver says so.
#
# WHY IT IS A GATE OF ITS OWN. The driver closed the handles of what a departed device had - its rings, its transfer
# pages, its data buffer - without unmapping them, and a mapped buffer is kept, frames and charge, until the process
# ends: every plug and unplug kept its pages for good while the class budget was refunded. Nothing read the figure that
# drifted, so nothing failed. `usb-legacy` reads the same figure for UHCI, OHCI and EHCI.
#
# THE HOST'S HALF ANSWERS THE PROBE'S CUES, as `usb-legacy`'s does: the guest prints the line the helper waits for, and
# the helper takes out or plugs in only then.
set -euo pipefail
GUEST_GATE_NAME="usb-release"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || {
	echo "usb-release: NOT PERFORMED: the hot-plugged stick is QMP's on the x86_64 machine"
	exit 0
}

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs xhci hidcheck lsdev
command -v mformat >/dev/null && command -v mcopy >/dev/null || fail "mtools (mformat, mcopy) is required for the stick this gate plugs"

sticks="$(mktemp -d "${TMPDIR:-/tmp}/liber-usb-release.XXXXXX")"
helper=""
kept="$root/../.build/logs/usb-release"
finish() {
	if [[ -n "$helper" ]]; then
		kill "$helper" 2>/dev/null || true
	fi
	rm -rf "$kept"
	mkdir -p "$kept"
	cp -f "$sticks/host.log" "$kept/" 2>/dev/null || true
	if [[ -n "$GUEST_LOG" ]]; then
		cp -f "$GUEST_LOG" "$kept/guest.log" 2>/dev/null || true
	fi
	rm -rf "$sticks"
	guest_gate_cleanup
}
trap finish EXIT

# THE STICK THE HOST PLUGS: a FAT image carrying one file no other stick has.
truncate -s 16M "$sticks/release.img"
mformat -i "$sticks/release.img" :: >/dev/null
printf 'the release stick, plugged after the boot\n' | mcopy -i "$sticks/release.img" - "::release.txt"

export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-180}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-300}"
# THE COMPLETION LINE: the script's last line prints `usb-release-done`, after which nothing this gate needs can
# arrive. It also refuses a panic, which a late line could carry, so it keeps a settle window of 10 s after it.
export GUEST_GATE_DONE="usb-release-done" GUEST_GATE_SETTLE=10
qmp_socket="$GUEST_GATE_QMP"

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

# QMP COMMANDS on one connection, each a JSON object on its own argument; every answer is logged.
qmp() {
	python3 - "$qmp_socket" "$@" <<'EOF' >>"$sticks/host.log" 2>&1
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(10)
s.connect(sys.argv[1])
f = s.makefile('rw')
def answer():
	while True:
		line = json.loads(f.readline())
		if 'event' not in line:
			return line
answer()
f.write(json.dumps({'execute': 'qmp_capabilities'}) + '\n')
f.flush()
answer()
for request in sys.argv[2:]:
	f.write(request + '\n')
	f.flush()
	print(json.loads(request).get('execute'), json.dumps(answer()))
EOF
}

# THE HOST'S HALF: the boot's stick out; this run's stick in, on the xHCI's bus; and out again.
host_side() {
	wait_for "hidcheck: usb wait - act now" 0 || return 0
	qmp '{"execute": "device_del", "arguments": {"id": "usbstick"}}'
	wait_for "hidcheck: usb wait - act now" 1 || return 0
	qmp "{\"execute\": \"blockdev-add\", \"arguments\": {\"driver\": \"raw\", \"node-name\": \"plug-release\", \"file\": {\"driver\": \"file\", \"filename\": \"$sticks/release.img\"}}}" \
		'{"execute": "device_add", "arguments": {"driver": "usb-storage", "bus": "usb.0", "drive": "plug-release", "id": "plug-release"}}'
	wait_for "hidcheck: usb wait - act now" 2 || return 0
	qmp '{"execute": "device_del", "arguments": {"id": "plug-release"}}'
}

host_side &
helper=$!
# THE READINGS: the xHCI driver's DMA memory once the boot's stick has left, and again once the stick plugged after it
# has been read through `vol://usb` - its data buffer grown to the read - and has left too.
#
# NO READ BETWEEN THE TWO STICKS: the second is read straight after the first left, which is the swap a stale mount
# would answer from the first one's directory.
guest_gate_run $'cat vol://usb/hello.txt\nhidcheck usb wait\necho usb-release-dma before\nlsdev\nhidcheck usb wait\ncat vol://usb/release.txt\nhidcheck usb wait\necho usb-release-dma after\nlsdev\necho usb-release-done' ""
wait "$helper" 2>/dev/null || true
helper=""

expect() {
	grep -aqE -- "$1" "$GUEST_LINES" || fail "$2 (no line matching /$1/)"
}

grep -aq 'usb-release-done' "$GUEST_LINES" || fail "the script did not run to its end"
! grep -aq 'panicked at' "$GUEST_LINES" || fail "something panicked"
expect 'Hello from the OS ramdisk!' "the boot's stick must be read through vol://usb"
expect 'the release stick, plugged after the boot' "the stick plugged after the boot must be read through vol://usb, with no request between it and the stick before it"
expect 'driver.xhci: the stick left - its block wire is withdrawn and offered again' "a stick that left must take its publication with it"
python3 "$root/tools/usb-dma-readings.py" "$GUEST_LINES" usb-release-dma before after:xhci ||
	fail "the xHCI driver kept DMA memory the stick that left had held"
echo "usb-release: the boot's stick out, a stick plugged in and read as itself with no request between the two, and taken out - and the xHCI driver holds the DMA memory it held before that stick came"
echo "usb-release: PASS"
