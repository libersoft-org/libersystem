#!/usr/bin/env bash
# Shared QEMU-runner plumbing. Architecture launchers own firmware, machine/CPU,
# boot protocol, interrupt model, test exit handling and final device arguments.

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
