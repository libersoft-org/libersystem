#!/usr/bin/env bash
# P02M0196's dedicated tree fixture on the two device-tree ports. A fixture-only node names the GPIO
# controller's virtio child through interrupts-extended; its driver verifies the scoped connection and
# absence of a wired interrupt, then acknowledges two edges raised by the host backend.
set -euo pipefail
GUEST_GATE_NAME="firmware-tree"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

ready="tree-fixture: dt:/liber-fixture bound through GPIO line 2 (rising), no wired interrupt; ready"
first="tree-fixture: event 1: GPIO line 2 high, acknowledged"
second="tree-fixture: event 2: GPIO line 2 high, acknowledged"

oracle() {
	local log="$1" companion="$2" line
	for line in "$ready" "$first" "$second" "$companion"; do
		grep -a -q -F -- "$line" "$log" || return 1
	done
	! grep -a -q -F 'tree-fixture: refused:' "$log"
}

# The exact reducer below must reject each missing observation, and a driver refusal even when all
# success markers are present. These inputs are private files, never edits to the tree or a guest log.
self_test() {
	local work line omitted
	work="$(mktemp -d)"
	local -a required=("$ready" "$first" "$second" "fixture companion joined")
	printf '%s\n' "${required[@]}" >"$work/log"
	oracle "$work/log" "fixture companion joined" || {
		rm -rf "$work"
		return 1
	}
	for omitted in "${required[@]}"; do
		: >"$work/log"
		for line in "${required[@]}"; do
			[[ "$line" == "$omitted" ]] || printf '%s\n' "$line" >>"$work/log"
		done
		if grep -q -F -- "$omitted" "$work/log" || oracle "$work/log" "fixture companion joined"; then
			rm -rf "$work"
			return 1
		fi
	done
	printf '%s\n' "${required[@]}" 'tree-fixture: refused: invalid scope' >"$work/log"
	if oracle "$work/log" "fixture companion joined"; then
		rm -rf "$work"
		return 1
	fi
	rm -rf "$work"
}
self_test || guest_gate_fail "the log oracle accepted an incomplete/refused fixture"
if [[ "${1:-}" == --self-test && $# == 1 ]]; then
	echo "firmware-tree: the oracle accepts complete evidence and refuses every missing observation and a driver refusal"
	exit 0
fi

guest_gate_arch "$@"
case "$GUEST_ARCH" in
aarch64) companion='dt:/pcie@10000000/gpio@16,0 is the companion of 0000:00:16.0' ;;
riscv64) companion='dt:/soc/pci@30000000/gpio@16,0 is the companion of 0000:00:16.0' ;;
*) guest_gate_fail "this fixture requires aarch64 or riscv64" ;;
esac
guest_gate_require_programs acpi_fixture virtio_gpio

backend=""
helper=""
kept="$root/../.build/logs/firmware-tree-$GUEST_ARCH"
finish() {
	local pid
	for pid in "$helper" "$backend"; do
		if [[ -n "$pid" ]]; then
			kill "$pid" 2>/dev/null || true
			wait "$pid" 2>/dev/null || true
		fi
	done
	mkdir -p "$kept"
	cp -f "$guest_gate_work"/guest "$guest_gate_work"/run "$guest_gate_work"/backend.log "$guest_gate_work"/host.log "$kept/" 2>/dev/null || true
	guest_gate_cleanup
}
trap finish EXIT
python3 src/harness/vhost-i2c-gpio.py --i2c "$guest_gate_work/i2c.sock" --gpio "$guest_gate_work/gpio.sock" --control "$guest_gate_work/control.sock" --ready "$guest_gate_work/ready" >"$guest_gate_work/backend.log" 2>&1 &
backend=$!
for _ in $(seq 1 100); do
	[[ -e "$guest_gate_work/ready" ]] && break
	sleep 0.05
done
[[ -e "$guest_gate_work/ready" ]] || guest_gate_fail "the GPIO backend did not start"
export I2C_FIXTURE=tree I2C_SOCKET="$guest_gate_work/i2c.sock" GPIO_SOCKET="$guest_gate_work/gpio.sock" NET_NONE=1
export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-900}" GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-1200}"

# Raise each edge only after the preceding acknowledgement is visible. A line whose acknowledge or
# controller join is missing cannot produce the second observation.
python3 - "$guest_gate_work" "$ready" "$first" "$second" "$GUEST_GATE_SECONDS" >"$guest_gate_work/host.log" 2>&1 <<'PY' &
import pathlib
import socket
import sys
import time

work = pathlib.Path(sys.argv[1])
deadline = time.monotonic() + float(sys.argv[5])

def wait_for(marker):
	while time.monotonic() < deadline:
		if (work / 'guest').exists() and marker.encode() in (work / 'guest').read_bytes():
			return
		time.sleep(0.1)
	raise SystemExit('the guest never said: ' + marker)

def command(value):
	with socket.socket(socket.AF_UNIX) as connection:
		connection.settimeout(5)
		connection.connect(str(work / 'control.sock'))
		connection.sendall((value + '\n').encode())
		answer = connection.recv(4096).decode().strip()
		print(value + ': ' + answer, flush=True)
		if not answer.startswith('ok'):
			raise SystemExit('backend refused the edge')

wait_for(sys.argv[2])
command('lower 2')
command('raise 2')
wait_for(sys.argv[3])
command('lower 2')
command('raise 2')
wait_for(sys.argv[4])
PY
helper=$!
guest_gate_run 'lsdev' ''
wait "$helper" || guest_gate_fail "the host could not deliver both edges"
helper=""
oracle "$GUEST_LOG" "$companion" || guest_gate_fail "the companion, scoped bind and two acknowledged GPIO edges were not all observed (see $kept)"
echo "firmware-tree: PASS on $GUEST_ARCH - the dedicated tree node bound through its PCI companion's GPIO line, with no wired interrupt, and acknowledged both host edges"
