#!/usr/bin/env bash
# Shared QEMU-runner plumbing. Architecture functions in qemu-run.sh own firmware,
# machine/CPU, boot protocol, interrupt model, test exits and final launch commands.

qemu_parse_displays() {
	local runner="$1"
	want_vnc=0
	want_spice=0
	local display
	for display in ${DISPLAYS:-}; do
		case "$display" in
		vnc) want_vnc=1 ;;
		spice) want_spice=1 ;;
		none | "") ;;
		*)
			echo "$runner: unknown display '$display' (expected vnc and/or spice)" >&2
			return 1
			;;
		esac
	done

	DISPLAY_ARGS=()
	if [[ "$want_vnc" == "1" ]]; then
		DISPLAY_ARGS+=(-vnc "${VNC_ADDR:-0.0.0.0:0}")
	else
		DISPLAY_ARGS+=(-display none)
	fi
	if [[ "$want_spice" == "1" ]]; then
		DISPLAY_ARGS+=(-spice "port=${SPICE_PORT:-5930},addr=0.0.0.0,disable-ticketing=on")
	fi
}

qemu_append_debug_args() {
	local -n arr=$1
	if [[ "${DEBUG:-0}" == "1" ]]; then
		arr+=(-s -S)
		echo "[qemu-run] waiting for GDB on :1234 (run 'just gdb' in another panel)"
	fi
}

# Recreate the system disk when the factory package is newer. Merely overlaying LBA 0
# is insufficient: an older LiberFS backup GPT header at the disk end would remount the
# stale filesystem and stale userspace binaries.
qemu_prepare_system_disk() {
	local volume_pkg="$1"
	local disk="$2"
	local size=$((128 * 1024 * 1024))
	local package_exists=0
	[[ -f "$volume_pkg" ]] && package_exists=1

	if [[ ! -f "$disk" || "$(stat -c%s "$disk")" -ne "$size" || ($package_exists == 1 && "$volume_pkg" -nt "$disk") ]]; then
		rm -f "$disk"
		truncate -s "$size" "$disk"
	fi
	if [[ "$package_exists" == "1" ]]; then
		dd if="$volume_pkg" of="$disk" bs=512 conv=notrunc status=none
		return 0
	fi
	return 1
}

# Build the reusable exFAT/FAT, ISO9660 and UDF images. The caller owns QEMU attachment
# order and transport flags because those are part of each architecture's device model.
qemu_prepare_media_images() {
	local suffix="$1"
	local mount_suffix="$2"
	local udf_mount_options="${3:-loop}"
	local allow_fallbacks="${4:-0}"
	local voldir="$QEMU_BOOT_DIR/../volume"

	FAT_DISK="$QEMU_BUILD_DIR/fat-media${suffix}.img"
	if [[ ! -f "$FAT_DISK" ]] && command -v mkfs.exfat >/dev/null; then
		truncate -s 16M "$FAT_DISK"
		if mkfs.exfat "$FAT_DISK" >/dev/null 2>&1; then
			local fmnt="$QEMU_BUILD_DIR/media-mnt${mount_suffix}"
			mkdir -p "$fmnt"
			if mount -o loop "$FAT_DISK" "$fmnt" 2>/dev/null; then
				cp "$voldir/hello.txt" "$voldir/motd.txt" "$fmnt"/ 2>/dev/null || true
				umount "$fmnt" 2>/dev/null || true
			fi
			rmdir "$fmnt" 2>/dev/null || true
		else
			rm -f "$FAT_DISK"
		fi
	fi
	if [[ "$allow_fallbacks" == "1" && ! -f "$FAT_DISK" ]] && command -v mformat >/dev/null && command -v mcopy >/dev/null; then
		truncate -s 16M "$FAT_DISK"
		mformat -i "$FAT_DISK" -F ::
		mcopy -i "$FAT_DISK" "$voldir/hello.txt" ::hello.txt
		mcopy -i "$FAT_DISK" "$voldir/motd.txt" ::motd.txt
	fi

	ISO_DISK="$QEMU_BUILD_DIR/iso-media${suffix}.iso"
	if [[ ! -f "$ISO_DISK" ]]; then
		if command -v xorriso >/dev/null; then
			xorriso -as mkisofs -quiet -J -R -o "$ISO_DISK" "$voldir" 2>/dev/null || true
		elif [[ "$allow_fallbacks" == "1" ]] && command -v genisoimage >/dev/null; then
			genisoimage -quiet -J -R -o "$ISO_DISK" "$voldir" 2>/dev/null || true
		fi
	fi

	UDF_DISK="$QEMU_BUILD_DIR/udf-media${suffix}.udf"
	if [[ ! -f "$UDF_DISK" ]] && command -v mkfs.udf >/dev/null; then
		dd if=/dev/zero of="$UDF_DISK" bs=1M count=8 status=none 2>/dev/null || true
		if mkfs.udf --media-type=hd --blocksize=2048 "$UDF_DISK" >/dev/null 2>&1; then
			local umnt="$QEMU_BUILD_DIR/udf-mnt${mount_suffix}"
			mkdir -p "$umnt"
			if mount -o "$udf_mount_options" "$UDF_DISK" "$umnt" 2>/dev/null; then
				cp "$voldir"/* "$umnt"/ 2>/dev/null || true
				umount "$umnt" 2>/dev/null || true
			fi
			rmdir "$umnt" 2>/dev/null || true
		else
			rm -f "$UDF_DISK"
		fi
	fi
}

qemu_prepare_usb_image() {
	local suffix="$1"
	local voldir="$QEMU_BOOT_DIR/../volume"
	USB_DISK="$QEMU_BUILD_DIR/usb-media${suffix}.img"
	if [[ -f "$USB_DISK" ]]; then
		return
	fi
	truncate -s 16M "$USB_DISK"
	if command -v mformat >/dev/null && command -v mcopy >/dev/null; then
		mformat -i "$USB_DISK" -F ::
		mcopy -i "$USB_DISK" "$voldir/hello.txt" ::hello.txt 2>/dev/null || true
		mcopy -i "$USB_DISK" "$voldir/motd.txt" ::motd.txt 2>/dev/null || true
	fi
}

# Append virtio-blk device to a nameref array. Accepts drive/device IDs and
# optional disable-legacy flag.
qemu_attach_virtio_blk() {
	local -n arr=$1
	local file="$2"
	local drive_id="$3"
	local legacy="${4:-}"
	arr+=(-drive "file=$file,if=none,id=$drive_id,format=raw")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-blk-pci,drive=$drive_id,$legacy")
	else
		arr+=(-device "virtio-blk-pci,drive=$drive_id")
	fi
}

# Append virtio-net device to a nameref array with optional hostfwd and legacy flag.
qemu_attach_virtio_net() {
	local -n arr=$1
	local net_id="$2"
	local hostfwd="${3:-}"
	local legacy="${4:-}"
	local net_user="user,id=$net_id"
	[[ -n "$hostfwd" ]] && net_user="$net_user,$hostfwd"
	arr+=(-netdev "$net_user")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-net-pci,netdev=$net_id,$legacy")
	else
		arr+=(-device "virtio-net-pci,netdev=$net_id")
	fi
}

# Append xHCI USB hub/keyboard/tablet topology to a nameref array.
# Optional USB storage drive_id attaches a mass-storage device on port 1.3.
qemu_attach_xhci() {
	local -n arr=$1
	local usb_drive_id="${2:-}"
	arr+=(
		-device "qemu-xhci,id=usb"
		-device "usb-hub,bus=usb.0,port=1"
		-device "usb-kbd,bus=usb.0,port=1.1"
		-device "usb-tablet,bus=usb.0,port=1.2"
	)
	if [[ -n "$usb_drive_id" ]]; then
		arr+=(-device "usb-storage,bus=usb.0,drive=$usb_drive_id,id=usbstick")
	fi
}

# Append interactive ramfb/virtio-input/sound/virtconsole block for virt machines.
# Suffix is for console output filename, legacy is optional disable-legacy flag.
qemu_attach_virt_interactive() {
	local -n arr=$1
	local suffix="$2"
	local legacy="${3:-}"
	local vcon_out="$QEMU_BUILD_DIR/virtio-console${suffix}.out"
	arr+=(-device "ramfb")
	if [[ -n "$legacy" ]]; then
		arr+=(
			-device "virtio-keyboard-pci,$legacy"
			-device "virtio-tablet-pci,$legacy"
			-device "virtio-serial-pci,$legacy"
		)
	else
		arr+=(
			-device "virtio-keyboard-pci"
			-device "virtio-tablet-pci"
			-device "virtio-serial-pci"
		)
	fi
	arr+=(
		-device "virtconsole,chardev=vcon"
		-chardev "file,id=vcon,path=$vcon_out"
	)
	if [[ "$want_spice" == "1" ]]; then
		arr+=(-audiodev "spice,id=snd0")
	else
		arr+=(-audiodev "none,id=snd0")
	fi
	arr+=(-device "virtio-sound-pci,audiodev=snd0")
}

# Build a FAT EFI System Partition for ARM/RISC-V UEFI boot: strip kernel,
# create FAT, install loader as BOOT*.EFI and kernel at /kernel.
# Returns ESP path in ESP, stripped kernel in STAGED_KERNEL.
qemu_build_esp() {
	local arch="$1"
	local kernel="$2"
	local loader_efi="$3"
	local boot_name="$4"
	ESP="$QEMU_BUILD_DIR/esp-${arch}.img"
	STAGED_KERNEL="$QEMU_BUILD_DIR/kernel-${arch}.stripped"
	llvm-strip --strip-debug -o "$STAGED_KERNEL" "$kernel" 2>/dev/null || cp "$kernel" "$STAGED_KERNEL"
	local esp_mb=$((($(stat -c%s "$STAGED_KERNEL") + $(stat -c%s "$loader_efi")) / 1048576 + 16))
	rm -f "$ESP"
	truncate -s "${esp_mb}M" "$ESP"
	mformat -i "$ESP" ::
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT
	mcopy -i "$ESP" "$loader_efi" "::/EFI/BOOT/$boot_name"
	mcopy -i "$ESP" "$STAGED_KERNEL" ::/kernel
}
