#!/usr/bin/env bash
# THE SECOND PORT, PROVED BY THE PORT'S OWN CHARDEV.
#
# WHAT THIS GATE IS. One `virtio-serial-pci` carrying a `virtconsole` AND a named `virtserialport`,
# which is a topology only a driver that negotiates `VIRTIO_CONSOLE_F_MULTIPORT` can use at all: a
# generic port is opened through the CONTROL QUEUE, and without the feature there is no control
# queue. A single-port driver does not see the port, cannot open it and cannot write to it - so the
# line this gate reads off the port's own capture is an effect the feature produces and nothing else
# does.
#
# AND IT IS READ OFF THE RIGHT FILE. The two ports have two chardevs, so "the bytes went to the port
# they were addressed to" is checkable rather than assumed: the stamp must be on the generic port's
# capture and must NOT be on the console's.
#
# A PORT THAT OPENS IS NOT YET A PORT ANYTHING CAN USE, which is the other half this checks. The
# control queue can open a port that nothing in the image is able to reach - that is what this driver
# did when the control half landed - so the gate reads the driver's SERVED count and the manager's
# published `console-bytes` count as well as the bytes on the wire.
#
# THROUGH THE TEST HARNESS RATHER THAN A HAND-BUILT QEMU LINE, for the reason the iommu gate gives:
# the profile inherits the harness's timeouts, log collection and staleness checks instead of a
# second copy of all three that can drift from them.
set -euo pipefail
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
# shellcheck source=/dev/null
source "$HERE/result-logs.sh"
BUILD="$(pwd)/.build/boot"

fail() {
	echo "qemu-virtio-multiport: $*" >&2
	exit 1
}

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
devices="$(qemu-system-x86_64 -device help 2>/dev/null || true)"
for device in virtio-serial-pci virtserialport virtconsole; do
	case "$devices" in
	*"name \"$device\""*) ;;
	*) fail "this QEMU has no $device, and multiport is not testable without it" ;;
	esac
done

work="$(mktemp -d "${TMPDIR:-/tmp}/virtio-multiport.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# THE CAPTURES THIS RUN WRITES ARE THE ONES THIS RUN READS. Each boot names its files with its own
# pid, so the newest pair after the run is this run's - and the sweep in the harness removes the ones
# whose run is gone. Recorded BEFORE the boot so a pre-existing file cannot be mistaken for evidence.
# NEWEST FIRST WITHOUT A PIPE INTO A READER THAT STOPS EARLY. `ls | head` under `pipefail` reads a
# successful search as a failed pipeline the moment the reader closes, which is the trap the hygiene
# gate names - so the listing goes into an array and the first element is taken from it.
newest() {
	local pattern="$1" found=()
	mapfile -t found < <(ls -t $pattern 2>/dev/null)
	printf '%s' "${found[0]:-}"
}
before_console="$(newest "$BUILD/virtio-console-test.*.out")"
before_port="$(newest "$BUILD/virtio-serialport-test.*.out")"

# THE BOOT TAG AND NOT THE SMOKE ONE, because what this gate reads is what the DRIVERS say: the smoke
# selection runs before the device chain has reported, so its guest log carries no driver line at all.
echo "qemu-virtio-multiport: booting the test machine, whose virtio-serial bus carries two ports"
./test.sh --arch x86_64 --tags boot >"$work/run.log" 2>&1 || {
	echo "qemu-virtio-multiport: the boot suite failed on the two-port machine" >&2
	tail -20 "$work/run.log" >&2
	exit 1
}

mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"
log="$work/run.result"
cat "${logs[@]}" >"$log"

# 1. THE DRIVER FOUND BOTH PORTS AND SERVES THE GENERIC ONE. `multiport 2/2/1` is announced over
#    open over SERVED on the two-port device, and a driver without the feature reports `one port` -
#    so this line alone separates the two. The third number is the half the control queue does not
#    prove on its own: a port can be open at the device with nothing in this image able to reach it,
#    which is exactly what this driver did before the port became a byte stream.
grep -aq "driver.virtio-console: online (.*multiport 2/2/1" "$log" || {
	echo "qemu-virtio-multiport: the driver did not report two ports found, open and one of them served" >&2
	grep -a -m 5 "virtio-console" "$log" >&2 || true
	exit 1
}

# 2. AND THE PUBLICATION REACHED THE CATALOGUE. The driver's own report says what the driver
#    believes; this is the manager counting what it actually holds, which is what a consumer can
#    then ask for. A stream the driver serves and the catalogue never published is reachable by
#    nobody, and the driver cannot tell that from success.
grep -aq "DeviceManager: providers published after every device - .*console-bytes" "$log" || {
	echo "qemu-virtio-multiport: the served port was not published as a console-bytes provider" >&2
	grep -a -m 5 "providers published" "$log" >&2 || true
	exit 1
}

console="$(newest "$BUILD/virtio-console-test.*.out")"
port="$(newest "$BUILD/virtio-serialport-test.*.out")"
[[ -n "$port" && "$port" != "$before_port" ]] || fail "this boot wrote no generic-port capture"
[[ -n "$console" && "$console" != "$before_console" ]] || fail "this boot wrote no console capture"

# 3. THE BYTES WENT TO THE PORT THEY WERE ADDRESSED TO, which is the whole claim: on the generic
#    port's own chardev, and on no other.
stamp="virtio-console: generic port open, this is port"
grep -aq "$stamp" "$port" || {
	echo "qemu-virtio-multiport: the generic port's capture carries nothing the driver wrote" >&2
	head -c 400 "$port" >&2 || true
	exit 1
}
if grep -aq "$stamp" "$console"; then
	fail "the generic port's line is on the CONSOLE's capture too, so the ports are not separate streams"
fi

# 4. AND THE CONSOLE IS STILL THE CONSOLE. The feature must not cost the port that ships today: the
#    banner is written over port 0 after the handshake, and it is what a console capture holds.
grep -aq "virtio-console driver online" "$console" || {
	echo "qemu-virtio-multiport: the console port lost its own output under multiport" >&2
	head -c 400 "$console" >&2 || true
	exit 1
}

echo "qemu-virtio-multiport: two ports found and opened through the control queue, the generic one published as a byte stream, and each port's bytes on its own chardev"
