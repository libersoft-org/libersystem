#!/bin/bash
# LE beyond one mouse, against the in-guest fixture's LE devices: every pairing model through the prompts, privacy
# both ways, several links, this host's attribute server, and bonded peripherals reconnecting through the accept list.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO: its LE devices' Security Manager half, key
# distribution, private addresses and attribute servers are `drivers::bt_le_world`, on `bt_peer`'s own AES, CMAC and
# key functions - held to the specification's sample data apart from the host stack's.
#
# WHAT IS ASSERTED (`btclassic le`):
#
#   privacy                    the tag is heard from a resolvable private address and bonded under the identity it
#                                gives with its resolving key; this host scans and connects from a private address of
#                                its own, which the tag resolves with the identity this host gave it
#   every LE model             Numeric Comparison with the tag (the prompt's digits are the tag's), Passkey Entry
#                                this host types for the display, legacy Passkey Entry this host shows for the
#                                remote - refused until `pair-legacy`, never over a Secure Connections bond
#   several links              the tag, the display and the remote connected at once
#   the attribute server       the tag reads this host's name through it
#   the battery                the tag's Battery Service, read by the stack itself, is its level in its status
#   reconnection               the tag and the remote, trusted for input, come back through the accept list - the
#                                tag named by its identity through the resolving list - each encrypted on the key its
#                                pairing made, the remote's with its EDIV and Rand
#   an application's GATT      `btgatt`, granted a GATT client by its policy row on the peer aliased `tag-1` for the
#     client                     battery and a custom service: those two listed and neither GAP nor GATT, the battery
#                                level read, a handle outside the grant refused, a write and the peer's notification
#   the other radio's key      `btclassic ctkd`, with the dual-mode phone: paired on BR/EDR, its LE key derived over
#                                the BR/EDR Security Manager and its LE half encrypted on it; paired anew on LE, its
#                                BR/EDR key derived from the LTK and its BR/EDR half authenticated on it - each time
#                                the fixture deriving the same key with its own functions

set -euo pipefail
GUEST_GATE_NAME="bluetooth-le"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs bt_fixture btclassic btgatt bluetooth_service bluetooth_bond_store

export QEMU_EXTRA="-device edu,addr=0x1d"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-160}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-330}"

guest_gate_run $'btclassic le\nbtgatt\nbtclassic ctkd' ""
lines="$GUEST_LINES"
for verdict in "btclassic: PASS le" "btgatt: PASS" "btclassic: PASS ctkd"; do
	grep -qF "$verdict" "$lines" || {
		echo "bluetooth-le: expected \"$verdict\"" >&2
		grep -aE 'btclassic|btgatt|bt-fixture|BluetoothService|PermissionManager' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
done
grep -aE '^(btclassic|btgatt): ' "$lines" | sed 's/^/bluetooth-le: /'
echo "bluetooth-le: PASS - privacy both ways, Numeric Comparison, Passkey Entry and legacy on request, several links, the attribute server, reconnection through the accept list, an application's GATT grant, and keys derived across transports both ways"
