#!/usr/bin/env bash
# qemu-riscv64.sh - boot the riscv64 kernel under `qemu-system-riscv64 -machine virt`
# over OpenSBI (M117 bring-up).
#
# QEMU loads OpenSBI (the -bios firmware, M-mode) and the kernel ELF (-kernel);
# OpenSBI's fw_dynamic jumps to the kernel entry `_start` in S-mode with a0=hartid,
# a1=DTB. This is the minimal direct-boot runner; disks + virtio devices are added in
# later increments (the aarch64 runner `qemu-aarch64.sh` is the template).

set -euo pipefail

KERNEL="${1:?usage: qemu-riscv64.sh <kernel-elf>}"
SERIAL="${SERIAL:-mon:stdio}"
SMP="${SMP:-1}"
MEM="${MEM:-512M}"
# `default` uses QEMU's bundled OpenSBI (fw_dynamic); override with BIOS=<path>.
BIOS="${BIOS:-default}"
# UEFI=1 boots through the own riscv64 UEFI loader under U-Boot instead of QEMU's direct
# `-kernel` load. UBOOT is the S-mode U-Boot payload QEMU runs (via -kernel) on top of
# OpenSBI; its EFI boot manager launches the loader off the FAT ESP.
UEFI="${UEFI:-0}"
UBOOT="${UBOOT:-/usr/lib/u-boot/qemu-riscv64_smode/u-boot.bin}"

# System volume disk: a virtio-blk disk holding the packed volume archive at LBA 0.
# StorageService reads that factory archive, formats a LiberFS past it, and seeds
# vol://system - so the userspace boot chain reaches the pinned service set. The kernel
# build.rs writes volume-riscv64.pkg into this script's .build directory. The userspace
# virtio-blk driver polls the disk (no interrupt needed), so this works over the PLIC
# without the per-device interrupt path. Attach only when the volume package exists.
HERE="$(cd "$(dirname "$0")" && pwd)"
VOLUME_PKG="$HERE/.build/volume-riscv64.pkg"
VIRTIO_DISK="$HERE/.build/virtio-blk-riscv64.img"
DISK_ARGS=()
if [[ -f "$VOLUME_PKG" ]]; then
	VIRTIO_DISK_SIZE=$((128 * 1024 * 1024))
	if [[ ! -f "$VIRTIO_DISK" || "$(stat -c%s "$VIRTIO_DISK")" -ne "$VIRTIO_DISK_SIZE" ]]; then
		rm -f "$VIRTIO_DISK"
		truncate -s "$VIRTIO_DISK_SIZE" "$VIRTIO_DISK"
	fi
	dd if="$VOLUME_PKG" of="$VIRTIO_DISK" bs=512 conv=notrunc status=none
	DISK_ARGS=(-drive "if=none,id=vol0,format=raw,file=$VIRTIO_DISK" -device "virtio-blk-pci,drive=vol0")
fi

# Media volumes for the extra StorageService instances (vol://media|iso|udf|usb) - the
# same set the aarch64/x86 runners build, so the boot chain (the shell hard-depends on
# media/iso/udf/usb storage) reaches the shell on riscv64 too. Each is a real filesystem
# image seeded from the shared volume/ dir, built once and reused (so only the first run
# pays the mkfs cost); the virtio-blk drivers poll, so these need no interrupt path.
# Skipped if its mkfs toolchain is missing. Attached in every run (including the TEST
# topology), so the boot-chain test's five StorageService instances all come up.
VOLDIR="$HERE/../volume"
# exFAT media disk (vol://media), read-write.
FAT_DISK="$HERE/.build/fat-media-riscv64.img"
if [[ ! -f "$FAT_DISK" ]] && command -v mkfs.exfat >/dev/null; then
	truncate -s 16M "$FAT_DISK"
	if mkfs.exfat "$FAT_DISK" >/dev/null 2>&1; then
		FMNT="$HERE/.build/media-mnt-rv64"
		mkdir -p "$FMNT"
		if mount -o loop "$FAT_DISK" "$FMNT" 2>/dev/null; then
			cp "$VOLDIR/hello.txt" "$VOLDIR/motd.txt" "$FMNT"/ 2>/dev/null || true
			umount "$FMNT" 2>/dev/null || true
		fi
		rmdir "$FMNT" 2>/dev/null || true
	else
		rm -f "$FAT_DISK"
	fi
fi
[[ -f "$FAT_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=med0,format=raw,file=$FAT_DISK" -device "virtio-blk-pci,drive=med0")
# ISO9660 disk (vol://iso), read-only.
ISO_DISK="$HERE/.build/iso-media-riscv64.iso"
if [[ ! -f "$ISO_DISK" ]] && command -v xorriso >/dev/null; then
	xorriso -as mkisofs -quiet -J -R -o "$ISO_DISK" "$VOLDIR" 2>/dev/null || true
fi
[[ -f "$ISO_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=iso0,format=raw,file=$ISO_DISK" -device "virtio-blk-pci,drive=iso0")
# UDF disk (vol://udf), read-only.
UDF_DISK="$HERE/.build/udf-media-riscv64.udf"
if [[ ! -f "$UDF_DISK" ]] && command -v mkfs.udf >/dev/null; then
	dd if=/dev/zero of="$UDF_DISK" bs=1M count=8 status=none 2>/dev/null || true
	if mkfs.udf --media-type=hd --blocksize=2048 "$UDF_DISK" >/dev/null 2>&1; then
		UMNT="$HERE/.build/udf-mnt-rv64"
		mkdir -p "$UMNT"
		if mount -o loop "$UDF_DISK" "$UMNT" 2>/dev/null; then
			cp "$VOLDIR"/* "$UMNT"/ 2>/dev/null || true
			umount "$UMNT" 2>/dev/null || true
		fi
		rmdir "$UMNT" 2>/dev/null || true
	else
		rm -f "$UDF_DISK"
	fi
fi
[[ -f "$UDF_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=udf0,format=raw,file=$UDF_DISK" -device "virtio-blk-pci,drive=udf0")

# A virtio-net NIC on user networking, so DeviceManager brings up the virtio_net driver
# and NetworkService comes online. TimeService depends on it and the shell transitively
# on both. On QEMU virt the NIC signals via wired INTx routed to the PLIC (no MSI), so
# this exercises the per-device PLIC interrupt path. Attached in every run (including the
# TEST topology, which mirrors aarch64's blk/net/usb device set for the device tests).
DISK_ARGS+=(-netdev "user,id=vnet0" -device "virtio-net-pci,netdev=vnet0")

# xHCI USB host controller + a hub with a keyboard, tablet, and a FAT mass-storage stick
# backing vol://usb (seeded from volume/ when mtools is present) - the same USB set the
# aarch64/x86 runners attach; the device-table tests expect the xHCI controller present.
USB_DISK="$HERE/.build/usb-media-riscv64.img"
if [[ ! -f "$USB_DISK" ]]; then
	truncate -s 16M "$USB_DISK"
	if command -v mformat >/dev/null && command -v mcopy >/dev/null; then
		mformat -i "$USB_DISK" -F ::
		mcopy -i "$USB_DISK" "$VOLDIR/hello.txt" ::hello.txt 2>/dev/null || true
		mcopy -i "$USB_DISK" "$VOLDIR/motd.txt" ::motd.txt 2>/dev/null || true
	fi
fi
DISK_ARGS+=(
	-device "qemu-xhci,id=usb"
	-device "usb-hub,bus=usb.0,port=1"
	-device "usb-kbd,bus=usb.0,port=1.1"
	-device "usb-tablet,bus=usb.0,port=1.2"
	-drive "if=none,id=vusb,format=raw,file=$USB_DISK"
	-device "usb-storage,bus=usb.0,drive=vusb,id=usbstick"
)

# Test mode (TEST=1, the `cargo test` runner): enable RISC-V semihosting so the in-kernel
# harness can terminate QEMU with a pass/fail exit code (arch::exit_qemu -> SYS_EXIT), and
# route the serial console to stdout so the test report is captured. The kernel built with
# `cargo test` branches to the harness after core bring-up (no shell).
TEST_ARGS=()
if [[ "${TEST:-0}" == "1" ]]; then
	TEST_ARGS+=(-semihosting)
	SERIAL="stdio"
fi

if [[ "$UEFI" == "1" ]]; then
	# Boot through the own UEFI loader under U-Boot. Build a FAT EFI System Partition
	# holding the loader as /EFI/BOOT/BOOTRISCV64.EFI (the riscv64 UEFI default boot
	# path) and the kernel at /kernel (the loader reads it off the volume it booted
	# from), then attach it as a virtio-blk disk. QEMU runs OpenSBI (-bios default) and
	# the S-mode U-Boot (-kernel), whose EFI boot manager scans the disk and launches
	# the loader; the loader hands the kernel the DTB and boot hart id.
	LOADER_EFI="${LOADER_EFI:-$HERE/../loader/target/riscv64gc-unknown-none-elf/debug/libersystem-loader.efi}"
	[[ -f "$LOADER_EFI" ]] || {
		echo "qemu-riscv64: loader EFI not found: $LOADER_EFI (run 'just loader-riscv64')" >&2
		exit 1
	}
	[[ -f "$UBOOT" ]] || {
		echo "qemu-riscv64: U-Boot not found: $UBOOT (install the u-boot-qemu package)" >&2
		exit 1
	}
	ESP="$HERE/.build/esp-riscv64.img"
	STAGED_KERNEL="$HERE/.build/kernel-riscv64.stripped"
	llvm-strip --strip-debug -o "$STAGED_KERNEL" "$KERNEL" 2>/dev/null || cp "$KERNEL" "$STAGED_KERNEL"
	ESP_MB=$((($(stat -c%s "$STAGED_KERNEL") + $(stat -c%s "$LOADER_EFI")) / 1048576 + 16))
	rm -f "$ESP"
	truncate -s "${ESP_MB}M" "$ESP"
	mformat -i "$ESP" ::
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT
	mcopy -i "$ESP" "$LOADER_EFI" ::/EFI/BOOT/BOOTRISCV64.EFI
	mcopy -i "$ESP" "$STAGED_KERNEL" ::/kernel
	# The ESP is an NVMe disk, not virtio-blk: U-Boot's default boot order is
	# "nvme0 virtio0 virtio1 scsi0 dhcp", so nvme0 is tried first and reliably found
	# regardless of how many virtio-blk volumes precede it. The kernel has no NVMe
	# driver, so DeviceManager skips it - the virtio-blk system/media/iso/udf volumes
	# keep their PCI enumeration order (and StorageService binds them, not the ESP).
	DISK_ARGS+=(-drive "if=none,id=esp,format=raw,file=$ESP" -device "nvme,serial=libersystem-esp,drive=esp")
	exec qemu-system-riscv64 \
		-machine virt \
		-cpu rv64 \
		-smp "$SMP" \
		-m "$MEM" \
		-bios "$BIOS" \
		-kernel "$UBOOT" \
		-serial "$SERIAL" \
		-display none \
		-no-reboot \
		"${DISK_ARGS[@]}" \
		"${TEST_ARGS[@]}" \
		${QEMU_EXTRA:-}
fi

exec qemu-system-riscv64 \
	-machine virt \
	-cpu rv64 \
	-smp "$SMP" \
	-m "$MEM" \
	-bios "$BIOS" \
	-kernel "$KERNEL" \
	-serial "$SERIAL" \
	-display none \
	-no-reboot \
	"${DISK_ARGS[@]}" \
	"${TEST_ARGS[@]}" \
	${QEMU_EXTRA:-}
