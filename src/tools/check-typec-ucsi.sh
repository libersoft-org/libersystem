#!/usr/bin/env bash
# USB TYPE-C OVER UCSI, end to end on x86_64 q35: the fixture SSDT's `\_SB.UCSI` (`USBC000` with `_CID` `PNP0CA0`, so
# the `_CID` match is what binds it), its mailbox shared with its own AML, its `_DSM` and line 4's `Notify`, and
# `ucsi-ppm.py` as the platform's policy manager behind them - through the `ucsi-acpi` driver, TypeCService, PowerService,
# the `typec` tool and the `typeccheck` probe. Four boots, one PPM profile each:
#
#   1. v12 (UCSI 1.2, alternate-mode override): a charger attached - its offers and the contract in `typec`, an online
#      `usb-c` source in PowerService BESIDE THE FIXTURE'S ACPI AC ADAPTER, which stays a source of its own - and
#      detached; a host attached and a data-role swap accepted; a power-role swap refused, the PPM's error in `typec`;
#      a DisplayPort partner entered at `typeccheck`'s request under the override, pin assignment D asked for; a PPM
#      gone silent, the connectors reported not answering, and recovered within the bound.
#   2. v21 (UCSI 2.1, no override, power readings, SET_PDR refused): the charger's contract and its MEASURED VBUS; the
#      configuration's SET_PDR refused and said; DisplayPort refused before any command without the override, then
#      entered by the PPM on its own and reported; a connector status past its size and one naming connector 7, each
#      refused and the PPM reset and read again.
#   3. slow: the first reset and the two commands after it each over five seconds - the binding ready inside
#      DeviceManager's deadline and its connectors published once the slow start is over.
#   4. spoiling: a PPM that spoils its inbound copy once it has notified - an attach and a detach served as on the others.
#   5. sleep (S3 offered): a suspend to idle and an S3 cycle, each answered by `ucsi-acpi` - the PPM's command log, stamped
#      by the host, showing no command from the driver's `SUSPENDED` to the kernel's `sleep: resumed`, and after it the
#      notifications enabled again and every connector read again; a detach the PPM makes while the guest is in S3
#      reported after the resume.
#
# THE PPM'S COMMAND LOG FAILS EVERY BOOT on a second command while a completion is unacknowledged, a completion left
# unacknowledged, a write not followed by function 1, function 2 after a notification and before the next command, or
# a connector change acknowledged other than beside its status.
#
# WHAT IT DOES NOT CLAIM: a vendor UCSI transport, a real laptop's EC.
#
# IT BOOTS ITS OWN DEVELOPMENT INSTANCES in private state, one at a time, and takes the last one down from the EXIT trap.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "typec-ucsi: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "the UCSI fixture is x86_64's: it is ACPI on q35"
	;;
*) fail "unexpected argument '$1'" ;;
esac
command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-typec.XXXXXX")"
fixture="$state/fixture"
mkdir -p "$fixture"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/typec-ucsi"
rm -rf "$kept"
mkdir -p "$kept"
label="none"
backend_pid=""
ppm_pid=""
follower=""

keep_log() {
	cp -f "$state/dev-serial.log" "$kept/serial-$label.log" 2>/dev/null || true
}

stop_helpers() {
	for pid in "$ppm_pid" "$backend_pid"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	backend_pid=""
	ppm_pid=""
}

cleanup() {
	local status=$?
	keep_log
	./dev.sh down >"$state/down.log" 2>&1 || echo "typec-ucsi: teardown reported a problem (see $kept/down.log)" >&2
	stop_helpers
	[[ -z "$follower" ]] || kill "$follower" 2>/dev/null || true
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

has() {
	grep -a -q -F -- "$1" "$(serial_log)" || fail "$label: $2 (no line containing: $1)"
}

await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$label: $what (waited ${limit} s for a new line containing: $needle)"
}

# ONE PROBE RUN, its output kept; a run that does not say PASS, or says FAIL, fails the gate.
probe() {
	local out="$state/probe-$label.log" result
	echo "\$ typeccheck $*" >>"$out"
	result="$(./dev.sh launch --timeout 150 typeccheck "$@" 2>&1)" || true
	echo "$result" >>"$out"
	if grep -q "typeccheck: FAIL" <<<"$result" || ! grep -q "typeccheck: \(PASS\|answer\|[0-9]* connector(s)\)" <<<"$result"; then
		echo "$result" >&2
		fail "$label: typeccheck $* did not pass"
	fi
	grep -a "typeccheck: \(PASS\|answer\|[0-9]* connector(s)\)" <<<"$result" | tail -1
}

# One line to the PPM's control socket; its answer.
ppm() {
	python3 - "$fixture/ppm.sock" "$1" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(5)
print(s.recv(4096).decode().strip())
EOF
}

start_helpers() {
	local profile="$1" version="$2"
	python3 src/harness/acpi-fixture.py --out "$fixture/fixture.aml" --memory "$fixture/ivshmem.bin" --ucsi-version "$version"
	rm -f "$fixture/ready" "$fixture/ppm-ready"
	python3 src/harness/vhost-i2c-gpio.py --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$fixture/backend-$label.log" 2>&1 &
	backend_pid="$!"
	for _ in $(seq 1 100); do
		[[ -e "$fixture/ready" ]] && break
		sleep 0.05
	done
	[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start"
	python3 src/harness/ucsi-ppm.py --memory "$fixture/ivshmem.bin" --gpio-control "$fixture/control.sock" --control "$fixture/ppm.sock" --log "$fixture/ppm-$label.log" --profile "$profile" --ready "$fixture/ppm-ready" >"$fixture/ppm-out-$label.log" 2>&1 &
	ppm_pid="$!"
	for _ in $(seq 1 100); do
		[[ -e "$fixture/ppm-ready" ]] && break
		sleep 0.05
	done
	[[ -e "$fixture/ppm-ready" ]] || fail "the harness PPM did not start (see $kept/ppm-out-$label.log)"
	export ACPI_FIXTURE="$fixture/fixture.aml" ACPI_FIXTURE_MEMORY="$fixture/ivshmem.bin"
	export I2C_FIXTURE=bus I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
}

boot() {
	label="$1"
	start_helpers "$2" "$3"
	echo "typec-ucsi: boot '$label' (profile $2, state $state, host port $HOSTFWD_PORT)"
	if ! ./dev.sh up --timeout 400 >"$state/up-$label.log" 2>&1; then
		tail -20 "$state/up-$label.log" >&2
		fail "the development instance for '$label' did not come up (see $kept/up-$label.log)"
	fi
	await_line "driver.ucsi-acpi: " "the UCSI driver never bound" 0
	await_line "publishes typec-connector" "the UCSI driver never published its connectors" 0 120
	await_line "TypeCService: online" "TypeCService never came online" 0
}

# THE PPM'S VERDICT on the OPM's discipline over the whole boot.
no_violations() {
	local answer
	answer="$(ppm violations)"
	[[ "$answer" == "ok none" ]] || fail "$label: the PPM saw the OPM break the discipline: $answer (see $kept/ppm-$label.log)"
	echo "typec-ucsi: $label: the PPM saw no break of the command discipline"
}

take_down() {
	no_violations
	keep_log
	./dev.sh down >"$state/down-$label.log" 2>&1 || fail "the '$label' instance did not come down"
	stop_helpers
	cp -f "$fixture/ppm-$label.log" "$kept/" 2>/dev/null || true
}

typec_tool() {
	./dev.sh launch --timeout 60 typec >>"$state/typec-$label.log" 2>&1 || true
}

# ------------------------------------------------------------------ 1. UCSI 1.2, with the override

boot v12 v12 0x0120
has "UCSI 1.2, messages of 16 bytes" "the driver must read VERSION 1.2 and lay the mailbox out for it"
[[ "$(probe list)" == *"typeccheck: 2 connector(s)"* ]] || fail "v12: TypeCService must hold the PPM's two connectors"
ppm "attach 1 charger" >/dev/null
probe await 1 contract 9000
probe source 1 online
probe beside-ac 1
typec_tool
grep -q "partner: a charger" "$state/typec-$label.log" || fail "v12: the tool must show the charger"
grep -q "contract: offer 2 (fixed 9.000 V 3.000 A), operating 2000 mA, maximum 3000 mA" "$state/typec-$label.log" || fail "v12: the tool must show the contract as the request named it"
grep -q "offer 4: fixed 20.000 V 3.000 A" "$state/typec-$label.log" || fail "v12: the tool must show every offer the charger made"
ppm "detach 1" >/dev/null
probe await 1 detached
probe source 1 offline
ppm "attach 1 host" >/dev/null
probe await 1 partner host
result="$(probe data 1 host)"
[[ "$result" == *"answer data 1: done"* ]] || fail "v12: the data-role swap must be done (read: $result)"
probe await 1 data host
result="$(probe power 1 source)"
[[ "$result" == *"answer power 1: refused Some(RefusedByPlatform) error 0x200"* ]] || fail "v12: the power-role swap must be refused with the PPM's error (read: $result)"
probe await 1 refused platform
ppm "attach 2 dp" >/dev/null
probe await 2 partner device
result="$(probe enter 2 ff01 000c0045)"
[[ "$result" == *"answer enter 2: done"* ]] || fail "v12: DisplayPort must be entered under the override (read: $result)"
probe await 2 entered ff01
grep -q "SET_NEW_CAM connector 2 enter offset 0 configuration 0x00000805" "$fixture/ppm-$label.log" || fail "v12: the PPM must have been asked for DisplayPort with pin assignment D"
# THE PPM GONE SILENT: the change it announces is read into silence, the connectors reported not answering, and the
# PPM reset and read again once it answers.
silent_out="$state/await-silent.log"
./dev.sh launch --timeout 150 typeccheck await 2 silent >"$silent_out" 2>&1 &
silent_pid="$!"
for _ in $(seq 1 60); do
	grep -q "watching connector 2 for silent" "$silent_out" 2>/dev/null && break
	sleep 1
done
ppm "silent-next 12" >/dev/null
ppm "detach 2" >/dev/null
wait "$silent_pid" || true
grep -q "typeccheck: PASS await 2 silent" "$silent_out" || fail "v12: a silent PPM must be reported as not answering (see $kept/await-silent.log)"
cp -f "$silent_out" "$kept/"
probe await 2 answering
has "the PPM answers again after its reset; every connector was read again" "the PPM must be recovered by a reset"
take_down

# ------------------------------------------------------------------ 2. UCSI 2.1, no override, readings, SET_PDR refused

boot v21 v21 0x0210
has "UCSI 2.1, messages of 256 bytes" "the driver must read VERSION 2.1 and lay the mailbox out for it"
has "SET_PDR was refused (0x800)" "the configuration's refused SET_PDR must be said"
ppm "attach 1 charger" >/dev/null
probe await 1 contract 9000
probe source 1 online 9020
ppm "attach 2 dp" >/dev/null
probe await 2 partner device
result="$(probe enter 2 ff01 000c0045)"
[[ "$result" == *"answer enter 2: refused Some(PlatformEntersModes)"* ]] || fail "v21: without the override DisplayPort must be refused before any command (read: $result)"
if grep -q "SET_NEW_CAM" "$fixture/ppm-$label.log"; then
	fail "v21: a mode request refused before any command reached the PPM"
fi
ppm "ppm-enter 2" >/dev/null
probe await 2 entered ff01
ppm "malformed length" >/dev/null
ppm "detach 1" >/dev/null
await_line "the PPM failed (Length" "a connector status past its size must be refused" 0 60
await_line "the PPM answers again after its reset" "and the PPM reset and read again" 0 60
ppm "malformed connector" >/dev/null
ppm "attach 1 charger" >/dev/null
await_line "the PPM failed (Connector(7))" "an answer naming connector 7 must be refused" 0 60
await_line "the PPM answers again after its reset" "and the PPM reset and read again" 1 60
probe await 1 contract 9000
take_down

# ------------------------------------------------------------------ 3. the slow PPM

boot slow slow 0x0210
window="$(grep -a -o -m1 "bind window ucsi_acpi = [0-9]* tick" "$(serial_log)" | grep -o "[0-9]*" || true)"
[[ -n "$window" ]] || fail "slow: DeviceManager did not report the UCSI binding's bind window"
((window < 200)) || fail "slow: the binding was ready after $window ticks - past DeviceManager's two-second deadline"
read_ms="$(grep -a -o -m1 "connector(s) read in [0-9]* ms" "$(serial_log)" | grep -o "[0-9]* ms" | grep -o "[0-9]*" || true)"
[[ -n "$read_ms" ]] || fail "slow: the driver did not say when its connectors were read"
((read_ms >= 15000)) || fail "slow: the connectors were read in $read_ms ms - the slow start was not slow"
echo "typec-ucsi: slow: ready in $window ticks, the connectors published after the slow start ($read_ms ms)"
ppm "attach 1 charger" >/dev/null
probe await 1 contract 9000
take_down

# ------------------------------------------------------------------ 4. the spoiling PPM

boot spoiling spoiling 0x0210
ppm "attach 1 charger" >/dev/null
probe await 1 contract 9000
ppm "detach 1" >/dev/null
probe await 1 detached
take_down

# ------------------------------------------------------------------ 5. a sleep

# ONE QMP COMMAND, events skipped - the run state and the wake.
qmp() {
	python3 - "$state/qemu-qmp.sock" "$1" <<'EOF'
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
if sys.argv[2] == 'query-status':
	print(result['return']['status'])
EOF
}

await_state() {
	for _ in $(seq 1 "$(($2 * 2))"); do
		[[ "$(qmp query-status 2>/dev/null || true)" == "$1" ]] && return 0
		sleep 0.5
	done
	fail "$label: QEMU was not $1 after $2 s"
}

# THE SERIAL LOG, STAMPED AS IT ARRIVES, to place the driver's and the kernel's lines against the PPM's stamped log.
follow_serial() {
	python3 -u - "$(serial_log)" "$state/serial-stamped-$label.log" <<'EOF' &
import os, sys, time
path, out = sys.argv[1], sys.argv[2]
at, partial = 0, b''
with open(out, 'w') as sink:
	while True:
		size = os.path.getsize(path) if os.path.exists(path) else 0
		if size > at:
			with open(path, 'rb') as log:
				log.seek(at)
				chunk = log.read(size - at)
			at = size
			now = time.time()
			lines = (partial + chunk).split(b'\n')
			partial = lines.pop()
			for line in lines:
				sink.write(f'{now:.3f} {line.decode(errors="replace")}\n')
			sink.flush()
		time.sleep(0.02)
EOF
	follower=$!
}

# THE PPM'S LOG ACROSS ONE SLEEP: no command from the driver's SUSPENDED to the kernel's `sleep: resumed`, then the
# notifications enabled and both connectors read.
ppm_across() {
	local what="$1"
	python3 - "$state/serial-stamped-$label.log" "$fixture/ppm-$label.log" "$what" <<'EOF' || fail "$label: $what - the PPM saw the sleep wrongly (see $kept/ppm-$label.log)"
import re, sys
serial, ppm, what = sys.argv[1], sys.argv[2], sys.argv[3]
def last(pattern):
	stamp = None
	for line in open(serial, errors='replace'):
		if pattern in line:
			stamp = float(line.split(' ', 1)[0])
	return stamp
suspended, resumed = last('driver.ucsi-acpi: '), None
suspended = last('suspended - no command is sent until the resume')
resumed = last('sleep: resumed (')
if suspended is None or resumed is None or resumed < suspended:
	sys.exit(f'{what}: the driver said no SUSPENDED or the kernel no resume ({suspended}, {resumed})')
commands = []
for line in open(ppm, errors='replace'):
	found = re.match(r'([0-9.]+) command (\w+) control (0x[0-9a-f]+)', line)
	if found:
		commands.append((float(found.group(1)), found.group(2), int(found.group(3), 16)))
inside = [name for stamp, name, _ in commands if suspended < stamp < resumed]
if inside:
	sys.exit(f'{what}: {len(inside)} command(s) between SUSPENDED and the resume: {inside}')
after = [(name, (control >> 16) & 0x7F) for stamp, name, control in commands if stamp > resumed]
if 'SET_NOTIFICATION_ENABLE' not in [name for name, _ in after]:
	sys.exit(f'{what}: the notifications were not enabled again after the resume')
read = {connector for name, connector in after if name == 'GET_CONNECTOR_STATUS'}
if not {1, 2} <= read:
	sys.exit(f'{what}: the connectors read again after the resume were {sorted(read)}, not both')
print(f'typec-ucsi: sleep: {what} - no command while asleep, the notifications enabled and both connectors read after it')
EOF
}

export QEMU_EXTRA="-global ICH9-LPC.disable_s3=0"
boot sleep v21 0x0210
follow_serial
ppm "attach 1 charger" >/dev/null
probe await 1 contract 9000
ended=$(seen "ServiceManager: sleep: the transaction ended")
./dev.sh launch --timeout 120 sleepctl suspend idle 5 >"$state/sleepctl-idle.log" 2>&1 || true
await_line "ServiceManager: sleep: the transaction ended" "the suspend to idle never ended" "$ended" 120
ended="$(grep -a "ServiceManager: sleep: the transaction ended" "$(serial_log)" | tail -1 || true)"
grep -q "slept and woke" <<<"$ended" || fail "sleep: the suspend to idle did not sleep and wake"
sleep 2
ppm_across "suspend to idle"
ended=$(seen "ServiceManager: sleep: the transaction ended")
./dev.sh launch --timeout 200 sleepctl suspend ram >"$state/sleepctl-ram.log" 2>&1 &
await_state suspended 60
# A DETACH WHILE THE GUEST IS IN S3, reported after the resume.
ppm "detach 1" >/dev/null
sleep 2
qmp system_wakeup >/dev/null
await_state running 30
await_line "ServiceManager: sleep: the transaction ended" "the S3 cycle never ended" "$ended" 180
ended="$(grep -a "ServiceManager: sleep: the transaction ended" "$(serial_log)" | tail -1 || true)"
grep -q "slept and woke" <<<"$ended" || fail "sleep: the S3 cycle did not sleep and wake"
sleep 2
ppm_across "S3"
probe await 1 detached
echo "typec-ucsi: sleep: the detach the PPM made while the guest was in S3 was reported after the resume"
kill "$follower" 2>/dev/null || true
cp -f "$state/serial-stamped-$label.log" "$kept/" 2>/dev/null || true
take_down
unset QEMU_EXTRA
echo "typec-ucsi: PASS - UCSI 1.2 and 2.1 through the _CID match: a charger's offers and contract, its supply beside the ACPI adapter and measured under 2.1, a data-role swap done, a power-role swap refused with the PPM's error, DisplayPort entered under the override and refused without it, a mode the PPM entered itself reported, malformed answers refused and a silent PPM recovered, a slow PPM served past its slow start, a spoiling one served as the others, a suspend to idle and an S3 cycle with no command while asleep and every connector read after it, a detach made in S3 reported - and no break of the command discipline"
