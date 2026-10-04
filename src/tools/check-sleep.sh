#!/usr/bin/env bash
# SLEEP AND RESUME, end to end on x86_64 q35 with S3 offered - set explicitly (`-global ICH9-LPC.disable_s3=0`), never
# trusted as a default.
#
# THE ORACLES ARE THE HOST'S. For S3 it is QEMU's run state, which is durable: `query-status` asked per reading answers
# `suspended` while the guest is in S3 and `running` after the wake, and no standing QMP connection is needed to see
# it. For suspend to idle QEMU never leaves `running` and refuses `system_wakeup`, so the oracle is the serial log's
# TIMING: every line of it is stamped by the host as it arrives, the kernel's `sleep: entered` line is on the wire before
# the cores park and its `sleep: resumed` line before any driver resumes, and `sleepcheck` - an application with no
# authority, launched as one - prints a counter every 100 ms whose lines the host stamps as well.
#
#   1. SUSPEND TO IDLE, woken by the timed wake `sleepctl` arms: the counter SILENT for the whole host-measured interval
#      between the two kernel lines; that interval at least the armed wake less the margin; the counter's next line the
#      next value, its monotonic clock moved by less than the sleep and its boot-time clock by at least it; and the
#      parking, from the last sleep's record: no core woke for a device, cpu0 at most once for the timer, every other
#      core at most once, for the IPI that ends its park. A Timer armed before the sleep fires after its remaining
#      awake time - the sleep is on its boot-time clock and not on its monotonic one.
#   2. S3, woken by `system_wakeup`: `suspended`, then `running`, the kernel's lines, the transaction ending "slept and
#      woke". With `swtpm` behind the CRB front-end on this boot: PCRs 0 to 7 read back unchanged after it - the
#      firmware's fallback to Startup(CLEAR) would have extended an error separator into each - PCRs 16 and 23, extended
#      before it, zero after it - a Shutdown(STATE) saves 0 to 15 - and a secret sealed to PCR 4 before it unsealing.
#   3. S3, woken by the RTC alarm `sleepctl` arms: `suspended`, then `running` with no host wake at all - QEMU wakes a
#      suspended guest on the RTC only with RTC_EN set, so this proves the kernel's arming too - and the wake named.
#   AFTER EVERY WAKE: the network answers a ping, a file written before the sleep reads back, a frame reaches the
#   display (the screen changes for a line typed on the emulated keyboard), a line typed at the serial console is
#   answered, the wall clock is within two seconds of the host's, and a wait armed after the resume completes on time.
#   4. After an S3, a function `device_add`-ed into the empty native hot-plug port is seen by the kernel - the restored
#      Slot Control.
#   5. A job stopped with Ctrl+Z at the serial shell before a sleep is still stopped after the thaw, and `fg` resumes it
#      at its next value.
#   6. A driver made to refuse - the sleep gate's fixture, an `ivshmem-plain` function hot-plugged into the empty port and
#      so the last binding and the first suspended - aborts the suspend: unwound at the drivers' step, the binding named,
#      the system running.
#   7. A PERSON'S SHUTDOWN DURING A SLEEP THAT WOULD OTHERWISE SUCCEED: the fixture holds the drivers' step, `shutdown` is
#      typed at the serial shell, and once ServiceManager says it took the orderly sequence the fixture is released - no
#      `sleep: entered`, the unwind naming the sequence and its door, the power verb after it, and QEMU's exit.
#
# THE PLATFORM BOOT: the fixture SSDT with the sleep gate's devices and the development switch naming the CMOS RTC absent.
# Closing the lid turns the screen off through the power-state service's policy - black on the host's screendump, a line
# typed then unseen, the machine running - and opening it turns the screen on; then, the lid set to suspend
# (`power.lid`) and the policy killed and relaunched, closing it suspends to RAM; the Time and Alarm Device's clock - the
# harness plays it five years ahead - is the wall clock, stamps files and measures an S3; its timer is what the driver
# programs from the sleep's timed wake, and the host wakes the guest at it; THE FANS' DRIVERS answer SUSPENDED and
# RESUMED across a suspend to idle and an S3, and the level FAN1 was commanded through its curve - cleared in the pages
# by the host while the guest is in S3 - is applied again at the resume; THE BUTTONS, each a `platform-switch` the policy
# follows - the fixed power button the kernel declares a row for among them: the control-method sleep button set to
# `nothing` (`power.sleep-button`, read by the relaunched instance) does nothing, and at its default, read by an instance
# relaunched again, suspends; and the control-method power button, at its default, powers off IN ORDER - ServiceManager's
# sequence through `system-shutdown`, then the registered `\_S5`.
# THE DEVICE POWER STATES, read from the fixture's pages - the lid and the TAD share one power resource in D0, and the
# lid's wake needs another: after the boot the shared one is on, turned on ONCE for its two holders; while the lid's S3
# is suspended both devices ran `_PS3`, the shared resource is off and the wake one on; while the TAD's timed S3 is
# suspended the TAD - its timer to wake the machine, and no `_S3W` to say a deeper state still wakes - kept D0, so the
# shared resource stayed on with the lid in D3hot; after each wake both are in D0 and the wake resource off again.
# THE BATTERY BOOT: the same fixture, and the fixture's battery discharged past critical with the adapter off line - the
# owner's default does nothing, says so, and the machine runs on; then `power.critical power-off` set and the power
# service killed and relaunched, and the relaunched instance, finding the battery critical, runs the orderly power-off
# with no hibernation asked through its own clients: the forced deadline armed, ServiceManager's sequence, the
# registered `\_S5`, and QEMU gone within the bound.
# THE FALLBACK BOOT: the switch refusing the sleep-type registration, and `shutdown` taking the fixed ports.
# EVERY ORDERLY POWER-OFF HERE - the power button's, the battery's and the typed `shutdown` - is also checked for a driver
# whose device was taken while it still ran: no ring-3 fault after the sequence began, and DeviceManager stopped within
# its bound.
#
# A KEY WAKE FROM S3 IS NOT REQUIRED: QEMU requests a system wakeup from its PS/2 keyboard, which this system does not
# drive, and delivers a USB or virtio key to a suspended machine as nothing that wakes it. Case 6 checks this rather
# than assumes it, and says what it saw.
#
# IT BOOTS ITS OWN INSTANCE in private state, and takes it down from the EXIT trap whatever happened.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "sleep: $*" >&2
	exit 1
}

say() {
	echo "sleep: $*"
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
command -v swtpm >/dev/null || fail "swtpm is not installed - setup.sh installs it, and this gate fails rather than skips without it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

# The timed wake of the suspend to idle, and how far the host's measurement of it may fall short: the kernel's two
# lines are stamped when they arrive, a poll apart.
IDLE_WAKE_S=5
IDLE_MARGIN_MS=300
RTC_WAKE_S=8

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-sleep.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
export QEMU_EXTRA="-global ICH9-LPC.disable_s3=0"
# The launched programs' output is read as it arrives, which a block-buffered pipe would batch.
export PYTHONUNBUFFERED=1
kept="$(pwd)/.build/logs/sleep"
rm -rf "$kept"
mkdir -p "$kept"
follower=0
tpm_pid=""

cleanup() {
	local status=$?
	# EVERY HELPER THIS SCRIPT STARTED: the serial follower, the first boot's TPM, and the platform boot's backend and
	# clock.
	kill $(jobs -p) 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "sleep: teardown reported a problem (see $kept/down.log)" >&2
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

serial="$state/dev-serial.log"
stamped="$state/serial-stamped.log"

# How many lines of the guest's serial log match a pattern, right now.
seen() {
	grep -a -c -F -- "$1" "$serial" 2>/dev/null || true
}

# Wait for a line to ARRIVE past `baseline`, or fail saying which did not.
await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$what (waited ${limit} s for a new line matching: $needle)"
}

# ONE QMP COMMAND on a connection of its own, events skipped: the run state, a wake, a screen, a device.
qmp() {
	python3 - "$state/qemu-qmp.sock" "$@" <<'EOF'
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
def command(execute, arguments=None):
	stream.write(json.dumps({'execute': execute, 'arguments': arguments or {}}) + '\n')
	stream.flush()
	return answer()
answer()
command('qmp_capabilities')
arguments = json.loads(sys.argv[3]) if len(sys.argv) > 3 else None
result = command(sys.argv[2], arguments)
if 'error' in result:
	print(result['error'].get('desc', 'error'))
	sys.exit(1)
if sys.argv[2] == 'query-status':
	print(result['return']['status'])
EOF
}

# THE RUN STATE, asked again when QMP does not answer: its socket takes one client at a time.
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

# A program launched through the development agent, its output stamped by the host line by line.
launch_stamped() {
	local out="$1"
	shift
	./dev.sh launch --timeout 300 "$@" 2>&1 | python3 -u -c '
import sys, time
for line in sys.stdin:
	print(f"{time.time():.3f} {line}", end="", flush=True)' >"$out"
}

launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# A LINE TYPED AT THE SERIAL CONSOLE, byte by byte through the console socket as a person's terminal would send it - not
# through the broker, which waits for a prompt a background job printing behind it never leaves. `\x1a` alone is a
# Ctrl+Z, sent with no Enter.
type_serial() {
	python3 - "$state/dev-console.sock" "$1" <<'EOF'
import socket, sys, threading, time
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[1])
sock.sendall(b'ATTACH rw\n')
sock.settimeout(5)
granted = b''
while not granted.endswith(b'\n'):
	granted += sock.recv(1)
if not granted.startswith(b'OK rw'):
	sys.exit(f'the console refused the attach: {granted!r}')
# WHAT THE CONSOLE SENDS IS READ AND DROPPED: the broker drops a client whose socket fills - the replay and a counter
# printing behind the prompt fill it in moments - and a dropped writer's keystrokes reach nobody.
def drain():
	try:
		while sock.recv(65536):
			pass
	except OSError:
		pass
threading.Thread(target=drain, daemon=True).start()
line = sys.argv[2].encode()
payload = line if line == b'\x1a' else line + b'\r'
for byte in payload:
	sock.sendall(bytes([byte]))
	time.sleep(0.02)
time.sleep(0.5)
EOF
}

# THE SERIAL LOG, STAMPED AS IT ARRIVES: a follower polls it every 20 ms from the byte it has read to, and writes each
# complete line with the host's time.
follow_serial() {
	python3 -u - "$serial" "$stamped" <<'EOF' &
import os, sys, time
path, out = sys.argv[1], sys.argv[2]
at, partial = 0, b''
with open(out, 'w') as sink:
	while True:
		try:
			size = os.path.getsize(path)
		except OSError:
			size = 0
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

# The host time a stamped line matching `needle` arrived at, the last one after line `from` of the stamped log.
stamp_of() {
	local needle="$1" from="$2"
	tail -n +"$((from + 1))" "$stamped" | grep -a -- "$needle" | tail -n 1 | cut -d' ' -f1
}

ms_between() {
	python3 -c "import sys; print(int((float(sys.argv[2]) - float(sys.argv[1])) * 1000))" "$1" "$2"
}

# THE ORDER THE TRANSACTION ENDS IN: `sleepctl`, thawed, prints the record, and ServiceManager says it ended.
await_end() {
	local baseline="$1" what="$2"
	await_line "ServiceManager: sleep: the transaction ended" "$what: the transaction never ended" "$baseline" 180
	# THE LAST ONE IS THIS TRANSACTION'S, the wait having seen it past `baseline` - and nothing else's line count: the
	# boots after the first truncate the log, so a count kept from an earlier boot names no line of this one.
	local ended
	ended="$(grep -a "ServiceManager: sleep: the transaction ended" "$serial" | tail -n 1)"
	[[ "$ended" == *"slept and woke"* ]] || fail "$what: the transaction ended as: $ended"
}

# AN ORDERLY POWER-OFF STOPS EVERY DRIVER BEFORE ITS DEVICE IS TAKEN FROM IT: no ring-3 fault after the sequence began -
# a driver whose claim went while it still ran faults on its revoked registers - and DeviceManager stopped, within its
# bound.
orderly_without_faults() {
	local what="$1"
	python3 - "$serial" <<'EOF' || fail "$what: the orderly power-off was not orderly (see $kept)"
import sys
text = open(sys.argv[1], 'rb').read()
at = text.rfind(b'supervisor: the orderly power-off begins')
if at < 0:
	sys.exit('no orderly power-off began')
after = text[at:]
for bad in (b'fault: ring-3', b'did not stop its drivers within its bound'):
	if bad in after:
		line = after[after.find(bad):].split(b'\n', 1)[0].decode(errors='replace')
		sys.exit(f'after the sequence began: {line}')
if b'supervisor: device_manager stopped' not in after:
	sys.exit('DeviceManager was never stopped')
EOF
}

# A SCREEN, as QEMU shows it.
screen() {
	qmp screendump "{\"filename\": \"$1\", \"format\": \"png\"}" >/dev/null || fail "QEMU refused the screendump"
}

# WHETHER A SCREEN IS BLACK, every pixel.
black() {
	python3 -c 'import sys; from PIL import Image; sys.exit(0 if Image.open(sys.argv[1]).convert("RGB").getextrema() == ((0, 0), (0, 0), (0, 0)) else 1)' "$1"
}

marker_n=0
# EVERYTHING A WAKE MUST LEAVE WORKING, read after each one.
after_wake() {
	local label="$1" out now_host now_guest skew before after
	marker_n=$((marker_n + 1))
	out="$(launch ping -c 2 10.0.2.2)"
	grep -q "2 packets transmitted, 2 received" <<<"$out" || fail "$label: the network did not answer a ping after the wake: $out"
	out="$(launch cat "sleep-marker-$label.txt")"
	grep -q "written before the $label sleep" <<<"$out" || fail "$label: the file written before the sleep did not read back: $out"
	# A FRAME: the screen before and after a line typed on the emulated keyboard differ.
	screen "$state/screen-$label-before.png"
	./dev.sh key --text "echo frame-$label" >/dev/null 2>&1 || fail "$label: the emulated keyboard took no keys"
	sleep 2
	screen "$state/screen-$label-after.png"
	cmp -s "$state/screen-$label-before.png" "$state/screen-$label-after.png" && fail "$label: no frame reached the display after the wake - the screen did not change for a typed line"
	# THE SERIAL CONSOLE answers a typed line.
	before=$(seen "LiberSystem 0.0.1 x86_64")
	type_serial uname || fail "$label: the serial console took no input after the wake"
	await_line "LiberSystem 0.0.1 x86_64" "$label: a line typed at the serial console was not answered after the wake" "$before" 20
	# THE WALL CLOCK, within two seconds of the host's.
	launch sleepcheck clocks >>"$state/clocks.log"
	now_guest="$(launch date | grep -a -o -m 1 '[0-9]\{4\}-[0-9][0-9]-[0-9][0-9]T[0-9:]*Z' || true)"
	now_host="$(date -u +%s)"
	[[ -n "$now_guest" ]] || fail "$label: date printed no time"
	skew=$((now_host - $(date -u -d "$now_guest" +%s)))
	((skew >= -2 && skew <= 3)) || fail "$label: the wall clock is ${skew} s from the host's after the wake ($now_guest)"
	# A WAIT ARMED AFTER THE RESUME, on time: a second on the rebased tick.
	launch_stamped "$state/timer-after-$label.log" sleepcheck timer 1000
	out="$(cat "$state/timer-after-$label.log")"
	local armed fired
	armed="$(grep -a "sleepcheck: timer armed" <<<"$out" | cut -d' ' -f1)"
	fired="$(grep -a "sleepcheck: timer fired" <<<"$out" | cut -d' ' -f1)"
	[[ -n "$armed" && -n "$fired" ]] || fail "$label: the timer armed after the wake did not report: $out"
	local waited
	waited=$(ms_between "$armed" "$fired")
	((waited >= 800 && waited <= 1600)) || fail "$label: a one-second wait armed after the resume took ${waited} ms"
	say "$label: after the wake - ping answered, the file read back, a frame presented, the serial line answered, the wall clock ${skew} s off, a 1 s wait took ${waited} ms"
}

before_sleep() {
	local label="$1" out
	out="$(launch write "sleep-marker-$label.txt" "written before the $label sleep")"
	grep -qi "wrote\|written" <<<"$out" || fail "$label: the file before the sleep was not written: $out"
}

# THE TPM ACROSS AN S3, for the first boot: swtpm behind the CRB front-end.
start_tpm() {
	mkdir -p "$state/tpm"
	swtpm socket --tpm2 --tpmstate "dir=$state/tpm" --ctrl "type=unixio,path=$state/swtpm.sock" --log "file=$state/swtpm.log" &
	tpm_pid="$!"
	for _ in $(seq 1 100); do
		[[ -S "$state/swtpm.sock" ]] && break
		sleep 0.05
	done
	[[ -S "$state/swtpm.sock" ]] || fail "swtpm did not open its control socket"
	export TPM_SOCKET="$state/swtpm.sock" TPM_FRONTEND=crb
}

stop_tpm() {
	unset TPM_SOCKET TPM_FRONTEND
	[[ -n "$tpm_pid" ]] || return 0
	kill "$tpm_pid" 2>/dev/null || true
	wait "$tpm_pid" 2>/dev/null || true
	tpm_pid=""
}

pcr() {
	launch tpm pcr "$1" | grep -a -o "tpm: pcr [0-9a-f]*" | tail -n 1 | cut -d' ' -f3
}

declare -a pcrs_before
tpm_before_sleep() {
	local n out
	for n in 0 1 2 3 4 5 6 7; do
		pcrs_before[n]="$(pcr "$n")"
		[[ ${#pcrs_before[n]} -eq 64 ]] || fail "tpm: PCR $n was not read before the sleep"
	done
	for n in 16 23; do
		out="$(launch tpm extend "$n" "before-the-sleep-$n")"
		grep -q "tpm: extended PCR $n with" <<<"$out" || fail "tpm: PCR $n was not extended before the sleep: $out"
		[[ "$(pcr "$n")" != "$ZERO_PCR" ]] || fail "tpm: PCR $n reads zero after its extend"
	done
	out="$(launch tpm seal 4 sealed-before-the-sleep sleep-secret.sealed)"
	grep -q "tpm: sealed to PCR 4" <<<"$out" || fail "tpm: the secret was not sealed to PCR 4: $out"
}

tpm_after_sleep() {
	local n out
	for n in 0 1 2 3 4 5 6 7; do
		[[ "$(pcr "$n")" == "${pcrs_before[n]}" ]] || fail "tpm: PCR $n changed across the S3 - the firmware did not resume the TPM's saved state"
	done
	for n in 16 23; do
		[[ "$(pcr "$n")" == "$ZERO_PCR" ]] || fail "tpm: PCR $n is not zero after the S3, though a Shutdown(STATE) does not save it"
	done
	out="$(launch tpm unseal sleep-secret.sealed)"
	grep -q "tpm: unsealed: sealed-before-the-sleep" <<<"$out" || fail "tpm: the secret sealed to PCR 4 before the S3 did not unseal after it: $out"
	say "tpm: PCRs 0 to 7 unchanged across the S3, 16 and 23 zero after it, and the secret sealed to PCR 4 unsealed"
}
ZERO_PCR="$(printf '0%.0s' $(seq 1 64))"

# THE INSTANCE.
say "booting a development instance with S3 offered and swtpm behind the CRB front-end"
start_tpm
./dev.sh up >"$state/up.log" 2>&1 || fail "the development instance did not come up (see $kept/up.log)"
follow_serial

# 1. SUSPEND TO IDLE. The counter and the Timer run in the background at the serial shell - the development agent serves
#    one launch at a time, so a launched counter would have ended before `sleepctl` started - and their lines reach the
#    serial log, where the follower stamps them with the kernel's.
before_sleep idle
lines_before=$(wc -l <"$stamped")
baseline_lines=$(wc -l <"$serial")
ended=$(seen "ServiceManager: sleep: the transaction ended")
counted=$(seen "sleepcheck: count done")
fired_n=$(seen "sleepcheck: timer fired")
# WHAT SHOWS A BACKGROUND JOB STARTED is its first line.
armed_n=$(seen "sleepcheck: timer armed")
first=$(seen "sleepcheck: count 1 ")
type_serial "sleepcheck count 40 &" || fail "the serial console took no input"
await_line "sleepcheck: count 1 " "the counter did not start in the background at the serial shell" "$first" 20
type_serial "sleepcheck timer 20000 &" || fail "the serial console took no input"
await_line "sleepcheck: timer armed" "the Timer did not start in the background at the serial shell" "$armed_n" 20
sleep 2
launch sleepctl suspend idle "$IDLE_WAKE_S" >"$state/sleepctl-idle.log" || true
await_end "$ended" "suspend to idle"
await_line "sleepcheck: count done" "the counter never finished" "$counted" 60
await_line "sleepcheck: timer fired" "the Timer armed before the sleep never fired" "$fired_n" 60
sleep 1
tail -n +"$((lines_before + 1))" "$stamped" >"$state/counter.log"
entered=$(stamp_of "sleep: entered (suspend to idle" "$lines_before")
resumed=$(stamp_of "sleep: resumed (the timed wake" "$lines_before")
[[ -n "$entered" ]] || fail "suspend to idle: no 'sleep: entered' line"
[[ -n "$resumed" ]] || fail "suspend to idle: no 'sleep: resumed' line naming the timed wake"
slept=$(ms_between "$entered" "$resumed")
((slept >= IDLE_WAKE_S * 1000 - IDLE_MARGIN_MS)) || fail "suspend to idle: the host measured ${slept} ms between the kernel's lines, under the ${IDLE_WAKE_S} s wake"
# THE COUNTER: silent between the lines, the next value after, the clocks apart by the sleep.
python3 - "$state/counter.log" "$entered" "$resumed" "$IDLE_WAKE_S" <<'EOF' || fail "suspend to idle: the counter across the sleep (see $kept/counter.log)"
import re, sys
path, entered, resumed, wake_s = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4])
rows = []
for line in open(path, errors='replace'):
	found = re.match(r'([0-9.]+) .*sleepcheck: count (\d+) mono-ms (\d+) boot-ms (\d+)', line)
	if found:
		rows.append((float(found.group(1)), int(found.group(2)), int(found.group(3)), int(found.group(4))))
if not rows:
	sys.exit('no counter line')
inside = [row for row in rows if entered < row[0] < resumed]
if inside:
	sys.exit(f'{len(inside)} counter line(s) arrived while the machine slept, the first count {inside[0][1]}')
if [row[1] for row in rows] != list(range(rows[0][1], rows[0][1] + len(rows))):
	sys.exit('the counter skipped or repeated a value')
if not (rows[0][0] < entered and rows[-1][0] > resumed):
	sys.exit('the counter did not run across the sleep')
# THE GAP: the widest boot-time step between two consecutive lines is the sleep's.
gap = max(range(1, len(rows)), key=lambda i: rows[i][3] - rows[i - 1][3])
mono = rows[gap][2] - rows[gap - 1][2]
boot = rows[gap][3] - rows[gap - 1][3]
if boot < wake_s * 1000 - 300:
	sys.exit(f'the boot-time clock moved {boot} ms across the sleep, under the {wake_s} s wake')
if mono >= wake_s * 1000 - 300 or mono > 2000:
	sys.exit(f'the monotonic clock moved {mono} ms across the sleep - it took the sleep in')
print(f'sleep: suspend to idle - count {rows[gap - 1][1]} then {rows[gap][1]}: monotonic +{mono} ms, boot-time +{boot} ms')
EOF
# THE TIMER armed 20 s ahead, some seconds before the sleep: its remaining awake time after the resume, give or take the
# transaction - never at the resume.
python3 - "$state/counter.log" "$IDLE_WAKE_S" <<'EOF' || fail "suspend to idle: the Timer across the sleep (see $kept/counter.log)"
import re, sys
path, wake_s = sys.argv[1], int(sys.argv[2])
armed = fired = None
for line in open(path, errors='replace'):
	found = re.match(r'([0-9.]+) .*sleepcheck: timer (armed for \d+ ms|fired) mono-ms (\d+) boot-ms (\d+)', line)
	if found:
		row = (float(found.group(1)), int(found.group(3)), int(found.group(4)))
		if found.group(2) == 'fired':
			fired = row
		else:
			armed = row
if not armed or not fired:
	sys.exit('the timer did not report both its arming and its firing')
mono, boot = fired[1] - armed[1], fired[2] - armed[2]
# A tick either side: the Timer is armed in ticks of 10 ms, and the stamps are read around the arming.
if not 19980 <= mono <= 20500:
	sys.exit(f'the timer fired {mono} ms of monotonic time after it was armed, not 20 s')
if boot < 20000 + wake_s * 1000 - 300:
	sys.exit(f'the timer fired {boot} ms of boot time after it was armed - at the resume, not after its remaining awake time')
print(f'sleep: suspend to idle - a 20 s Timer fired after {mono} ms awake and {boot} ms since boot')
EOF
# THE PARKING, from the last sleep's record.
record="$(launch sleepctl last)"
echo "$record" >"$state/record-idle.log"
grep -q "woken by the timed wake" <<<"$record" || fail "suspend to idle: the record does not name the timed wake: $record"
python3 - "$state/record-idle.log" <<'EOF' || fail "suspend to idle: a parked core woke for something other than the wake (see $kept/record-idle.log)"
import re, sys
cores = 0
for line in open(sys.argv[1]):
	found = re.search(r'cpu(\d+): woke (\d+) time\(s\) for the timer, (\d+) for an IPI, (\d+) for a device', line)
	if not found:
		continue
	cores += 1
	cpu, timer, ipi, device = map(int, found.groups())
	if device:
		sys.exit(f'cpu{cpu} woke {device} time(s) for a device')
	if cpu == 0 and (timer > 1 or ipi > 1):
		sys.exit(f'cpu0 woke {timer} time(s) for the timer and {ipi} for an IPI')
	if cpu != 0 and (timer or ipi > 1):
		sys.exit(f'cpu{cpu} woke {timer} time(s) for the timer and {ipi} for an IPI')
if not cores:
	sys.exit('the record lists no core')
print(f'sleep: suspend to idle - {cores} cores parked, none woke for anything but the wake')
EOF
say "suspend to idle - the host measured ${slept} ms between 'sleep: entered' and 'sleep: resumed' for a ${IDLE_WAKE_S} s wake"
after_wake idle

# 2. S3, WOKEN BY THE HOST.
before_sleep ram
tpm_before_sleep
baseline_lines=$(wc -l <"$serial")
ended=$(seen "ServiceManager: sleep: the transaction ended")
launch sleepctl suspend ram >"$state/sleepctl-ram.log" 2>&1 &
await_state suspended 60 "suspend to RAM"
sleep 3
[[ "$(run_state)" == "suspended" ]] || fail "suspend to RAM: the guest woke by itself"
qmp system_wakeup >/dev/null || fail "QEMU refused system_wakeup"
await_state running 30 "suspend to RAM after system_wakeup"
await_end "$ended" "suspend to RAM"
tail -n +"$((baseline_lines + 1))" "$serial" | grep -a -q "sleep: entered (suspend to RAM" || fail "suspend to RAM: no 'sleep: entered' line"
tail -n +"$((baseline_lines + 1))" "$serial" | grep -a -q "sleep: resumed (the power button" || fail "suspend to RAM: the resume did not name the power button system_wakeup presses"
say "suspend to RAM - suspended, woken by system_wakeup, running, and the transaction ended"
after_wake ram
tpm_after_sleep

# 3. S3, WOKEN BY THE RTC ALARM.
before_sleep rtc
baseline_lines=$(wc -l <"$serial")
ended=$(seen "ServiceManager: sleep: the transaction ended")
launch sleepctl suspend ram "$RTC_WAKE_S" >"$state/sleepctl-rtc.log" 2>&1 &
await_state suspended 60 "suspend to RAM with an RTC wake"
await_state running $((RTC_WAKE_S + 20)) "suspend to RAM with an RTC wake, with no host wake"
await_end "$ended" "suspend to RAM with an RTC wake"
tail -n +"$((baseline_lines + 1))" "$serial" | grep -a -q "sleep: resumed (the RTC alarm" || fail "suspend to RAM: the RTC wake was not named"
say "suspend to RAM - woken by the RTC alarm the kernel armed, with no host wake"
after_wake rtc

# 5. A STOPPED JOB ACROSS A SLEEP: Ctrl+Z at the serial shell before it, still stopped after the thaw, `fg` resuming it at
#    its next value. THE JOB IS STARTED IN THE BACKGROUND AND BROUGHT TO THE FOREGROUND: a governed tool typed without `&`
#    runs to completion inside the line, with no job to stop, and only a job the shell tracks is handed to the terminal
#    for its signal keys.
first=$(seen "sleepcheck: count 1 ")
type_serial "sleepcheck count 60 &" || fail "the serial console took no input"
await_line "sleepcheck: count 1 " "the job at the serial shell printed no count" "$first" 20
type_serial fg || fail "the serial console took no input"
sleep 1
stops=$(seen "^Z")
type_serial $'\x1a' || fail "the serial console took no Ctrl+Z"
await_line "^Z" "the terminal did not take Ctrl+Z for the foreground job" "$stops" 10
sleep 1
stopped_at=$(seen "sleepcheck: count")
baseline_lines=$(wc -l <"$serial")
ended=$(seen "ServiceManager: sleep: the transaction ended")
launch sleepctl suspend idle 3 >"$state/sleepctl-job.log" || true
await_end "$ended" "the sleep with a stopped job"
sleep 2
(($(seen "sleepcheck: count") == stopped_at)) || fail "the job stopped with Ctrl+Z ran again after the thaw"
last="$(grep -a -o "sleepcheck: count [0-9]*" "$serial" | tail -n 1 | cut -d' ' -f3)"
resumed_n=$(seen "sleepcheck: count $((last + 1)) ")
counted=$(seen "sleepcheck: count done")
type_serial fg || fail "the serial console took no input"
await_line "sleepcheck: count $((last + 1)) " "fg did not resume the stopped job at its next value" "$resumed_n" 20
# THE JOB RUN TO ITS END: it holds the serial terminal's foreground, and a line typed for the shell before it ends is
# the job's input - case 7 types at that shell.
await_line "sleepcheck: count done" "the resumed job never finished" "$counted" 120
say "a job stopped with Ctrl+Z stayed stopped across the sleep, and fg resumed it at count $((last + 1))"

# 4. THE HOT-PLUG SLOT AFTER THE S3s: the sleep gate's fixture - an `ivshmem-plain` function over a file the harness
#    shares - `device_add`-ed into the empty native hot-plug port and seen by the kernel, which proves the restored Slot
#    Control; and bound, as the last binding, which the next cases need.
grep -a -q "carries hot-plug slot .* - empty" "$serial" || fail "this machine has no empty native hot-plug slot"
fixture_memory="$state/sleep-fixture.bin"
python3 -c "open('$fixture_memory', 'wb').write(bytes(1 << 20))"
arrived=$(seen "a device arrived in the slot behind")
online=$(seen "driver.sleep-fixture: online")
qmp object-add "{\"qom-type\": \"memory-backend-file\", \"id\": \"sleepfix\", \"size\": 1048576, \"mem-path\": \"$fixture_memory\", \"share\": true}" >/dev/null || fail "QEMU refused the fixture's memory"
qmp device_add '{"driver": "ivshmem-plain", "memdev": "sleepfix", "bus": "hotplug0", "id": "sleepfix0"}' >/dev/null || fail "QEMU refused the device_add"
await_line "a device arrived in the slot behind" "a device plugged in after the S3 was not seen - the slot's control was not restored" "$arrived" 60
await_line "driver.sleep-fixture: online" "the sleep fixture's driver did not bind the hot-plugged function" "$online" 60
say "a function hot-plugged after the S3s was seen by the kernel and bound"

# THE FIXTURE'S WORDS: the control word at 0, the release word at 4.
fixture_word() {
	python3 - "$fixture_memory" "$1" "$2" <<'EOF'
import struct, sys
with open(sys.argv[1], 'r+b', buffering=0) as handle:
	handle.seek(int(sys.argv[2]))
	handle.write(struct.pack('<I', int(sys.argv[3], 0)))
EOF
}
HOLD=0x444C4F48
REFUSE=0x53554652
RELEASED=0x534C4552

# 6. A DRIVER MADE TO REFUSE aborts the suspend: the transaction unwound at the drivers' step naming the fixture, and the
#    system still running.
fixture_word 0 "$REFUSE"
ended=$(seen "ServiceManager: sleep: the transaction ended")
baseline_lines=$(wc -l <"$serial")
launch sleepctl suspend idle 3 >"$state/sleepctl-refuse.log" 2>&1 || true
await_line "ServiceManager: sleep: the transaction ended" "the refused suspend never ended" "$ended" 60
refused="$(tail -n +"$((baseline_lines + 1))" "$serial" | grep -a "ServiceManager: sleep: the transaction ended" | tail -n 1)"
grep -q "unwound at Drivers" <<<"$refused" || fail "the refusal did not unwind the drivers' step: $refused"
grep -q "sleep_fixture\|01:00.0" <<<"$refused" || fail "the refusal did not name the fixture's binding: $refused"
tail -n +"$((baseline_lines + 1))" "$serial" | grep -a -q "sleep: entered" && fail "a refused suspend entered the sleep anyway"
out="$(launch uname)" || true
grep -q "LiberSystem" <<<"$out" || fail "the system is not running after the refused suspend"
say "a driver's refusal unwound the suspend, named: $refused"

# 7. A PERSON'S SHUTDOWN DURING A SLEEP THAT WOULD OTHERWISE SUCCEED. The fixture holds its answer; `sleepctl` runs in the
#    background at the serial shell, so the shell is back at its prompt; `shutdown` is typed once the fixture says it
#    holds, and the fixture is released once ServiceManager says it took the orderly sequence. Required: no `sleep:
#    entered`, the unwind naming the sequence and its door and no driver, the power verb after it, QEMU's exit - and
#    never a refusal as a transaction already running.
fixture_word 0 "$HOLD"
fixture_word 4 0
baseline_lines=$(wc -l <"$serial")
holds=$(seen "driver.sleep-fixture: holds its answer")
took=$(seen "took the orderly sequence")
type_serial "sleepctl suspend idle 30 &" || fail "the serial console took no input"
await_line "driver.sleep-fixture: holds its answer" "the fixture never held the drivers' step" "$holds" 60
type_serial shutdown || fail "the serial console took no input"
await_line "took the orderly sequence (door: the admin channel's !poweroff)" "ServiceManager did not take the shutdown during the held step" "$took" 30
fixture_word 4 "$RELEASED"
for _ in $(seq 1 120); do
	[[ "$(run_state)" == "unanswered" ]] && break
	sleep 1
done
[[ "$(run_state)" == "unanswered" ]] || fail "QEMU did not exit after the shutdown taken during the sleep"
after="$(tail -n +"$((baseline_lines + 1))" "$serial")"
grep -a -q "sleep: entered" <<<"$after" && fail "the machine slept although the shutdown ended the transaction"
grep -a -q "SLEEPING\|already running" <<<"$after" && fail "the shutdown was refused as a transaction already running"
unwound="$(grep -a "ServiceManager: sleep: the transaction ended" <<<"$after" | tail -n 1)"
grep -q "the orderly sequence (the admin channel's !poweroff)" <<<"$unwound" || fail "the unwind did not name the orderly sequence and its door: $unwound"
grep -q "sleep_fixture\|01:00.0" <<<"$unwound" && fail "the unwind named a driver, not the sequence: $unwound"
echo "$after" >"$state/shutdown-during-sleep.log"
python3 - "$state/shutdown-during-sleep.log" <<'EOF' || fail "ServiceManager's power verb did not follow the unwind"
import sys
text = open(sys.argv[1], errors='replace').read()
at = text.rfind('the transaction ended')
if at < 0 or 'power-off' not in text[at:]:
	sys.exit(1)
EOF
say "a shutdown typed during a held drivers' step ended the transaction, and the machine powered off without sleeping"

# ------------------------------------------------------------------ the platform boot

# THE SECOND BOOT: P02M0196d's fixture SSDT with the sleep gate's devices - the lid, the control-method power and sleep
# buttons and a Time and Alarm Device - and the development switch naming the CMOS RTC absent, so the wall clock is the
# TAD's and a suspend to RAM's timed wake is the TAD's timer. The harness plays the TAD's clock FIVE YEARS AHEAD of the
# host's, and keeps it running.
TAD_AHEAD_S=$((5 * 365 * 86400))

start_fixture() {
	fixture="$state/fixture"
	mkdir -p "$fixture"
	python3 src/harness/acpi-fixture.py --out "$fixture/fixture.aml" --memory "$fixture/ivshmem.bin" --sleep
	python3 src/harness/vhost-i2c-gpio.py --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$state/backend.log" 2>&1 &
	backend=$!
	for _ in $(seq 1 100); do
		[[ -e "$fixture/ready" ]] && break
		sleep 0.05
	done
	[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start (see $kept/backend.log)"
	# THE TAD'S CLOCK, RUNNING: its sixteen bytes rewritten four times a second from the host's clock and the offset.
	python3 - "$fixture/ivshmem.bin" "$TAD_AHEAD_S" <<'EOF' &
import os, struct, sys, time, datetime
path, ahead = sys.argv[1], int(sys.argv[2])
while True:
	at = datetime.datetime.fromtimestamp(int(time.time()) + ahead, tz=datetime.timezone.utc)
	grt = struct.pack('<HBBBBBBHhB3x', at.year, at.month, at.day, at.hour, at.minute, at.second, 1, 0, 2047, 0)
	with open(path, 'r+b', buffering=0) as handle:
		handle.seek(0x60)
		handle.write(grt)
	time.sleep(0.25)
EOF
	ticker=$!
	export ACPI_FIXTURE="$fixture/fixture.aml" ACPI_FIXTURE_MEMORY="$fixture/ivshmem.bin"
	export I2C_FIXTURE=bus I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
	export QEMU_EXTRA="-global ICH9-LPC.disable_s3=0 -fw_cfg name=opt/org.libersystem/absent,string=rtc"
	# AND NOTHING OUTSIDE TO ASK: TimeService disciplines the wall clock against an NTP server where the network reaches
	# one, which would replace the TAD's clock this boot checks the kernel reads.
	export NET_RESTRICT=1
}

sleep_event() {
	python3 src/harness/acpi-fixture.py --sleep-event "$fixture/ivshmem.bin" "$fixture/control.sock" "$1" >>"$state/events.log" 2>&1 || fail "the fixture could not raise $1 (see $kept/events.log)"
}

# ONE SUSPEND TO RAM WITH NO TIMED WAKE, asked for by the platform - `what` names it - and woken by the host.
await_s3_and_wake() {
	local what="$1" ended="$2" while_suspended="${3:-}"
	await_state suspended 60 "$what"
	sleep 2
	if [[ -n "$while_suspended" ]]; then
		"$while_suspended"
	fi
	qmp system_wakeup >/dev/null || fail "QEMU refused system_wakeup"
	await_state running 30 "$what after system_wakeup"
	await_end "$ended" "$what"
}

# FAN1'S LEVEL IN THE PAGES, waited for.
fan_await() {
	local want="$1" what="$2" value=""
	for _ in $(seq 1 60); do
		value="$(python3 src/harness/acpi-fixture.py --processor-read "$fixture/ivshmem.bin" | python3 -c 'import json, sys; print(json.load(sys.stdin)["fan1"])')"
		[[ "$value" == "$want" ]] && return 0
		sleep 0.5
	done
	fail "$what (its level reads $value, not $want)"
}

# IN S3, FAN1'S LEVEL CLEARED in the pages - what firmware that resets a fan across a sleep leaves.
fan_cleared() {
	python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" fan1=0
}

# THE POWER RESOURCES AND STATES THE PAGES HOLD, checked against a Python condition over them: `pslp_on`, `pslp_ons`,
# `pslp_offs`, `pwak_on`, `pwak_ons`, `pwak_offs`, `lid_ps` and `tad_ps` (0xFF before a device's first `_PSx`).
power_check() {
	local label="$1" condition="$2" power
	power="$(python3 src/harness/acpi-fixture.py --power-read "$fixture/ivshmem.bin")"
	python3 - "$power" "$condition" <<'EOF' || fail "$label: the power resources are not as they must be ($2): $power"
import json, sys
values = json.loads(sys.argv[1])
sys.exit(0 if eval(sys.argv[2], {}, values) else 1)
EOF
	say "platform: $label - $power"
}

lid_s3_power() {
	power_check "during the lid's S3" "lid_ps == 3 and tad_ps == 3 and pslp_on == 0 and pslp_offs == 1 and pslp_ons == 1 and pwak_on == 1 and pwak_ons == 1"
}

# THE WALL CLOCK IS THE TAD'S: `date` in the guest within two seconds of the host's plus the offset.
tad_clock_check() {
	local label="$1" guest host skew
	guest="$(launch date | grep -a -o -m 1 '[0-9]\{4\}-[0-9][0-9]-[0-9][0-9]T[0-9:]*Z' || true)"
	host=$(($(date -u +%s) + TAD_AHEAD_S))
	[[ -n "$guest" ]] || fail "$label: date printed no time"
	skew=$((host - $(date -u -d "$guest" +%s)))
	((skew >= -2 && skew <= 3)) || fail "$label: the wall clock is ${skew} s from the TAD's ($guest) - the kernel is not reading the TAD's clock"
	# A FILE WRITTEN NOW is stamped with the TAD's time, to the minute: written to this run's USB stick - a FAT volume,
	# whose entries StorageService stamps from the kernel's RTC read, where the live system volume carries no times - and
	# its directory entry read back on the host from the stick's private copy.
	launch write "vol://usb/tad-$label.txt" "stamped by the TAD's clock" >/dev/null
	local stick entry
	stick="$(run_stick)"
	[[ -n "$stick" ]] || fail "$label: this run's USB stick was not found under .build/boot"
	entry="$(MTOOLS_SKIP_CHECK=1 mdir -i "$stick" "::tad-$label.txt" 2>&1 || true)"
	python3 - "$entry" "$host" "tad-$label.txt" <<'EOF' || fail "$label: the file written is not stamped with the TAD's time ($(date -u -d "@$host" +'%Y-%m-%d %H:%M')): $entry"
import datetime, re, sys
entry, host, name = sys.argv[1], int(sys.argv[2]), sys.argv[3]
# mdir pads an hour under ten with a space, not a zero.
found = re.search(r'(\d{4}-\d\d-\d\d) +(\d{1,2}:\d\d) +' + re.escape(name), entry)
if not found:
	sys.exit(1)
stamped = datetime.datetime.strptime(f'{found.group(1)} {found.group(2)}', '%Y-%m-%d %H:%M').replace(tzinfo=datetime.timezone.utc).timestamp()
sys.exit(0 if abs(stamped - host) <= 120 else 1)
EOF
	say "platform: $label - the wall clock is the TAD's (${skew} s off), and a file written is stamped with it"
}

# THIS RUN'S USB STICK: the private copy `qemu-run.sh` made for the instance's QEMU (`usb-media-dev-STATE.KEY.PID.img`,
# the PID its own, since the runner execs QEMU) - the QEMU whose command line names this gate's private state.
run_stick() {
	local pid sticks cmdline
	for pid in $(ps -eo pid=,comm= | awk '$2 ~ /^qemu-system/ {print $1}'); do
		cmdline="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)"
		if [[ "$cmdline" == *"$state/"* ]]; then
			sticks=(.build/boot/usb-media*."$pid".img)
			[[ -f "${sticks[0]}" ]] && printf '%s\n' "${sticks[0]}"
			return 0
		fi
	done
}

run_platform() {
	./dev.sh down >"$state/down-s3.log" 2>&1 || true
	stop_tpm
	start_fixture
	say "platform: booting with the sleep fixture and the CMOS RTC named absent"
	# A NEW BOOT, A NEW LOG: the first boot's follower is stopped and its stamps kept aside.
	kill "$follower" 2>/dev/null || true
	cp -f "$stamped" "$state/serial-stamped-s3.log" 2>/dev/null || true
	cp -f "$serial" "$state/serial-s3.log" 2>/dev/null || true
	: >"$serial"
	: >"$stamped"
	./dev.sh up >"$state/up-platform.log" 2>&1 || fail "the platform instance did not come up (see $kept/up-platform.log)"
	follow_serial
	for needle in "driver.acpi-button: acpi:\\_SB_.LID0: online" "driver.acpi-button: acpi:\\_SB_.PWRB: online" "driver.acpi-button: acpi:\\_SB_.SLPB: online" "driver.acpi-button: kernel:pwrbtn: online (PowerButton, fixed hardware)" "driver.acpi-tad: acpi:\\_SB_.TAD0: online"; do
		await_line "$needle" "a sleep device's driver never came online" 0 120
	done
	await_line "and is handed to the kernel" "the TAD's clock was never handed to the kernel" 0 60
	await_line "PowerService: sleep policy: follows the lid" "the sleep policy never followed the lid" 0 60
	# BOTH POWER BUTTONS - the fixed one and PWRB - AND THE SLEEP BUTTON, each a provider the policy follows.
	await_line "PowerService: sleep policy: follows the power button" "the sleep policy never followed both power buttons" 1 60
	await_line "PowerService: sleep policy: follows the sleep button" "the sleep policy never followed the sleep button" 0 60
	tad_clock_check boot
	# BOTH HOLDERS IN D0 and the shared resource turned on once.
	await_line "acpi:\\_SB_.TAD0 is in D0" "the TAD was never put in D0" 0 60
	await_line "acpi:\\_SB_.LID0 is in D0" "the lid was never put in D0" 0 60
	power_check "after the boot" "lid_ps == 0 and tad_ps == 0 and pslp_on == 1 and pslp_ons == 1 and pslp_offs == 0 and pwak_on == 0"

	# THE LID, AS THE OWNER'S DEFAULTS HAVE IT: closing it turns the screen off - black, and a line typed then reaches no
	# frame - and suspends nothing; opening it turns the screen on again.
	local ended asked out off on
	await_line "PowerService: sleep policy: closing the lid turns the screen off, idleness after 900 s suspends nothing, a critical battery does nothing, the power button powers off in order, the sleep button suspends" "the policy did not start with the owner's defaults" 0 60
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	off=$(seen "DisplayService: the screen is off")
	on=$(seen "DisplayService: the screen is on")
	screen "$state/screen-lid-open.png"
	black "$state/screen-lid-open.png" && fail "the screen is black before the lid closed - nothing to tell an off screen by"
	sleep_event lid-close
	await_line "PowerService: sleep policy: turns the screen off" "closing the lid did not turn the screen off" 0 30
	await_line "DisplayService: the screen is off" "DisplayService did not turn the screen off" "$off" 30
	sleep 1
	screen "$state/screen-lid-closed.png"
	black "$state/screen-lid-closed.png" || fail "the screen is not black with the lid closed"
	./dev.sh key --text "echo dark" >/dev/null 2>&1 || fail "the emulated keyboard took no keys with the lid closed"
	sleep 2
	screen "$state/screen-lid-typed.png"
	black "$state/screen-lid-typed.png" || fail "a line typed with the lid closed reached the screen"
	[[ "$(run_state)" == running ]] || fail "the machine is not running with the lid closed - $(run_state)"
	(($(seen "ServiceManager: sleep: the transaction ended") == ended)) || fail "closing the lid put the machine to sleep"
	sleep_event lid-open
	await_line "DisplayService: the screen is on" "opening the lid did not turn the screen on" "$on" 30
	sleep 2
	screen "$state/screen-lid-opened.png"
	black "$state/screen-lid-opened.png" && fail "the screen stayed black after the lid opened"
	say "platform: closing the lid turned the screen off - black, a typed line unseen - and suspended nothing; opening it turned the screen on"

	# THE LID SET TO SUSPEND AND THE SLEEP BUTTON TO NOTHING, AND THE POLICY KILLED AND RELAUNCHED: the relaunched instance
	# reads both settings and suspends - to RAM, which this machine offers - through its own `system-sleep` client, with the
	# lid as the reason. The settings are given back to their defaults once the relaunched instance has read them: the tree
	# outlives this instance's boots.
	local restarted sleep_follows
	out="$(launch set power.lid suspend)" || fail "set power.lid suspend was not run: $out"
	grep -q "ok" <<<"$out" || fail "set power.lid suspend was refused: $out"
	out="$(launch set power.sleep-button nothing)" || fail "set power.sleep-button nothing was not run: $out"
	grep -q "ok" <<<"$out" || fail "set power.sleep-button nothing was refused: $out"
	restarted=$(seen "supervisor: power_service restarted")
	sleep_follows=$(seen "PowerService: sleep policy: follows the sleep button")
	out="$(launch stop '!crash power_service')" || fail "the crash hook was not reached: $out"
	await_line "supervisor: power_service restarted" "ServiceManager did not relaunch the killed power service" "$restarted" 60
	await_line "PowerService: sleep policy: follows the lid" "the relaunched policy never followed the lid" 1 60
	await_line "PowerService: sleep policy: follows the sleep button" "the relaunched policy never followed the sleep button" "$sleep_follows" 60
	await_line "PowerService: sleep policy: closing the lid suspends, idleness after 900 s suspends nothing, a critical battery does nothing, the power button powers off in order, the sleep button does nothing" "the relaunched policy did not read power.lid and power.sleep-button" 0 30
	for key in "power.lid screen-off" "power.sleep-button suspend"; do
		# shellcheck disable=SC2086 # the key and its value are words of their own, as `set` takes them
		out="$(launch set $key)" || fail "set $key was not run: $out"
		grep -q "ok" <<<"$out" || fail "${key%% *} could not be given back to its default: $out"
	done
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	asked=$(seen "PowerService: sleep policy: asks for a suspend (Lid)")
	sleep_event lid-close
	await_line "PowerService: sleep policy: asks for a suspend (Lid)" "the relaunched policy asked for no suspend on the lid" "$asked" 30
	await_s3_and_wake "the lid's suspend" "$ended" lid_s3_power
	sleep_event lid-open
	out="$(launch sleepctl last)" || true
	grep -q "(Lid)" <<<"$out" || fail "the last sleep's record does not name the lid"
	power_check "after the lid's wake" "lid_ps == 0 and tad_ps == 0 and pslp_on == 1 and pslp_ons == 2 and pwak_on == 0 and pwak_offs == 1"
	await_line "the clock's source says the sleep lasted" "the kernel took no sleep length from the TAD's clock" 0 30
	tad_clock_check lid
	say "platform: the lid set to suspend, the relaunched power service suspended to RAM on it, and the TAD's clock measured the sleep"

	# THE SLEEP BUTTON SET TO NOTHING: told to the policy, which does nothing - said, no transaction, the machine running.
	local told nothing
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	told=$(seen "SLPB: pressed - told the power-state policy")
	nothing=$(seen "PowerService: sleep policy: the sleep button was pressed - it does nothing")
	sleep_event sleep-button
	await_line "SLPB: pressed - told the power-state policy" "the sleep button's press was not told to the policy" "$told" 30
	await_line "PowerService: sleep policy: the sleep button was pressed - it does nothing" "the policy did not say the sleep button set to nothing does nothing" "$nothing" 30
	sleep 5
	(($(seen "ServiceManager: sleep: the transaction ended") == ended)) || fail "the sleep button set to nothing put the machine to sleep"
	[[ "$(run_state)" == running ]] || fail "the machine is not running after the sleep button set to nothing - $(run_state)"
	say "platform: the sleep button set to nothing was told to the policy, which did nothing"

	# THE TAD'S TIMED WAKE: the driver programs the timer in its step, and the host - QEMU has no TAD to raise it - wakes
	# the guest at that time.
	local calls
	calls="$(python3 src/harness/acpi-fixture.py --tad-read "$fixture/ivshmem.bin" | python3 -c 'import json,sys; print(json.load(sys.stdin)["stv_calls"])')"
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	launch sleepctl suspend ram 30 >"$state/sleepctl-tad.log" 2>&1 &
	await_state suspended 60 "the TAD's timed suspend"
	local timers
	timers="$(python3 src/harness/acpi-fixture.py --tad-read "$fixture/ivshmem.bin")"
	python3 - "$timers" "$calls" <<'EOF' || fail "the TAD's driver did not program the sleep's timed wake: $timers"
import json, sys
timers, before = json.loads(sys.argv[1]), int(sys.argv[2])
if timers['stv_calls'] <= before or not 25 <= timers['ac_timer'] <= 31:
	sys.exit(1)
EOF
	# THE TAD WAKES THIS SLEEP: it stayed in D0 and so did the resource it shares; the lid went to D3hot.
	power_check "during the TAD's timed S3" "tad_ps == 0 and lid_ps == 3 and pslp_on == 1 and pslp_offs == 1 and pwak_on == 1"
	sleep 5
	qmp system_wakeup >/dev/null || fail "QEMU refused system_wakeup"
	await_state running 30 "the TAD's timed suspend after its wake"
	await_end "$ended" "the TAD's timed suspend"
	grep -a -q "its timer is set to wake the machine in" "$serial" || fail "the TAD's driver did not say it set its timer"
	tad_clock_check timed
	power_check "after the TAD's wake" "lid_ps == 0 and tad_ps == 0 and pslp_on == 1 and pslp_offs == 1 and pwak_on == 0"
	say "platform: the TAD's driver set its timer to the sleep's timed wake ($timers)"

	# THE FAN ACROSS A SLEEP: FAN1 - left to the operating system in fine steps - set through its curve, then a suspend to
	# idle and an S3 cycle, each answered SUSPENDED and RESUMED by both fans' drivers; in S3 the host clears FAN1's level
	# in the pages, as firmware may, and the resume applies the level commanded before the sleep again.
	# KEPT WHOLE, THEN SEARCHED: under pipefail a grep that stops reading early fails the launch it reads.
	out="$(launch powerctl curve '\_SB_.FAN1' 20:60)" || true
	grep -a -q "follows the curve" <<<"$out" || fail "fan: powerctl could not set FAN1's curve: $out"
	fan_await 60 "fan: FAN1 was not set to its curve's 60 %"
	local suspended resumed
	suspended=$(seen "DeviceManager: suspended acpi-fan")
	resumed=$(seen "DeviceManager: resumed acpi-fan")
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	launch sleepctl suspend idle 5 >"$state/sleepctl-fan-idle.log" 2>&1 || true
	await_end "$ended" "the fan's suspend to idle"
	(($(seen "DeviceManager: suspended acpi-fan") >= suspended + 2 && $(seen "DeviceManager: resumed acpi-fan") >= resumed + 2)) || fail "fan: both fans' drivers did not answer SUSPENDED and RESUMED across the suspend to idle"
	fan_await 60 "fan: after the suspend to idle FAN1 does not hold its level"
	suspended=$(seen "DeviceManager: suspended acpi-fan")
	resumed=$(seen "DeviceManager: resumed acpi-fan")
	local found
	found=$(seen "at the resume it was found at level 0 (0 rpm) - level 60 applied again")
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	launch sleepctl suspend ram >"$state/sleepctl-fan-ram.log" 2>&1 &
	await_s3_and_wake "the fan's S3" "$ended" fan_cleared
	(($(seen "DeviceManager: suspended acpi-fan") >= suspended + 2 && $(seen "DeviceManager: resumed acpi-fan") >= resumed + 2)) || fail "fan: both fans' drivers did not answer SUSPENDED and RESUMED across S3"
	await_line "at the resume it was found at level 0 (0 rpm) - level 60 applied again" "fan: FAN1's driver did not find the cleared level and apply its own again" "$found" 30
	fan_await 60 "fan: after the S3 resume FAN1 does not hold the level commanded before the sleep"
	say "fan: across a suspend to idle and S3 both fans' drivers answered SUSPENDED and RESUMED, and the level cleared in S3 was applied again"

	# THE CONTROL-METHOD SLEEP BUTTON AT ITS DEFAULT, read by the policy relaunched once more: told to the policy, which
	# suspends - to RAM - with the sleep button as the reason.
	local defaults asked_button
	restarted=$(seen "supervisor: power_service restarted")
	defaults=$(seen "a critical battery does nothing, the power button powers off in order, the sleep button suspends")
	sleep_follows=$(seen "PowerService: sleep policy: follows the sleep button")
	out="$(launch stop '!crash power_service')" || fail "the crash hook was not reached: $out"
	await_line "supervisor: power_service restarted" "ServiceManager did not relaunch the killed power service" "$restarted" 60
	await_line "a critical battery does nothing, the power button powers off in order, the sleep button suspends" "the policy relaunched again did not read the defaults back" "$defaults" 60
	await_line "PowerService: sleep policy: follows the sleep button" "the policy relaunched again never followed the sleep button" "$sleep_follows" 60
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	told=$(seen "SLPB: pressed - told the power-state policy")
	asked_button=$(seen "PowerService: sleep policy: asks for a suspend (SleepButton)")
	sleep_event sleep-button
	await_line "SLPB: pressed - told the power-state policy" "the sleep button's press was not told to the policy" "$told" 30
	await_line "PowerService: sleep policy: asks for a suspend (SleepButton)" "the policy asked for no suspend on the sleep button" "$asked_button" 30
	await_s3_and_wake "the sleep button's suspend" "$ended"
	out="$(launch sleepctl last)" || true
	grep -q "(SleepButton)" <<<"$out" || fail "the last sleep's record does not name the sleep button"
	say "platform: the control-method sleep button suspended the machine"

	# THE CONTROL-METHOD POWER BUTTON, LAST: told to the policy, which powers the machine off in order - ServiceManager's
	# sequence through `system-shutdown`, then the registered \_S5.
	sleep_event power-button
	await_line "PWRB: pressed - told the power-state policy" "the power button's press was not told to the policy" 0 30
	await_line "PowerService: sleep policy: the power button was pressed - it powers off in order" "the policy did not power off on the power button" 0 30
	await_line "supervisor: system-shutdown asked for the orderly power-off" "ServiceManager did not take the power button's orderly power-off" 0 30
	for _ in $(seq 1 60); do
		[[ "$(run_state)" == "unanswered" ]] && break
		sleep 1
	done
	grep -a -q "power-off: the registered \\\\_S5" "$serial" || fail "the power-off did not name the registered \\_S5"
	orderly_without_faults "platform: the power button's power-off"
	say "platform: the control-method power button, told to the policy, powered the machine off in order through the registered \\_S5 - every driver stopped before its device was taken"
	kill "$ticker" "$backend" 2>/dev/null || true
}

# THE CRITICAL BATTERY - see the head of this file. The forced bound is the policy's ten seconds; QEMU must be gone within
# it of the line that says it is armed, give or take the serial poll.
FORCED_BOUND_S=10
run_battery() {
	./dev.sh down >"$state/down-platform.log" 2>&1 || true
	start_fixture
	say "battery: booting with the fixture's battery on mains"
	kill "$follower" 2>/dev/null || true
	cp -f "$serial" "$state/serial-platform.log" 2>/dev/null || true
	: >"$serial"
	: >"$stamped"
	./dev.sh up >"$state/up-battery.log" 2>&1 || fail "the battery instance did not come up (see $kept/up-battery.log)"
	follow_serial
	await_line "PowerService: sleep policy: follows the lid" "the sleep policy never started" 0 60
	await_line "PowerService: sleep policy: closing the lid turns the screen off, idleness after 900 s suspends nothing, a critical battery does nothing" "battery: the policy did not start with the owner's defaults" 0 60
	status_out="$(launch sleepctl status)"
	grep -q "hibernation: not set up" <<<"$status_out" || fail "battery: hibernation is set up on a machine with no hibernation partition: $status_out"
	# THE OWNER'S DEFAULT: the battery discharged past critical with the adapter off line, and nothing done - said, and
	# the machine still running and answering a while after.
	python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" ac-online=0 battery-state=5 battery-rate=9000 battery-remaining=1000 >/dev/null || fail "battery: the fixture's pages could not be written"
	python3 - "$fixture/control.sock" <<'EOF' || fail "battery: the power line could not be raised"
import socket, sys, time
def ask(text):
	sock = socket.socket(socket.AF_UNIX)
	sock.connect(sys.argv[1])
	sock.settimeout(5)
	sock.sendall((text + '\n').encode())
	return sock.recv(4096).decode()
ask('lower 3')
time.sleep(0.3)
if 'fired' not in ask('raise 3'):
	sys.exit('raising line 3 fired no event')
EOF
	await_line "PowerService: sleep policy: a battery is critical - the policy does nothing, as it is set to" "battery: the policy did not say it does nothing for the critical battery" 0 60
	sleep $((FORCED_BOUND_S + 5))
	(($(seen "PowerService: sleep policy: a battery is critical - powers the machine off in order") == 0)) || fail "battery: the policy powered off, which the owner's default does not"
	(($(seen "PowerService: sleep policy: a battery is critical - asks for hibernation") == 0)) || fail "battery: the policy asked for hibernation, which the owner's default does not"
	(($(seen "supervisor: system-shutdown asked for the orderly power-off") == 0)) || fail "battery: an orderly power-off was asked for under the owner's default"
	status_out="$(launch sleepctl status)" || fail "battery: the machine did not answer $((FORCED_BOUND_S + 5)) s after its battery went critical: $status_out"
	say "battery: a critical battery did nothing, as the owner's default has it - said, and the machine still running"
	# THE SETTING THAT POWERS OFF: `power.critical power-off`, read by a relaunched instance - which finds the battery
	# critical at its first reading and acts on it through its own clients: the forced deadline, ServiceManager's sequence,
	# the registered `\_S5`, and QEMU gone within the bound. The key stays set: the machine is off at the end of it.
	local restarted out
	out="$(launch set power.critical power-off)" || fail "set power.critical power-off was not run: $out"
	grep -q "ok" <<<"$out" || fail "set power.critical power-off was refused: $out"
	# QEMU'S EXIT, TIMED BY A CONNECTION HELD UNTIL IT CLOSES: opened before the relaunch, its close stamped by the host.
	local events="$state/qmp-battery.log"
	python3 - "$state/qemu-qmp.sock" "$events" <<'EOF' &
import json, socket, sys, time
sock = socket.socket(socket.AF_UNIX)
sock.connect(sys.argv[1])
stream = sock.makefile('rw')
stream.readline()
stream.write(json.dumps({'execute': 'qmp_capabilities'}) + '\n')
stream.flush()
with open(sys.argv[2], 'w') as out:
	for line in stream:
		message = json.loads(line)
		if 'return' in message and not out.tell():
			out.write('connected\n')
		elif 'event' in message:
			out.write(f"{time.time():.3f} {message['event']}\n")
		out.flush()
	out.write(f'closed {time.time():.3f}\n')
EOF
	for _ in $(seq 1 50); do
		grep -q '^connected' "$events" 2>/dev/null && break
		sleep 0.2
	done
	grep -q '^connected' "$events" 2>/dev/null || fail "battery: the held QMP connection was not taken"
	restarted=$(seen "supervisor: power_service restarted")
	out="$(launch stop '!crash power_service')" || fail "battery: the crash hook was not reached: $out"
	await_line "supervisor: power_service restarted" "battery: ServiceManager did not relaunch the killed power service" "$restarted" 60
	await_line "PowerService: sleep policy: closing the lid turns the screen off, idleness after 900 s suspends nothing, a critical battery powers off in order" "battery: the relaunched policy did not read power.critical" 0 60
	await_line "PowerService: sleep policy: a battery is critical - powers the machine off in order" "battery: the relaunched policy did not act on the critical battery" 0 60
	await_line "PowerService: sleep policy: the machine is off within ${FORCED_BOUND_S} s" "battery: the forced deadline was not armed" 0 30
	local armed_at gone_at
	armed_at="$(stamp_of "the machine is off within ${FORCED_BOUND_S} s" 0)"
	for _ in $(seq 1 $((FORCED_BOUND_S * 4))); do
		grep -q '^closed' "$events" && break
		sleep 0.5
	done
	grep -q '^closed' "$events" || fail "battery: QEMU was still up $((FORCED_BOUND_S * 2)) s after the forced deadline was armed"
	gone_at="$(grep '^closed' "$events" | cut -d' ' -f2)"
	grep -a -q "supervisor: system-shutdown asked for the orderly power-off" "$serial" || fail "battery: ServiceManager did not take the orderly power-off through system-shutdown"
	grep -a -q "power-off: the registered \\\\_S5" "$serial" || fail "battery: the power-off did not name the registered \\_S5"
	orderly_without_faults "battery"
	local took
	took=$(ms_between "$armed_at" "$gone_at")
	((took <= FORCED_BOUND_S * 1000 + 1000)) || fail "battery: QEMU exited ${took} ms after the forced deadline was armed, past its ${FORCED_BOUND_S} s bound"
	say "battery: set to power off, a critical battery powered the machine off in order through the relaunched policy, QEMU gone ${took} ms after the ${FORCED_BOUND_S} s deadline was armed"
	kill "$ticker" "$backend" 2>/dev/null || true
}

# THE FIXED PORTS' HALF OF SOFT-OFF: a boot whose development switch refuses the sleep-type registration, so nothing is
# registered and power-off takes the fixed ports.
run_soft_off_fallback() {
	./dev.sh down >"$state/down-platform.log" 2>&1 || true
	unset ACPI_FIXTURE ACPI_FIXTURE_MEMORY I2C_FIXTURE I2C_SOCKET GPIO_SOCKET
	export QEMU_EXTRA="-fw_cfg name=opt/org.libersystem/absent,string=sleep-types"
	cp -f "$serial" "$state/serial-platform.log" 2>/dev/null || true
	: >"$serial"
	./dev.sh up >"$state/up-fallback.log" 2>&1 || fail "the fallback instance did not come up (see $kept/up-fallback.log)"
	type_serial shutdown || fail "the serial console took no input"
	for _ in $(seq 1 120); do
		grep -a -q "power-off: the fixed ports" "$serial" && break
		sleep 1
	done
	grep -a -q "power-off: the fixed ports (no \\\\_S5 registered)" "$serial" || fail "the power-off with nothing registered did not take the fixed ports"
	orderly_without_faults "soft-off"
	say "soft-off: with the registration refused, power-off took the fixed ports"
}

# THE WATCHDOG BOOT: P02M0200's i6300esb armed by the policy at 15 s, and `-action watchdog=pause`, which every QMP boot
# carries - so an expiry leaves QEMU in the run state `watchdog` until somebody continues it. A suspend to idle three
# times longer than the timeout, `running` at every reading through it and after; then a resume made to hang after the
# drivers (ServiceManager's development hook) - the driver re-armed first, at most `RESUME_WATCH_MS`, and nothing after
# it answering `alive` - which ends in `watchdog`.
WATCHDOG_TIMEOUT_MS=15000
RESUME_WATCH_S=120

hold_running() {
	local seconds="$1" what="$2" now
	for at in $(seq 1 "$seconds"); do
		now="$(run_state)"
		[[ "$now" == "running" ]] || fail "$what: the guest was $now ${at} s into ${seconds} s"
		sleep 1
	done
}

run_watchdog() {
	./dev.sh down >"$state/down-fallback.log" 2>&1 || true
	export QEMU_EXTRA="-device i6300esb"
	kill "$follower" 2>/dev/null || true
	cp -f "$serial" "$state/serial-fallback.log" 2>/dev/null || true
	: >"$serial"
	: >"$stamped"
	./dev.sh up >"$state/up-watchdog.log" 2>&1 || fail "the watchdog instance did not come up (see $kept/up-watchdog.log)"
	follow_serial
	await_line "driver.i6300esb: online" "the i6300esb's driver never came online" 0 120
	local out armed
	armed=$(seen "WatchdogService: armed i6300esb at")
	for pair in "watchdog.enabled on" "watchdog.device i6300esb" "watchdog.timeout-ms $WATCHDOG_TIMEOUT_MS" "watchdog.period-ms 4000" "watchdog.deadline-ms 2000"; do
		# shellcheck disable=SC2086 # the key and its value are two words, as `set` takes them
		out="$(launch set $pair)" || fail "set $pair was not run: $out"
		grep -q "ok" <<<"$out" || fail "set $pair was refused: $out"
	done
	launch stop watchdog_service >/dev/null
	launch start watchdog_service >/dev/null
	await_line "WatchdogService: armed i6300esb at" "the watchdog service did not arm the i6300esb" "$armed" 60
	hold_running 5 "the i6300esb armed"
	# A SUSPEND TO IDLE LONGER THAN THE TIMEOUT: `running` throughout and after.
	local ended
	ended=$(seen "ServiceManager: sleep: the transaction ended")
	launch sleepctl suspend idle 45 >"$state/sleepctl-watchdog.log" 2>&1 &
	hold_running 50 "a suspend to idle three times the watchdog's timeout"
	await_end "$ended" "the suspend to idle past the watchdog's timeout"
	hold_running 45 "the i6300esb after the long sleep"
	say "watchdog: a suspend to idle three times its timeout and the time after it passed with the guest running"
	# THE RESUME MADE TO HANG: `watchdog` within the resume's bound.
	out="$(launch stop '!sleep-hang')" || fail "the sleep-hang hook was not reached: $out"
	grep -q "SLEEP HANG ARMED" <<<"$out" || fail "the development hook did not arm the hang: $out"
	local hangs
	hangs=$(seen "the resume hangs after the drivers")
	launch sleepctl suspend idle 5 >"$state/sleepctl-hang.log" 2>&1 &
	await_line "the resume hangs after the drivers" "the resume did not stop at the hook" "$hangs" 60
	local start now
	start=$(date +%s)
	while true; do
		now="$(run_state)"
		[[ "$now" == "watchdog" ]] && break
		(($(date +%s) - start <= RESUME_WATCH_S + 30)) || fail "the watchdog did not expire within $((RESUME_WATCH_S + 30)) s of a resume that hung - the guest is $now"
		sleep 1
	done
	say "watchdog: a resume that hung after the drivers ended in the watchdog's expiry after $(($(date +%s) - start)) s"
	qmp quit >/dev/null 2>&1 || true
}

run_platform
run_battery
run_soft_off_fallback
run_watchdog

say "PASS"
