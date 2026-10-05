#!/usr/bin/env bash
# THE SYSTEM VOLUME ON A CONTROLLER THAT IS NOT VIRTIO-BLK, END TO END: the disk the medium is paired with put behind
# an NVMe controller and then behind an AHCI one, with no virtio-blk system disk in the machine at all.
#
# A SECONDARY DISK PROVES NOTHING HERE. Both drivers already format, read, write and flush a scratch disk through
# their oracles while the machine boots from virtio-blk; what that cannot show is the staging - a driver loaded from
# the volume cannot be the one that makes the volume reachable - nor how the volume is chosen when several disks are
# present, nor what happens when it cannot be reached. So, per controller:
#   1. two boots on ONE persistent disk: the first from the paired block volume (not a copy in memory), `lsblk`
#      naming the controller as the device under `vol://system`, and a file written; the second, cold, reading that
#      file back - written through that controller, flushed, and found again after a power-off.
# And once each:
#   2. THE DECOY: an unpaired LiberFS volume on a virtio-blk disk beside the paired one on NVMe. The volume is chosen
#      by the medium's pairing uuid and never by bus order or transport, so `vol://system` is still the NVMe disk.
#   3. THE FALLBACK, which is none by design: the paired volume behind a virtio-scsi controller, which no driver bound
#      before the volume serves. The boot must refuse by name - "selected root volume is missing" - rather than run
#      the system from anything else.
set -euo pipefail
GUEST_GATE_NAME="boot-volume-controllers"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
[[ "$GUEST_ARCH" == x86_64 ]] || guest_gate_fail "the system disk's controller is chosen on x86_64 runs only (SYSTEM_DISK)"

fail() { guest_gate_fail "$@"; }

bootable="$root/../.build/boot/system-volume-bootable-x86_64.img"
template="$root/../.build/boot/system-volume-x86_64.img"
[[ -f "$bootable" ]] || fail "there is no bootable system volume beside the image - build it:  ./image.sh"
[[ -f "$template" ]] || fail "there is no unpaired system volume to use as the decoy - build it:  ./image.sh"
export NET_NONE=1

expect() {
	local lines="$1" line="$2" why="$3"
	grep -aqF -- "$line" "$lines" || {
		echo "boot-volume-controllers: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		grep -aE 'loader:|boot:|StorageService|storage:|vol://|driver\.(nvme|ahci|virtio-blk)|bootvol' "$lines" >&2 || cat "$lines" >&2
		exit 1
	}
}

# FROM THE DISK, NOT FROM A COPY OF IT. A volume copied into memory is forgotten by the power-off, so the second boot
# would read back nothing it could have lost.
from_the_disk() {
	local lines="$1"
	if grep -aqF "vol://system is a live copy in memory" "$lines" || ! grep -aqF "boot: the system volume is a paired block volume" "$lines"; then
		fail "$2: the system did not run from the paired disk - a copy of the medium's image in memory proves nothing about the controller"
	fi
}

# `lsblk`'s row for the system volume, as the block driver behind it names its device.
system_device() {
	grep -aoE '^vol://system +[a-z0-9-]+' "$1" | awk '{print $2}' | tail -n 1 || true
}

# ---- 1. per controller: write through it, power off, read it back cold.
for bus in nvme ahci; do
	export SYSTEM_DISK="$bus" RUN_DISK="$guest_gate_work/$bus-system.img"
	export GUEST_GATE_SECONDS=150 GUEST_GATE_TIMEOUT=200
	proof="written-through-$bus"
	guest_gate_run $'lsblk\nwrite boot-volume-proof.txt '"$proof"$'\ncat boot-volume-proof.txt\npoweroff' ""
	first="$guest_gate_work/$bus-first"
	cp "$GUEST_LINES" "$first"
	from_the_disk "$first" "$bus, first boot"
	expect "$first" "StorageService: online (vol://system)" "the system volume must be served from the $bus disk"
	device="$(system_device "$first")"
	[[ "$device" == "$bus" ]] || fail "$bus, first boot: lsblk named '${device:-nothing}' as the device under vol://system"
	echo "boot-volume-controllers: $bus: vol://system served from the paired disk, and lsblk names $bus as its device"
	expect "$first" "wrote vol://system/boot-volume-proof.txt" "the file must be written to the system volume"
	guest_gate_run $'lsblk\ncat boot-volume-proof.txt\npoweroff' ""
	second="$guest_gate_work/$bus-second"
	cp "$GUEST_LINES" "$second"
	from_the_disk "$second" "$bus, second boot"
	device="$(system_device "$second")"
	[[ "$device" == "$bus" ]] || fail "$bus, second boot: lsblk named '${device:-nothing}' as the device under vol://system"
	# THE LINE ALONE, which only `cat` prints: the typed command echoes the text after its own name.
	grep -aqxF -- "$proof"$'\r' "$second" || grep -aqxF -- "$proof" "$second" || fail "$bus, second boot: the file written through $bus before the power-off did not read back"
	echo "boot-volume-controllers: $bus: the file written in the first boot read back after a cold reboot"
done

# ---- 2. the decoy: an unpaired LiberFS volume on virtio-blk beside the paired one on NVMe.
cp --reflink=auto "$template" "$guest_gate_work/decoy.img"
export SYSTEM_DISK=nvme RUN_DISK="$guest_gate_work/decoy-system.img"
export QEMU_EXTRA="-drive file=$guest_gate_work/decoy.img,if=none,id=decoy,format=raw -device virtio-blk-pci,drive=decoy"
export GUEST_GATE_SECONDS=120 GUEST_GATE_TIMEOUT=170
guest_gate_run $'lsblk\npoweroff' ""
unset QEMU_EXTRA
decoy="$guest_gate_work/decoy"
cp "$GUEST_LINES" "$decoy"
from_the_disk "$decoy" "decoy"
if grep -aqF "selected root volume is ambiguous" "$decoy"; then
	fail "decoy: two LiberFS volumes were read as one choice - the pairing uuid must tell them apart"
fi
device="$(system_device "$decoy")"
[[ "$device" == nvme ]] || fail "decoy: lsblk named '${device:-nothing}' under vol://system, and the paired volume is on NVMe"
echo "boot-volume-controllers: the paired volume on NVMe was chosen over an unpaired LiberFS volume on virtio-blk"

# ---- 3. the fallback: the paired volume where no driver bound before it can reach it.
export SYSTEM_DISK=virtio-scsi RUN_DISK="$guest_gate_work/scsi-system.img"
export GUEST_GATE_SECONDS=90 GUEST_GATE_TIMEOUT=120
guest_gate_run $'lsblk' ""
fallback="$guest_gate_work/fallback"
cp "$GUEST_LINES" "$fallback"
expect "$fallback" "StorageService: selected root volume is missing; refused" "a paired volume no boot-critical driver reaches must be refused by name"
if grep -aqF "StorageService: online (vol://system)" "$fallback"; then
	fail "fallback: vol://system came online from a disk the medium is not paired with"
fi
echo "boot-volume-controllers: the paired volume behind virtio-scsi was refused by name, and nothing else was served as vol://system"

echo "boot-volume-controllers: PASS - the system volume served, written, and read back after a cold reboot through NVMe and through AHCI with lsblk naming each; chosen by its pairing over a decoy; refused by name where no driver bound before it reaches it"
