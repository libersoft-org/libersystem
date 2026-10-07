#!/usr/bin/env bash
# THE HARDWARE WATCHDOG, end to end on x86_64 q35, one device at a time with the policy naming it.
#
# THE ORACLE IS DURABLE: every boot here has a QMP socket and `-action watchdog=pause`, so an expiry stops the guest
# in the run state `watchdog` until somebody continues it - and `query-status` (`./dev.sh run-state`) read at any
# time says whether an expiry happened at any point before. A stretch read as `running` throughout is a stretch with
# no expiry in it; nothing has to be listening when the timer fires.
#
# THE TIMING: timeout 15 s, a liveness question every 4 s, an answer within 2 s - T at least 3P, D under P, so one
# late round never resets the machine. T below is the EFFECTIVE timeout the arm answered, from the service's line.
#
#   1. THE i6300ESB beside q35's TCO, policy on and naming the i6300esb: the service arms the i6300esb and disarms
#      the TCO; three timeouts of a healthy system, still `running`; the watchdog service killed and restarted (the
#      replacement arms the i6300esb again) and three timeouts `running`; the i6300esb's driver killed and rebound
#      (the new instance takes the running timer over at bind, the service takes it again) and three timeouts
#      `running`; an orderly `reboot` with the timer armed - the notice delivered, answered and the timer disarmed,
#      named in the log BEFORE the first service is stopped; and in the next boot ServiceManager's hook stops
#      answering `alive` at T0: `running` until T0 + T - P - D less a margin, `watchdog` by T0 + T plus it.
#   2. THE RESET, with `-action watchdog=reset` and no `-no-reboot`: the same silence ends in a second boot on the
#      serial log, whose i6300esb reports that the last reset was its watchdog's and whose TCO does not.
#   3. THE TCO alone, policy naming it: armed - No-Reboot cleared and read back - three timeouts `running`, and the
#      silence ends in `watchdog`.
#   4. A WDAT over the TCO's registers, from `harness/wdat-table.py` through `-acpitable`: the WDAT driver online and
#      NO TCO driver in that boot (the table suppresses the LPC row), armed, three timeouts `running`, and the
#      silence ends in `watchdog`.
#
#   5. THE BMC'S, through the IPMI driver on ISA KCS and QEMU's simulated BMC, beside q35's TCO. Its expiry is a
#      chassis reset - no QEMU watchdog action - so the oracle is the BMC's own record, Get Watchdog Timer as the next
#      boot's driver reads it at bind (the bind line names the initial countdown and the expiration flag) with the
#      serial log's boot count: an orderly `reboot` gives it the boot bound, which the next bind finds running; the
#      same reboot with the notice skipped by ServiceManager's development hook leaves the short timeout - the next bind
#      finds it, running or expired; and the silence expires it - the machine resets and the next boot's driver reports
#      the watchdog as the cause, the TCO's reporting none.
#
# ACROSS A SLEEP is the sleep milestone's to add - there is no sleep entry to drive yet. The i6300esb on aarch64 and riscv64 runs as the scenario `watchdog-i6300esb` under
# `./lab.sh scenario-cold ARCH`, with `QEMU_EXTRA=-device i6300esb`.
#
# IT BOOTS ITS OWN INSTANCES in private state, as the development lifecycle gate does, one at a time, and takes the
# last one down from the EXIT trap whatever happened.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "watchdog: $*" >&2
	exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

TIMEOUT_MS=15000
PERIOD_MS=4000
DEADLINE_MS=2000
# How far either side of the stated window a reading may land: the scheduling of a guest, and of this script.
MARGIN=3

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-wd.XXXXXX")"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/watchdog"
rm -rf "$kept"
mkdir -p "$kept"
label="none"

# The boot's serial log, kept under its label before the next boot truncates it.
keep_log() {
	cp -f "$state/dev-serial.log" "$kept/serial-$label.log" 2>/dev/null || true
}

cleanup() {
	local status=$?
	keep_log
	./dev.sh down >"$state/down.log" 2>&1 || echo "watchdog: teardown reported a problem (see $kept/down.log)" >&2
	cp -f "$state"/*.log "$kept/" 2>/dev/null || true
	rm -f .build/boot/*"-dev-$(basename "$state")"* 2>/dev/null || true
	rm -rf "$state"
	return "$status"
}
trap cleanup EXIT

serial_log() {
	echo "$state/dev-serial.log"
}

# How many lines of the guest's serial log match a pattern, right now.
seen() {
	grep -a -c -- "$1" "$(serial_log)" 2>/dev/null || true
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

# THE RUN STATE, asked again when QMP does not answer: QEMU's QMP socket takes one client at a time, so a reading can
# land while the last one is still being let go. A missed reading loses nothing - `watchdog` stays until `cont` - so
# three in a row unanswered, not one, is the failure.
run_state() {
	local now
	for _ in 1 2 3; do
		if now="$(./dev.sh run-state 2>/dev/null)"; then
			echo "$now"
			return 0
		fi
		sleep 0.5
	done
	echo "unanswered"
}

# `running` at every reading for `seconds`, one a second.
hold_running() {
	local seconds="$1" what="$2" now
	for at in $(seq 1 "$seconds"); do
		now="$(run_state)"
		[[ "$now" == "running" ]] || fail "$what: the guest was $now ${at} s into ${seconds} s"
		sleep 1
	done
}

# A shell tool, launched through the development agent.
launch() {
	./dev.sh launch --timeout 60 "$@" 2>&1
}

# THE POLICY FOR ONE DEVICE, and a stop and start of the watchdog service so it reads it.
policy() {
	local device="$1" out online
	for pair in "watchdog.enabled on" "watchdog.device $device" "watchdog.timeout-ms $TIMEOUT_MS" "watchdog.period-ms $PERIOD_MS" "watchdog.deadline-ms $DEADLINE_MS"; do
		# shellcheck disable=SC2086 # the key and its value are two words, as `set` takes them
		out="$(launch set $pair)" || fail "set $pair was not run: $out"
		grep -q "ok" <<<"$out" || fail "set $pair was refused: $out"
	done
	online=$(seen "WatchdogService: online - armed by policy")
	out="$(launch stop watchdog_service)" || fail "the watchdog service was not stopped: $out"
	out="$(launch start watchdog_service)" || fail "the watchdog service was not started: $out"
	grep -q "watchdog_service" <<<"$out" || fail "the watchdog service did not start again: $out"
	await_line "WatchdogService: online - armed by policy" "the restarted watchdog service did not read the policy" "$online"
}

# The effective timeout of the last arm of `device`, in whole seconds rounded up.
effective() {
	local device="$1" ms
	ms="$(grep -a -o -- "WatchdogService: armed $device at [0-9]* ms" "$(serial_log)" | tail -1 | grep -o '[0-9]* ms' | grep -o '[0-9]*')"
	[[ -n "$ms" ]] || fail "no arm of $device was reported"
	echo $(((ms + 999) / 1000))
}

# THE SILENCE: ServiceManager stops answering `alive` at T0; `running` until T0 + T - P - D - MARGIN, and
# `watchdog` by T0 + T + D + MARGIN.
silence_expires() {
	local device="$1" t out start early late now elapsed
	t="$(effective "$device")"
	out="$(launch stop '!liveness-silence')" || fail "the liveness hook was not reached: $out"
	grep -q "LIVENESS SILENT" <<<"$out" || fail "the development hook did not silence ServiceManager: $out"
	start="$(date +%s)"
	early=$((t - PERIOD_MS / 1000 - DEADLINE_MS / 1000 - MARGIN))
	late=$((t + DEADLINE_MS / 1000 + MARGIN))
	while true; do
		now="$(run_state)"
		elapsed=$(($(date +%s) - start))
		if [[ "$now" == "watchdog" ]]; then
			((elapsed >= early)) || fail "$device expired ${elapsed} s after the silence, before T - P - D less the margin (${early} s)"
			echo "watchdog: $device expired ${elapsed} s after the silence (window ${early}..${late} s, T ${t} s)"
			return 0
		fi
		[[ "$now" == "running" ]] || fail "the guest was $now ${elapsed} s after the silence"
		((elapsed <= late)) || fail "$device had not expired ${elapsed} s after the silence (T ${t} s)"
		sleep 0.5
	done
}

boot() {
	label="$1"
	echo "watchdog: boot '$label' (state $state, host port $HOSTFWD_PORT)"
	if ! ./dev.sh up --timeout 300 >"$state/up-$label.log" 2>&1; then
		tail -20 "$state/up-$label.log" >&2
		fail "the development instance for '$label' did not come up (see $kept/up-$label.log)"
	fi
	await_line "WatchdogService: online" "the watchdog service never came online" 0
}

take_down() {
	keep_log
	./dev.sh down >"$state/down-$label.log" 2>&1 || fail "the '$label' instance did not come down"
}

# ------------------------------------------------------------------ 1. the i6300esb beside the TCO

export QEMU_EXTRA="-device i6300esb"
export WATCHDOG_ACTION=pause
boot i6300esb
await_line "driver.i6300esb: online" "the i6300esb's driver never came online" 0
await_line "driver.tco: online" "the TCO's driver never came online beside it" 0
armed=$(seen "WatchdogService: armed i6300esb at")
disarmed=$(seen "WatchdogService: disarmed tco")
policy i6300esb
await_line "WatchdogService: armed i6300esb at" "the service did not arm the i6300esb" "$armed"
await_line "WatchdogService: disarmed tco" "the service did not disarm the TCO beside it" "$disarmed"
t="$(effective i6300esb)"
echo "watchdog: the i6300esb is armed at ${t} s and the TCO disarmed - three timeouts of a healthy system"
hold_running $((3 * t)) "a healthy system with the i6300esb armed"

# THE SERVICE KILLED: restarted, and the replacement arms the i6300esb again.
armed=$(seen "WatchdogService: armed i6300esb at")
restarted=$(seen "supervisor: watchdog_service restarted")
out="$(launch stop '!crash watchdog_service')" || fail "the crash hook was not reached: $out"
grep -q "CRASHED" <<<"$out" || fail "ServiceManager did not kill the watchdog service: $out"
await_line "supervisor: watchdog_service restarted" "ServiceManager did not restart the killed watchdog service" "$restarted"
await_line "WatchdogService: armed i6300esb at" "the replacement did not arm the i6300esb again" "$armed"
hold_running $((3 * t)) "the i6300esb after the watchdog service's restart"
echo "watchdog: the watchdog service was killed and restarted, and the timer never expired"

# THE DRIVER KILLED: rebound, the running timer taken over at bind, and taken again by the service.
armed=$(seen "WatchdogService: armed i6300esb at")
online=$(seen "driver.i6300esb: online - running at bind")
out="$(./dev.sh kernel-console kill-driver 8086:25ab 2>&1)" || fail "the development kernel did not kill the i6300esb's driver: $out"
await_line "driver.i6300esb: online - running at bind" "the rebound driver did not take the running timer over" "$online"
await_line "WatchdogService: armed i6300esb at" "the service did not take the rebound i6300esb again" "$armed"
hold_running $((3 * t)) "the i6300esb after its driver's rebind"
echo "watchdog: the i6300esb's driver was killed and rebound, and the timer never expired"

# THE ORDERLY REBOOT: the notice answered and the timer disarmed before the first service is stopped.
before="$(wc -l <"$(serial_log)")"
online=$(seen "WatchdogService: online")
./lab.sh sh --timeout 20 reboot >/dev/null 2>&1 || true
await_line "WatchdogService: online" "the system did not come back from the orderly reboot" "$online" 300
after="$(tail -n +"$((before + 1))" "$(serial_log)")"
first_stop="$(grep -a -n -m 1 -E -- "^supervisor: [a-z_]+ stopped" <<<"$after" | cut -d: -f1)"
disarm_at="$(grep -a -n -m 1 -- "WatchdogService: disarmed i6300esb for the shutdown" <<<"$after" | cut -d: -f1)"
answer_at="$(grep -a -n -m 1 -- "supervisor: watchdog_service answered the shutdown notice" <<<"$after" | cut -d: -f1)"
[[ -n "$disarm_at" ]] || fail "the watchdog service did not disarm the i6300esb for the reboot"
[[ -n "$answer_at" ]] || fail "the watchdog service's answer to the shutdown notice is not in the log"
[[ -n "$first_stop" ]] || fail "no service was stopped by the orderly reboot"
((disarm_at < first_stop && answer_at < first_stop)) || fail "the notice was not answered before the first service stopped (disarm line $disarm_at, answer line $answer_at, first stop line $first_stop)"
echo "watchdog: the orderly reboot told the watchdog service first - the i6300esb was disarmed before any service stopped"

# THE SILENCE, in a boot the instance has recorded - the orderly reboot's was not, so the development channel would
# refuse it - with the policy set again, since a development instance's volume does not keep it across a reboot:
# the i6300esb armed again, then expired.
./dev.sh reboot --timeout 300 >"$state/reboot.log" 2>&1 || fail "the instance was not recorded again after the orderly reboot (see $kept/reboot.log)"
armed=$(seen "WatchdogService: armed i6300esb at")
policy i6300esb
await_line "WatchdogService: armed i6300esb at" "the service did not arm the i6300esb after the reboot" "$armed"
hold_running 2 "the i6300esb armed after the reboot"
silence_expires i6300esb
take_down

# ------------------------------------------------------------------ 2. the reset

export WATCHDOG_ACTION=reset
boot reset
armed=$(seen "WatchdogService: armed i6300esb at")
policy i6300esb
await_line "WatchdogService: armed i6300esb at" "the service did not arm the i6300esb" "$armed"
esb_boots=$(seen "driver.i6300esb: online")
tco_boots=$(seen "driver.tco: online")
first_boot_ends="$(wc -l <"$(serial_log)")"
out="$(launch stop '!liveness-silence')" || fail "the liveness hook was not reached: $out"
grep -q "LIVENESS SILENT" <<<"$out" || fail "the development hook did not silence ServiceManager: $out"
await_line "driver.i6300esb: online" "no second boot followed the expiry" "$esb_boots" 180
await_line "driver.tco: online" "the TCO's driver never came online in the second boot" "$tco_boots" 120
second="$(tail -n +"$((first_boot_ends + 1))" "$(serial_log)")"
grep -q "driver.i6300esb: online - .*the last reset was its own" <<<"$second" || fail "the i6300esb did not report that the last reset was its watchdog's"
if grep -q "driver.tco: online - .*the last reset was its own" <<<"$second"; then
	fail "the TCO reported a reset the i6300esb caused"
fi
echo "watchdog: the i6300esb's expiry reset the machine, and only the i6300esb said so in the next boot"
take_down

# ------------------------------------------------------------------ 3. the TCO

export QEMU_EXTRA=""
export WATCHDOG_ACTION=pause
boot tco
await_line "driver.tco: online" "the TCO's driver never came online" 0
armed=$(seen "WatchdogService: armed tco at")
policy tco
await_line "WatchdogService: armed tco at" "the service did not arm the TCO (No-Reboot may have stayed set)" "$armed"
t="$(effective tco)"
hold_running $((3 * t)) "a healthy system with the TCO armed"
silence_expires tco
take_down

# ------------------------------------------------------------------ 4. WDAT over the TCO

python3 src/harness/wdat-table.py "$state/wdat.bin" || fail "the WDAT fixture was not written"
export QEMU_EXTRA="-acpitable file=$state/wdat.bin"
boot wdat
await_line "driver.wdat: online" "the WDAT's driver never came online" 0
(($(seen "driver.tco: online") == 0)) || fail "a TCO driver came online in a boot with a WDAT"
armed=$(seen "WatchdogService: armed wdat at")
policy wdat
await_line "WatchdogService: armed wdat at" "the service did not arm the WDAT" "$armed"
(($(seen "driver.tco: online") == 0)) || fail "a TCO driver came online in a boot with a WDAT"
t="$(effective wdat)"
hold_running $((3 * t)) "a healthy system with the WDAT armed"
silence_expires wdat
take_down

# ------------------------------------------------------------------ 5. the BMC's

# The effective timeout of the last arm of `device`, in milliseconds as the service said it.
effective_ms() {
	local ms
	ms="$(grep -a -o -- "WatchdogService: armed $1 at [0-9]* ms" "$(serial_log)" | tail -1 | grep -o '[0-9]* ms' | grep -o '[0-9]*')"
	[[ -n "$ms" ]] || fail "no arm of $1 was reported"
	echo "$ms"
}

# The first bind line of the IPMI driver's watchdog past `baseline`, waited for.
next_bind() {
	local baseline="$1"
	await_line "watchdog [a-z]* at bind, initial countdown" "no boot's IPMI driver read its BMC's watchdog at bind" "$baseline" 300
	grep -a -o -- "watchdog [a-z]* at bind, initial countdown [0-9]* ms.*" "$(serial_log)" | tr -d '\r' | sed -n "$((baseline + 1))p"
}

binds() {
	seen "watchdog [a-z]* at bind, initial countdown"
}

# THE BOOT BOUND the orderly reboot arms, set for this case: the 120 s first proposed did not cover this machine's
# development image - its shutdown sequence and the next boot to the IPMI driver's bind - and the BMC reset the booting
# machine; the default is ten minutes since. The case is that the notice arms the CONFIGURED bound, so it configures one
# - the default's value - rather than trusting the default.
BOOT_BOUND_MS=600000

# THE BMC ARMED SHORT: the policy naming it, the boot bound set, the TCO disarmed beside it.
arm_bmc() {
	local armed disarmed out
	armed=$(seen "WatchdogService: armed bmc at")
	disarmed=$(seen "WatchdogService: disarmed tco")
	out="$(launch set watchdog.boot-bound-ms "$BOOT_BOUND_MS")" || fail "set watchdog.boot-bound-ms was not run: $out"
	grep -q "ok" <<<"$out" || fail "set watchdog.boot-bound-ms was refused: $out"
	policy bmc
	await_line "WatchdogService: armed bmc at" "the service did not arm the BMC's watchdog" "$armed"
	await_line "WatchdogService: disarmed tco" "the service did not disarm the TCO beside the BMC's" "$disarmed"
}

export QEMU_EXTRA="-device ipmi-bmc-sim,id=bmc0,guid=11111111-2222-3333-4444-00000000b0c0 -device isa-ipmi-kcs,bmc=bmc0,irq=0"
export WATCHDOG_ACTION=pause
boot bmc
await_line "watchdog [a-z]* at bind, initial countdown" "the IPMI driver never read its BMC's watchdog at bind" 0
await_line "driver.tco: online" "the TCO's driver never came online beside the BMC" 0

# THE ORDERLY REBOOT: the notice gives the BMC the boot bound, and the next bind finds it running with it.
arm_bmc
hold_running 2 "the BMC's watchdog armed"
before="$(wc -l <"$(serial_log)")"
baseline="$(binds)"
./lab.sh sh --timeout 20 reboot >/dev/null 2>&1 || true
bind="$(next_bind "$baseline")"
after="$(tail -n +"$((before + 1))" "$(serial_log)")"
grep -q "WatchdogService: gave bmc $BOOT_BOUND_MS ms and a last pet for the shutdown" <<<"$after" || fail "the orderly reboot did not give the BMC's watchdog the boot bound"
[[ "$bind" == "watchdog running at bind, initial countdown $BOOT_BOUND_MS ms" ]] || fail "after the orderly reboot the BMC's watchdog must be found running with the boot bound and no expiry (read: $bind)"
echo "watchdog: the orderly reboot gave the BMC's watchdog the boot bound, and the next bind found it running with it"

# THE SAME REBOOT WITHOUT THE NOTICE: the short timeout survives - found running, or expired first.
./dev.sh reboot --timeout 300 >"$state/reboot-bmc.log" 2>&1 || fail "the instance was not recorded again after the orderly reboot (see $kept/reboot-bmc.log)"
arm_bmc
short="$(effective_ms bmc)"
hold_running 2 "the BMC's watchdog armed again"
before="$(wc -l <"$(serial_log)")"
baseline="$(binds)"
launch stop '!reboot-without-notice' >/dev/null 2>&1 || true
bind="$(next_bind "$baseline")"
after="$(tail -n +"$((before + 1))" "$(serial_log)")"
grep -q "service_manager: the shutdown notice is skipped (development hook)" <<<"$after" || fail "the development hook did not skip the notice"
if grep -q "WatchdogService: gave bmc .* for the shutdown" <<<"$after"; then
	fail "the watchdog service was told of a reboot whose notice was skipped"
fi
case "$bind" in
"watchdog running at bind, initial countdown $short ms") echo "watchdog: without the notice the next bind found the short timeout ($short ms) still running" ;;
"watchdog stopped at bind, initial countdown $short ms, the last reset was its own") echo "watchdog: without the notice the short timeout ($short ms) ran out first, and the next bind found its expiry" ;;
*) fail "without the notice the next bind must find the short timeout of $short ms, running or expired (read: $bind)" ;;
esac

# THE EXPIRY: the silence, a chassis reset, and the next boot naming the BMC's watchdog - the TCO's naming none.
./dev.sh reboot --timeout 300 >"$state/reboot-bmc2.log" 2>&1 || fail "the instance was not recorded again (see $kept/reboot-bmc2.log)"
arm_bmc
hold_running 2 "the BMC's watchdog armed for the silence"
boots=$(seen "driver.tco: online")
first_boot_ends="$(wc -l <"$(serial_log)")"
baseline="$(binds)"
out="$(launch stop '!liveness-silence')" || fail "the liveness hook was not reached: $out"
grep -q "LIVENESS SILENT" <<<"$out" || fail "the development hook did not silence ServiceManager: $out"
bind="$(next_bind "$baseline")"
await_line "driver.tco: online" "the TCO's driver never came online in the boot after the expiry" "$boots" 120
[[ "$(run_state)" == "running" ]] || fail "the BMC's expiry must reset the machine, not stop it"
second="$(tail -n +"$((first_boot_ends + 1))" "$(serial_log)")"
[[ "$bind" == *", the last reset was its own" ]] || fail "the next boot's IPMI driver did not report that the last reset was its BMC watchdog's (read: $bind)"
if grep -q "driver.tco: online - .*the last reset was its own" <<<"$second"; then
	fail "the TCO reported a reset the BMC caused"
fi
echo "watchdog: the BMC's expiry reset the machine, and only its driver said so in the next boot"
echo "watchdog: PASS - the i6300esb, the TCO, a WDAT over the TCO and the BMC's each armed by name; the chipset timers fed through three timeouts, the i6300esb through a service kill, a driver kill and an orderly reboot, each expiring once ServiceManager stopped answering with the i6300esb's reset reported by it alone; the BMC's given the boot bound by an orderly reboot, keeping its short timeout through one without the notice, and its expiry reported by its own driver alone"
