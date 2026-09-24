#!/bin/bash
# THE TRUSTED ADMINISTRATIVE PATH, cold, in a development image - one person's confirmation, on a protected
# screen, of one bound operation, and at most one attempt at it.
#
# WHAT IT RUNS. `lab.sh scenario-cold` builds the development image, boots it with the in-guest executor's
# QEMU test device at its pinned address and a persistent system volume, and drives `admin-path.toml`: every
# decision is a key through QEMU's emulated keyboard, never the console `input` shortcut. The scenario
# asserts each probe's own verdicts in order; this script then checks what the scenario cannot:
#
#   presentation      the frames captured while the protected screen was up are the protected screen - its
#                       field over most of the frame, its band and its text - and the hostile client's
#                       green, painted the whole time, is nowhere in them
#   effects           the executor performed exactly the four writes four confirmed attempts made, and the
#                       probes reported no failure
#   the journal       three instances - boot, restart and reboot - with strictly rising epochs, and the two
#                       later ones recovered the committed records
#   the backends      the chosen keyboard and display backends were the ones the path trusts, and the
#                       executor was reached only through AdminService's catalogue connection
#
# WHAT IT DOES NOT CLAIM. The executor is a development fixture: this proves the generic seam, not USB DFU,
# and an x86_64 fixture proves nothing about physical approval on another architecture without the
# configured trusted keyboard and display there.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
fail() {
	echo "qemu-admin-path: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "this gate is the x86_64 one; the ports cross-build the shared contracts and do not run it"
	;;
*) fail "unexpected argument '$1'" ;;
esac

scenario="$root/harness/scenarios/admin-path.toml"
log="$repo/.build/boot/cold-x86_64.log"
idle="$repo/.build/boot/admin-path-idle.ppm"
prompt="$repo/.build/boot/admin-path-prompt.ppm"
rm -f "$idle" "$prompt"

# THE EXECUTOR'S DEVICE, at the address its registry entry pins.
export QEMU_EXTRA="-device edu,addr=0x18"
# THE SYSTEM DISK, the volume the medium is paired with: the journal recovered after the scenario's
# reset is evidence only if it was written to the volume the next boot runs from, and a copy of the
# medium's own image in memory is forgotten by the reset.
disk_dir="$(mktemp -d)"
trap 'rm -rf "$disk_dir"' EXIT
export RUN_DISK="$disk_dir/system.img"
"$repo/lab.sh" scenario-cold x86_64 "$scenario" || fail "the scenario failed (serial log: $log)"
[[ -f "$log" ]] || fail "the scenario left no serial log at $log"

# WHAT WAS RUN, by digest, so the evidence below names the build it is evidence about.
image="$repo/.build/image/x86_64-unknown-none"
built="$repo/.build/cargo/user/x86_64-unknown-none/debug"
for artifact in "$repo/.build/cargo/kernel/x86_64-unknown-none/debug/kernel" "$repo/.build/boot/system-volume-bootable-x86_64.img" "$built/admin_service" "$image/libexec/permission_manager" "$image/libexec/display_service" "$image/libexec/input_service" "$built/admin_fixture" "$built/admincheck" "$scenario"; do
	if [[ -f "$artifact" ]]; then
		echo "qemu-admin-path: input $(sha256sum "$artifact" | cut -c1-16) ${artifact#"$repo/"}"
	else
		echo "qemu-admin-path: input missing ${artifact#"$repo/"}"
	fi
done

expect() {
	grep -aqF "$1" "$log" || fail "expected \"$1\" in the guest log - $2"
}

if grep -aE 'admincheck: FAIL|adminhelper: FAIL|adminhostile: FAIL' "$log" >&2; then
	fail "a probe reported a failure"
fi
if grep -aqF "vol://system is a live copy in memory" "$log" || ! grep -aqF "boot: the system volume is a paired block volume" "$log"; then
	fail "the system did not run from the disk, so the recovery after the reset would prove nothing"
fi

# THE BACKENDS. The fixture is bound and registered through the catalogue connection AdminService holds; the
# keyboard driver that fed the trusted sink and the display that showed the frames are the ones this image
# trusts for this property.
expect "driver.admin-fixture: online" "the executor fixture must bind"
expect "AdminService: an executor was registered for probe-write" "AdminService must reach the executor through its catalogue connection"
expect "AdminService: the trusted keyboard is idle and the session is armed" "the trusted keyboard must arm a session"

# THE EFFECTS: four writes, from four confirmed attempts, and nothing else.
writes="$(grep -ac 'driver.admin-fixture: the probe write was performed' "$log" || true)"
[[ "$writes" == 4 ]] || fail "the executor performed $writes writes; four confirmed attempts were made"
echo "qemu-admin-path: the executor performed exactly the four writes four confirmations authorized"

# THE JOURNAL ACROSS A RESTART AND A REBOOT.
mapfile -t online < <(grep -aoE 'AdminService: online, epoch [0-9]+, [0-9]+ journal records recovered' "$log")
((${#online[@]} == 3)) || fail "expected three AdminService instances - boot, restart and reboot - and saw ${#online[@]}"
previous=0
for at in 0 1 2; do
	epoch="$(awk '{print $4}' <<<"${online[$at]}" | tr -d ,)"
	recovered="$(awk '{print $5}' <<<"${online[$at]}")"
	((epoch > previous)) || fail "epoch $epoch did not follow $previous: a restart must start a fresh epoch"
	previous="$epoch"
	if ((at > 0)) && ((recovered == 0)); then
		fail "instance $((at + 1)) recovered no journal records"
	fi
	echo "qemu-admin-path: instance $((at + 1)) epoch $epoch recovered $recovered records"
done

# THE FRAMES. P6 PPM, as QEMU's `screendump` writes it.
python3 - "$idle" "$prompt" <<'EOF' || fail "the captured frames are not the protected screen"
import sys

FIELD = (0x10, 0x20, 0x50)
BAND = (0xff, 0xb0, 0x00)
TEXT = (0xff, 0xff, 0xff)
GREEN = (0x00, 0xff, 0x00)


def pixels(path):
	with open(path, 'rb') as handle:
		data = handle.read()
	fields = []
	at = 0
	while len(fields) < 4:
		while data[at:at + 1].isspace():
			at += 1
		if data[at:at + 1] == b'#':
			at = data.index(b'\n', at) + 1
			continue
		end = at
		while not data[end:end + 1].isspace():
			end += 1
		fields.append(data[at:end])
		at = end
	if fields[0] != b'P6' or fields[3] != b'255':
		raise SystemExit(f'{path}: not an 8-bit P6 frame')
	width, height = int(fields[1]), int(fields[2])
	body = data[at + 1:at + 1 + width * height * 3]
	counts = {}
	for index in range(0, len(body), 3):
		key = (body[index], body[index + 1], body[index + 2])
		counts[key] = counts.get(key, 0) + 1
	return width * height, counts


for path in sys.argv[1:]:
	total, counts = pixels(path)
	field = counts.get(FIELD, 0) / total
	print(f'qemu-admin-path: {path.rsplit("/", 1)[-1]}: field {field:.0%}, band {counts.get(BAND, 0)} px, text {counts.get(TEXT, 0)} px, hostile green {counts.get(GREEN, 0)} px')
	if field < 0.5 or counts.get(BAND, 0) == 0 or counts.get(TEXT, 0) == 0:
		raise SystemExit(f'{path}: the protected screen is not what the display showed')
	if counts.get(GREEN, 0) != 0:
		raise SystemExit(f'{path}: the hostile client painted over the protected screen')
EOF

echo "qemu-admin-path: PASS - one confirmation through the emulated keyboard, on a protected screen nothing else could cover, authorized one attempt at one bound operation; refusal, contention, an unavailable path, expiry, replay, a forged grant, a wrong or replaced target, a substituted payload, an ended owner however its endpoints were delegated, held acknowledgments, a failing journal and a restarted service authorized nothing; and the decisions came back after a reboot (the generic seam against the in-guest executor; not USB DFU)"
