#!/usr/bin/env bash
# DISPLAY BRIGHTNESS THROUGH THE FIRMWARE: a laptop's panel as ACPI's video extension describes it, on the ACPI fixture
# (`acpi-fixture.py --brightness`) - a video adapter that is q35's own VGA function (`VGA=std`, so the boot framebuffer
# lies in its BAR), opened with `Scope` on QEMU's node for it with a `_DOS` that records its argument and a `_DOD`; below
# it the panel's output with `_BCL`, a `_BCM` that writes the region and a `_BQC` that reads it; and an `ACPI0008` light
# sensor whose `_ALI` reads the region. A `consumer-keys` gadget (`keys-sim.py`) is the keyboard's brightness keys.
#
#   the join     output 0's source the boot framebuffer decoded by 00:01.0, and the panel joined to it as
#                `firmware-adapter`; `_DOS` saw 0x04; the region starts at the firmware's AC default
#   the levels   a set reaches the region; 0x86, 0x87, 0x85 and 0x88 raised through the fixture's notification path each
#                move it one step - 0x88 to the floor - and 0x89 changes nothing
#   the keys     the gadget's up, up, down move it one step each
#   automatic    on, the level follows the illuminance the harness writes and announces with 0x80, through `_ALR`'s curve
#   restore      automatic brightness off, a level set and held past the two-second settle; after a reset on the same
#                volume - the region back at the firmware default, as a machine's power-on leaves it - that level is
#                restored, and the settings are still the first boot's
#   a sleep      a suspend to idle and an S3 cycle, each answered SUSPENDED and RESUMED by both drivers; in S3 the region
#                is put back at the firmware default and `_DOS`'s record cleared, and after the resume `_DOS` saw 0x04 again
#                and the level set before the sleep is back
#
# THE RUN BEGINS WITH `idle off`, stored, so no dim in either boot moves a level a check compares.
#
# IT BOOTS ITS OWN INSTANCE in private state and takes it down from the EXIT trap. THE GADGET IS A PERMISSION (the
# owner's, 2026-09-21, with the rules `usb-gadget.sh` enforces): without root and `dummy_hcd` this gate FAILS.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "brightness-acpi: $*" >&2
	exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-brightness-acpi.XXXXXX")"
fixture="$state/fixture"
mkdir -p "$fixture"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/brightness-acpi"
rm -rf "$kept"
mkdir -p "$kept"
backend_pid=""
keys_pid=""
gadget=""

cleanup() {
	local status=$?
	cp -f "$state/dev-serial.log" "$kept/serial.log" 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "brightness-acpi: teardown reported a problem (see $kept/down.log)" >&2
	for pid in "$backend_pid" "$keys_pid"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	if [[ -n "$gadget" ]]; then
		src/harness/usb-gadget.sh teardown >>"$state/gadget.log" 2>&1 || echo "brightness-acpi: the gadget's teardown reported a problem" >&2
		src/harness/usb-gadget.sh verify >>"$state/gadget.log" 2>&1 || echo "brightness-acpi: the host still carries something of the gadget's" >&2
	fi
	cp -f "$state"/*.log "$fixture"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

serial_log() {
	echo "$state/dev-serial.log"
}

seen() {
	grep -a -c -F -- "$1" "$(serial_log)" 2>/dev/null || true
}

await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line containing: $needle)"
}

launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# One probe case, its output kept, and a line it must print.
probe() {
	local expected="$1" what="$2"
	shift 2
	local output
	output="$(launch brightcheck "$@")" || true
	printf '%s\n' "$output" >>"$state/probes.log"
	grep -aqE -- "$expected" <<<"$output" || {
		printf '%s\n' "$output" >&2
		fail "$what (brightcheck $* printed no line matching: $expected)"
	}
	echo "brightness-acpi: brightcheck $* - $(grep -aE -- "$expected" <<<"$output" | sed -n '1p')"
}

# THE REGION'S VALUES, as JSON keys.
region() {
	python3 src/harness/acpi-fixture.py --brightness-read "$fixture/ivshmem.bin" | python3 -c "import json, sys; print(json.load(sys.stdin)['$1'])"
}

# The region's level reaching `want` within `limit` seconds.
await_level() {
	local want="$1" what="$2" limit="${3:-20}" level=""
	for _ in $(seq 1 $((limit * 5))); do
		level="$(region level)"
		[[ "$level" == "$want" ]] && {
			echo "brightness-acpi: the region holds $want - $what"
			return 0
		}
		sleep 0.2
	done
	fail "$what: the region holds $level, not $want"
}

# A notification raised through the fixture's path: the value in the event byte, its line raised.
notify() {
	python3 src/harness/acpi-fixture.py --brightness-event "$fixture/ivshmem.bin" "$fixture/control.sock" "$1" >>"$state/events.log" 2>&1 || fail "the notification $1 fired no event in the GPIO backend"
}

# THE FIXTURE, ITS BACKEND AND THE KEYPAD.
python3 src/harness/acpi-fixture.py --out "$fixture/fixture.aml" --memory "$fixture/ivshmem.bin" --brightness
python3 src/harness/vhost-i2c-gpio.py --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$fixture/backend.log" 2>&1 &
backend_pid="$!"
for _ in $(seq 1 100); do
	[[ -e "$fixture/ready" ]] && break
	sleep 0.05
done
[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start (see $kept/backend.log)"
if ! USB_GADGET_ID="$(src/harness/usb-gadget.sh setup consumer-keys 2>>"$state/gadget.log")"; then
	cat "$state/gadget.log" >&2
	fail "the consumer-keys gadget could not be built - it needs root and the host's dummy_hcd and usb_f_hid"
fi
gadget=1
KEYS_SIM_TRIGGER="$state/press" python3 src/harness/keys-sim.py 2>"$state/keys-sim.log" &
keys_pid="$!"
export USB_GADGET_ID USB_GADGET_PORT=3
export ACPI_FIXTURE="$fixture/fixture.aml" ACPI_FIXTURE_MEMORY="$fixture/ivshmem.bin"
export I2C_FIXTURE=bus I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
export VGA=std
# A SYSTEM DISK THAT OUTLIVES THE RESET: the stored level and settings are ConfigService's, written through to the
# volume, and a boot from the medium's own volume keeps nothing across a reset.
export RUN_DISK="$state/system.img"
# S3 OFFERED, as a laptop's firmware offers it: q35 hides it by default.
export QEMU_EXTRA="-global ICH9-LPC.disable_s3=0"

echo "brightness-acpi: boot (state $state, host port $HOSTFWD_PORT)"
if ! ./dev.sh up --timeout 400 >"$state/up.log" 2>&1; then
	tail -20 "$state/up.log" >&2
	fail "the development instance did not come up (see $kept/up.log)"
fi
await_line "AcpiService: online - instance 1" "the ACPI service never came online" 0

# FIRST, SO NO DIM MOVES A LEVEL THIS GATE COMPARES.
probe "brightcheck: idle off" "the idle timeout must go off first" idle-off
probe "brightcheck: active backlight acpi:.*LCD0 kind=Firmware output=0 reason=firmware-adapter standing=active" "the panel must join output 0 by its adapter" wait 60
probe "brightcheck: output 0 source boot-framebuffer decoder 00:01.0" "output 0 must be the boot framebuffer, decoded by q35's VGA function" outputs
[[ "$(region dos)" == "4" ]] || fail "_DOS saw $(region dos), not 0x04"
echo "brightness-acpi: the adapter's _DOS saw 0x04 ($(region dos_calls) call(s))"
await_level 70 "the panel starts at the firmware's AC default"

# A SET, AND THE FIRMWARE'S HOTKEYS: one step each, 0x88 to the floor, 0x89 nothing.
probe "brightcheck: set level=40" "a set must be applied" set 40
await_level 40 "the set reached the region"
notify 0x86
await_level 50 "0x86 stepped up"
notify 0x87
await_level 40 "0x87 stepped down"
notify 0x85
await_level 50 "0x85 cycled up"
notify 0x88
await_level 10 "0x88 went to the floor and not below"
before="$(region bcm_calls)"
notify 0x89
sleep 3
[[ "$(region level)" == "10" && "$(region bcm_calls)" == "$before" ]] || fail "0x89 moved the level ($(region level), _BCM called $(($(region bcm_calls) - before)) time(s))"
grep -aq "display off (0x89)" "$(serial_log)" || fail "0x89 was not logged"
echo "brightness-acpi: 0x89 changed no level and was logged"

# THE KEYBOARD'S KEYS: up, up, down.
touch "$state/press"
await_level 20 "the first key press stepped up" 30
await_level 30 "the second stepped up" 15
await_level 20 "and the third stepped down" 15
# The keypad says so after its last pause.
for _ in $(seq 1 20); do
	grep -aq "all three presses made" "$state/keys-sim.log" && break
	sleep 0.5
done
grep -aq "all three presses made" "$state/keys-sim.log" || fail "the keypad did not make its three presses"

# AUTOMATIC BRIGHTNESS through `_ALR`'s curve: 300 lx is 100 % of normal, 10 lx 73 % - seventy on this panel.
probe "brightcheck: auto on" "automatic brightness must turn on" auto on
python3 src/harness/acpi-fixture.py --brightness-set "$fixture/ivshmem.bin" illuminance=300
notify 0x80
await_level 100 "three hundred lux follows the curve to the top"
python3 src/harness/acpi-fixture.py --brightness-set "$fixture/ivshmem.bin" illuminance=10
notify 0x80
await_level 70 "ten lux follows it to seventy"
probe "brightcheck: auto off" "automatic brightness must turn off" auto off

# THE LEVEL TO BE RESTORED, held past the two-second settle.
probe "brightcheck: set level=60" "the level to restore must be set" set 60
await_level 60 "the level to restore"
stored="$(seen "stored at 60")"
for _ in $(seq 1 20); do
	stored="$(seen "stored at 60")"
	((stored > 0)) && break
	sleep 0.5
done
((stored > 0)) || fail "the policy never stored 60"
echo "brightness-acpi: the policy stored 60 once it settled"

# A RESET ON THE SAME VOLUME, the region back at the firmware default and `_DOS`'s record cleared, as power-on leaves them.
online="$(seen "BrightnessPolicy: online")"
python3 src/harness/acpi-fixture.py --brightness-set "$fixture/ivshmem.bin" level=70 dos=0 dos-calls=0
if ! ./dev.sh reboot --timeout 400 >"$state/reboot.log" 2>&1; then
	tail -20 "$state/reboot.log" >&2
	fail "the second boot did not come up (see $kept/reboot.log)"
fi
await_line "BrightnessPolicy: online" "the brightness policy never came online on the second boot" "$online" 180
[[ "$(region dos)" == "4" ]] || fail "_DOS was not evaluated again on the second boot"
await_level 60 "the stored level was restored on the second boot" 60
probe "brightcheck: settings automatic=false idle=off" "the settings must still be the first boot's" settings

# A SLEEP: a suspend to idle and an S3 cycle, each answered SUSPENDED and RESUMED by both drivers; while the guest is in
# S3 the harness puts the firmware default back in the region and clears `_DOS`'s record, as firmware may, and after the
# resume `_DOS` saw 0x04 again and the level set before the sleep is back.
qmp() {
	python3 - "$state/qemu-qmp.sock" "$@" <<'PY'
import json, socket, sys
sock = socket.socket(socket.AF_UNIX)
sock.settimeout(10)
sock.connect(sys.argv[1])
stream = sock.makefile('rw')
def answer():
	while True:
		line = json.loads(stream.readline())
		if 'event' not in line:
			return line
def command(execute):
	stream.write(json.dumps({'execute': execute}) + '\n')
	stream.flush()
	return answer()
answer()
command('qmp_capabilities')
result = command(sys.argv[2])
if 'error' in result:
	sys.exit(1)
if sys.argv[2] == 'query-status':
	print(result['return']['status'])
PY
}

run_state() {
	local now
	for _ in 1 2 3; do
		if now="$(qmp query-status 2>/dev/null)"; then
			echo "$now"
			return 0
		fi
		sleep 0.5
	done
	echo "unanswered"
}

await_state() {
	local want="$1" limit="$2" what="$3"
	for _ in $(seq 1 "$((limit * 2))"); do
		[[ "$(run_state)" == "$want" ]] && return 0
		sleep 0.5
	done
	fail "$what: QEMU was $(run_state), not $want, after ${limit} s"
}

# Both drivers answered the sleep's two steps since the counts given.
answered() {
	local what="$1" backlight_suspended="$2" backlight_resumed="$3" sensor_suspended="$4" sensor_resumed="$5"
	(($(seen "DeviceManager: suspended acpi-backlight") > backlight_suspended && $(seen "DeviceManager: resumed acpi-backlight") > backlight_resumed)) || fail "$what: the backlight's driver did not answer SUSPENDED and RESUMED"
	(($(seen "DeviceManager: suspended acpi-als") > sensor_suspended && $(seen "DeviceManager: resumed acpi-als") > sensor_resumed)) || fail "$what: the light sensor's driver did not answer SUSPENDED and RESUMED"
}

counts() {
	echo "$(seen "DeviceManager: suspended acpi-backlight") $(seen "DeviceManager: resumed acpi-backlight") $(seen "DeviceManager: suspended acpi-als") $(seen "DeviceManager: resumed acpi-als")"
}

ended="$(seen "ServiceManager: sleep: the transaction ended")"
read -r bs br ss sr <<<"$(counts)"
launch sleepctl suspend idle 5 >"$state/sleepctl-idle.log" 2>&1 || true
await_line "ServiceManager: sleep: the transaction ended" "the suspend to idle never ended" "$ended" 180
answered "the suspend to idle" "$bs" "$br" "$ss" "$sr"
await_level 60 "the level held across the suspend to idle"
echo "brightness-acpi: a suspend to idle answered by both drivers, the level held"

ended="$(seen "ServiceManager: sleep: the transaction ended")"
read -r bs br ss sr <<<"$(counts)"
dos_calls="$(region dos_calls)"
launch sleepctl suspend ram >"$state/sleepctl-ram.log" 2>&1 &
await_state suspended 60 "S3"
sleep 2
python3 src/harness/acpi-fixture.py --brightness-set "$fixture/ivshmem.bin" level=70 dos=0
qmp system_wakeup >/dev/null || fail "QEMU refused system_wakeup"
await_state running 30 "S3 after system_wakeup"
await_line "ServiceManager: sleep: the transaction ended" "the S3 cycle never ended" "$ended" 180
answered "S3" "$bs" "$br" "$ss" "$sr"
await_level 60 "the level set before the sleep is back after S3"
[[ "$(region dos)" == "4" && "$(region dos_calls)" -gt "$dos_calls" ]] || fail "_DOS was not evaluated with 0x04 again at the resume"
echo "brightness-acpi: S3 answered by both drivers; _DOS saw 0x04 again and the level set before the sleep is back"
echo "brightness-acpi: PASS"
