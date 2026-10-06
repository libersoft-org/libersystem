#!/bin/bash
# Files over Bluetooth, against the in-guest fixture's phone and serial device: OBEX Object Push both ways, on L2CAP
# where the peer's record offers it and on RFCOMM where it does not, through `btctl send` and `btctl receive` and the
# shell's own redirections.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its OBEX server and client and the enhanced
# retransmission mode its L2CAP channel runs in (`drivers::bt_world`'s `opp`) are written apart from the host stack's
# `service_logic::obex` and `l2cap_bredr::Ertm` - its own packet reader, headers and frame check - so an object both
# report whole is one two implementations agree on. Bumble has no OBEX; with the owner's word on adopting it, the
# independent peer here is a harness peer on Bumble's L2CAP and RFCOMM, still this team's.
#
# WHAT IS ASSERTED:
#
#   nobody waiting       a push the phone tries while no receiver waits is refused at this host's door
#   sending, L2CAP       `btctl send PHONE big.txt < big.txt` - a 5 KiB file the shell read - reaches the phone over its
#                          GOEP L2CAP channel in enhanced retransmission mode, its size and its digest the phone's
#   sending, RFCOMM      the same file to the serial device, whose record offers RFCOMM alone: the same size and digest
#   receiving            `btctl receive PHONE 8192 > note.txt` takes the phone's 3100-byte push, names it on its
#                          diagnostics, and the file holds all of it - a hundred numbered lines, the last the hundredth
#   the bound            `btctl receive PHONE 1000` refuses the same push before a byte of it, and says so

set -euo pipefail
GUEST_GATE_NAME="bluetooth-transfer"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

# A FAILURE SAYS WHAT THE GUEST DID: the probe's, the tool's, the fixture's and the service's lines, before the work
# directory goes.
fail() {
	if [[ -n "${GUEST_LINES:-}" && -f "$GUEST_LINES" ]]; then
		grep -aE 'btclassic|btctl|bt-fixture|BluetoothService|PermissionManager|shell:|sent |[0-9]+ (big|note|small)\.txt|line [0-9]+' "$GUEST_LINES" >&2 || cat "$GUEST_LINES" >&2
	fi
	guest_gate_fail "$@"
}

guest_gate_require_programs bt_fixture btclassic btctl bluetooth_service bluetooth_bond_store sleepcheck wc tail

export QEMU_EXTRA="-device edu,addr=0x1d"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-180}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-320}"

phone="00:1b:dc:20:00:02"
serial="00:1b:dc:20:00:05"
guest_gate_run "btclassic transfer
sleepcheck count 10 > big.txt
wc -c big.txt
btctl send $phone big.txt < big.txt
btctl send $serial big.txt < big.txt
btclassic push 3100
btctl receive $phone 8192 > note.txt
wc -c note.txt
tail -n 1 note.txt
btclassic push 3100
btctl receive $phone 1000 > small.txt" ""
lines="$GUEST_LINES"

expect() {
	local line="$1" why="$2"
	grep -qF "$line" "$lines" || {
		echo "bluetooth-transfer: expected \"$line\" - $why" >&2
		grep -aE 'btclassic|btctl|bt-fixture|BluetoothService|sent |[0-9]+ (big|note)\.txt|line [0-9]+' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "bluetooth-transfer: $line"
}

expect "btclassic: PASS transfer" "the phone and the serial device must bond, and a push with no receiver waiting be refused"

size="$(grep -aoE '^[0-9]+ big\.txt' "$lines" | head -n 1 | awk '{print $1}')"
[[ -n "$size" ]] || fail "wc never reported the file's size"
over_l2cap="$(grep -aoE "bt-fixture: phone received big\.txt over L2CAP: [0-9]+ bytes, digest [0-9a-f]{8}" "$lines" | head -n 1)"
[[ -n "$over_l2cap" ]] || fail "the phone never received the file over L2CAP"
over_rfcomm="$(grep -aoE "bt-fixture: serial received big\.txt over RFCOMM: [0-9]+ bytes, digest [0-9a-f]{8}" "$lines" | head -n 1)"
[[ -n "$over_rfcomm" ]] || fail "the serial device never received the file over RFCOMM"
[[ "$over_l2cap" == *": $size bytes, digest "* ]] || fail "the phone received a different size than the file's $size bytes: $over_l2cap"
[[ "${over_l2cap##* digest }" == "${over_rfcomm##* digest }" && "$over_rfcomm" == *": $size bytes, digest "* ]] || fail "the two transports delivered different bytes: $over_l2cap / $over_rfcomm"
expect "sent \"big.txt\", $size bytes, to $phone bredr" "btctl must report the phone's push with the file's size"
expect "sent \"big.txt\", $size bytes, to $serial bredr" "btctl must report the serial device's push with the file's size"
echo "bluetooth-transfer: the $size-byte file reached the phone over L2CAP and the serial device over RFCOMM, digest ${over_l2cap##* digest } both times"

expect "btctl: receiving \"fixture-note.txt\" from $phone bredr: 3100 bytes, type \"text/plain\"" "the receiver must name the object as the phone gave it"
expect "btctl: received 3100 bytes" "the receiver must take the whole object"
expect "3100 note.txt" "the file the redirection wrote must hold the whole object"
expect "line 00100 of the phone's note" "the file's last line must be the phone's hundredth"
expect "bt-fixture: phone pushed fixture-note.txt: 3100 bytes" "the phone must see its push answered success"

expect "btctl: the object did not arrive whole: 0 bytes received" "an object past the receiver's bound must be refused before a byte of it"
expect "bt-fixture: phone's push was answered 0xcd" "the phone must be told the object is too large"

echo "bluetooth-transfer: PASS - Object Push sent on L2CAP and RFCOMM and received through the shell's redirections, refused with nobody waiting and past the receiver's bound"
