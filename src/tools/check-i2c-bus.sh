#!/usr/bin/env bash
# The I2C and GPIO bus contracts, ON THE TRANSLATED MACHINE.
#
# The kernel suite's bus oracles - the virtio-i2c controller serving one address per connection and every
# SMBus transaction it declares, and the virtio-gpio controller delivering each line's event once and holding
# the line until it is acknowledged - booted with the x86_64 machine's virtio-iommu (`IOMMU=1`, the variable a
# gate that owns its profile sets). The two controllers' device side is `vhost-i2c-gpio.py` behind QEMU's
# vhost-user devices, attached with `iommu_platform=on`, so every ring and buffer address the drivers hand
# over crosses the backend's vhost-user IOTLB: its updates, its misses and its invalidations. That is the
# machine the gates riding this GPIO line boot, and the reason this row exists beside the suite's own run.
#
#   the bus                  one address per connection, a second refused and served again after the first
#                              leaves, plain I2C through `hid-i2c`'s bus, every declared SMBus transaction
#                              with the PEC right and wrong, the block read with the device's count refused,
#                              the bound held, an empty address failing, a killed controller's connections
#                              closed
#   the lines                names, a level connection, every trigger delivered once and masked until
#                              acknowledged, a level line delivered again, one connection per line, a killed
#                              controller's connections closed

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
source "$HERE/result-logs.sh"

fail() {
	echo "i2c-bus: $*" >&2
	exit 1
}

[[ "${1:-}" == "" || "${1:-}" == "--arch" && "${2:-x86_64}" == "x86_64" ]] || fail "this gate is the x86_64 one; the ports run the same oracles in their emulated sweep"
command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"
# THE PROFILE NEEDS THESE DEVICES TO EXIST IN THIS QEMU, named rather than assumed: a QEMU without them would
# boot a machine on which both oracles say NOT RUN.
devices="$(qemu-system-x86_64 -device help 2>/dev/null || true)"
for device in vhost-user-i2c-pci vhost-user-gpio-pci virtio-iommu-pci; do
	case "$devices" in
	*"name \"$device\""*) ;;
	*) fail "this QEMU has no $device, and the bus is not testable without it" ;;
	esac
done

work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log; rm -rf "$work"' EXIT

tests=(
	kernel.hardware.virtio_i2c_serves_one_address_per_connection_and_every_transaction_it_declares
	kernel.hardware.virtio_gpio_delivers_each_event_once_and_holds_the_line_until_it_is_acknowledged
)
selection="$(
	IFS=,
	echo "${tests[*]}"
)"

echo "i2c-bus: booting the test kernel on the translated machine with the I2C and GPIO controllers attached"
IOMMU=1 I2C_FIXTURE=bus TEST_SELECTION="$selection" ./test.sh --arch x86_64 >"$work/run.log" 2>&1 || {
	echo "i2c-bus: the bus oracles failed" >&2
	grep -aE "virtio-i2c|virtio-gpio|vhost-i2c-gpio" "$work/run.log" | tail -40 >&2 || true
	tail -20 "$work/run.log" >&2
	exit 1
}
mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"

# THE MACHINE WAS THE TRANSLATED ONE, by the runner's word and the kernel's, and not a machine that merely asked.
grep -aqh "qemu-run: run mode gate, DMA mode enforcing-required (harness provenance) via fw_cfg" "${logs[@]}" || fail "the run did not boot the translated machine under the gate row"
grep -aqh "dma: boot DMA mode enforcing-required" "${logs[@]}" || fail "the kernel did not adopt enforcing-required"
# EACH ORACLE'S OWN WORD, and not only the suite's exit: an oracle that found no fixture says NOT RUN and passes.
if grep -aqhE "virtio-(i2c|gpio): NOT RUN" "${logs[@]}"; then
	fail "an oracle found no I2C fixture on a run that attached one"
fi
grep -aqh "virtio-i2c: one address per connection" "${logs[@]}" || fail "the I2C oracle did not report its claims"
grep -aqh "virtio-gpio: line names" "${logs[@]}" || fail "the GPIO oracle did not report its claims"
for test in "${tests[@]}"; do
	grep -aqh "$test" "${logs[@]}" || fail "$test did not run"
done
if grep -aqh "\[failed\]" "${logs[@]}"; then
	fail "a selected test failed"
fi

echo "i2c-bus: PASS - one address and one line per scoped connection, every transaction the virtio-i2c controller declares, each GPIO event delivered once and held until acknowledged, and a killed controller's connections closed, through the vhost-user IOTLB on the translated machine"
