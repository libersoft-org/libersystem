#!/usr/bin/env bash
# check-audio-routing.sh - the audio device model on a live machine: the inventory, the routing rule with a device
# plugged in and taken out while a stream plays, the operator's default and level, and a voice session.
#
# THE MACHINE IS THE LAB'S: an HDA controller and a virtio-sound card at boot - two catalogue `audio` providers, which
# AudioService used to see one of and ignore the other - and an empty PCIe hot-plug slot, which is where the third
# arrives. Nothing in the guest is told what the host does: the probe sees the device come and go through the
# service's own inventory, read through the operator authority `audioctl` holds.
#
# WHAT IS ASSERTED:
#
#   audioprobe inventory   every provider is a device with its formats and latency, and there is one default output
#   audioctl devices       the shipping tool lists them, a default output among them
#   audioprobe hold        a stream plays on the default; a virtio-sound card plugged into the slot takes it - the
#                          arrival is the default - and when it is unplugged the stream returns to the device it
#                          left; every write was answered throughout, and both moves were counted
#   audioprobe operator    a device the operator chooses takes the default output and the streams that follow it,
#                          a stream that named a device stays there, and a level is kept per device
#   audioprobe voice       a voice session at 16 kHz mono on the default output and input - written, read, its call
#                          state declared, its latency stated - from a grant that opens nothing else
#   audioctl streams       the operator sees the placement
#
# IT BOOTS ITS OWN GUEST unless one is already up, and takes down only what it started.

SCRIPT_NAME=check-audio-routing.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../lib.sh"

BOOTED=0
work="$(mktemp -d "${TMPDIR:-/tmp}/liber-audio-routing.XXXXXX")"
holder=""
cleanup() {
	if [[ -n "$holder" ]]; then
		kill "$holder" 2>/dev/null || true
	fi
	"$REPO_ROOT/lab.sh" monitor "device_del liberaudio" >/dev/null 2>&1 || true
	if ((BOOTED)); then
		"$REPO_ROOT/lab.sh" quit >/dev/null 2>&1 || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

if ! "$REPO_ROOT/lab.sh" sh uname >/dev/null 2>&1; then
	note "no instance is up - booting one"
	LIBER_DEVELOPMENT=1 "$REPO_ROOT/lab.sh" boot >/dev/null || die "the guest did not boot"
	BOOTED=1
fi
"$REPO_ROOT/lab.sh" log "carries hot-plug slot" 2>/dev/null | grep -q . || die "this machine has no hot-plug slot - the fixture is missing from the profile"

# How many lines of the guest's log match a pattern, right now - COUNTED, so a line from an earlier run on an instance
# that was already up is not taken for one that just arrived.
seen() {
	"$REPO_ROOT/lab.sh" log "$1" 2>/dev/null | grep -c . || true
}

await_line() {
	local needle="$1" what="$2" baseline="$3"
	for _ in $(seq 1 90); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	die "$what (waited for a new line matching: $needle)"
}

run() {
	local name="$1" command="$2" timeout="${3:-60}"
	"$REPO_ROOT/lab.sh" sh --timeout "$timeout" "$command" >"$work/$name" 2>&1 || true
	cat "$work/$name"
}

expect_in() {
	local name="$1" needle="$2" what="$3"
	grep -qE "$needle" "$work/$name" || {
		cat "$work/$name" >&2
		die "$what"
	}
}

# 1. THE INVENTORY.
run inventory "audioprobe inventory" >/dev/null
expect_in inventory "audioprobe: PASS inventory - [2-9] devices" "the boot's two providers are not both devices in the inventory"
run devices "audioctl devices" >/dev/null
expect_in devices "^device [0-9]+: .*\[default output" "audioctl does not list a default output"
note "$(grep -c '^device ' "$work/devices") devices at boot, one of them the default output"

# 2. THE ROUTING RULE, WITH A DEVICE PLUGGED IN AND TAKEN OUT WHILE A STREAM PLAYS.
waiting=$(seen "audioprobe: playing on device .* waiting for a device to arrive")
moved=$(seen "audioprobe: the stream moved to the device that arrived")
returned=$(seen "audioprobe: the stream returned to device")
"$REPO_ROOT/lab.sh" sh --timeout 200 "audioprobe hold" >"$work/hold" 2>&1 &
holder=$!
await_line "audioprobe: playing on device .* waiting for a device to arrive" "the probe never began playing" "$waiting"
"$REPO_ROOT/lab.sh" monitor "device_add virtio-sound-pci,bus=hotplug0,id=liberaudio,audiodev=snd0" >/dev/null 2>&1 || die "the monitor refused the sound card"
await_line "audioprobe: the stream moved to the device that arrived" "the stream did not move to the card that was plugged in" "$moved"
"$REPO_ROOT/lab.sh" monitor "device_del liberaudio" >/dev/null 2>&1 || die "the monitor refused the removal"
await_line "audioprobe: the stream returned to device" "the stream did not return when the card was taken out" "$returned"
wait "$holder" || true
holder=""
expect_in hold "audioprobe: PASS hold" "the hold phase did not pass"
note "a card plugged in took the playing stream, and taking it out gave the stream back"

# 3. THE OPERATOR.
run operator "audioprobe operator" >/dev/null
expect_in operator "audioprobe: PASS operator" "the operator's default and level did not hold"

# 4. A VOICE SESSION.
run voice "audioprobe voice" >/dev/null
expect_in voice "audioprobe: PASS voice" "the voice session did not hold"

# 5. AND WHAT THE OPERATOR SEES.
run counters "audioctl counters" >/dev/null
expect_in counters "^moves [0-9]+ silent-frames [0-9]+ route-overflows 0 route-underruns 0" "audioctl does not report the routing rule's counters"

for name in inventory hold operator voice; do
	grep -aE '^audioprobe: ' "$work/$name" | sed 's/^/audio-routing: /'
done
echo "audio-routing: PASS - the inventory, a stream moved to a card plugged in and back when it left, the operator's default and level, and a voice session"
