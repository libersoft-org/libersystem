#!/usr/bin/env bash
# A TYPE-C PORT CONTROLLER AND THE POWER DELIVERY SINK THE SYSTEM RUNS, end to end on x86_64 q35: `vhost-i2c-gpio.py
# --tcpc`'s TCPCI register file at 0x52 on the vhost-user I2C controller, its alert on line 5 of the vhost-user GPIO
# controller, and the Power Delivery SOURCE on its cable (`tcpc_partner.py`, written from the source side and sharing
# no code with the sink) - described by the SSDT `qemu-run.sh` loads for `I2C_FIXTURE=tcpc` (a `PRP0001` device with
# `compatible` `tcpci` and its `connector` data node: Fixed 5 V at 3 A and 15 V at 2 A, operating at 15 W) - through the
# `tcpci` driver bound as a child, TypeCService, PowerService, the `typec` tool and the `typeccheck` probe. One boot:
#
#   a revision 3.1 charger (5, 9, 12, 15, 20 V and PPS): the contract at 15 V at the sink PDO's 2 A, and 9, 12, 20 V and
#     PPS never requested; the 5 V to 15 V transition inside the alarm envelope, no alarm; PR_Swap, DR_Swap, VCONN_Swap
#     and Discover_Identity answered Not_Supported with the contract kept; Get_Sink_Cap answered with the board's PDOs;
#     malformed messages (an object count past the bytes, a reserved type, an extended message) not believed; Wait
#     then Accept; Reject; a new Source_Capabilities with less power (a new request); a transition that overshoots and
#     VBUS driven past the alarm during the 15 V contract (each: the sink path off before the Hard Reset, `source-fault`
#     reported); a Hard Reset the partner sends, VBUS to vSafe0V and back (no detach, the contract made anew); no answer
#     to a Request (Hard Reset not before 24 ms, the hard-reset count kept through the reset); Accept without PS_RDY
#     (Hard Reset not before 450 ms); a Hard Reset after which VBUS stays off (a detach once tSrcRecover and tSrcTurnOn
#     have passed); a detach mid-negotiation;
#   a revision 2.0 charger; a charger below the operating power (5 V, capability mismatch); non-PD chargers at 1.5 A and
#     3 A; a Power Delivery source that sends no Source_Capabilities (Hard Reset not before 310 ms, then the Type-C
#     current after the hard-reset count);
#   the virtio-i2c binding disabled and enabled through the device policy during a 15 V contract - the driver stopped
#     as a lost dependency, bound again with no alarm, and the contract made anew through Soft_Reset with VBUS never
#     dropping;
#   THE TIMING RUN: 200 negotiations, the response from each Source_Capabilities' alert to the Request's TRANSMIT write
#     in this gate's log - under KVM at most 15 ms at the 99th percentile and never 24 ms.
#
# THE PARTNER'S RECORD FAILS THE GATE on any request outside the latest offers or their currents, the sink path enabled
# without VBUS, a hard reset sent with the sink path on, the sink path or the automatic discharge on when a hard reset
# takes VBUS down, or a message sent while the supply moves.
#
# WHAT IT DOES NOT CLAIM: a sleep (carried with the sleep transaction), the ports' tree description (their emulated
# sweep), a real port controller.
#
# IT BOOTS ITS OWN DEVELOPMENT INSTANCE in private state, and takes it down from the EXIT trap.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "typec-tcpci: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "this gate is x86_64's, through the SSDT; the ports run the tree in their emulated sweep"
	;;
*) fail "unexpected argument '$1'" ;;
esac
command -v python3 >/dev/null || fail "python3 is not installed, and the lab is written in it"
[[ ! -S .build/boot/lab-ctl.sock ]] || fail "an ad-hoc lab guest is up (.build/boot/lab-ctl.sock) - take it down with ./lab.sh quit first"

state="$(mktemp -d "${TMPDIR:-/tmp}/liber-tcpci.XXXXXX")"
fixture="$state/fixture"
mkdir -p "$fixture"
export LIBER_DEV_STATE="$state"
HOSTFWD_PORT="$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])')"
export HOSTFWD_PORT
kept="$(pwd)/.build/logs/typec-tcpci"
rm -rf "$kept"
mkdir -p "$kept"
backend_pid=""
case_name="boot"

cleanup() {
	local status=$?
	cp -f "$state/dev-serial.log" "$kept/serial.log" 2>/dev/null || true
	./dev.sh down >"$state/down.log" 2>&1 || echo "typec-tcpci: teardown reported a problem (see $kept/down.log)" >&2
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
	grep -a -q -F -- "$1" "$(serial_log)" || fail "$case_name: $2 (no line containing: $1)"
}

await_line() {
	local needle="$1" what="$2" baseline="$3" limit="${4:-120}"
	for _ in $(seq 1 "$limit"); do
		if (($(seen "$needle") > baseline)); then
			return 0
		fi
		sleep 1
	done
	fail "$case_name: $what (waited ${limit} s for a new line containing: $needle)"
}

# ONE PROBE RUN, its output kept; a run that does not say PASS, or says FAIL, fails the gate.
probe() {
	local out="$state/probe.log" result
	echo "\$ typeccheck $*" >>"$out"
	result="$(./dev.sh launch --timeout 150 typeccheck "$@" 2>&1)" || true
	echo "$result" >>"$out"
	if grep -q "typeccheck: FAIL" <<<"$result" || ! grep -q "typeccheck: \(PASS\|answer\|[0-9]* connector(s)\)" <<<"$result"; then
		echo "$result" >&2
		fail "$case_name: typeccheck $* did not pass"
	fi
	grep -a "typeccheck: \(PASS\|answer\|[0-9]* connector(s)\)" <<<"$result" | tail -1
}

# A PROBE STARTED BEFORE WHAT IT WATCHES FOR: returned once its cue is printed, judged by `probe_judged`.
probe_started() {
	local out="$state/probe-$1.log"
	shift
	./dev.sh launch --timeout 150 typeccheck "$@" >"$out" 2>&1 &
	started_pid="$!"
	for _ in $(seq 1 60); do
		grep -q "typeccheck: watching" "$out" 2>/dev/null && return 0
		sleep 1
	done
	fail "$case_name: typeccheck $* never began watching (see $kept/$(basename "$out"))"
}

# ITS PASS LINE LANDS IN `judged`: a command substitution would run this in a subshell, which cannot wait for the
# probe this shell started and would judge the log before the probe had finished.
probe_judged() {
	local out="$state/probe-$1.log"
	wait "$started_pid" || true
	cp -f "$out" "$kept/"
	grep -q "typeccheck: PASS" "$out" || fail "$case_name: $2 (see $kept/$(basename "$out"))"
	judged="$(grep -a "typeccheck: PASS" "$out" | tail -1)"
	echo "$judged"
}

# One command to the partner, through the backend's control socket; its answer.
tcpc() {
	python3 - "$fixture/control.sock" "tcpc $*" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(10)
print(s.recv(65536).decode().strip())
EOF
}

# A partner command that must answer ok.
tcpc_ok() {
	local answer
	answer="$(tcpc "$@")"
	[[ "$answer" == ok* ]] || fail "$case_name: the partner refused 'tcpc $*': $answer"
	echo "$answer"
}

# A field of the partner's status line.
status_of() {
	local status
	status="$(tcpc status)"
	sed -n "s/.* $1 \([^ ]*\).*/\1/p" <<<"$status"
}

no_violations() {
	local answer
	answer="$(tcpc violations)"
	[[ "$answer" == "ok none" ]] || fail "$case_name: the partner saw the sink break a rule: $answer (see $kept/tcpc.log)"
}

typec_tool() {
	./dev.sh launch --timeout 60 typec >"$state/typec-$case_name.log" 2>&1 || true
	cp -f "$state/typec-$case_name.log" "$kept/"
}

# The last hard reset from the sink: how many milliseconds after what.
last_hard_reset() {
	local answer
	answer="$(tcpc hard-resets)"
	sed 's/^ok //' <<<"$answer" | tr ';' '\n' | tail -1 | sed 's/^ *//'
}

at_least() {
	local text="$1" bound="$2" what="$3" ms
	ms="$(grep -o "^[0-9.]*" <<<"$text" || true)"
	[[ -n "$ms" ]] || fail "$case_name: $what - no time recorded ($text)"
	python3 -c "import sys; sys.exit(0 if float('$ms') >= $bound else 1)" || fail "$case_name: $what came after $ms ms, before its $bound ms"
	echo "typec-tcpci: $case_name: $what after $ms ms (not before $bound ms)"
}

contracted() {
	probe await 1 contract "$1" >/dev/null
}

attach() {
	tcpc_ok attach "$@" >/dev/null
}

detach() {
	tcpc_ok detach >/dev/null
	probe await 1 detached >/dev/null
}

# ------------------------------------------------------------------ the boot

stretch=1
python3 src/harness/vhost-i2c-gpio.py --tcpc --tcpc-log "$fixture/tcpc.log" --stretch "$stretch" --i2c "$fixture/i2c.sock" --gpio "$fixture/gpio.sock" --control "$fixture/control.sock" --ready "$fixture/ready" >"$fixture/backend.log" 2>&1 &
backend_pid="$!"
for _ in $(seq 1 100); do
	[[ -e "$fixture/ready" ]] && break
	sleep 0.05
done
[[ -e "$fixture/ready" ]] || fail "the vhost-user backend did not start (see $kept/backend.log)"
export I2C_FIXTURE=tcpc I2C_SOCKET="$fixture/i2c.sock" GPIO_SOCKET="$fixture/gpio.sock"
echo "typec-tcpci: booting (state $state, host port $HOSTFWD_PORT, partner stretch $stretch)"
if ! ./dev.sh up --timeout 400 >"$state/up.log" 2>&1; then
	tail -20 "$state/up.log" >&2
	fail "the development instance did not come up (see $kept/up.log)"
fi
await_line "driver.tcpci: " "the port controller's driver never bound" 0
await_line "publishes typec-connector and power-source" "the driver never published its connector" 0 120
await_line "TypeCService: online" "TypeCService never came online" 0
has "the board describes a sink: fixed 5000 mV 3000 mA, fixed 15000 mV 2000 mA; operating at 15000000 uW" "the driver must read the connector from the SSDT's _DSD"
has "VBUS measured with alarms" "the controller measures VBUS"
[[ "$(probe list)" == *"typeccheck: 1 connector(s)"* ]] || fail "boot: TypeCService must hold the controller's one connector"

# ------------------------------------------------------------------ a revision 3.1 charger

case_name="charger31"
alarms_before=$(seen "a VBUS alarm")
attach charger31
contracted 15000
probe source 1 online 15000 >/dev/null
typec_tool
grep -q "a port controller; it can: sink, device, Power Delivery" "$state/typec-$case_name.log" || fail "$case_name: the tool must say the connector is a port controller's"
grep -q "contract: offer 4 (fixed 15.000 V 3.000 A), operating 2000 mA, maximum 2000 mA" "$state/typec-$case_name.log" || fail "$case_name: 15 V must be contracted at the sink PDO's 2 A, not the charger's 3 A"
grep -q "offer 6: " "$state/typec-$case_name.log" || fail "$case_name: the tool must show every offer, the PPS one among them"
requests="$(tcpc requests)"
[[ "$requests" == "ok 4 15000 2000 2000 0" ]] || fail "$case_name: one request, for 15 V at 2 A, and nothing else was asked: $requests"
(($(seen "a VBUS alarm") == alarms_before)) || fail "$case_name: the 5 V to 15 V transition must stay inside the alarm envelope"
echo "typec-tcpci: $case_name: 15 V at 2 A, 9, 12, 20 V and PPS never requested, the transition inside the envelope"

case_name="swaps"
for message in pr-swap dr-swap vconn-swap discover-identity get-sink-cap; do
	tcpc_ok send "$message" >/dev/null
	sleep 1
done
answers="$(tcpc answers)"
for pair in "PR_Swap -> Not_Supported" "DR_Swap -> Not_Supported" "VCONN_Swap -> Not_Supported" "Discover_Identity -> Not_Supported" "Get_Sink_Cap -> Sink_Capabilities"; do
	[[ "$answers" == *"$pair"* ]] || fail "$case_name: expected '$pair' (answers: $answers)"
done
grep -q "sink capabilities 0001912c 0004b0c8" "$fixture/tcpc.log" || fail "$case_name: Get_Sink_Cap must be answered with the board's sink PDOs"
contracted 15000
echo "typec-tcpci: $case_name: every swap and Discover_Identity answered Not_Supported, the board's PDOs given, the contract kept"

case_name="malformed"
tcpc_ok mark >/dev/null
for kind in count reserved extended; do
	tcpc_ok malformed "$kind" >/dev/null
	sleep 1
done
has "a message failed its check (Length) - not believed" "an object count past the bytes must not be believed"
has "a message failed its check (Reserved) - not believed" "a reserved type must not be believed"
has "a message failed its check (Extended) - not believed" "an extended message must not be believed"
[[ "$(status_of marked-hard-resets)" == 0 && "$(status_of marked-soft-resets)" == 0 && "$(status_of contract)" == 15000 ]] || fail "$case_name: nothing may be inferred from a malformed message ($(tcpc status))"
contracted 15000
echo "typec-tcpci: $case_name: three malformed messages not believed, nothing inferred"

case_name="wait"
count=$(grep -c "request position" "$fixture/tcpc.log" || true)
tcpc_ok script wait >/dev/null
tcpc_ok caps >/dev/null
sleep 2
grep -q "answering the request with Wait" "$fixture/tcpc.log" || fail "$case_name: the partner must have answered Wait"
(($(grep -c "request position" "$fixture/tcpc.log") >= count + 2)) || fail "$case_name: the sink must ask again after Wait"
contracted 15000
echo "typec-tcpci: $case_name: Wait, then the request again and Accept"

case_name="reject"
tcpc_ok script reject >/dev/null
tcpc_ok caps >/dev/null
sleep 2
grep -q "answering the request with Reject" "$fixture/tcpc.log" || fail "$case_name: the partner must have answered Reject"
contracted 15000
echo "typec-tcpci: $case_name: Rejected, the contract kept"

case_name="less"
tcpc_ok caps less >/dev/null
contracted 5000
tcpc_ok caps >/dev/null
contracted 15000
(($(seen "a VBUS alarm") == alarms_before)) || fail "$case_name: 15 V to 5 V and back must stay inside the alarm envelope"
echo "typec-tcpci: $case_name: less power, a new request at 5 V; more again, 15 V - no alarm"

case_name="vbus-alarm"
alarms_before=$(seen "a VBUS alarm (high)")
probe_started fault source 1 fault
tcpc_ok vbus 17500 >/dev/null
probe_judged fault "the alarm must report source-fault"
await_line "a VBUS alarm (high)" "VBUS past the window must raise the alarm" "$alarms_before" 20
contracted 15000
no_violations
echo "typec-tcpci: $case_name: VBUS at 17.5 V in the 15 V contract - the sink path off before the Hard Reset, source-fault, the contract made anew"

case_name="overshoot"
alarms_before=$(seen "a VBUS alarm (high) at 18000 mV")
tcpc_ok caps less >/dev/null
contracted 5000
resets_before=$(status_of hard-resets-from-sink)
probe_started overshoot source 1 fault
tcpc_ok script overshoot >/dev/null
tcpc_ok caps >/dev/null
probe_judged overshoot "the overshoot must report source-fault"
await_line "a VBUS alarm (high) at 18000 mV" "the overshoot must raise the alarm" "$alarms_before" 20
(($(status_of hard-resets-from-sink) == resets_before + 1)) || fail "$case_name: the alarm must be answered with one Hard Reset ($(tcpc status))"
contracted 15000
no_violations
echo "typec-tcpci: $case_name: 5 V to 15 V overshooting to 18 V - the sink path off before the Hard Reset, source-fault"

case_name="partner-hard-reset"
back_before=$(seen "VBUS is back after the hard reset")
detached_before=$(seen ": detached")
probe_started steady steady 1 5
tcpc_ok hard-reset >/dev/null
probe_judged steady "the partner must stay attached through a Hard Reset that cycles VBUS"
await_line "VBUS is back after the hard reset" "VBUS must come back" "$back_before" 20
(($(seen ": detached") == detached_before)) || fail "$case_name: a Hard Reset that cycles VBUS is no detach"
contracted 15000
no_violations
echo "typec-tcpci: $case_name: VBUS to vSafe0V and back, no detach, the contract made anew"

case_name="no-answer"
sent_before=$(seen "hard reset sent (1 of 2)")
kept_before=$(seen "VBUS is back after the hard reset - the partner stayed attached, hard resets so far: 1")
tcpc_ok script no-answer >/dev/null
tcpc_ok caps >/dev/null
await_line "hard reset sent (1 of 2)" "an unanswered Request must end in a Hard Reset" "$sent_before" 20
at_least "$(last_hard_reset)" 24 "the Hard Reset for an unanswered request"
await_line "VBUS is back after the hard reset - the partner stayed attached, hard resets so far: 1" "the hard-reset count must be kept through the reset" "$kept_before" 20
contracted 15000
echo "typec-tcpci: $case_name: the count kept through the reset, the contract made anew"

case_name="no-ps-rdy"
tcpc_ok script no-ps-rdy >/dev/null
tcpc_ok caps >/dev/null
sleep 3
[[ "$(last_hard_reset)" == *"after Accept reached it"* ]] || fail "$case_name: Accept without PS_RDY must end in a Hard Reset ($(last_hard_reset))"
at_least "$(last_hard_reset)" 450 "the Hard Reset for a missing PS_RDY"
contracted 15000

case_name="vbus-stays-off"
tcpc_ok script vbus-stays-off >/dev/null
probe_started stays-off await 1 detached
tcpc_ok hard-reset >/dev/null
probe_judged stays-off "a Hard Reset after which VBUS stays off must end in a detach"
after="$(grep -o "after [0-9]* ms" <<<"$judged" | grep -o "[0-9]*")"
((after >= 1275)) || fail "$case_name: detached after $after ms, before tSrcRecover and tSrcTurnOn (1275 ms) had passed"
echo "typec-tcpci: $case_name: detached $after ms after the Hard Reset"
tcpc_ok detach >/dev/null

case_name="detach-mid"
attach charger31
contracted 15000
tcpc_ok script detach-mid >/dev/null
tcpc_ok caps >/dev/null
probe await 1 detached >/dev/null
probe source 1 offline >/dev/null
no_violations
echo "typec-tcpci: $case_name: detached in the middle of a negotiation, the supply offline"

# ------------------------------------------------------------------ the other partners

case_name="charger20"
attach charger20
contracted 15000
has "the source speaks Power Delivery 2.0" "a revision 2.0 source must be spoken to in 2.0"
tcpc_ok send pr-swap >/dev/null
sleep 1
[[ "$(tcpc answers)" == *"PR_Swap -> Reject"* ]] || fail "$case_name: a swap from a 2.0 partner must be answered Reject"
detach
echo "typec-tcpci: $case_name: 15 V from a revision 2.0 charger, its swap answered Reject"

case_name="weak"
attach weak
contracted 5000
typec_tool
grep -q "contract: offer 1 (fixed 5.000 V 2.000 A), operating 2000 mA, maximum 2000 mA, capability mismatch" "$state/typec-$case_name.log" || fail "$case_name: 5 V with capability mismatch"
detach
echo "typec-tcpci: $case_name: below the operating power - 5 V, capability mismatch"

for profile in typec15 typec30; do
	case_name="$profile"
	level=medium
	[[ "$profile" == typec30 ]] && level=high
	sent_before=$(seen "hard reset sent (2 of 2)")
	attach "$profile"
	probe await 1 current "$level" >/dev/null
	await_line "hard reset sent (2 of 2)" "a source with no Power Delivery must see the hard-reset count run out" "$sent_before" 30
	sleep 3
	probe await 1 current "$level" >/dev/null
	typec_tool
	grep -q "power by Type-C current, $([[ $level == medium ]] && echo 1.5 || echo 3) A" "$state/typec-$case_name.log" || fail "$case_name: the Type-C current must stand"
	detach
	echo "typec-tcpci: $case_name: the Type-C current stands after the hard-reset count"
done

case_name="no-caps"
sent_before=$(seen "hard reset sent (2 of 2)")
tcpc_ok script no-caps >/dev/null
attach charger31
await_line "hard reset sent (2 of 2)" "no Source_Capabilities must run the hard-reset count out" "$sent_before" 30
sleep 3
resets="$(tcpc hard-resets)"
checked=0
while IFS= read -r entry; do
	entry="${entry# }"
	[[ "$entry" == *"after it began receiving"* ]] || fail "$case_name: each Hard Reset must answer missing capabilities ($entry)"
	at_least "$entry" 310 "a Hard Reset for missing Source_Capabilities"
	checked=$((checked + 1))
done < <(sed 's/^ok //' <<<"$resets" | tr ';' '\n' | tail -2)
((checked == 2)) || fail "$case_name: two Hard Resets expected, $checked read"
probe await 1 current high >/dev/null
detach

# ------------------------------------------------------------------ the controller's controller cycled

case_name="cycle"
attach charger31
contracted 15000
alarms_before=$(seen "a VBUS alarm")
bound_before=$(seen "bound with VBUS present and the sink path on - no contract is trusted; it is negotiated anew through Soft_Reset")
tcpc_ok mark >/dev/null
probe cycle >/dev/null
await_line "bound with VBUS present and the sink path on - no contract is trusted; it is negotiated anew through Soft_Reset" "the rebind must find VBUS present and trust no contract" "$bound_before" 30
contracted 15000
[[ "$(status_of marked-hard-resets)" == 0 ]] || fail "$case_name: no Hard Reset may cycle VBUS ($(tcpc status))"
[[ "$(status_of marked-path-offs)" == 0 ]] || fail "$case_name: the sink path must be left as the rebind found it ($(tcpc status))"
(($(status_of marked-soft-resets) >= 1)) || fail "$case_name: the contract must be made anew through Soft_Reset ($(tcpc status))"
(($(status_of vbus-low) >= 15000)) || fail "$case_name: VBUS dropped to $(status_of vbus-low) mV"
(($(seen "a VBUS alarm") == alarms_before)) || fail "$case_name: the rebind must raise no alarm"
no_violations
echo "typec-tcpci: $case_name: stopped as a lost dependency, bound again with no alarm, the contract anew through Soft_Reset - VBUS never below 15 V"

# ------------------------------------------------------------------ the timing run

case_name="timing"
tcpc_ok negotiate 200 >/dev/null
timing=""
for _ in $(seq 1 120); do
	timing="$(tcpc timing)"
	[[ "$timing" == "ok done"* ]] && break
	sleep 1
done
[[ "$timing" == "ok done responses 200 "* ]] || fail "$case_name: the 200 negotiations did not finish ($timing)"
tcpc responses >"$kept/responses.txt"
echo "typec-tcpci: $case_name: ${timing#ok done }"
p99="$(sed -n 's/.* p99 \([0-9.]*\) ms.*/\1/p' <<<"$timing")"
max="$(sed -n 's/.* max \([0-9.]*\) ms.*/\1/p' <<<"$timing")"
timeouts="$(sed -n 's/.* timeouts \([0-9]*\).*/\1/p' <<<"$timing")"
if [[ -e /dev/kvm && "${NOKVM:-0}" != "1" ]]; then
	python3 -c "import sys; sys.exit(0 if float('$p99') <= 15.0 and float('$max') < 24.0 and int('$timeouts') == 0 else 1)" || fail "$case_name: the response budget is missed under KVM - p99 $p99 ms (at most 15), max $max ms (never 24), $timeouts timeout(s)"
	echo "typec-tcpci: $case_name: within the budget under KVM"
else
	echo "typec-tcpci: $case_name: recorded, not gated - this guest does not run under KVM"
fi
contracted 15000
no_violations
tcpc_ok detach >/dev/null
echo "typec-tcpci: PASS - a port controller's Power Delivery sink: 15 V at the board's current and nothing it does not describe, swaps and identity not supported, malformed messages not believed, Wait, Reject and less power renegotiated, both alarms answered with the sink path off first, every Hard Reset and its timer not before its bound, 2.0, below-operating, non-PD and silent sources, a lost controller renegotiated through Soft_Reset without VBUS dropping, and the response inside its budget"
