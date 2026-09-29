#!/bin/bash
# THE BMC'S ADMINISTRATIVE ACTIONS, cold, in development images: the `bmc` tool asks, AdminService shows the frozen
# operation on the protected screen, and one key through QEMU's keyboard decides - each case a boot with ONE BMC.
#
#   the log's erasure        declined, the log keeps its records; approved, nothing written before the clear is left
#                            (a simulated BMC on ISA KCS, `ipmi-sel-clear.toml`)
#   an erasure cancelled     the harness BMC adds a record once the protected screen is ready for a decision, which
#                            cancels the preparation's reservation: the approved clear fails and erases nothing, the new
#                            record included (`ipmi-sel-clear-cancelled.toml`)
#   hard reset               a second boot on the serial log - the run has no `-no-reboot`
#   power down               QEMU's run state `shutdown` under `-no-shutdown`, and no power-button line
#   soft shutdown            the same run state, DeviceManager's power-button line before it
#   power cycle              QEMU's simulator refuses it with 0xD5: the tool reports the refusal, the administrative
#                            outcome is `failed`, the machine keeps running; its BMC has no GUID, so the request names
#                            the fallback identity
#
# THE ORACLES ARE DURABLE, since the harness holds no standing QMP listener: a boot count on the serial log, and the
# run state `query-status` reads. `lab.sh scenario-cold` builds the development image and drives each scenario through
# the emulated keyboard; this script then reads what a scenario cannot - AdminService's journal lines, the driver's, the
# serial log's boots and what the harness BMC received, in order.
#
# WHAT IT DOES NOT CLAIM: physical approval on another architecture, or a BMC's own power cycle, which QEMU's does not
# implement.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
fail() {
	echo "qemu-ipmi-admin: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "this gate is the x86_64 one: QEMU puts ISA IPMI on q35"
	;;
*) fail "unexpected argument '$1'" ;;
esac

log="$repo/.build/boot/cold-x86_64.log"
work="$(mktemp -d "${TMPDIR:-/tmp}/liber-ipmi-admin.XXXXXX")"
kept="$repo/.build/logs/ipmi-admin"
rm -rf "$kept"
mkdir -p "$kept"
harness=""
helper=""
cleanup() {
	for pid in "$helper" "$harness"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	cp -f "$work"/*.log "$kept/" 2>/dev/null || true
	[[ "$work" == */liber-ipmi-admin.* ]] && rm -rf -- "$work"
}
trap cleanup EXIT

python3 "$root/harness/ipmi-harness-bmc.py" --sdr-out "$work/sdr.bin" --fru-out "$work/fru.bin"

sim() {
	local product="$1" guid_arg=""
	[[ -n "${2:-}" ]] && guid_arg=",guid=$2"
	printf -- '-device ipmi-bmc-sim,id=bmc0,product_id=%s%s,sdrfile=%s,frudatafile=%s -device isa-ipmi-kcs,bmc=bmc0,irq=0' "$product" "$guid_arg" "$work/sdr.bin" "$work/fru.bin"
}

# ONE SCENARIO, cold, its serial log kept under its name.
run() {
	local name="$1"
	export QEMU_EXTRA="$2"
	rm -f "$log"
	echo "qemu-ipmi-admin: $name"
	if ! "$repo/lab.sh" scenario-cold x86_64 "$root/harness/scenarios/$name.toml"; then
		cp -f "$log" "$kept/$name.log" 2>/dev/null || true
		fail "the scenario $name failed (serial log: $kept/$name.log)"
	fi
	cp -f "$log" "$kept/$name.log" || fail "the scenario $name left no serial log"
}

has() {
	grep -aqF -- "$2" "$kept/$1.log" || fail "$1: expected \"$2\" - $3"
}

lacks() {
	if grep -aqF -- "$2" "$kept/$1.log"; then
		fail "$1: found \"$2\" - $3"
	fi
}

lines() {
	local found
	found="$(grep -a -c -F -- "$2" "$kept/$1.log" || true)"
	echo "${found:-0}"
}

# AdminService's journal lines for this run's requests, in order: `request N <event>`.
journal() {
	grep -a -o -E "AdminService: request [0-9]+ (requested|granted|declined|consumed|completed|failed|outcome unknown)" "$kept/$1.log" | awk '{print $4}' | tr '\n' ' '
}

# ------------------------------------------------------------------ the log's erasure, declined and approved

run ipmi-sel-clear "$(sim 0x51 11111111-2222-3333-4444-0000000000a1)"
has ipmi-sel-clear "completed: the log is erased" "the approved clear must erase the log under the preparation's reservation"
has ipmi-sel-clear "AdminService: the protected screen is presented" "each request must reach the protected screen"
events="$(journal ipmi-sel-clear)"
[[ "$events" == *"declined"*"completed"* ]] || fail "ipmi-sel-clear: the journal must record the decline and then the completion (read: $events)"
echo "qemu-ipmi-admin: declined, the log kept its records; approved, it was erased - the journal says both ($events)"

# ------------------------------------------------------------------ an erasure cancelled by an event

control() {
	python3 - "$work/control.sock" "$1" <<'EOF'
import socket
import sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall((sys.argv[2] + '\n').encode())
s.settimeout(5)
print(s.recv(4096).decode().strip())
EOF
}

# THE EVENT, added by the harness BMC once the protected screen is ready for a decision - the preparation is done.
add_when_ready() {
	local polls=0
	until grep -aqF "AdminService: the session is ready for a decision" "$log" 2>/dev/null; do
		sleep 0.2
		polls=$((polls + 1))
		((polls < 6000)) || return 0
	done
	control "sel-add" >>"$work/host.log"
}

rm -f "$work/ready"
python3 "$root/harness/ipmi-harness-bmc.py" --serve "$work/bmc.sock" --control "$work/control.sock" --record "$work/record-cancelled.log" --ready "$work/ready" --guid 111111112222333344440000000000a2 >"$work/harness.log" 2>&1 &
harness=$!
for _ in $(seq 1 100); do
	[[ -e "$work/ready" ]] && break
	sleep 0.05
done
[[ -e "$work/ready" ]] || fail "the harness BMC did not start"
rm -f "$log"
add_when_ready &
helper=$!
run ipmi-sel-clear-cancelled "-chardev socket,id=extbmc,path=$work/bmc.sock,reconnect-ms=1000 -device ipmi-bmc-extern,id=bmcx,chardev=extbmc -device isa-ipmi-kcs,bmc=bmcx,irq=0"
wait "$helper" 2>/dev/null || true
helper=""
kill "$harness" 2>/dev/null || true
wait "$harness" 2>/dev/null || true
harness=""
has ipmi-sel-clear-cancelled "failed: the BMC refused it, and nothing was erased" "the clear must fail on the cancelled reservation"
events="$(journal ipmi-sel-clear-cancelled)"
[[ "$events" == *"failed"* ]] || fail "ipmi-sel-clear-cancelled: the journal must record the outcome failed (read: $events)"
# WHAT THE BMC RECEIVED, in order: the preparation's Reserve SEL, the event added, then the clear refused with 0xC5.
python3 - "$work/record-cancelled.log" <<'EOF' || fail "ipmi-sel-clear-cancelled: the harness BMC did not receive the reservation, the event and the refused clear in that order"
import sys
lines = open(sys.argv[1]).read().splitlines()
reserve = max(at for at, line in enumerate(lines) if line.startswith('0x0a 0x42 '))
added = next(at for at, line in enumerate(lines) if line == 'control sel-add')
clear = next(at for at, line in enumerate(lines) if line.startswith('0x0a 0x47 '))
if not (reserve < added < clear and lines[clear].endswith('-> 0xc5')):
    raise SystemExit(f'reserve {reserve}, added {added}, clear {clear}: {lines[clear]}')
print(f'qemu-ipmi-admin: the BMC received the reservation (line {reserve + 1}), the event (line {added + 1}) and the clear refused with 0xc5 (line {clear + 1})')
EOF
echo "qemu-ipmi-admin: an event between preparation and approval cancelled the erasure - failed, and every record kept"

# ------------------------------------------------------------------ the chassis

run ipmi-hard-reset "$(sim 0x52 11111111-2222-3333-4444-0000000000a3)"
[[ "$(lines ipmi-hard-reset 'shell attached - ')" == 2 ]] || fail "ipmi-hard-reset: the hard reset must be followed by exactly one second boot"
echo "qemu-ipmi-admin: a hard reset through the BMC booted the machine again"

run ipmi-power-down "$(sim 0x53 11111111-2222-3333-4444-0000000000a4) -no-shutdown"
lacks ipmi-power-down "DeviceManager: the power button was pressed" "a power down is the BMC cutting the power, not a button press"
echo "qemu-ipmi-admin: a power down through the BMC stopped the machine, with no button pressed"

run ipmi-soft-shutdown "$(sim 0x54 11111111-2222-3333-4444-0000000000a5) -no-shutdown"
python3 - "$kept/ipmi-soft-shutdown.log" <<'EOF' || fail "ipmi-soft-shutdown: DeviceManager's power-button line must follow the chassis request"
import sys
text = open(sys.argv[1], 'rb').read()
sent = text.find(b'the chassis SoftShutdown is sent to the BMC')
pressed = text.find(b'DeviceManager: the power button was pressed')
if not 0 <= sent < pressed:
    raise SystemExit(f'sent at {sent}, pressed at {pressed}')
EOF
echo "qemu-ipmi-admin: a soft shutdown through the BMC pressed the power button, and the machine powered off"

run ipmi-power-cycle "$(sim 0x55)"
has ipmi-power-cycle "bmccheck: the boot's event is in the log of bmc:00000-0055-20@acpi:" "the BMC without a GUID must be named by its fallback identity"
grep -aqE "bmc: bmc:00000-0055-20@acpi:[^ ]+ - confirm it on the protected screen" "$kept/ipmi-power-cycle.log" || fail "ipmi-power-cycle: the request must name the fallback identity"
has ipmi-power-cycle "the BMC refused the chassis PowerCycle with 0xd5" "QEMU's BMC refuses a power cycle with 0xD5"
events="$(journal ipmi-power-cycle)"
[[ "$events" == *"failed"* ]] || fail "ipmi-power-cycle: the journal must record the outcome failed (read: $events)"
echo "qemu-ipmi-admin: a power cycle the BMC refused was reported as its refusal, failed, and the machine kept running"
echo "qemu-ipmi-admin: PASS - the BMC's log erased only on a confirmation and never across an event added after its preparation; a hard reset, a power down, a soft shutdown and a refused power cycle, each confirmed on the protected screen, each with its durable oracle"
