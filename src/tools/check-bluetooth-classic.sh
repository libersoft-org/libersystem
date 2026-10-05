#!/bin/bash
# BR/EDR, end to end, against the in-guest fixture's five classic devices - across a service restart and a
# cold reboot of the same volume.
#
# THE ORACLE IS THE IN-GUEST FIXTURE, WRITTEN BY THE SAME TEAM, AND SAID SO. Its devices' L2CAP, SDP, RFCOMM and
# pairing half are `drivers::bt_world`, written apart from the host stack's `service_logic` and held to the
# specification's own frames by their own host tests; an independent radio and independent peers are the
# milestone's preferred oracle and wait on the owner's word about adopting them.
#
# WHAT IS ASSERTED, AND WHO SAYS SO. What the radio did is the fixture's account; what the host reports is
# the host's:
#
#   policy at rest            a dual-mode controller with nothing trusted and no watcher is neither
#                               connectable nor discoverable; `discoverable 2` scans for inquiries and stops
#   one scan, both radios     inquiry beside the LE scan finds the five devices with their names, classes
#                               and service classes, and the LE mouse
#   every BR/EDR model        Numeric Comparison (the digits the phone shows are the prompt's), Passkey
#     through the prompts       Entry (this host shows them, keypresses reach the watcher), Just Works
#                               outgoing (no prompt), each at the level it earns; NoInputNoOutput declared
#                               with no watcher, and a P-192 device bonded at P-192
#   pairable only watched     a watcher makes the radio connectable; an incoming pairing is refused with
#                               nobody watching and asks consent with a watcher
#   a PIN on request only     legacy pairing refused until `pair-legacy`, then a PIN prompt and a legacy bond
#   no downgrade              P-192 over a P-256 bond, and Just Works over an authenticated one, refused with
#                               the bonds kept
#   the inbound policy        a trusted keyboard's channel waits for its link to be secured with the stored
#                               key; HID refused for security to a peer not trusted for input; RFCOMM refused
#                               without voice trust; a channel no role serves refused; SDP answers anyone
#   SDP and RFCOMM, client    `connect serial spp` pages, secures, searches the device's records, opens
#                               channel 3, and credits flow both ways; an idle link goes into sniff
#   bonds outlive it          after a restart and a cold reboot the trusted keyboard's page is taken and
#                               secured with the stored key - the fingerprint the first boot's pairing made -
#                               and nothing pairs again; after a forget its page is refused

set -euo pipefail
GUEST_GATE_NAME="bluetooth-classic"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "this gate is the x86_64 one; the ports cross-build the stack and do not run it"

fail() { guest_gate_fail "$@"; }

guest_gate_require_programs bt_fixture btclassic bluetooth_service bluetooth_bond_store

# THE FIXTURE'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x1d"
# ONE DISK FOR BOTH BOOTS: the cold reboot is only a reboot of the same volume if the second boot attaches the
# disk the first one wrote.
[[ -f "$root/../.build/boot/system-volume-bootable-x86_64.img" ]] || fail "there is no bootable system volume beside the image - build it:  LIBER_DEVELOPMENT=1 ./image.sh"
export RUN_DISK="$guest_gate_work/system.img"
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-240}"
export GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-400}"

expect() {
	local lines="$1" line="$2" why="$3"
	grep -qF "$line" "$lines" || {
		echo "bluetooth-classic: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'btclassic|bt-fixture|BluetoothService|BluetoothBondStore' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
	echo "bluetooth-classic: $line"
}

# ---- boot one: the pairings, the policy, and a restart of the service.
guest_gate_run $'btclassic pair\nbtclassic policy\nstop bluetooth_service\nstart bluetooth_service\nbtclassic reuse' ""
first="$guest_gate_work/first"
cp "$GUEST_LINES" "$first"
if grep -aqF "vol://system is a live copy in memory" "$first" || ! grep -aqF "boot: the system volume is a paired block volume" "$first"; then
	fail "the system did not run from the disk - the cold reboot below would prove nothing"
fi
expect "$first" "btclassic: PASS pair" "every BR/EDR pairing model must run through the prompts at its level, and no downgrade may pass"
expect "$first" "btclassic: PASS policy" "the inbound policy, SDP and RFCOMM with credits must hold"
expect "$first" "btclassic: PASS reuse" "after the restart the trusted keyboard must be secured with its stored key"
paired="$(grep -aoE 'bt-fixture: paired keyboard type 0x08 key [0-9a-f]{8}' "$first" | tail -n 1 | awk '{print $NF}')"
[[ -n "$paired" ]] || fail "the fixture never reported the keyboard's pairing"
echo "bluetooth-classic: the keyboard paired, key fingerprint $paired"
after_restart="$guest_gate_work/first-after-restart"
sed -n '/btclassic: PASS policy/,$p' "$first" >"$after_restart"
expect "$after_restart" "bt-fixture: authenticated keyboard with a remembered key $paired" "after the restart the stack must present the keyboard's bonded key"
if grep -aqE 'bt-fixture: paired keyboard' "$after_restart"; then
	fail "the keyboard paired again after the restart"
fi

# ---- boot two: the same disk, a cold reboot, reuse, then forget.
guest_gate_run $'btclassic reuse\nbtclassic forget' ""
second="$guest_gate_work/second"
cp "$GUEST_LINES" "$second"
presented="$(grep -m 1 -aoE 'bt-fixture: authenticated keyboard with a key this boot never paired, key [0-9a-f]{8}' "$second" | awk '{print $NF}')"
[[ -n "$presented" ]] || fail "the stack never secured the keyboard's link on the second boot"
[[ "$presented" == "$paired" ]] || fail "after the cold reboot the stack presented key $presented, and the pairing produced $paired"
echo "bluetooth-classic: after the cold reboot the stack presented the key the first boot's pairing produced ($presented)"
expect "$second" "btclassic: PASS reuse" "after the cold reboot the bond and its input trust must be used"
expect "$second" "btclassic: PASS forget" "a forgotten keyboard's page must be refused"
if grep -aqE 'bt-fixture: paired ' "$second"; then
	fail "something paired on the second boot; the bonds must be reused"
fi

echo "bluetooth-classic: PASS - the policy at rest, inquiry, every BR/EDR pairing model and its level, no downgrade, the inbound policy, SDP and RFCOMM with credits, sniff, and bonds across a restart and a cold reboot"
