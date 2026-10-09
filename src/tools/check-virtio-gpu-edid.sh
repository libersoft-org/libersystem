#!/usr/bin/env bash
# The shipping virtio-gpu reads QEMU's EDID and DisplayService adopts it. The second cold boot
# removes the EDID feature: discovery stays unavailable and acknowledged presentation still works.
set -euo pipefail
GUEST_GATE_NAME="virtio-gpu-edid"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

python3 src/harness/edid-oracle.py --self-test
if [[ "${1:-}" == --self-test && $# == 1 ]]; then
	exit 0
fi
guest_gate_arch "$@"
guest_gate_require_programs virtio_gpu display_service

kept="$root/../.build/logs/virtio-gpu-edid-$GUEST_ARCH"
finish() {
	mkdir -p "$kept"
	cp -f "$guest_gate_work"/guest "$guest_gate_work"/run "$kept/" 2>/dev/null || true
	guest_gate_cleanup
}
trap finish EXIT
mkdir -p "$kept"
# Development mode supplies the discoverable PCI GPU on both virt-machine ports. Private dev state
# prevents its control socket from aliasing a separately driven guest.
export DEV_PROFILE=1 LIBER_DEV_STATE="$guest_gate_work/dev" NET_NONE=1 GPU_SIZE=1600x900
mkdir -p "$LIBER_DEV_STATE"
for edid in on off; do
	export QEMU_EXTRA="-global virtio-gpu-device.edid=$edid -global virtio-gpu-device.xres=1600 -global virtio-gpu-device.yres=900"
	guest_gate_run $'lsdev\npoweroff' ''
	cp "$GUEST_LOG" "$kept/$edid.log"
	python3 src/harness/edid-oracle.py --log "$GUEST_LOG" --edid "$edid" || guest_gate_fail "EDID $edid failed on $GUEST_ARCH (see $kept)"
done
echo "virtio-gpu-edid: PASS on $GUEST_ARCH - bound EDID identity, physical size and modes reached DisplayService; disabling EDID kept metadata absent and presentation working"
