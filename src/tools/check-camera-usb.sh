#!/usr/bin/env bash
# USB Video through the real CameraService and an owner-bound client, on each supported QEMU architecture.
set -euo pipefail
GUEST_GATE_NAME="camera-usb"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"
guest_gate_arch "$@"
guest_gate_require_programs uvccheck camera_service xhci
kept="$root/../.build/logs/camera-usb-$GUEST_ARCH"
mkdir -p "$kept"
backend=""
finish() {
	if [[ -n "$backend" ]]; then
		kill "$backend" 2>/dev/null || true
		wait "$backend" 2>/dev/null || true
	fi
	cp -f "$guest_gate_work"/camera.log "$guest_gate_work"/guest "$guest_gate_work"/run "$kept/" 2>/dev/null || true
	guest_gate_cleanup
}
trap finish EXIT
export USB_REDIR_SOCKET="$guest_gate_work/camera.sock" NET_NONE=1
python3 src/harness/usbredir_device.py --emulate uvc-iso --uvc-unplug-at 3 --socket "$USB_REDIR_SOCKET" --ready "$guest_gate_work/ready" >"$guest_gate_work/camera.log" 2>&1 &
backend=$!
for _ in $(seq 1 100); do
	[[ ! -e "$guest_gate_work/ready" ]] || break
	kill -0 "$backend" 2>/dev/null || guest_gate_fail "camera model exited during startup"
	sleep 0.05
done
[[ -e "$guest_gate_work/ready" ]] || guest_gate_fail "camera model failed to listen"
if [[ "$GUEST_ARCH" == x86_64 ]]; then
	export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-300}" GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-480}"
else
	export GUEST_GATE_SECONDS="${GUEST_GATE_SECONDS:-1200}" GUEST_GATE_TIMEOUT="${GUEST_GATE_TIMEOUT:-1600}"
fi
guest_gate_run $'graph\nuvccheck capture\ngraph\nuvccheck cycle\nuvccheck unplug\ngraph\npoweroff' ''
if grep -aq 'uvccheck: FAIL' "$GUEST_LINES"; then
	grep -a 'uvccheck: FAIL' "$GUEST_LINES" >&2
	guest_gate_fail "a USB camera client assertion failed"
fi
for required in 'uvccheck: PASS inventory:' 'uvccheck: PASS capture:' 'uvccheck: PASS cycle:' 'uvccheck: PASS unplug:'; do
	grep -aqF "$required" "$GUEST_LINES" || guest_gate_fail "missing client evidence: $required"
done
[[ "$(grep -ac 'committed stream ' "$guest_gate_work/camera.log")" == 3 ]] || guest_gate_fail "expected exactly three USB stream commits: capture, cycle, replacement capture"
for required in 'committed stream 1' 'committed stream 2' 'committed stream 3' 'left the bus for good'; do
	grep -aqF "$required" "$guest_gate_work/camera.log" || guest_gate_fail "missing far-end evidence: $required"
done
# Preserve graph observations beside the client's actual transfer/latency/recovery figures.
grep -aE 'uvccheck: PASS|name=(xhci|camera_service)' "$GUEST_LINES" >"$kept/measurements.log"
cat "$kept/measurements.log"
echo "camera-usb: PASS on $GUEST_ARCH - exact USB frames through CameraService, stable leases, controller replacement and unplug revoke the old grants"
