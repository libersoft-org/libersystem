#!/usr/bin/env bash
# THE PROCESSORS' AND THE THERMAL POLICY'S POWER, end to end on x86_64 q35: the fixture SSDT's processor objects
# (`harness/acpi-fixture.py --processors`) evaluated by the ACPI service, installed in the kernel by
# ProcessorPowerService, and the registers the kernel writes read by this script from the region the fixture's
# `ivshmem-plain` function shares - so what the governor ASKED FOR is seen, not inferred.
#
#   1. THE PROCESSORS - three cores, the invariant TSC asked for (`INVTSC=on`):
#        - C000's `_PSS` through `_PCT`'s registers, C001's CPPC with its energy preference, and every core's `_LPI`
#          naming the same two entry registers, installed - none refused; C002's `_PCT` of model-specific registers
#          refused whole by the kernel;
#        - at rest the slowest state written to `_PCT`'s control register and CPPC's lowest desired performance; under a
#          load the fastest, on whichever of the two cores the load ran - this scheduler places a thread where it was
#          started and balances nothing, so the load is launched again until it lands on one of them;
#        - the profiles through `powerctl`: performance pins both cores at their fastest (0x10 and 255) with preference 0,
#          power saving caps C000 at its second state with preference 192;
#        - a `_PPC` notification (line 7) capping C000 under the performance profile: read again, acknowledged through
#          `_OST`, and the state written held to the cap;
#        - the `_LPI` states with their latencies: where this host's CPU model gives the invariant TSC, the scripted idle
#          pattern enters them and the per-state counts say so; where it does not, every state deeper than the halt is
#          left unentered with the reason on the log, and the counts show only the halt;
#        - a live latency request while AudioService plays, in the per-core record `powerctl` prints;
#        - ProcessorPowerService killed and relaunched: every table installed again, none refused but C002's.
#   2. THE ZONE HEATED, through the relaunched instance and its clients: past `_AC0` FAN0's level (`_FSL`) rises and
#      FAN1 follows the default curve; past `_PSV` passive cooling engages, `_TMP` sampled every `_TSP`, and - the
#      performance profile pinning C000 at its fastest - the state written falls; past `_CRT` - and `_HOT` with it - the
#      forced deadline is armed, the orderly power-off runs, and QEMU is gone within the bound.
#   3. THE ZONE HEATED WITH PROCESSORPOWERSERVICE STOPPED: the zone's driver arms the forced deadline itself, and QEMU is
#      gone at the bound with no orderly power-off.
#
# IT BOOTS ITS OWN INSTANCES in private state, one at a time, and takes the last one down from the EXIT trap.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

FORCED_BOUND_S=10
# The fixture's `_PSS` control values, fastest first.
PSS=(16 17 18 19)

fail() {
	echo "processor-power: $*" >&2
	exit 1
}

say() {
	echo "processor-power: $*"
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-procpower.XXXXXX")"
fixture="$state/fixture"
mkdir -p "$fixture"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/processor-power"
rm -rf "$kept"
mkdir -p "$kept"
label="none"
backend_pid=""
loads=()

keep_log() {
	cp -f "$state/dev-serial.log" "$kept/serial-$label.log" 2>/dev/null || true
}

cleanup() {
	local status=$?
	keep_log
	for pid in "${loads[@]}"; do
		kill "$pid" 2>/dev/null || true
	done
	./dev.sh down >"$state/down.log" 2>&1 || echo "processor-power: teardown reported a problem (see $kept/down.log)" >&2
	if [[ -n "$backend_pid" ]]; then
		kill "$backend_pid" 2>/dev/null || true
		wait "$backend_pid" 2>/dev/null || true
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

has() {
	grep -a -q -F -- "$1" "$(serial_log)" || fail "$2 (no line containing: $1)"
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

# A LAUNCH WHOSE OUTPUT MUST SAY `pattern`, kept in `$state/<log>.log` and searched once the launch has ended:
# `launch | grep -q` under pipefail fails a launch that prints a line after grep has stopped reading.
launch_says() {
	local pattern="$1" log="$2"
	shift 2
	launch "$@" >"$state/$log.log" || true
	grep -a -q -- "$pattern" "$state/$log.log"
}

# ONE VALUE THE KERNEL OR THE FIRMWARE WROTE INTO THE PAGES (`acpi-fixture.py --processor-read`).
page() {
	python3 src/harness/acpi-fixture.py --processor-read "$fixture/ivshmem.bin" | python3 -c "import json, sys; print(json.load(sys.stdin)['$1'])"
}

# WAIT UNTIL `name` READS `want`, at most `limit` seconds.
await_page() {
	local name="$1" want="$2" what="$3" limit="${4:-20}" value=""
	for _ in $(seq 1 $((limit * 4))); do
		value="$(page "$name")"
		[[ "$value" == "$want" ]] && return 0
		sleep 0.25
	done
	fail "$what ($name reads $value, not $want, after ${limit} s)"
}

# A LOAD ON EVERY CORE for `seconds`: one launch, a thread per core - launches through the agent run one at a time.
load_cores() {
	local seconds="$1" cores="$2"
	loads=()
	./dev.sh launch --timeout $((seconds + 30)) procprobe load "$seconds" "$cores" >>"$state/load.log" 2>&1 &
	loads+=("$!")
}

end_load() {
	for pid in "${loads[@]}"; do
		wait "$pid" 2>/dev/null || true
	done
	loads=()
}

# QEMU'S RECORD OF C000's `_OST`, as "event status": the q35 DSDT declares `\_SB.CPUS.C000._OST` for CPU hot-plug, it
# stands before the fixture's objects, and QEMU keeps the last event and status each processor's `_OST` was given.
ospm_c000() {
	python3 - "$state/qemu-qmp.sock" <<'EOF'
import json, socket, sys
sock = socket.socket(socket.AF_UNIX)
sock.settimeout(10)
sock.connect(sys.argv[1])
stream = sock.makefile('rw')
stream.readline()
def ask(command):
	stream.write(json.dumps({'execute': command}) + '\n')
	stream.flush()
	while True:
		message = json.loads(stream.readline())
		if 'return' in message or 'error' in message:
			return message
ask('qmp_capabilities')
for info in ask('query-acpi-ospm-status').get('return', []):
	if info.get('slot-type') == 'CPU' and info.get('slot') == '0':
		print(f"{info['source']} {info['status']}")
		break
else:
	print('none')
EOF
}

raise() {
	python3 src/harness/acpi-fixture.py --raise "$fixture/control.sock" "$1" >/dev/null || fail "line $1 fired no event"
}

start_helpers() {
	python3 src/harness/acpi-fixture.py --out "$fixture/fixture.aml" --memory "$fixture/ivshmem.bin" --processors
	python3 src/harness/vhost-i2c-gpio.py --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$fixture/backend.log" 2>&1 &
	backend_pid="$!"
	for _ in $(seq 1 100); do
		[[ -e "$fixture/ready" ]] && break
		sleep 0.05
	done
	[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start (see $kept/backend.log)"
	export ACPI_FIXTURE="$fixture/fixture.aml" ACPI_FIXTURE_MEMORY="$fixture/ivshmem.bin"
	export I2C_FIXTURE=bus I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
	export SMP=3 INVTSC=on
}

boot() {
	label="$1"
	say "boot '$label' (state $state, host port $HOSTFWD_PORT)"
	if ! ./dev.sh up --timeout 400 >"$state/up-$label.log" 2>&1; then
		tail -20 "$state/up-$label.log" >&2
		fail "the development instance for '$label' did not come up (see $kept/up-$label.log)"
	fi
	await_line "ProcessorPowerService: online" "ProcessorPowerService never came online" 0
}

# QEMU'S EXIT, TIMED BY A QMP CONNECTION HELD UNTIL IT CLOSES - opened before the heating, its events and its close
# stamped by the host.
hold_qmp() {
	local events="$1"
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
	grep -q '^connected' "$events" 2>/dev/null || fail "the held QMP connection was not taken"
}

# THE ZONE AT `tenths` KELVIN, read by its driver at the power line's notification.
heat() {
	python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" zone-temperature="$1"
	raise 3
}

# ------------------------------------------------------------------ 1. the processors

start_helpers
boot processors
for core in 0 1 2; do
	await_line "\\_SB_.CPUS.C00$core: core $core's idle table installed - 3 state(s)" "core $core's _LPI was not installed" 0 60
done
await_line "\\_SB_.CPUS.C000: core 0's performance table installed - 4 level(s)" "C000's _PSS was not installed" 0 60
await_line "\\_SB_.CPUS.C001: core 1's performance table installed - 32 level(s)" "C001's CPPC was not installed" 0 60
await_line "processor: core 2's performance table is refused - " "the kernel did not refuse C002's model-specific _PCT" 0 60
await_line "\\_SB_.CPUS.C002: core 2's performance table was refused" "ProcessorPowerService did not say C002's table was refused" 0 60
(($(seen "idle table of") == 0)) || fail "an idle table was refused"
say "every core's _LPI installed, C000's _PSS and C001's CPPC installed, and C002's model-specific _PCT refused whole"

# AT REST THE SLOWEST - balanced, the default on line power.
await_page pct_control "${PSS[3]}" "at rest, C000 was not left at its slowest state"
await_page cpc_desired 50 "at rest, C001's desired performance is not CPPC's lowest"
await_page cpc_preference 128 "the balanced profile's energy preference was not written"
say "at rest C000's control register reads 0x13 and C001's desired performance 50, with the balanced preference 128"

# UNDER LOAD THE FASTEST, on whichever of C000 and C001 the load ran: launched again until it lands on one of them.
loaded=""
for attempt in 1 2 3 4 5; do
	load_cores 6 3
	for _ in $(seq 1 20); do
		if [[ "$(page pct_control)" == "${PSS[0]}" ]]; then
			loaded=C000
			break
		fi
		if [[ "$(page cpc_desired)" == 255 ]]; then
			loaded=C001
			break
		fi
		sleep 0.25
	done
	end_load
	[[ -n "$loaded" ]] && break
done
[[ -n "$loaded" ]] || fail "under five loads neither C000 nor C001 was given its fastest level"
await_page pct_control "${PSS[3]}" "after the load, C000 did not fall back to its slowest state"
await_page cpc_desired 50 "after the load, C001 did not fall back to CPPC's lowest"
say "under load $loaded was given its fastest level (attempt $attempt), and fell back to its slowest after"

# THE PROFILES: performance pins both cores at their fastest, power saving caps C000.
launch_says "the profile is performance" profile-performance powerctl profile performance || fail "powerctl could not set the performance profile"
await_page cpc_preference 0 "the performance profile's energy preference was not written"
await_page pct_control "${PSS[0]}" "under the performance profile, C000 is not at its fastest state"
await_page cpc_desired 255 "under the performance profile, C001's desired performance is not CPPC's highest"
say "performance pins C000 at 0x10 and C001 at 255, with preference 0"

# `_PPC` CAPS C000: read again on its `Notify`, acknowledged through `_OST`, obeyed above the performance profile.
before=$(seen "_OST(0x80, 0) acknowledged")
ospm="$(ospm_c000)"
[[ "$ospm" != "128 0" ]] || fail "QEMU recorded C000's _OST as (0x80, 0) before any notification"
python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" ppc=2
raise 7
await_line "\\_SB_.CPUS.C000 notified 0x80" "C000's Notify(0x80) was not followed" 0 30
await_line "_OST(0x80, 0) acknowledged" "the _PPC change was not acknowledged through _OST" "$before" 30
ospm="$(ospm_c000)"
[[ "$ospm" == "128 0" ]] || fail "QEMU's record of C000's _OST is '$ospm', not (0x80, 0)"
await_page pct_control "${PSS[2]}" "with _PPC 2, C000 was not held to its third state above the performance profile"
python3 src/harness/acpi-fixture.py --set "$fixture/ivshmem.bin" ppc=0
raise 7
await_line "_OST(0x80, 0) acknowledged" "the _PPC release was not acknowledged" "$((before + 1))" 30
await_page pct_control "${PSS[0]}" "with _PPC back at 0, C000 did not return to its fastest state"
say "a _PPC of 2 notified: read again, acknowledged through _OST, and C000 held to 0x12 above the performance profile"

launch_says "the profile is power-saving" profile-power-saving powerctl profile power-saving || fail "powerctl could not set the power-saving profile"
await_page cpc_preference 192 "the power-saving profile's energy preference was not written"
await_page pct_control "${PSS[3]}" "at rest in power saving, C000 is not at its slowest state"
launch_says "core 0: 3 idle state(s), level [0-9]* of 4, window 1\.\.3" status-power-saving powerctl status || fail "power saving did not cap C000's window at its second state"
launch_says "the profile is balanced" profile-balanced powerctl profile balanced || fail "powerctl could not set the balanced profile"
await_page cpc_preference 128 "the balanced profile's preference was not written back"
say "power saving caps C000's window at 1..3 with preference 192, and balanced writes 128 back"

# THE `_LPI` STATES: entered where the invariant TSC is there, left unentered with the reason where it is not.
status_before="$(launch powerctl status)"
launch_says "idle done" idle-pattern procprobe idle 6 || fail "the idle pattern did not run"
status_after="$(launch powerctl status)"
printf '%s\n' "$status_before" >"$state/status-before.log"
printf '%s\n' "$status_after" >"$state/status-after.log"
# THE ENTRIES `powerctl status` (on stdin) gives core $1's state $2 - the program in `-c`, since a here-document would
# take the place of the stdin the status arrives on.
entries() {
	python3 -c '
import re, sys
core, state = sys.argv[1], int(sys.argv[2])
text = sys.stdin.read()
section = text.split(f"core {core}:", 1)[1] if f"core {core}:" in text else ""
match = re.search(rf"state {state}: .*? - (\d+) entries", section)
print(match.group(1) if match else "none")
' "$1" "$2"
}
halt_before="$(entries 0 0 <<<"$status_before")"
halt_after="$(entries 0 0 <<<"$status_after")"
[[ "$halt_before" != none && "$halt_after" != none ]] || fail "powerctl printed no entries for core 0's halt"
((halt_after > halt_before)) || fail "the idle pattern entered core 0's halt no more times ($halt_before, then $halt_after)"
if (($(seen "is not entered - this CPU has no invariant TSC") > 0)); then
	for core in 0 1 2; do
		for index in 1 2; do
			has "processor: core $core's idle state $index" "core $core's state $index was neither entered nor said to be left unentered"
			[[ "$(entries "$core" "$index" <<<"$status_after")" == 0 ]] || fail "core $core's state $index, unenterable on this host, was entered"
		done
	done
	say "this host's CPU model gives no invariant TSC: the _LPI states deeper than the halt are left unentered with the reason on the log, and the pattern entered only the halt ($halt_before, then $halt_after)"
else
	deep_before="$(entries 0 1 <<<"$status_before")"
	deep_after="$(entries 0 1 <<<"$status_after")"
	((deep_after > deep_before)) || fail "the idle pattern's long waits never entered core 0's state 1 ($deep_before, then $deep_after)"
	say "the idle pattern entered core 0's halt and its _LPI state 1"
fi

# A LIVE LATENCY REQUEST while AudioService plays.
./dev.sh launch --timeout 60 beep 440 4000 >"$state/beep.log" 2>&1 &
beep_pid=$!
held=""
for _ in $(seq 1 20); do
	if launch_says "live latency request(s), the smallest 1000 us" status-latency powerctl status; then
		held=yes
		break
	fi
	sleep 0.2
done
wait "$beep_pid" 2>/dev/null || true
[[ -n "$held" ]] || fail "no live latency request was seen while AudioService played"
say "AudioService's playback held a 1000 us latency request, in the per-core record"

# RELAUNCHED: every table installed again, none refused but C002's.
restarted=$(seen "supervisor: processor_power_service restarted")
installed=$(seen "core 0's idle table installed")
out="$(launch stop '!crash processor_power_service')" || fail "the crash hook was not reached: $out"
await_line "supervisor: processor_power_service restarted" "ServiceManager did not relaunch ProcessorPowerService" "$restarted" 60
await_line "ProcessorPowerService: online" "the relaunched ProcessorPowerService never came online" 1 60
await_line "core 0's idle table installed" "the relaunched instance did not install core 0's table again" "$installed" 60
for core in 1 2; do
	(($(seen "core $core's idle table installed") >= 2)) || fail "the relaunched instance did not install core $core's idle table again"
done
(($(seen "core 1's performance table installed") >= 2)) || fail "the relaunched instance did not install C001's CPPC again"
(($(seen "idle table of") == 0)) || fail "an idle table was refused after the relaunch"
say "ProcessorPowerService killed and relaunched: every table installed again, none refused but C002's model-specific one"

# ------------------------------------------------------------------ 2. the zone heated, through the relaunched instance

await_line "follows \\_TZ_.TZ00" "the relaunched instance did not follow the zone" 1 60
heat 3450
await_page fan0 100 "past _AC0, FAN0 was not run at its fastest level"
await_page fan1 75 "at 71.8 C, FAN1 did not follow the default curve's 75 %"
say "past _AC0 FAN0 runs at level 100, and FAN1 follows the default curve"

launch_says "the profile is performance" profile-heating powerctl profile performance || fail "powerctl could not set the performance profile before the heating"
await_page pct_control "${PSS[0]}" "under the performance profile before passive cooling, C000 is not at its fastest state"
heat 3550
await_line "TZ00 is past _PSV" "passive cooling did not engage past _PSV" 0 30
await_line "_TMP is read every 1000 ms for the thermal policy" "the zone was not sampled every _TSP" 0 30
slower=""
for _ in $(seq 1 60); do
	value="$(page pct_control)"
	if ((value > PSS[0])); then
		slower="$value"
		break
	fi
	sleep 0.25
done
[[ -n "$slower" ]] || fail "past _PSV the state C000 was given did not fall"
say "past _PSV passive cooling engaged, sampled every _TSP, and C000 was given $(printf '%#x' "$slower") above the performance profile"

events="$state/qmp-critical.log"
hold_qmp "$events"
heat 3700
await_line "is past _CRT - the machine powers off" "the policy did not act on _CRT" 0 30
await_line "the forced power-off deadline is armed" "the forced deadline was not armed" 0 30
armed_at="$(date +%s.%N)"
for _ in $(seq 1 $((FORCED_BOUND_S * 4))); do
	grep -q '^closed' "$events" && break
	sleep 0.5
done
grep -q '^closed' "$events" || fail "QEMU was still up $((FORCED_BOUND_S * 2)) s after the forced deadline was armed"
grep -q 'SHUTDOWN' "$events" || fail "QMP sent no SHUTDOWN event"
gone_at="$(grep '^closed' "$events" | cut -d' ' -f2)"
has "supervisor: system-shutdown asked for the orderly power-off" "ServiceManager did not take the orderly power-off"
took=$(python3 -c "import sys; print(int((float(sys.argv[2]) - float(sys.argv[1])) * 1000))" "$armed_at" "$gone_at")
((took <= FORCED_BOUND_S * 1000 + 1000)) || fail "QEMU exited ${took} ms after the forced deadline was armed, past its ${FORCED_BOUND_S} s bound"
keep_log
say "past _CRT: the forced deadline armed, the orderly power-off run, QEMU gone ${took} ms after"

# ------------------------------------------------------------------ 3. the zone heated with the policy stopped

./dev.sh down >"$state/down-processors.log" 2>&1 || true
kill "$backend_pid" 2>/dev/null || true
wait "$backend_pid" 2>/dev/null || true
start_helpers
boot stopped
await_line "publishes its ThermalZone state, and its cooling half" "the zone's driver did not publish its cooling half" 0 60
out="$(launch stop processor_power_service)"
grep -q "^stopped:" <<<"$out" || fail "ProcessorPowerService could not be stopped: $out"
events="$state/qmp-stopped.log"
hold_qmp "$events"
heat 3700
await_line "the zone is past _CRT - the forced power-off is armed" "the zone's driver did not arm the forced power-off itself" 0 30
armed_at="$(date +%s.%N)"
for _ in $(seq 1 $((FORCED_BOUND_S * 4))); do
	grep -q '^closed' "$events" && break
	sleep 0.5
done
grep -q '^closed' "$events" || fail "with the policy stopped, QEMU was still up $((FORCED_BOUND_S * 2)) s after the zone's driver armed the deadline"
gone_at="$(grep '^closed' "$events" | cut -d' ' -f2)"
took=$(python3 -c "import sys; print(int((float(sys.argv[2]) - float(sys.argv[1])) * 1000))" "$armed_at" "$gone_at")
((took >= FORCED_BOUND_S * 1000 - 1500 && took <= FORCED_BOUND_S * 1000 + 1500)) || fail "with the policy stopped, QEMU exited ${took} ms after the deadline was armed - not at the ${FORCED_BOUND_S} s bound"
(($(seen "system-shutdown asked for the orderly power-off") == 0)) || fail "with the policy stopped, an orderly power-off still ran"
keep_log
say "with ProcessorPowerService stopped, the zone's driver armed the deadline and QEMU was gone ${took} ms after, at the bound"
say "PASS - the processors' tables installed and obeyed, _PPC and the profiles, the _LPI states, a latency request, a relaunch, and the zone heated through the policy and without it"
