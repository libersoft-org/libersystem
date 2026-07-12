#!/usr/bin/env bash
# Boot the aarch64 kernel under QEMU's `virt` machine (M116 bring-up).
#
# Used as the cargo runner for the aarch64-unknown-none target and standalone.
# The kernel is a low-linked static ELF; QEMU `-kernel` loads it into DRAM at
# 0x4008_0000 with the MMU off and enters `_start`. QEMU's ELF path does not hand
# the kernel a device tree, so we dump the machine's generated DTB and load it at
# a fixed address (DTB_ADDR, which the kernel scans for). Serial (PL011) is the
# console. Headless by default; SERIAL= overrides the serial sink.
set -euo pipefail

KERNEL="${1:?usage: qemu-aarch64.sh <kernel-elf>}"
SERIAL="${SERIAL:-mon:stdio}"
# Match the guest's core count to the host's (like the x86 runner), capped at 8: the
# GICv2 interrupt controller QEMU's `virt` machine uses addresses at most 8 CPU
# interfaces, so more would fail to start. Override with SMP=<n>.
SMP="${SMP:-$(nproc | awk '{print ($1 > 8) ? 8 : $1}')}"
MEM="${MEM:-512M}"
DTB_ADDR="${DTB_ADDR:-0x4A000000}"

# UEFI=1 boots through the own aarch64 UEFI loader under the AAVMF firmware (a PE
# EFI app on a FAT ESP) instead of QEMU's direct `-kernel` load. The loader reads the
# kernel off the ESP, places it at its physical link addresses, and enters the same
# boot stub `-kernel` does - so the rest of the boot is identical.
UEFI="${UEFI:-0}"
AAVMF_CODE="${AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.fd}"
AAVMF_VARS="${AAVMF_VARS:-/usr/share/AAVMF/AAVMF_VARS.fd}"

# Graphical display backends (the same knobs the x86 runner exposes): DISPLAYS is a
# space-separated list of `vnc` and/or `spice` (empty = headless, serial only). VNC
# serves the virtio-gpu framebuffer on VNC_ADDR (default :0 = port 5900); SPICE serves
# it plus the audio stream on SPICE_PORT (default 5930). The interactive device set below
# (virtio-gpu/input/sound/serial) is attached whenever this is not a TEST run.
want_vnc=0
want_spice=0
for _d in ${DISPLAYS:-}; do
	case "$_d" in
	vnc) want_vnc=1 ;;
	spice) want_spice=1 ;;
	none | "") ;;
	*) echo "qemu-aarch64: unknown display '$_d' (expected vnc and/or spice)" >&2 && exit 1 ;;
	esac
done
DISPLAY_ARGS=()
if [[ "$want_vnc" == "1" ]]; then
	DISPLAY_ARGS+=(-vnc "${VNC_ADDR:-0.0.0.0:0}")
else
	DISPLAY_ARGS+=(-display none)
fi
[[ "$want_spice" == "1" ]] && DISPLAY_ARGS+=(-spice "port=${SPICE_PORT:-5930},addr=0.0.0.0,disable-ticketing=on")

MACHINE="virt,gic-version=2"
DTB_FILE="$(mktemp /tmp/qemu-virt-XXXXXX.dtb)"
trap 'rm -f "$DTB_FILE"' EXIT

# System volume disk: a virtio-blk disk holding the packed volume archive at LBA 0.
# StorageService reads that factory archive from LBA 0, formats a LiberFS past it,
# and seeds vol://system - so the userspace boot chain can reach the shell. The
# kernel build.rs writes volume-aarch64.pkg into this script's .build directory.
HERE="$(cd "$(dirname "$0")" && pwd)"
VOLUME_PKG="$HERE/.build/volume-aarch64.pkg"
VIRTIO_DISK="$HERE/.build/virtio-blk-aarch64.img"
DISK_ARGS=()
if [[ -f "$VOLUME_PKG" ]]; then
	VIRTIO_DISK_SIZE=$((128 * 1024 * 1024))
	# Recreate the disk when missing, the wrong size, or OLDER than the freshly staged
	# volume archive: a LiberFS formatted by an earlier boot leaves its backup GPT
	# header at the END of the disk, so StorageService would mount the old filesystem
	# (with the old staged binaries) instead of reseeding from the new archive.
	if [[ ! -f "$VIRTIO_DISK" || "$(stat -c%s "$VIRTIO_DISK")" -ne "$VIRTIO_DISK_SIZE" || ("$VOLUME_PKG" -nt "$VIRTIO_DISK") ]]; then
		rm -f "$VIRTIO_DISK"
		truncate -s "$VIRTIO_DISK_SIZE" "$VIRTIO_DISK"
	fi
	# Re-lay the factory archive at LBA 0 every boot (conv=notrunc keeps the disk at
	# its full size); StorageService reformats and reseeds from it.
	dd if="$VOLUME_PKG" of="$VIRTIO_DISK" bs=512 conv=notrunc status=none
	DISK_ARGS=(-drive "if=none,id=vol0,format=raw,file=$VIRTIO_DISK" -device "virtio-blk-pci,drive=vol0,disable-legacy=on")
fi

# Media volumes for the storage StorageService instances - the same set the x86 runner
# builds, so the boot chain (shell hard-depends on media/iso/udf storage) reaches the
# shell here too. Each is a genuine filesystem image seeded from the volume/ dir; built
# once and reused. Skipped if its mkfs toolchain is missing (that instance then fails,
# like x86 without the tool). Files come from the shared volume/ seed directory.
VOLDIR="$HERE/../volume"
# exFAT media disk (vol://media), read-write.
FAT_DISK="$HERE/.build/fat-media-aarch64.img"
if [[ ! -f "$FAT_DISK" ]] && command -v mkfs.exfat >/dev/null; then
	truncate -s 16M "$FAT_DISK"
	if mkfs.exfat "$FAT_DISK" >/dev/null 2>&1; then
		FMNT="$HERE/.build/media-mnt-a64"
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
[[ -f "$FAT_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=med0,format=raw,file=$FAT_DISK" -device "virtio-blk-pci,drive=med0,disable-legacy=on")
# ISO9660 disk (vol://iso), read-only.
ISO_DISK="$HERE/.build/iso-media-aarch64.iso"
if [[ ! -f "$ISO_DISK" ]] && command -v xorriso >/dev/null; then
	xorriso -as mkisofs -quiet -J -R -o "$ISO_DISK" "$VOLDIR" 2>/dev/null || true
fi
[[ -f "$ISO_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=iso0,format=raw,file=$ISO_DISK" -device "virtio-blk-pci,drive=iso0,disable-legacy=on")
# UDF disk (vol://udf), read-only.
UDF_DISK="$HERE/.build/udf-media-aarch64.udf"
if [[ ! -f "$UDF_DISK" ]] && command -v mkfs.udf >/dev/null; then
	dd if=/dev/zero of="$UDF_DISK" bs=1M count=8 status=none 2>/dev/null || true
	if mkfs.udf --media-type=hd --blocksize=2048 "$UDF_DISK" >/dev/null 2>&1; then
		UMNT="$HERE/.build/udf-mnt-a64"
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
[[ -f "$UDF_DISK" ]] && DISK_ARGS+=(-drive "if=none,id=udf0,format=raw,file=$UDF_DISK" -device "virtio-blk-pci,drive=udf0,disable-legacy=on")

# A virtio-net NIC on user networking, so DeviceManager brings up the virtio_net driver
# (its own per-device MSI-X vector via the GICv2m frame) and NetworkService comes
# online - the same device the x86 runner attaches. TimeService depends on it, and the
# shell transitively on both.
DISK_ARGS+=(-netdev "user,id=vnet0" -device "virtio-net-pci,netdev=vnet0,disable-legacy=on")

# xHCI USB host controller + devices (the same set the x86 runner attaches): the kernel
# discovers the controller by class (0x0c/0x03/0x30 -> scan_xhci), the userspace xhci
# driver maps it and runs the USB stack over its own MSI-X vector. A hub on port 1
# carries a USB keyboard + tablet (HID enumeration), and a USB mass-storage stick backs
# vol://usb - seeded as FAT with the volume/ files when mtools is present, so vol://usb
# mounts with content like x86.
USB_DISK="$HERE/.build/usb-media-aarch64.img"
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

# Interactive-only devices, the same graphical/input/audio set the x86 runner attaches,
# left out of the deterministic TEST device set (which boots only blk/net/usb) so the
# test topology stays fixed:
#   - ramfb: a simple framebuffer the kernel programs over fw-cfg and draws the boot log
#     into pixel-by-pixel (fb_present=1), then ConsoleService renders the shell onto the
#     same framebuffer (SYS_FRAMEBUFFER_MAP) - one unified display, boot-log to shell,
#     like x86. QEMU's `virt` has no VGA, and QEMU 10's virtio-gpu carries no combined
#     ramfb, so ramfb (not virtio-gpu) is the display here; runtime resize (a virtio-gpu
#     feature) is traded for the kernel-drawn early framebuffer.
#   - virtio-keyboard + virtio-tablet: virtio_input keyboard + pointer, so InputService
#     gets keystrokes and absolute pointer events on the graphical display.
#   - virtio-sound: the audio device the `beep` command drives; its audiodev is the SPICE
#     playback stream when SPICE is up, else a null sink.
#   - virtio-serial + virtconsole: mirrors a second console to a file, matching x86.
if [[ "${TEST:-0}" != "1" ]]; then
	VCON_OUT="$HERE/.build/virtio-console-aarch64.out"
	DISK_ARGS+=(
		-device "ramfb"
		-device "virtio-keyboard-pci,disable-legacy=on"
		-device "virtio-tablet-pci,disable-legacy=on"
		-device "virtio-serial-pci,disable-legacy=on"
		-device "virtconsole,chardev=vcon"
		-chardev "file,id=vcon,path=$VCON_OUT"
	)
	if [[ "$want_spice" == "1" ]]; then
		DISK_ARGS+=(-audiodev "spice,id=snd0")
	else
		DISK_ARGS+=(-audiodev "none,id=snd0")
	fi
	DISK_ARGS+=(-device "virtio-sound-pci,audiodev=snd0")
fi

# Dump the machine's device tree (same machine config as the boot below), then
# boot with it loaded at DTB_ADDR.

# Test mode (TEST=1, the `cargo test` runner): enable Arm semihosting so the in-kernel
# harness can terminate QEMU with a pass/fail exit code (arch::exit_qemu -> SYS_EXIT),
# and route the serial console to stdout so the test report is captured. The kernel
# built with `cargo test` branches to the harness after core bring-up (no shell).
TEST_ARGS=()
if [[ "${TEST:-0}" == "1" ]]; then
	TEST_ARGS+=(-semihosting)
	SERIAL="stdio"
fi

if [[ "$UEFI" == "1" ]]; then
	# Boot through the own UEFI loader under AAVMF. Build a FAT EFI System Partition
	# holding the loader as /EFI/BOOT/BOOTAA64.EFI (the aarch64 UEFI default boot path)
	# and the kernel at /kernel (the loader reads it off the volume it booted from),
	# then attach it as a virtio-blk disk alongside the writable firmware variables. The
	# firmware hands the kernel the DTB, so no dumpdtb / -kernel is used here.
	LOADER_EFI="${LOADER_EFI:-$HERE/../loader/target/aarch64-unknown-uefi/debug/libersystem-loader.efi}"
	[[ -f "$LOADER_EFI" ]] || {
		echo "qemu-aarch64: loader EFI not found: $LOADER_EFI (run 'just loader-aarch64')" >&2
		exit 1
	}
	[[ -f "$AAVMF_CODE" && -f "$AAVMF_VARS" ]] || {
		echo "qemu-aarch64: AAVMF firmware not found ($AAVMF_CODE / $AAVMF_VARS)" >&2
		exit 1
	}
	ESP="$HERE/.build/esp-aarch64.img"
	VARS="$HERE/.build/aavmf-vars.fd"
	STAGED_KERNEL="$HERE/.build/kernel-aarch64.stripped"
	llvm-strip --strip-debug -o "$STAGED_KERNEL" "$KERNEL" 2>/dev/null || cp "$KERNEL" "$STAGED_KERNEL"
	ESP_MB=$((($(stat -c%s "$STAGED_KERNEL") + $(stat -c%s "$LOADER_EFI")) / 1048576 + 16))
	rm -f "$ESP"
	truncate -s "${ESP_MB}M" "$ESP"
	mformat -i "$ESP" ::
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT
	mcopy -i "$ESP" "$LOADER_EFI" ::/EFI/BOOT/BOOTAA64.EFI
	mcopy -i "$ESP" "$STAGED_KERNEL" ::/kernel
	cp "$AAVMF_VARS" "$VARS"
	# The ESP goes LAST so the system volume (added first) enumerates ahead of it and
	# StorageService binds the volume, not this FAT boot filesystem.
	DISK_ARGS+=(-drive "if=none,id=esp,format=raw,file=$ESP" -device "virtio-blk-pci,drive=esp,disable-legacy=on")
	qemu-system-aarch64 \
		-machine "$MACHINE" \
		-cpu cortex-a72 \
		-smp "$SMP" \
		-m "$MEM" \
		-drive "if=pflash,format=raw,file=$AAVMF_CODE,readonly=on" \
		-drive "if=pflash,format=raw,file=$VARS" \
		-serial "$SERIAL" \
		"${DISPLAY_ARGS[@]}" \
		-no-reboot \
		"${TEST_ARGS[@]}" \
		"${DISK_ARGS[@]}" \
		${QEMU_EXTRA:-}
	exit $?
fi

# Direct `-kernel` boot: dump the machine's device tree (same machine config as the
# boot below) and load it at DTB_ADDR for the kernel to scan.
qemu-system-aarch64 \
	-machine "$MACHINE,dumpdtb=$DTB_FILE" \
	-cpu cortex-a72 \
	-smp "$SMP" \
	-m "$MEM" \
	-display none >/dev/null 2>&1

qemu-system-aarch64 \
	-machine "$MACHINE" \
	-cpu cortex-a72 \
	-smp "$SMP" \
	-m "$MEM" \
	-kernel "$KERNEL" \
	-device loader,file="$DTB_FILE",addr="$DTB_ADDR" \
	-serial "$SERIAL" \
	"${DISPLAY_ARGS[@]}" \
	-no-reboot \
	"${TEST_ARGS[@]}" \
	"${DISK_ARGS[@]}" \
	${QEMU_EXTRA:-}
