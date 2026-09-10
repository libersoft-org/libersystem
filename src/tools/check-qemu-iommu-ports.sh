#!/usr/bin/env bash
# THE VIRTIO-IOMMU PROFILES OF THE PORTS, BOOTED: five rows, two phases each, every phase a gate.
#
# WHAT THIS GATE IS. x86_64 has one enforcing gate; the AArch64 and RISC-V QEMU machines gained the
# same controller, the same endpoint options and the same pinned upstream bridge, and this is where
# each named profile proves the same three things the x86_64 gate proves - the transition out of
# bypass is measured, hostile DMA is stopped by the hardware, and ordinary endpoints keep working
# while translation is on - with the interrupt path each profile actually has: the GICv2m frame,
# the GICv3 ITS, the IMSIC.
#
# A ROW IS TWO PHASES ON TWO ARTIFACTS, and the phases are the catalog rows. The hostile and the
# gicv3-its transition phases boot the TEST kernel - `edu` is `#[cfg(test)]` and exists nowhere
# else, and a test boot enters `test_main()` and never starts userspace - so they cannot exercise a
# shipping driver; the UEFI transition phases and every ordinary phase boot the built system
# through `run.sh`, which cannot host `edu`. One green row key binding hostile evidence to shipping
# claims is the defect this shape refuses, so each phase has its own key, its own log, its own
# census and its own envelope, and the row is the umbrella that runs both.
#
#   aarch64:direct-gicv2       hostile (test kernel), ordinary (run.sh)      4 cores, GICv2 + v2m
#   aarch64:direct-gicv3-its   transition (test kernel), ordinary (run.sh)   4 cores, GICv3 + ITS
#   aarch64:uefi-gicv2         transition (run.sh, AAVMF), ordinary (run.sh) 4 cores, GICv2 + v2m
#   riscv64:direct-aia         hostile (test kernel), ordinary (run.sh)      4 cores, AIA/IMSIC
#   riscv64:uefi-aia           transition (run.sh, U-Boot), ordinary (run.sh) 4 cores, AIA/IMSIC
#
# EVERY PHASE KEEPS ITS CENSUS: the kernel's own list of the bus masters it admitted, with their
# addresses and translation, taken from the phase's result log - so a bus master added to a machine
# without a policy decision shows up as a census difference against the row it was added to, not as
# a quiet pass. The GICv3-without-ITS profile is not here on purpose: it has no PCI MSI backend and
# is never labelled DMA-isolated.
#
# EMULATED, SO MINUTES PER PHASE. `--only <arch>:<profile>` runs one row, `--only
# <arch>:<profile>:<phase>` one phase; the catalog registers the ten phases individually.
set -euo pipefail
# THE PHASE LOGS OUTLIVE THIS SCRIPT when a run is collecting evidence: copied into the run from the
# EXIT trap, before the directory is removed - on failure too, which is when they matter.
# shellcheck source=evidence.sh
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"
# shellcheck source=/dev/null
source "$(dirname "$0")/result-logs.sh"
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
# shellcheck source=/dev/null
source ./lib.sh

fail() {
	echo "iommu-ports: $*" >&2
	exit 1
}

ONLY=""
if [[ "${1:-}" == "--only" ]]; then
	ONLY="${2:?--only needs <arch>:<profile>[:<phase>]}"
fi
work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log "$work"/*.census; rm -rf "$work"' EXIT
RAN=0

# The rows, and what each boots with.
row_env() {
	case "$1:$2" in
	aarch64:direct-gicv2) printf 'GIC=2 UEFI=0' ;;
	aarch64:direct-gicv3-its) printf 'GIC=3its UEFI=0' ;;
	aarch64:uefi-gicv2) printf 'GIC=2 UEFI=1' ;;
	riscv64:direct-aia) printf 'UEFI=0' ;;
	riscv64:uefi-aia) printf 'UEFI=1' ;;
	*) return 1 ;;
	esac
}
row_phases() {
	case "$1:$2" in
	aarch64:direct-gicv2 | riscv64:direct-aia) printf 'hostile ordinary' ;;
	aarch64:direct-gicv3-its | aarch64:uefi-gicv2 | riscv64:uefi-aia) printf 'transition ordinary' ;;
	*) return 1 ;;
	esac
}
# The classes the transition must have quiesced on a row, from what its firmware touches.
row_quiesced() {
	case "$1:$2" in
	aarch64:uefi-gicv2) printf 'virtio xhci' ;;
	riscv64:uefi-aia) printf 'nvme xhci' ;;
	*) printf 'virtio' ;;
	esac
}

# THE CENSUS OF ONE PHASE: every bus master the kernel admitted, translated or refused, and every
# attachment, exactly as the kernel printed them.
census() {
	local log="$1" out="$2"
	grep -a -E '^dma: +[a-z-]+ at [0-9a-f:.]+|^iommu: [0-9a-f:.]+ attached to domain|^iommu: quiesced|^dma: every bus-mastering device is translated|^dma: DEGRADED' "$log" >"$out" || true
	echo "iommu-ports:     census: $(grep -c "attached to domain" "$out" || true) attachment(s), $(grep -c "^dma:  " "$out" || true) inventory line(s) -> $(basename "$out")"
}

# The transition, as every phase must show it: the controller is the ONE function mastering the bus
# before bypass goes off, every firmware-touched endpoint of the row's classes confirmed its reset,
# the bypass byte read back off, and no refusal of the transition anywhere.
assert_transition() {
	local log="$1" arch="$2" profile="$3" class
	grep -aq "iommu: the controller at [0-9a-f:.]* masters the bus" "$log" || fail "$arch:$profile: the controller never announced it masters the bus"
	grep -aq "iommu: virtio-iommu is translating - bypass is off and read back as off" "$log" || {
		grep -a -m 10 "iommu:" "$log" >&2 || true
		fail "$arch:$profile: the kernel did not confirm the bypass-off transition"
	}
	! grep -aq "did not confirm its reset\|did not confirm CC.EN" "$log" || fail "$arch:$profile: an endpoint did not confirm its reset and the transition should have refused"
	for class in $(row_quiesced "$arch" "$profile"); do
		grep -aq "iommu: quiesced $class at" "$log" || fail "$arch:$profile: no $class endpoint was quiesced before bypass-off, and this row has one"
	done
	# NOTHING MASTERED THE BUS BEFORE THE CONTROLLER: the first line that says a function masters
	# the bus is the controller's, and no driver was online before translation was confirmed.
	local first_master translating
	first_master="$(grep -a -n "masters the bus\|: online (" "$log" | sed -n '1p' || true)"
	[[ "$first_master" == *"iommu: the controller at"* ]] || fail "$arch:$profile: something mastered the bus before the controller did: ${first_master:-nothing printed}"
	translating="$(grep -a -n "iommu: virtio-iommu is translating" "$log" | sed -n '1p' | cut -d: -f1)"
	local early
	early="$(grep -a -n ": online (" "$log" | awk -F: -v t="$translating" '$1 < t' | sed -n '1p' || true)"
	[[ -z "$early" ]] || fail "$arch:$profile: a driver came online before the transition was confirmed: $early"
}

# A TEST-KERNEL PHASE: the enforcing fixture - controller, virtio-net, two `edu` functions - on the
# row's machine, with the `dma` suite. The hostile cases are the same six the x86_64 gate requires.
phase_test_kernel() {
	local arch="$1" profile="$2" phase="$3" row_environment
	row_environment="$(row_env "$arch" "$profile")"
	local run="$work/$arch-$profile-$phase.run.log"
	echo "iommu-ports: $arch:$profile:$phase - the test kernel with the enforcing fixture (${row_environment})"
	# shellcheck disable=SC2086
	if ! env $row_environment IOMMU=1 DMA_FIXTURE=1 SMP=4 QEMU_EXTRA="-device edu -device edu" ./test.sh --arch "$arch" --tags dma >"$run" 2>&1; then
		tail -25 "$run" >&2
		fail "$arch:$profile:$phase: the dma suite failed under the enforcing profile"
	fi
	local -a logs
	mapfile -t logs < <(result_logs "$run") || fail "$arch:$profile:$phase: the run did not say which logs it wrote"
	((${#logs[@]})) || fail "$arch:$profile:$phase: the run named no readable log"
	local log="$work/$arch-$profile-$phase.log"
	cat "${logs[@]}" >"$log"
	grep -aq "qemu-run: run mode gate, DMA mode enforcing-required (harness provenance)" "$log" || fail "$arch:$profile:$phase: the runner did not announce the gate row with enforcing-required"
	assert_transition "$log" "$arch" "$profile"
	local expected
	for expected in "case 1 PASSED" "case 3 PASSED" "case 5 PASSED" "case 6 PASSED" "case 7 PASSED" "forced-release case PASSED"; do
		grep -aq "iommu-fixture: $expected" "$log" || {
			grep -a "iommu-fixture:" "$log" >&2 || echo "    (the fixture printed nothing at all)" >&2
			fail "$arch:$profile:$phase: '$expected' is not in the guest log - the case did not run or did not pass"
		}
	done
	if grep -aq "iommu-fixture: absent" "$log" || grep -aq "iommu-fixture: .* skipped" "$log"; then
		grep -a "iommu-fixture:" "$log" >&2
		fail "$arch:$profile:$phase: a case reported itself absent or skipped under the enforcing profile"
	fi
	grep -aq "test suite complete: [0-9]* passed" "$log" || fail "$arch:$profile:$phase: the suite did not complete"
	census "$log" "$work/$arch-$profile-$phase.census"
	echo "iommu-ports:   $arch:$profile:$phase: the transition confirmed, the five hostile cases and the forced release refused by the hardware"
	RAN=$((RAN + 1))
}

# A RUN.SH PHASE: the built system on the row's machine, the verdict tool watching its console.
phase_run() {
	local arch="$1" profile="$2" phase="$3" case_name="$4" row_environment
	row_environment="$(row_env "$arch" "$profile")"
	local log="$work/$arch-$profile-$phase.log"
	echo "iommu-ports: $arch:$profile:$phase - the built system through run.sh (${row_environment})"
	# shellcheck disable=SC2086
	env $row_environment SERIAL="file:$log" src/tools/guest-verdict.py "$case_name" "$log" -- ./run.sh --arch "$arch" --smp 4 || fail "$arch:$profile:$phase: the verdict tool refused the boot"
	assert_transition "$log" "$arch" "$profile"
	census "$log" "$work/$arch-$profile-$phase.census"
	RAN=$((RAN + 1))
}

phase_ordinary() {
	local arch="$1" profile="$2"
	# THE TWO ENTRY PATHS SHOW DIFFERENT THINGS (P02M0173). A DIRECT `-kernel` boot has no loader, so
	# the kernel selects ROOT_NONE and no system volume is promoted - the service graph waits on a
	# root that never arrives, so there is no DHCP and no mount, and the enforcing evidence is the
	# kernel's DMA audit with the block driver bound behind the controller. A UEFI boot runs the
	# loader, which promotes a LiberFS volume, so the services come up and the network the degraded
	# profile refuses is proved back with a real lease and a real volume read.
	local log="$work/$arch-$profile-ordinary.log"
	if [[ "$profile" == uefi-* ]]; then
		phase_run "$arch" "$profile" ordinary iommu-port-ordinary-uefi
		local driver
		for driver in virtio-net virtio-blk; do
			grep -aq "driver\.$driver: online (" "$log" || fail "$arch:$profile:ordinary: $driver did not come online behind the controller"
		done
		echo "iommu-ports:   $arch:$profile:ordinary: a DHCP lease and the system volume through translated endpoints, nothing degraded, no fault"
	else
		phase_run "$arch" "$profile" ordinary iommu-port-ordinary-direct
		grep -aq "driver\.virtio-blk: online (" "$log" || fail "$arch:$profile:ordinary: the block driver did not come online behind the controller"
		echo "iommu-ports:   $arch:$profile:ordinary: every bus-mastering device translated, the block driver bound behind the controller, no untranslated admission, no fault (a direct boot promotes no root, so traffic is proved on the UEFI row)"
	fi
	! grep -aq "dma: DEGRADED ISOLATION\|ADMITTED UNTRANSLATED" "$log" || fail "$arch:$profile:ordinary: a bus master was admitted untranslated"
}

phase_transition_run() {
	local arch="$1" profile="$2"
	phase_run "$arch" "$profile" transition iommu-port-transition
	echo "iommu-ports:   $arch:$profile:transition: the firmware-touched endpoints were quiesced by class and bypass read back off before any driver mastered the bus"
}

run_row() {
	local arch="$1" profile="$2" only_phase="${3:-}" phase
	for phase in $(row_phases "$arch" "$profile"); do
		[[ -z "$only_phase" || "$only_phase" == "$phase" ]] || continue
		case "$phase" in
		hostile) phase_test_kernel "$arch" "$profile" hostile ;;
		transition)
			if [[ "$profile" == direct-* ]]; then
				phase_test_kernel "$arch" "$profile" transition
			else
				phase_transition_run "$arch" "$profile"
			fi
			;;
		ordinary) phase_ordinary "$arch" "$profile" ;;
		esac
	done
}

for row in aarch64:direct-gicv2 aarch64:direct-gicv3-its aarch64:uefi-gicv2 riscv64:direct-aia riscv64:uefi-aia; do
	arch="${row%%:*}"
	profile="${row#*:}"
	if [[ -n "$ONLY" ]]; then
		only_arch="${ONLY%%:*}"
		rest="${ONLY#*:}"
		only_profile="${rest%%:*}"
		only_phase=""
		[[ "$rest" == *:* ]] && only_phase="${rest#*:}"
		[[ "$only_arch" == "$arch" && "$only_profile" == "$profile" ]] || continue
		kernel=".build/cargo/kernel/$(target_triple "$arch")/debug/kernel"
		[[ -f "$kernel" ]] || fail "no $arch kernel - run ./build.sh --arch $arch"
		run_row "$arch" "$profile" "$only_phase"
	else
		kernel=".build/cargo/kernel/$(target_triple "$arch")/debug/kernel"
		[[ -f "$kernel" ]] || fail "no $arch kernel - run ./build.sh --arch $arch"
		run_row "$arch" "$profile"
	fi
done

((RAN > 0)) || fail "'${ONLY:-every row}' named no phase this gate knows - nothing was booted, so nothing is proved"
echo "iommu-ports: $RAN phase(s) booted, each with its transition confirmed, its census kept and its claim proved on the interrupt path its profile has"
