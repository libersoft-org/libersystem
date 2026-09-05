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
# shellcheck source=/dev/null
source "$HERE/result-logs.sh"
BUILD_ROOT=".build"
BUILD="$BUILD_ROOT/boot"
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

# THE SHIPPING MEDIUM IS THIS TREE'S - CHECKED FIRST, AND OVER EVERY INPUT IT CARRIES.
#
# FIRST, because the phases below build and boot test media, and a test build rewrites staged
# artifacts the shipping image is keyed on. A freshness question asked after them is asked about a
# tree the gate itself has moved; asked here it is about the tree the operator left.
#
# AND OVER EVERY INPUT: this compared the kernel ELF's mtime with the image's. The phases below
# exercise DeviceManager, the userspace drivers and the services on the system volume as much as
# they exercise the kernel, and `build.sh` rebuilds any of those - and the volume - without
# rewriting the kernel, so changed driver bytes could be newer than the ISO while the untouched
# kernel stayed older and this preflight would call the image fresh. Worse for a timestamp check
# still: several staged artifacts have their mtimes PINNED to `SOURCE_DATE_EPOCH` so images build
# reproducibly, so for those an mtime comparison can never answer anything at all.
#
# `mkimage.sh` computes a content-derived key over all of it - kernel, loader, init package, the
# bootable volume and its pairing sidecar, the service manifest and its normalized layout, the
# fallback bootstrap set, `product.conf` and the builders - and records the key each published image
# was built from beside it. This asks the builder for today's key and compares the two.
ISO="$BUILD/libersystem.iso"
[[ -f "$ISO" ]] || fail "no $ISO - run ./image.sh --format iso, which is what the ordinary-traffic half boots"
[[ -f "$ISO.build-key" ]] || fail "$ISO carries no build receipt, so nothing about it can be checked - rebuild it:  ./image.sh --format iso"
KERNEL_ELF="$BUILD_ROOT/cargo/kernel/x86_64-unknown-none/debug/kernel"
[[ -f "$KERNEL_ELF" ]] || fail "no built kernel at $KERNEL_ELF, so this gate cannot tell whether $ISO carries this tree - build first:  ./build.sh --arch x86_64"
# `$HERE`, not `dirname "$0"`: this script has already `cd`-ed to the repository root. stderr is kept
# because a builder that cannot answer has a reason, and swallowing it turns a diagnosable refusal
# into a blank one.
current_key="$(LIBER_IMAGE_PRINT_KEY=1 "$HERE/../harness/mkimage.sh" iso "$KERNEL_ELF" || true)"
[[ -n "$current_key" ]] || fail "the image builder could not compute this tree's image key - build first:  ./build.sh --arch x86_64"
# AND THE BYTES, NOT ONLY THE KEY THEY WERE BUILT FROM.
#
# The key describes the INPUTS. `mkimage.sh` records `$ISO.build-digest` separately, and verifies it
# on a cache hit, precisely because a matching input key says nothing about the output: an ISO that
# was truncated, edited or replaced after it was built sits beside a still-current key and this
# preflight called it "built from this tree". The builder already computes the digest it expects, so
# the gate reads the same sidecar rather than inventing a second answer.
if [[ -f "$ISO.build-digest" ]]; then
	actual_digest="$(sha256sum "$ISO" | awk '{print $1}')"
	if [[ "$actual_digest" != "$(<"$ISO.build-digest")" ]]; then
		echo "qemu-virtio-iommu: $ISO does not match the digest recorded when it was built" >&2
		echo "qemu-virtio-iommu:   recorded $(<"$ISO.build-digest")" >&2
		echo "qemu-virtio-iommu:   on disk  $actual_digest" >&2
		echo "qemu-virtio-iommu:   the image was changed after it was built - rebuild it:  ./image.sh --format iso" >&2
		exit 1
	fi
else
	fail "$ISO carries no build digest, so its bytes cannot be checked - rebuild it:  ./image.sh --format iso"
fi
if [[ "$current_key" != "$(<"$ISO.build-key")" ]]; then
	echo "qemu-virtio-iommu: $ISO was not built from this tree" >&2
	echo "qemu-virtio-iommu:   the image was built from $(<"$ISO.build-key")" >&2
	echo "qemu-virtio-iommu:   this tree computes     $current_key" >&2
	echo "qemu-virtio-iommu:   rebuild the image from this tree:  ./image.sh --format iso" >&2
	exit 1
fi
echo "qemu-virtio-iommu: the shipping image was built from this tree ($current_key)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# 1. THE HOSTILE ENDPOINTS. Two `edu` functions, because the domain-locality case is about two of
#    them: "the same number means different memory to different devices" is not a question one
#    device can be asked.
# AND ON THE FIXTURE M2 SPECIFIES, NOT ON THE ORDINARY TEST MACHINE (corrected 2026-09-04).
#
# This added the controller and the two `edu` functions to the machine `./test.sh` builds for every
# other suite - which carries a virtio-serial console, an xHCI controller with a hub, a keyboard, a
# tablet and a USB stick, three more virtio-blk media disks, a PCIe-to-PCI bridge with a device
# behind it, and a virtio-sound card. M2 says in as many words to keep this a dedicated fixture and
# not to inherit optional DMA devices from the ordinary harness, and it is not a tidiness point: the
# transition this gate exists to prove QUIESCES every non-controller endpoint before turning bypass
# off, so each of those bus masters was a participant in the security-sensitive step. The hostile
# cases passing there said nothing about the topology the milestone describes - and the bridge is
# one the fixture is supposed to REFUSE rather than generalize to.
#
# `DMA_FIXTURE=1` omits exactly those and keeps the firmware boot medium, the system volume and
# virtio-net, which is the list M2 gives.
# AND `IOMMU=1` RATHER THAN A CONTROLLER BOLTED ON THROUGH `QEMU_EXTRA` (corrected 2026-09-04).
#
# `./test.sh` reaches the runner with `TEST=1`, which selects the UNTRANSLATED profile: the machine
# stays plain `q35` with bus bypass allowed by default, and every endpoint is built without
# `iommu_platform=on` - so the endpoints were not told their addresses are translated and the
# controller arrived through `QEMU_EXTRA`, which is appended AFTER them, in a machine that permitted
# bypass anyway. The hostile cases were being refused on a profile that is not M2's.
#
# `IOMMU` is the documented way for "a gate that owns its own profile" to say so. It puts the
# controller in front of the endpoints it translates, marks each of them, and turns the machine's
# bypass default off - so only the two `edu` functions remain for `QEMU_EXTRA` to add.
echo "qemu-virtio-iommu: booting the enforcing profile on M2's dedicated fixture"
IOMMU=1 DMA_FIXTURE=1 QEMU_EXTRA="-device edu -device edu" \
	./test.sh --arch x86_64 --tags dma >"$work/run.log" 2>&1 || {
	echo "qemu-virtio-iommu: the dma suite failed under the enforcing profile" >&2
	tail -20 "$work/run.log" >&2
	exit 1
}

# THE LOGS THE RUN SAID IT WROTE. This was "the newest x86_64 guest log", which is a correct-looking
# read of ANOTHER guest's result the moment two runs of one architecture overlap - and this gate's
# whole subject is a security property, so a green taken from somebody else's boot is the worst kind
# of wrong answer it could give.
mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"
log="$work/run.result"
cat "${logs[@]}" >"$log"

# 2. THE TRANSITION HAPPENED, and the kernel says it read the bypass byte back rather than assuming
#    the write took.
grep -aq "iommu: virtio-iommu is translating - bypass is off and read back as off" "$log" || {
	echo "qemu-virtio-iommu: the kernel did not confirm the bypass-off transition" >&2
	grep -a -m 10 "iommu:" "$log" >&2 || true
	exit 1
}

# 3. EVERY REQUIRED CASE RAN AND PASSED. Named individually: a case that silently stopped running is
#    a case that stopped testing, and a count alone would not notice.
#    `forced-release case PASSED` is here for the same reason and was missing (added 2026-09-02).
#    The absent/skipped check below catches that case when it RUNS and declines; it says nothing
#    about a case that stops being registered at all - deleting the test, dropping its tag or
#    renaming its marker produces neither string and left this gate green over M9's mandatory
#    hostile-holder proof.
for expected in "case 1 PASSED" "case 3 PASSED" "case 5 PASSED" "case 6 PASSED" "case 7 PASSED" "forced-release case PASSED"; do
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
# The image and the kernel were checked at the top of this gate, before any phase could move them.
[[ -f "$OVMF_CODE" ]] || fail "no OVMF firmware at $OVMF_CODE"
echo "qemu-virtio-iommu: booting the shipping image with an ordinary virtio-net endpoint behind the controller"
traffic="$work/traffic.log"
cp "$OVMF_VARS" "$work/vars.fd"
src/tools/guest-verdict.py iommu-traffic "$traffic" -- qemu-system-x86_64 \
	-machine q35,default-bus-bypass-iommu=off \
	-m 2G -smp 4 -display none -no-reboot \
	-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE" \
	-drive "if=pflash,format=raw,file=$work/vars.fd" \
	-cdrom "$ISO" \
	-device virtio-iommu-pci,boot-bypass=on \
	-netdev user,id=n0 \
	-device virtio-net-pci,netdev=n0,disable-legacy=on,iommu_platform=on \
	-serial "file:$traffic"

grep -aq "iommu: virtio-iommu is translating" "$traffic" || fail "the shipping boot did not bring the controller up"
grep -aq "iommu: .* attached to domain" "$traffic" || {
	echo "qemu-virtio-iommu: no endpoint was attached in the shipping boot - the net driver never came under translation" >&2
	grep -a -m 10 "iommu:" "$traffic" >&2 || true
	exit 1
}
# THE DRIVER'S OWN LINE, WHICH IS THE ONE THAT REACHES THIS CONSOLE.
#
# This asked for `NetworkService: online`, and that string never appears on the serial console at all:
# a service reports in by SENDING that text to its supervisor, `ServiceManager` relays it to
# SystemManager over a channel, and SystemManager does not print it. So the assertion could not pass on
# any boot - and the recorded history for this gate is three runs and three failures, which is what
# that looks like from outside.
#
# `driver.virtio-net: online (bb:dd.f)` is the driver's own report, written to the console by the
# driver, and it says the thing this phase is about: the NIC bound and came up while its endpoint was
# behind the enforcing controller. The DHCP assertion below is what proves packets then crossed.
grep -aq "driver.virtio-net: online (" "$traffic" || {
	echo "qemu-virtio-iommu: virtio-net did not come up behind the enforcing controller" >&2
	grep -a -m 10 "virtio.net\|DeviceManager:\|iommu:" "$traffic" >&2 || true
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
src/tools/guest-verdict.py iommu-default "$default_log" -- ./run.sh --smp 4 --serial "file:$default_log"
[[ -s "$default_log" ]] || fail "the default run produced no serial output"
# AND IT DID NOT DIE AFTER SAYING SO.
#
# The watcher preserves the whole 120-second observation, and checks process exits as well as
# serial failures. A boot that printed all the lines below and panicked ten seconds later once
# passed this phase. The greps that follow assert the absence of degraded
# rows and of faults; these assert the absence of the boot ending badly, which is the same kind of
# check and was the one missing.
survived_the_boot() {
	local log="$1" phase="$2" bad boots
	for bad in "KERNEL PANIC" "loader: FATAL"; do
		if grep -aq "$bad" "$log"; then
			echo "qemu-virtio-iommu: the $phase run printed '$bad' - the machine may be translated and it did not survive the boot" >&2
			grep -a -m 5 -B 2 "$bad" "$log" >&2
			exit 1
		fi
	done
	# A RESET IS COUNTED, NOT GREPPED FOR - and the string this used to look for could never appear.
	#
	# `GUEST RESET` is synthesized by `test-kernel.sh` when it interprets a TEST-mode QEMU exit; the
	# guest never prints it, and an ordinary `./run.sh` is not test mode, so the grep matched nothing
	# on every run and passed. Worse, an ordinary run has no `-no-reboot` - that flag and the
	# debug-exit device exist only under `TEST=1` - so a triple fault after the required lines
	# REBOOTS, the second boot appends to the same serial file, and the assertions above still find
	# what the first boot printed.
	#
	# The loader prints its banner exactly once per boot, so more than one of them in one serial log
	# IS the reset. This is the oracle the phase was missing, and it works without changing how an
	# ordinary run is launched.
	boots="$(grep -ac "LiberSystem UEFI loader" "$log" || true)"
	if [[ "${boots:-0}" -gt 1 ]]; then
		echo "qemu-virtio-iommu: the $phase run booted $boots times - the guest reset after printing its lines, and a reboot is not a pass" >&2
		exit 1
	fi
	if [[ "${boots:-0}" -eq 0 ]]; then
		echo "qemu-virtio-iommu: the $phase run never printed the loader banner - it did not boot far enough for anything below to mean what it says" >&2
		exit 1
	fi
}

survived_the_boot "$default_log" "default"

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
# AND A SUMMARY THAT WAS RETRACTED IS NOT A CLEAN SUMMARY.
#
# The isolation summary is printed when the supervisor decides the system is up, and a device that
# binds afterwards can still be admitted untranslated - so `dma_policy` retracts the claim at the
# moment that happens. This gate asked only for the clean line and only rejected `DEGRADED
# ISOLATION`, so a boot could print the clean summary, explicitly say it was no longer true, and
# still be accepted. The retraction is a REJECTION here: it names a device mastering the bus
# untranslated on the machine this phase is about.
if grep -aq "dma: ADMITTED UNTRANSLATED AFTER THE ISOLATION SUMMARY" "$default_log"; then
	echo "qemu-virtio-iommu: the default run retracted its own isolation summary - a device was admitted untranslated after it was printed" >&2
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
# EXACTLY ONCE, which is what tells a driver that came up from one that keeps coming up.
#
# The line was asserted to be present and nothing looked at how many times. A driver that binds,
# dies and is restarted prints it again on each attempt, and a restart loop is indistinguishable
# from a clean bring-up by presence alone - on the very driver this milestone's default profile
# exists to prove works behind the controller.
gpu_lines="$(grep -ac "driver.virtio-gpu: online (" "$default_log" || true)"
if [[ "$gpu_lines" -eq 0 ]]; then
	echo "qemu-virtio-iommu: virtio-gpu did not come up on the default translated machine" >&2
	grep -a "virtio-gpu\|devmgr\|DeviceManager:" "$default_log" >&2 || true
	exit 1
fi
if [[ "$gpu_lines" -gt 1 ]]; then
	echo "qemu-virtio-iommu: virtio-gpu reported itself online $gpu_lines times - it is restarting, not running" >&2
	grep -a "virtio-gpu" "$default_log" >&2 || true
	exit 1
fi
# AND NOBODY RESTARTED IT. Counting the online lines catches a restart that SUCCEEDS; a driver that
# comes online once, dies, and whose restart attempt fails before it can report again leaves the
# count at one. DeviceManager says what it is doing on the way in, so the restart itself is the
# thing to refuse - which is what the definition of done asks for, rather than its usual symptom.
if grep -aq "DeviceManager: restarting virtio-gpu" "$default_log"; then
	echo "qemu-virtio-iommu: virtio-gpu was restarted on the default translated machine - it came up and did not stay up" >&2
	grep -a "virtio-gpu\|DeviceManager: restarting" "$default_log" >&2 || true
	exit 1
fi
# AND A FRAME REACHED THE DISPLAY, which is what M4 asks and what "it printed online" is not.
#
# The driver reports online before any frame exists - it has a device, not a picture - and the first
# frame is submitted later by ConsoleService, which used to DISCARD the presentation result. So a boot
# where every present failed behind the controller looked exactly like one where they all landed.
# ConsoleService now says which, once, and this is the assertion that makes the difference a gate can
# see rather than a reader.
if ! grep -aq "ConsoleService: a frame reached the display" "$default_log"; then
	echo "qemu-virtio-iommu: no frame reached the display on the default translated machine" >&2
	grep -a "ConsoleService:\|virtio-gpu\|display" "$default_log" >&2 || true
	exit 1
fi
if grep -aq "ConsoleService: a frame did NOT reach the display" "$default_log"; then
	echo "qemu-virtio-iommu: a frame failed to reach the display on the default translated machine" >&2
	grep -a "ConsoleService:\|virtio-gpu\|display" "$default_log" >&2 || true
	exit 1
fi
echo "qemu-virtio-iommu:   the default machine is translated, nothing is degraded, nothing faulted, the display driver runs and a frame reached the screen"

# AND `--no-iommu` STILL REACHES THE OTHER MACHINE, because a system that can only boot one of them
# has not made isolation optional - it has made the machine without it unreachable, and that machine
# is every one whose firmware offers no IOMMU.
echo "qemu-virtio-iommu: booting --no-iommu, the machine without one"
plain_log="$work/plain.log"
src/tools/guest-verdict.py iommu-plain "$plain_log" -- ./run.sh --no-iommu --smp 4 --serial "file:$plain_log"
[[ -s "$plain_log" ]] || fail "the --no-iommu run produced no serial output"
survived_the_boot "$plain_log" "--no-iommu"
grep -aq "iommu: no virtio-iommu on this machine" "$plain_log" || fail "--no-iommu still put a controller in the machine"
grep -aq "dma: DEGRADED ISOLATION" "$plain_log" || fail "--no-iommu did not report the degraded state it is for"
echo "qemu-virtio-iommu:   --no-iommu boots the untranslated machine and says so"

echo "qemu-virtio-iommu: the controller transitioned out of bypass, five hostile cases were refused by the hardware, an ordinary endpoint passes real traffic, and the DEFAULT machine is the isolated one"
