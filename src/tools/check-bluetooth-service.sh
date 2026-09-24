#!/bin/bash
# The Bluetooth host stack, end to end, against the in-guest fixture - across a service restart and a
# cold reboot of the same volume.
#
# WHY IT NEEDS A GUEST AND TWO BOOTS. The protocol decisions are held by host tests against the Core
# specification's own sample data; what only a running system can show is that the stack, the bond
# store, the supervisor's restart ladder and the input service fit together - that a pairing reaches a
# DURABLE bond, that a cursor actually moves, and that the bond is used again after the process that
# made it and the machine it ran on have both gone away. A bond that survived only in memory would pass
# every assertion of the first boot.
#
# WHAT IS ASSERTED, AND WHO SAYS SO. The host's own account is not taken for the two claims that
# matter; the fixture - an emulated controller and mouse whose cryptography is a separate
# implementation held to the same vectors - reports them:
#
#   an ordinary read client     `btread` lists the controllers and scans, and holds no operator
#     is not an operator          authority at all: its permission row grants the read authority alone
#   pairing reaches a bond      `btcheck pair` sees the pairing reach `bonded` at the security level
#     and the cursor moves        Just Works earns - encrypted and NOT authenticated - and a live
#                                 pointer client sees the cell move and the left button go down and up
#   a restart reuses the bond   after the service is stopped (by `btcheck refund`) and started, the
#                                 FIXTURE reports encryption with the key it remembers, and no second
#                                 pairing
#   a cold reboot reuses it     on the second boot of the SAME disk, the fixture - which remembers
#                                 nothing across a reboot, like a mouse with no flash would not - reports
#                                 the fingerprint of the key the host presented, and it is the
#                                 fingerprint the first boot's pairing produced; and no pairing happened
#   a forget is final           after `btcheck forget` the bond is gone and the cursor does not move
#   the Domains are finite      both services run under Domains whose six limits are the stated
#     and refunded                figures, read from ProcessService's accounting; a stopped service's
#                                 Domain is gone, and the restarted one has the same limits again
#   a bound refuses             the service's client table refuses at its bound and gives the slots
#                                 back, and the pointer service still answers a typed request
#   another pointer still works with the Bluetooth service stopped, the machine's own tablet - moved and
#                                 clicked from the host through QEMU's monitor - still reaches a live
#                                 pointer client, and nothing Bluetooth can have been its source
#   a lost transport ends       disabling the fixture's device mid-scan ends the scan and removes the
#     what it carried             controller; enabling it brings a NEW publication that is initialised
#                                 again and reconnects to the bond, and the scan from before is refused
#   no key, no store, no        `btcheck deny`: an operator power-off sent on a read connection, and the
#     stale handle                bond store's own lookup sent on a read and on an operator connection,
#                                 are not answered as what they claim and the radio stays on; and after
#                                 `btcheck refund`'s stop, a connection to the stopped instance answers
#                                 nothing
#
# THE FINGERPRINT, NEVER THE KEY. Four bytes of a CMAC under the key identify it without revealing it,
# and that is all the comparison needs.

set -euo pipefail
GUEST_GATE_NAME="bluetooth-service"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs bt_fixture btcheck btread bluetooth_service bluetooth_bond_store

# THE FIXTURE'S DEVICE, at the address its registry entry pins. A machine without it never binds the
# fixture, which is what keeps it out of every image this gate did not build.
export QEMU_EXTRA="-device edu,addr=0x1d"
# ONE DISK FOR BOTH BOOTS, in this gate's own work directory: the cold reboot is only a reboot of the
# same volume if the second boot attaches the disk the first one wrote. The runner makes it from the
# volume the medium is paired with, which is the only one the system runs from; the first boot is
# checked below for having run from it.
[[ -f "$root/../.build/boot/system-volume-bootable-x86_64.img" ]] || fail "there is no bootable system volume beside the image - build it:  LIBER_DEVELOPMENT=1 ./image.sh"
export RUN_DISK="$guest_gate_work/system.img"
# The scenario takes longer than a probe: pairing, a watched cursor and a service restart.
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-260}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-420}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "bluetooth-service: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'btcheck|btread|bt-fixture|BluetoothService|BluetoothBondStore' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "bluetooth-service: $line"
}

# THE UNRELATED POINTER, moved and clicked from the host once `btcheck refund` says the Bluetooth service is
# stopped: the machine's own tablet, through the monitor the runner gives every x86_64 guest. Bounded - a
# guest that never says so leaves it to give up by itself.
monitor="$root/../.build/boot/qemu-monitor.sock"
unrelated_pointer() {
	local polls=0
	until grep -aqF 'btcheck: the Bluetooth service is stopped; move another pointer now' "$guest_gate_work/guest" 2>/dev/null; do
		sleep 0.2
		polls=$((polls + 1))
		((polls < 3000)) || return 0
	done
	python3 - "$monitor" <<'EOF_MONITOR'
import socket
import sys
import time

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(sys.argv[1])
for command in ('mouse_move 4000 4000', 'mouse_move 12000 12000', 'mouse_button 1', 'mouse_button 0', 'mouse_move 20000 20000'):
	client.sendall(command.encode() + b'\n')
	time.sleep(0.4)
client.close()
EOF_MONITOR
}

# ---- boot one: a read client, a pairing, a cursor, and a restart of the service.
unrelated_pointer &
pointer=$!
guest_gate_run $'btread\nbtcheck pair\nbtcheck deny\nbtcheck limits\nbtcheck exhaust\nbtcheck refund\ndate\nstart bluetooth_service\nbtcheck limits\nbtcheck reuse\nbtcheck loss\nbtcheck reuse' ""
kill "$pointer" 2>/dev/null || true
first="$guest_gate_work/first"
cp "$GUEST_LINES" "$first"
if grep -aqF "vol://system is a live copy in memory" "$first" || ! grep -aqF "boot: the system volume is a paired block volume" "$first"; then
	fail "the system did not run from the disk - a copy of the medium's image in memory is forgotten by a reboot, so the cold reboot below would prove nothing"
fi
expect "$first" "btread: PASS" "a read client must list and scan with the read authority alone"
expect "$first" "btcheck: PASS pair" "the pairing must reach a bond and the cursor must move and click"
expect "$first" "btcheck: PASS deny" "a read connection must power nothing, and no client connection may answer the bond store's lookup"
expect "$first" "btcheck: PASS limits" "both services must run in Domains whose six limits are finite and the stated figures"
expect "$first" "btcheck: PASS exhaust" "the client table must refuse at its bound, give the slots back, and leave the pointer service answering"
expect "$first" "btcheck: PASS refund" "a stopped service's Domain must be gone rather than idle, its connections dead, and another pointer must still move and click"
limits_seen="$(grep -acF 'btcheck: PASS limits' "$first" || true)"
[[ "$limits_seen" == 2 ]] || fail "the restarted service's Domain was not checked again ($limits_seen limit checks passed; two are expected)"
expect "$first" "btcheck: PASS loss" "a transport lost mid-scan must end the scan and remove the controller, a returned one must be initialised again, and the old scan must be refused"
paired="$(grep -aoE 'bt-fixture: paired; key [0-9a-f]{8}' "$first" | tail -n 1 | awk '{print $NF}')"
[[ -n "$paired" ]] || fail "the fixture never reported a completed pairing"
echo "bluetooth-service: the fixture paired, key fingerprint $paired"
expect "$first" "bt-fixture: encryption with a remembered key $paired" "after the restart the stack must encrypt with the bonded key, not pair again"
expect "$first" "btcheck: PASS reuse" "after the restart the bond must be reused and the cursor must move"
reuses="$(grep -acF 'btcheck: PASS reuse' "$first" || true)"
[[ "$reuses" == 2 ]] || fail "the bond was reused $reuses times in the first boot; once after the restart and once after the transport came back are expected"
pairings="$(grep -acE 'bt-fixture: paired;' "$first" || true)"
[[ "$pairings" == 1 ]] || fail "the first boot paired $pairings times; the restart must reuse the bond rather than pair again"

# ---- boot two: the same disk, a cold reboot, reuse, then forget.
guest_gate_run $'btcheck reuse\nbtcheck forget' ""
second="$guest_gate_work/second"
cp "$GUEST_LINES" "$second"
if grep -aqE 'bt-fixture: paired;' "$second"; then
	fail "the second boot paired again; a cold reboot must reuse the stored bond"
fi
presented="$(grep -m 1 -aoE 'bt-fixture: encryption with a key this boot never paired, key [0-9a-f]{8}' "$second" | awk '{print $NF}')"
[[ -n "$presented" ]] || fail "the stack never encrypted on the second boot"
[[ "$presented" == "$paired" ]] || fail "after the cold reboot the stack presented key $presented, and the pairing produced $paired"
echo "bluetooth-service: after the cold reboot the stack presented the key the first boot's pairing produced ($presented)"
expect "$second" "btcheck: PASS reuse" "after the cold reboot the bond must be reused and the cursor must move"
expect "$second" "btcheck: PASS forget" "a forgotten bond must be gone and the mouse must no longer move the cursor"

echo "bluetooth-service: PASS - read denial, pairing, cursor, restart reuse, cold-reboot reuse and forget"
