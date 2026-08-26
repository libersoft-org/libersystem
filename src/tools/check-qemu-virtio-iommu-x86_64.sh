#!/usr/bin/env bash
# The boundary, proved by a hostile device that is told to cross it.
#
# WHAT THIS GATE IS. Two boots. The first runs the in-kernel conformance fixture with a
# `virtio-iommu` controller and two PCI `edu` functions attached - devices whose DMA engine copies
# between an arbitrary physical address and their own buffer on command, which is as close to a
# malicious driver as this tree can get. Each case writes a sentinel pattern into a frame, asks the
# device to reach it, and reads the pattern back: what is checked is the MEMORY, not the error code.
# The second boots the shipping image with an ordinary `virtio-net` endpoint behind the same
# controller, because a gate that proved only the refusals would pass a kernel whose DMA was broken
# altogether.
#
# THROUGH THE TEST HARNESS RATHER THAN A HAND-BUILT QEMU LINE. `QEMU_EXTRA` is documented for exactly
# this, and it means the profile inherits the harness's timeouts, its log collection and its
# staleness checks instead of a second copy of all three that can drift from them.
#
# THE CONTROLLER STARTS IN BOOT BYPASS. On the UEFI path OVMF needs untranslated DMA to read the boot
# medium, so the transition out of bypass is the KERNEL's - which is why it is a thing this gate
# observes rather than a flag it sets.
#
# A GATE THAT PROVES ONLY THAT THE DEVICE ENUMERATED IS NOT ISOLATION EVIDENCE, and this one refuses
# to pass on enumeration: it requires the named cases to have RUN, by name, and fails if any of them
# reports itself skipped.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
BUILD=".build/boot"
OVMF_CODE="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
OVMF_VARS="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"

fail() {
	echo "qemu-virtio-iommu: $*" >&2
	exit 1
}

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"

# THE PROFILE NEEDS THESE DEVICES TO EXIST IN THIS QEMU. Named rather than assumed, so a QEMU without
# them says so instead of producing a boot in which every case skips and the gate passes.
# The listing goes into a variable: `grep -q` stops at its first match, which under `pipefail` makes
# a successful search read as a failed pipeline.
devices="$(qemu-system-x86_64 -device help 2>/dev/null || true)"
for device in virtio-iommu-pci edu; do
	case "$devices" in
	*"name \"$device\""*) ;;
	*) fail "this QEMU has no $device, and the profile is not testable without it" ;;
	esac
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# 1. THE HOSTILE ENDPOINTS. Two `edu` functions, because the domain-locality case is about two of
#    them: "the same number means different memory to different devices" is not a question one
#    device can be asked.
echo "qemu-virtio-iommu: booting the enforcing profile"
QEMU_EXTRA="-device virtio-iommu-pci,boot-bypass=on -device edu -device edu" \
	./test.sh --arch x86_64 --tags dma >"$work/run.log" 2>&1 || {
	echo "qemu-virtio-iommu: the dma suite failed under the enforcing profile" >&2
	tail -20 "$work/run.log" >&2
	exit 1
}

# The guest log the run just produced. Sorted rather than `ls -t`, and read with `sed -n '$p'` so
# nothing here is a reader that stops early.
shopt -s nullglob
logs=(.build/logs/test/x86_64-*-guest.log)
shopt -u nullglob
((${#logs[@]})) || fail "the run produced no guest log"
readarray -t logs < <(printf '%s\n' "${logs[@]}" | sort)
log="${logs[-1]}"

# 2. THE TRANSITION HAPPENED, and the kernel says it read the bypass byte back rather than assuming
#    the write took.
grep -aq "iommu: virtio-iommu is translating - bypass is off and read back as off" "$log" || {
	echo "qemu-virtio-iommu: the kernel did not confirm the bypass-off transition" >&2
	grep -a -m 10 "iommu:" "$log" >&2 || true
	exit 1
}

# 3. EVERY REQUIRED CASE RAN AND PASSED. Named individually: a case that silently stopped running is
#    a case that stopped testing, and a count alone would not notice.
for expected in "case 1 PASSED" "case 3 PASSED" "case 5 PASSED" "case 6 PASSED" "case 7 PASSED"; do
	grep -aq "iommu-fixture: $expected" "$log" || {
		echo "qemu-virtio-iommu: '$expected' is not in the guest log - the case did not run or did not pass" >&2
		grep -a "iommu-fixture:" "$log" >&2 || echo "    (the fixture printed nothing at all)" >&2
		exit 1
	}
	echo "qemu-virtio-iommu:   $expected"
done

# 4. AND NOTHING SKIPPED. A profile in which the fixture could not find its devices produces a clean
#    run of nothing, which is the exact shape of a false green - and it is what the same boot without
#    a controller really does produce.
if grep -aq "iommu-fixture: absent" "$log" || grep -aq "iommu-fixture: .* skipped" "$log"; then
	echo "qemu-virtio-iommu: a case reported itself absent or skipped under the enforcing profile" >&2
	grep -a "iommu-fixture:" "$log" >&2
	exit 1
fi

# 5. AND ORDINARY TRAFFIC STILL WORKS. A kernel whose DMA was broken altogether would refuse
#    everything, including what should work, and satisfy every check above.
ISO="$BUILD/libersystem.iso"
[[ -f "$ISO" ]] || fail "no $ISO - run ./image.sh --format iso, which is what the ordinary-traffic half boots"
[[ -f "$OVMF_CODE" ]] || fail "no OVMF firmware at $OVMF_CODE"
echo "qemu-virtio-iommu: booting the shipping image with an ordinary virtio-net endpoint behind the controller"
traffic="$work/traffic.log"
cp "$OVMF_VARS" "$work/vars.fd"
timeout 300 qemu-system-x86_64 \
	-machine q35,default-bus-bypass-iommu=off \
	-m 2G -smp 4 -display none -no-reboot \
	-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
	-drive "if=pflash,format=raw,file=$work/vars.fd" \
	-cdrom "$ISO" \
	-device virtio-iommu-pci,boot-bypass=on \
	-netdev user,id=n0 \
	-device virtio-net-pci,netdev=n0,disable-legacy=on,iommu_platform=on \
	-serial "file:$traffic" >/dev/null 2>&1 || true

grep -aq "iommu: virtio-iommu is translating" "$traffic" || fail "the shipping boot did not bring the controller up"
grep -aq "iommu: .* attached to domain" "$traffic" || {
	echo "qemu-virtio-iommu: no endpoint was attached in the shipping boot - the net driver never came under translation" >&2
	grep -a -m 10 "iommu:" "$traffic" >&2 || true
	exit 1
}
grep -aq "NetworkService: online" "$traffic" || {
	echo "qemu-virtio-iommu: virtio-net did not come up behind the enforcing controller" >&2
	grep -a -m 10 "virtio_net\|devmgr\|iommu:" "$traffic" >&2 || true
	exit 1
}
# AND PACKETS ACTUALLY CROSSED, which "the service is online" does not say. This used to end at the
# line above, so the claim in this gate's own name - ordinary TRAFFIC works - rested on a service
# printing that it had started. A DHCP lease is the cheapest proof that is really traffic: the guest
# put a DISCOVER on the wire through a translated descriptor ring and read an OFFER and an ACK back
# through another, so both directions of the DMA path carried real packets. QEMU's user networking
# answers it without any host setup, so requiring it costs nothing and proves the thing.
grep -aq "network: configured via DHCP" "$traffic" || {
	echo "qemu-virtio-iommu: no DHCP lease behind the enforcing controller - the service started but no packet crossed the translated path" >&2
	grep -a -m 10 "network:\|NetworkService\|iommu: FAULT" "$traffic" >&2 || true
	exit 1
}
echo "qemu-virtio-iommu:   a DHCP lease was obtained through the enforcing controller - real packets both ways"

# 6. AND THE DEFAULT MACHINE IS THIS ONE. Every phase above builds its own QEMU command line, so all
#    of them would keep passing on a day when `run.sh` quietly stopped putting a controller in the
#    machine it boots - which is exactly the state this milestone found the tree in, where the
#    isolated path was proved by one gate and walked by nobody. So the last phase boots the way a
#    developer does, with no flags, and asks the boot what machine it is.
echo "qemu-virtio-iommu: booting the DEFAULT machine, the way an ordinary run does"
default_log="$work/default.log"
timeout 120 ./run.sh --smp 4 --serial "file:$default_log" >/dev/null 2>&1 || true
[[ -s "$default_log" ]] || fail "the default run produced no serial output"

grep -aq "dma: every bus-mastering device is translated" "$default_log" || {
	echo "qemu-virtio-iommu: the DEFAULT run is not translated - an ordinary boot walks the degraded path" >&2
	grep -a "iommu:\|dma:" "$default_log" >&2 || echo "    (it printed no isolation lines at all)" >&2
	exit 1
}
# A degraded row names a device reaching memory untranslated. On the default machine there are none,
# and asserting the ABSENCE is what keeps one from creeping back in unnoticed.
if grep -aq "dma: DEGRADED ISOLATION" "$default_log"; then
	echo "qemu-virtio-iommu: the default run left endpoints untranslated" >&2
	grep -a "dma:" "$default_log" >&2
	exit 1
fi
if grep -aq "iommu: FAULT" "$default_log"; then
	echo "qemu-virtio-iommu: a device faulted on the default machine" >&2
	grep -a "iommu: FAULT" "$default_log" >&2
	exit 1
fi
# THE DISPLAY DRIVER IN PARTICULAR. It is the one that could not run behind a controller at all, and
# it is the reason this was opt-in until the IOVA allocator stopped handing out the null address.
grep -aq "driver.virtio-gpu: online (" "$default_log" || {
	echo "qemu-virtio-iommu: virtio-gpu did not come up on the default translated machine" >&2
	grep -a "virtio-gpu\|devmgr\|DeviceManager:" "$default_log" >&2 || true
	exit 1
}
echo "qemu-virtio-iommu:   the default machine is translated, nothing is degraded, nothing faulted, and the display driver runs"

# AND `--no-iommu` STILL REACHES THE OTHER MACHINE, because a system that can only boot one of them
# has not made isolation optional - it has made the machine without it unreachable, and that machine
# is every one whose firmware offers no IOMMU.
echo "qemu-virtio-iommu: booting --no-iommu, the machine without one"
plain_log="$work/plain.log"
timeout 120 ./run.sh --no-iommu --smp 4 --serial "file:$plain_log" >/dev/null 2>&1 || true
grep -aq "iommu: no virtio-iommu on this machine" "$plain_log" || fail "--no-iommu still put a controller in the machine"
grep -aq "dma: DEGRADED ISOLATION" "$plain_log" || fail "--no-iommu did not report the degraded state it is for"
echo "qemu-virtio-iommu:   --no-iommu boots the untranslated machine and says so"

echo "qemu-virtio-iommu: the controller transitioned out of bypass, five hostile cases were refused by the hardware, an ordinary endpoint passes real traffic, and the DEFAULT machine is the isolated one"
