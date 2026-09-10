#!/usr/bin/env bash
# The DMA mode on the device-tree ports, booted: the produced value on every row, through both
# entry paths, in both machines, and the refusals each path owes.
#
# WHAT THIS GATE IS. AArch64 and RISC-V carried `no-iommu` on every row until they gained a
# controller (P02M0172); their ordinary machines have one now (P02M0173), so the ordinary rows carry
# `enforcing-required` and `--no-iommu` selects the explicit degraded machine and value - a trusted,
# produced, named value either way, never an absence. Two entry paths carry it: the runner's per-run
# ESP stages `EFI/BOOT/LSDM` beside the loader, which relays it with `harness` provenance; a direct
# `-kernel` boot reads it out of this product's node in the device tree, which the runner annotates.
# A UEFI boot and a direct boot carrying the same record reach the same admission decision - on the
# enforcing machine `virtio_net` admitted and passing traffic with every bus master translated, on
# the degraded one `virtio_net` refused by name and value and the trusted rows admitted with visible
# degradation. The degraded rows are the standing untranslated regression: they are what keeps the
# no-controller behaviour tested now that no ordinary boot walks it.
#
# And the refusals: no record on the ESP, no node in the tree, and - on the UEFI path - the tree's
# node beside the ESP file, an independent producer refused even though the values agree.
#
# THE REDUCED MACHINE ON EVERY ROW THAT ASSERTS ON A DRIVER (`DMA_ORDINARY=1`). This gate's subject
# is the CARRIER - which producer put the mode in front of admission, and what admission then did
# with it - not how many bus masters the machine happens to have. The full interactive machine puts
# about a dozen translated endpoints through attach-and-map inside DeviceManager's boot window,
# which an emulated port does not finish in time; the reduced machine keeps the NIC and the system
# volume, which are the two drivers every assertion here names. The three refusal rows below are
# left on their own machines: they refuse before a driver is reached at all.
#
# EMULATED, SO THIS IS MINUTES PER BOOT and there are twelve boots. Run at the end of a batch, like
# every other emulated gate. `--only aarch64` or `--only riscv64` runs one port.
set -euo pipefail
# THE PHASE LOGS OUTLIVE THIS SCRIPT when a run is collecting evidence: copied into the run from the
# EXIT trap, before the directory is removed - on failure too, which is when they matter.
# shellcheck source=evidence.sh
source "$(dirname "${BASH_SOURCE[0]}")/evidence.sh"
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "dma-mode-ports: $*" >&2
	exit 1
}

ONLY=""
if [[ "${1:-}" == "--only" ]]; then
	ONLY="${2:?--only needs aarch64 or riscv64}"
fi
work="$(mktemp -d)"
trap 'evidence_keep_gate "$work"/*.log "$work"/*/*.log; rm -rf "$work"' EXIT
# shellcheck source=/dev/null
source ./lib.sh

for port in aarch64 riscv64; do
	if [[ -n "$ONLY" && "$ONLY" != "$port" ]]; then
		continue
	fi
	kernel=".build/cargo/kernel/$(target_triple "$port")/debug/kernel"
	[[ -f "$kernel" ]] || fail "no $port kernel - run ./build.sh --arch $port"
	[[ -f ".build/boot/system-volume-$port.img" ]] || fail "no $port system volume - run ./build.sh --arch $port"
	loader_stamp=".build/state/built-$port-loader"
	if [[ ! -f "$loader_stamp" || "$(<"$loader_stamp")" != "$(source_digest "${LOADER_SOURCES[@]}")" ]]; then
		fail "the $port loader was not built from this tree - run ./build.sh --arch $port --part loader"
	fi

	# 1. THE UEFI ROW: the per-run ESP carries the record, the loader relays it - and the machine
	#    has a controller, so the record says `enforcing-required` and admission translates.
	echo "dma-mode-ports: $port UEFI - the ESP record relayed with harness provenance, the enforcing machine"
	log="$work/$port-uefi.log"
	DMA_ORDINARY=1 UEFI=1 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-admits "$log" -- ./run.sh --arch "$port" --smp 2
	grep -aq "loader: DMA mode enforcing-required (harness" "$log" || fail "$port UEFI: the loader did not report the harness hand-off"
	grep -aq "dma: boot DMA mode enforcing-required (harness provenance, the loader's BootInfo)" "$log" || fail "$port UEFI: admission did not take the mode from the loader's BootInfo"
	echo "dma-mode-ports:   $port UEFI: enforcing-required from the ESP, virtio_net admitted behind the controller with a lease, every bus master translated"

	# 2. THE DIRECT ROW: the same record in the tree, read before anything was admitted. The direct
	#    path carries one blob and starts SystemManager without a shell, which is enough: the
	#    admissions happen in DeviceManager, which the init package carries.
	#    AND IT JUDGES WITHOUT NetworkService, because a direct boot cannot have one: no loader runs,
	#    so the kernel selects ROOT_NONE, StorageService promotes nothing and every service waiting
	#    on storage stays unstarted. The claim here is the admission decision - the mode with its
	#    provenance, the isolation summary, and `virtio_net` admitted by name - not a lease.
	echo "dma-mode-ports: $port direct - the device tree's boot-policy node, the enforcing machine"
	log="$work/$port-direct.log"
	DMA_ORDINARY=1 UEFI=0 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-admits-direct "$log" -- src/harness/qemu-run.sh "$port" "$kernel"
	grep -aq "dma: boot DMA mode enforcing-required (harness provenance, the device tree's boot-policy record)" "$log" || fail "$port direct: admission did not take the mode from the tree"
	echo "dma-mode-ports:   $port direct: the same value, the same decision"

	# 2b. THE DEGRADED MACHINE, BOTH PATHS: `--no-iommu` takes the controller out, the record says
	#     `no-iommu`, and the boot is the explicit loud degraded profile - virtio_net refused by name
	#     and value, the trusted rows admitted and listed. This is the standing regression of the
	#     untranslated behaviour now that no ordinary boot walks it.
	echo "dma-mode-ports: $port UEFI --no-iommu - the explicit degraded machine"
	log="$work/$port-uefi-degraded.log"
	DMA_ORDINARY=1 UEFI=1 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-degraded "$log" -- ./run.sh --arch "$port" --smp 2 --no-iommu
	grep -aq "loader: DMA mode no-iommu (harness" "$log" || fail "$port UEFI --no-iommu: the loader did not report the degraded hand-off"
	grep -aq "dma: boot DMA mode no-iommu (harness provenance, the loader's BootInfo)" "$log" || fail "$port UEFI --no-iommu: admission did not take the degraded mode from the loader's BootInfo"
	echo "dma-mode-ports:   $port UEFI --no-iommu: no-iommu from the ESP, virtio_net refused by name and value, the trusted rows admitted and listed"
	echo "dma-mode-ports: $port direct with no controller - the degraded value in the tree"
	log="$work/$port-direct-degraded.log"
	DMA_ORDINARY=1 IOMMU=0 UEFI=0 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-degraded-direct "$log" -- src/harness/qemu-run.sh "$port" "$kernel"
	grep -aq "dma: boot DMA mode no-iommu (harness provenance, the device tree's boot-policy record)" "$log" || fail "$port direct --no-iommu: admission did not take the degraded mode from the tree"
	echo "dma-mode-ports:   $port direct with no controller: the same degraded value, the same refusal"

	# 3. THE ABSENT RECORD, on both paths. The loader halts; the kernel refuses every claim.
	echo "dma-mode-ports: $port UEFI with no ESP record - the loader refuses"
	log="$work/$port-uefi-absent.log"
	DMA_RECORD=absent UEFI=1 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-loader-refused "$log" -- ./run.sh --arch "$port" --smp 1
	grep -aq "harness carrier is absent" "$log" || fail "$port UEFI: the absent record was not named"
	echo "dma-mode-ports:   refused before the kernel started"
	echo "dma-mode-ports: $port direct with no node in the tree - the kernel refuses every claim"
	log="$work/$port-direct-absent.log"
	DMA_RECORD=absent UEFI=0 SERIAL="file:$log" src/tools/guest-verdict.py dma-kernel-refused "$log" -- src/harness/qemu-run.sh "$port" "$kernel"
	grep -aq "dma: the device tree carries no boot-policy record - REFUSED" "$log" || fail "$port direct: the absent node was not named"
	if grep -aq "driver\.[a-z-]*: online (" "$log"; then
		fail "$port direct: a driver came online with no DMA mode"
	fi
	echo "dma-mode-ports:   refused, no driver admitted"

	# 4. THE INDEPENDENT PRODUCER: the tree's node beside the ESP file, values agreeing.
	echo "dma-mode-ports: $port UEFI with the boot-policy node ALSO in the firmware's tree - two producers"
	log="$work/$port-uefi-independent.log"
	DMA_DTB_NODE=1 UEFI=1 SERIAL="file:$log" src/tools/guest-verdict.py dma-port-two-producers "$log" -- ./run.sh --arch "$port" --smp 1
	# EITHER COMPONENT, because which one sees the tree is the firmware's choice and not this rule's.
	# These ports' firmware boots in ACPI mode and publishes no device-tree configuration table, so
	# the loader is handed nothing to check and the kernel - which finds the machine's tree itself,
	# because this architecture needs one - is the component that reads the node and refuses.
	grep -aqE "the device tree's boot-policy node is present beside this entry path's DMA-mode input|the device tree carries the boot-policy node - two producers, REFUSED" "$log" || fail "$port UEFI: the independent tree node was not named"
	grep -aq "every device claim will be REFUSED\|loader: FATAL" "$log" || fail "$port UEFI: the boot did not refuse over the second producer"
	echo "dma-mode-ports:   refused although the values agree, and nothing was admitted"
done

echo "dma-mode-ports: the produced value reaches admission on both entry paths of both ports - enforcing on the machine with a controller, degraded and loud without one - and every refusal holds"
