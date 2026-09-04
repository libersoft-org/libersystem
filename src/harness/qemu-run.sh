#!/usr/bin/env bash
# Unified QEMU runner for all architectures: x86_64, aarch64, riscv64.
#
# Usage: qemu-run.sh [x86_64|aarch64|riscv64] [kernel-elf]
# No arguments: detect native arch from uname -m and use default kernel path.
# First arg matching an arch selects it; optional second arg overrides kernel ELF.
# First arg not matching an arch is treated as kernel ELF (backward-compatible).
#
# Environment variables (preserved across all architectures):
#   DEBUG=1   QEMU waits for GDB (-s -S) on port :1234
#   NOKVM=1   disable KVM (more reliable single-stepping under TCG)
#   TEST=1    test mode (isa-debug-exit or semihosting, maps exit code to pass/fail)
#   LIBER_BOOT_PROFILE  which profile name goes over fw_cfg with DEV_PROFILE=1. `development` is the
#                       interactive instance a PERSON boots; `development-trace` is what
#                       `perf-trace.py` boots, and it is the only one the kernel emits its raw
#                       `\x1ePERF` anchor on - a line addressed to a tool, shown only where one is.
#   DEV_PROFILE=1 development profile: names it over fw_cfg so the guest reports it and
#             DeviceManager starts a control agent, and attaches the channel the agent
#             answers on. On x86_64 that is the persistent instance `./dev.sh up` owns; on
#             the other targets it is a one-shot guest a scenario runner drives cold, which
#             is why this is not x86_64-only. Refused together with TEST.
#   SERIAL=   QEMU serial backend (default mon:stdio; e.g. file:boot.log or stdio)
#   SMP=N     override core/hart count (default: nproc, with arch-specific caps)
#   MEM=      override RAM (default varies by arch)
#   STRIP=    none | debug | all for a harness-created boot medium (default: all)
#   DISPLAYS= space-separated list of vnc and/or spice (empty = headless)
#   VNC_ADDR= VNC bind address and display (default 0.0.0.0:0 - every interface, unauthenticated)
#   SPICE_PORT= SPICE TCP port (default 5930)
#   SPICE_ADDR= SPICE bind address (default 0.0.0.0 - every interface, unauthenticated; see below)
#   SPICE_ADDR= SPICE bind address (default 127.0.0.1)
#   AUDIO_WAV= capture virtio-sound output to this WAV file (overrides spice/none)
#   QEMU_EXTRA= extra QEMU arguments
#   DAMAGE_SIGNED_MANIFEST=1
#             aarch64/riscv64 UEFI: flip one byte of the signed manifest on the ESP this script
#             assembles, so a gate can prove the loader refuses a tampered one on the two ports that
#             have no shipping ISO to tamper with. Used by `check-signed-boot.sh` and nothing else.
#   USB_HOST= vendorid:productid for USB passthrough (x86_64 interactive only)
#   UEFI=1    boot through own UEFI loader (aarch64/riscv64 only)
#   GIC=      aarch64: which interrupt controller the machine has - 2 (default: GICv2 with a
#             GICv2m MSI frame), 3 (GICv3, ITS off: the timer/IPI core profile) or 3its (GICv3
#             with the ITS enabled). A NAMED PROFILE RATHER THAN A QEMU_EXTRA RECIPE, because
#             which controller a boot exercised is the whole claim a discovery result makes.
#   EL2=1     aarch64: start the guest at EL2 (`virtualization=on`), which is where the UEFI
#             specification puts AArch64 firmware on most server-class parts - and which the
#             loader's EL2 branch had never once executed under, because QEMU's `virt` starts at
#             EL1 by default. Only meaningful with UEFI=1: the branch is in the loader.
#   INIT_PKG= boot-module archive for a direct (non-UEFI) aarch64/riscv64 boot
#             (default .build/boot/init-<arch>.pkg)
#   OVMF_*, AAVMF_*, BIOS=, UBOOT=, LOADER_EFI=, DTB_ADDR=, MODULES_ADDR= arch-specific firmware

set -euo pipefail

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
		# EVERY INTERFACE, DELIBERATELY - the same decision as SPICE below, for the same reason: a
		# console bound to loopback is reachable only from the host, which is the one place the
		# person running this usually is not. `:0` is the DISPLAY number, so this is port 5900.
		#
		# AND WITH NOTHING GUARDING IT. No password is set here and VNC is plaintext, so anyone who
		# can reach the port has this guest's screen and keyboard. `VNC_ADDR=127.0.0.1:0` puts it
		# back on loopback, which with an SSH tunnel is the arrangement that needs no keys.
		DISPLAY_ARGS+=(-vnc "${VNC_ADDR:-0.0.0.0:0}")
		echo "qemu-run: VNC console on ${VNC_ADDR:-0.0.0.0:0} (port 5900 + display) - NO PASSWORD, NO TLS: anyone who can reach that port has this guest's console" >&2
	else
		DISPLAY_ARGS+=(-display none)
	fi
	if [[ "$want_spice" == "1" ]]; then
		# EVERY INTERFACE, DELIBERATELY, AND WITH NOTHING GUARDING IT.
		#
		# A SPICE console bound to loopback is unreachable from anywhere but the host itself, which
		# is the one place a developer running this rarely is: the guest runs on a machine somewhere
		# and the person is not sitting at it. So the default binds `0.0.0.0` and the runner SAYS SO
		# on every start, rather than being quietly useless.
		#
		# WHAT THAT MEANS RIGHT NOW: `disable-ticketing=on` is no password, and this is plain TCP -
		# so anyone who can reach the port has the guest's console, keyboard and pointer. That is
		# acceptable only on a network where that is already true of the host. `SPICE_ADDR=127.0.0.1`
		# puts it back on loopback, and a password and TLS are the two things that would make the
		# open bind safe rather than merely convenient.
		DISPLAY_ARGS+=(-spice "port=${SPICE_PORT:-5930},addr=${SPICE_ADDR:-0.0.0.0},disable-ticketing=on")
		echo "qemu-run: SPICE console on ${SPICE_ADDR:-0.0.0.0}:${SPICE_PORT:-5930} - NO PASSWORD, NO TLS: anyone who can reach that port has this guest's console" >&2
	fi
}

# The nameref is deliberately not called `arr`: this is called from helpers whose own array
# nameref is, and bash refuses a nameref that points at itself - it warns and appends nothing,
# which left the sound card on the command line with the audio backend it names missing, and
# QEMU refuses to start on that. An interactive aarch64 or riscv64 boot could not come up.
qemu_append_audio() {
	local -n audio_args="$1"
	if [[ -n "${AUDIO_WAV:-}" ]]; then
		audio_args+=(-audiodev "wav,id=snd0,path=$AUDIO_WAV")
	elif [[ "$want_spice" == "1" ]]; then
		audio_args+=(-audiodev "spice,id=snd0")
	else
		audio_args+=(-audiodev "none,id=snd0")
	fi
}

qemu_append_debug_args() {
	local -n arr=$1
	if [[ "${DEBUG:-0}" == "1" ]]; then
		arr+=(-s -S)
		echo "[qemu-run] waiting for GDB on :1234 (run './run.sh --gdb' in another panel)"
	fi
}

# Recreate the system disk when the factory package is newer. Merely overlaying LBA 0
# is insufficient: an older LiberFS backup GPT header at the disk end would remount the
# stale filesystem and stale userspace binaries.
# Lay the system volume onto the disk QEMU attaches.
#
# This used to copy `volume.pkg` - a factory ARCHIVE - to LBA 0, and the storage service formatted
# a filesystem 32 MiB further in and seeded it from that archive on every boot. The volume is now
# built as a real LiberFS image, so the disk carries the volume itself: the same bytes the
# loader reads and the storage service mounts, in one place rather than two.
#
# The image is copied rather than attached directly because a guest writes to its system volume,
# and a test run must not edit the build output it was given.
qemu_prepare_system_disk() {
	local volume_image="$1"
	local disk="$2"
	[[ -f "$volume_image" ]] || return 1

	# Larger than the image so the volume has room to grow; the storage service reports a volume
	# that spans less than its container rather than silently resizing it.
	local size=$((128 * 1024 * 1024))
	# KEYED ON THE VOLUME'S CONTENT, not on its modification time.
	#
	# The test was `existence, exact size, and source -nt destination`. An mtime is not a content
	# identity: a restored file, a checkout, a copy that preserved timestamps, or any build that
	# writes an older stamp leaves the disk untouched with different bytes behind it. And the copy
	# went straight into the canonical path, so an interrupted `dd` left a half-written system disk
	# that the size check then accepted forever.
	local key
	key="$(sha256sum "$volume_image" | awk '{print $1}')"
	if [[ -f "$disk" && "$(stat -c%s "$disk")" -eq "$size" && -f "$disk.key" && "$(<"$disk.key")" == "$key" ]]; then
		return 0
	fi
	local candidate="$disk.$$.candidate"
	rm -f "$candidate"
	truncate -s "$size" "$candidate"
	dd if="$volume_image" of="$candidate" bs=1M conv=notrunc status=none || {
		rm -f "$candidate"
		echo "qemu-run: the system disk could not be written from $volume_image" >&2
		return 1
	}
	sync "$candidate" 2>/dev/null || true
	rm -f "$disk"
	mv "$candidate" "$disk"
	printf '%s\n' "$key" >"$disk.key.tmp.$$"
	mv "$disk.key.tmp.$$" "$disk.key"
	return 0
}

# THE DISK A GUEST IS GIVEN IS THIS RUN'S ALONE, AND IT COMES FROM A TEMPLATE.
#
# `qemu_prepare_system_disk` above builds the TEMPLATE - keyed on the volume's content, rebuilt when
# that changes, and never handed to a guest. Before this, that file WAS the guest's disk: two guests
# of one architecture in one mode wrote the same image, which is a result that is green and means
# nothing, and a third run inherited whatever the second had left in it. The content key hid it -
# the key describes the SOURCE, so a disk the previous guest had scribbled on still matched.
#
# Copied rather than shared even when nothing appears to write: a fixture that is only read is a
# claim about every test in the suite, and this is not the place to make it.
#
# `$$` is the run identity here as everywhere in this script, and `scratch_sweep` is the cleanup:
# the script `exec`s QEMU, so no exit trap can run, and a copy owned by a pid that is gone is a copy
# nothing will come back for.
qemu_run_disk() {
	local template="$1"
	local run_disk="${template%.img}.$$.img"
	scratch_sweep "${template%.img}" .img
	cp --reflink=auto "$template" "$run_disk" || return 1
	printf '%s\n' "$run_disk"
}

# THE FIXTURE MEDIA, AND WHY THEY ARE KEYED RATHER THAN MERELY PRESENT.
#
# These four images used to be generated only when the output path did not exist. Nothing else was
# consulted: not the files copied into them, not the recipe, not the tool that formatted them. So a
# change to `volume/hello.txt`, to `motd.txt`, to the mkfs options or to this script never
# invalidated anything - the images in this tree were dated three weeks before the script that
# claims to produce them, and every run reused them solely because they were there.
#
# Worse than stale: `xorriso` failure was swallowed with `|| true`, and mount, copy and umount
# failures likewise, AFTER the output file had already been created. A formatted but empty image, or
# a half-written one, was left at the canonical path where the existence check would protect it
# forever - so a filesystem test could exercise an empty filesystem and report it as the guest's
# behaviour, and a fixture-setup failure was indistinguishable from a guest regression.
#
# The rule now: build into a candidate, VERIFY the bytes contain what they were supposed to, then
# rename into place and record the key. A failure leaves the previous valid image or no image, never
# a partial canonical one.
FIXTURE_SOURCES=(hello.txt motd.txt)

# What a fixture medium is keyed on: the recipe, the tool that formats it, and every byte of the
# source directory.
media_key() {
	local kind="$1" voldir="$2"
	shift 2
	{
		printf 'format=liber-qemu-fixture-v1\n'
		printf 'kind=%s\n' "$kind"
		printf 'recipe=%s\n' "$(sha256sum "$QEMU_BOOT_DIR/qemu-run.sh" | awk '{print $1}')"
		local tool version
		for tool in "$@"; do
			# Captured whole, THEN first-lined. `$tool --version | head -n 1` inside a command
			# substitution is the shape `check-source-hygiene.sh` refuses: `head` closes the pipe
			# and `pipefail` turns a successful read into a failed pipeline.
			version="$("$tool" --version 2>&1 || echo unknown)"
			printf 'tool=%s path=%s version=%s\n' "$tool" "$(command -v "$tool" 2>/dev/null || echo absent)" "${version%%$'\n'*}"
		done
		find "$voldir" -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
	} | sha256sum | awk '{print $1}'
}

media_current() {
	local path="$1" key="$2"
	[[ -f "$path" && -f "$path.key" && "$(<"$path.key")" == "$key" ]]
}

# OLD GENERATIONS OF A CONTENT-ADDRESSED FIXTURE, SWEPT.
#
# A name that carries its key grows one file per distinct fixture set and nothing else would ever
# remove them. A file a live guest is READING is never touched: `fuser` answers that, and its absence
# is answered by keeping the file, which is the safe direction for a sweep. Age is the second guard -
# a generation younger than twelve hours may be another run's, whether or not it has it open yet.
media_sweep() {
	local prefix="$1" ext="$2" keep="$3" stale
	for stale in "$prefix"*"$ext"; do
		[[ -f "$stale" && "$stale" != "$keep" ]] || continue
		[[ -n "$(find "$stale" -mmin +720 -print -quit 2>/dev/null)" ]] || continue
		if command -v fuser >/dev/null && fuser -s "$stale" 2>/dev/null; then
			continue
		fi
		rm -f "$stale" "$stale.key"
	done
}

# Publish a verified candidate: rename first, then record the key. In that order, because a key
# written before the rename would describe an image that is not there yet.
media_publish() {
	local candidate="$1" final="$2" key="$3"
	mv "$candidate" "$final"
	printf '%s\n' "$key" >"$final.key.tmp.$$"
	mv "$final.key.tmp.$$" "$final.key"
}

# Does this image actually CONTAIN the fixture files? Verification is per format, because "the
# bytes are in there somewhere" is only readable for the format that stores names as plain ASCII.
#
# What every one of these has to distinguish is "formatted and populated" from "formatted and
# empty" - the second is exactly the state that used to be written to the canonical path and then
# protected by the existence check forever.

# ISO9660 keeps its directory records in ASCII, so the names are in the image as written.
media_iso_holds_fixtures() {
	local image="$1" name
	for name in "${FIXTURE_SOURCES[@]}"; do
		grep -qa "$name" "$image" || return 1
	done
	return 0
}

# A FAT built with mtools is read back with mtools.
media_fat_holds_fixtures() {
	local image="$1" listing name
	listing="$(mdir -i "$image" :: 2>/dev/null)" || return 1
	for name in "${FIXTURE_SOURCES[@]}"; do
		# mtools prints the 8.3 and long names in one listing; the stem is what both share.
		grep -qi "${name%%.*}" <<<"$listing" || return 1
	done
	return 0
}

# Mount, copy, VERIFY WHILE MOUNTED, unmount.
#
# The mounted filesystem is the only reader either exFAT or UDF has here, so the check belongs
# inside the mount rather than after it: names on both are UTF-16 and a raw byte search finds
# nothing even when the copy worked. Each file has to be present and be the size of its source,
# which is what separates a copy that happened from one that was suppressed with `|| true`.
#
# And the UNMOUNT IS CHECKED. It used to be suppressed, which leaves a host loop mount live on an
# image QEMU is about to attach writable from the other side - both halves then writing one
# filesystem, which is the corruption this file is otherwise careful to avoid.
media_populate_mounted() {
	local image="$1" mountpoint="$2" options="$3" voldir="$4"
	mkdir -p "$mountpoint"
	mount -o "$options" "$image" "$mountpoint" 2>/dev/null || {
		rmdir "$mountpoint" 2>/dev/null || true
		return 1
	}
	# THE FIXTURE FILES, named. `cp "$voldir"/*` swept in `audio/`, `bin/` and `wallpapers/` too,
	# and `cp` without `-r` refuses a directory - so the copy reported failure on every run for
	# reasons that had nothing to do with the fixtures, which is part of why its status was being
	# thrown away.
	local status=0 name
	for name in "${FIXTURE_SOURCES[@]}"; do
		cp "$voldir/$name" "$mountpoint/$name" 2>/dev/null || status=1
	done
	for name in "${FIXTURE_SOURCES[@]}"; do
		if [[ ! -f "$mountpoint/$name" ]] || [[ "$(stat -c%s "$mountpoint/$name")" -ne "$(stat -c%s "$voldir/$name")" ]]; then
			status=1
		fi
	done
	sync
	if ! umount "$mountpoint" 2>/dev/null; then
		echo "qemu-run: could not unmount $mountpoint - the host still holds $image and QEMU must not attach it" >&2
		return 2
	fi
	rmdir "$mountpoint" 2>/dev/null || true
	return "$status"
}

# Build the reusable exFAT/FAT, ISO9660 and UDF images. The caller owns QEMU attachment
# order and transport flags because those are part of each architecture's device model.
qemu_prepare_media_images() {
	local suffix="$1"
	# Per RUN, not per target: the mount point is created and removed while an image is populated,
	# and two runs sharing the name means one removes the directory the other is mounted on.
	local mount_suffix="$2.$$"
	local udf_mount_options="${3:-loop}"
	local allow_fallbacks="${4:-0}"
	local voldir="$QEMU_BOOT_DIR/../volume"
	if [[ "${TEST:-0}" == "1" ]] && ! command -v mkfs.udf >/dev/null; then
		echo "qemu-run: mkfs.udf is required for the test UDF fixture (install udftools)" >&2
		exit 1
	fi

	# CONTENT-ADDRESSED FINAL NAMES. The key was already content-derived and was written BESIDE a
	# fixed name, so two runs whose fixtures differ - a different volume directory, a different
	# generation of the same one - wrote and read one path: the second published over the first while
	# the first's guest was reading it. The key is in the NAME now, so two generations coexist and a
	# run reads only its own; the `.key` sidecar stays, because it is what says the file was
	# completely written rather than merely named.
	local fat_key candidate
	fat_key="$(media_key fat "$voldir" mkfs.exfat mformat mcopy)"
	FAT_DISK="$QEMU_BUILD_DIR/fat-media${suffix}.$fat_key.img"
	media_sweep "$QEMU_BUILD_DIR/fat-media${suffix}." .img "$FAT_DISK"
	if ! media_current "$FAT_DISK" "$fat_key"; then
		candidate="$FAT_DISK.$$.candidate"
		rm -f "$candidate"
		if command -v mkfs.exfat >/dev/null; then
			truncate -s 16M "$candidate"
			if mkfs.exfat "$candidate" >/dev/null 2>&1 && media_populate_mounted "$candidate" "$QEMU_BUILD_DIR/media-mnt${mount_suffix}" loop "$voldir"; then
				media_publish "$candidate" "$FAT_DISK" "$fat_key"
			else
				rm -f "$candidate"
			fi
		fi
		if [[ "$allow_fallbacks" == "1" ]] && ! media_current "$FAT_DISK" "$fat_key" && command -v mformat >/dev/null && command -v mcopy >/dev/null; then
			rm -f "$candidate"
			truncate -s 16M "$candidate"
			if mformat -i "$candidate" -F :: && mcopy -i "$candidate" "$voldir/hello.txt" ::hello.txt && mcopy -i "$candidate" "$voldir/motd.txt" ::motd.txt && media_fat_holds_fixtures "$candidate"; then
				media_publish "$candidate" "$FAT_DISK" "$fat_key"
			else
				rm -f "$candidate"
			fi
		fi
	fi

	local iso_key
	iso_key="$(media_key iso "$voldir" xorriso genisoimage)"
	ISO_DISK="$QEMU_BUILD_DIR/iso-media${suffix}.$iso_key.iso"
	media_sweep "$QEMU_BUILD_DIR/iso-media${suffix}." .iso "$ISO_DISK"
	if ! media_current "$ISO_DISK" "$iso_key"; then
		candidate="$ISO_DISK.$$.candidate"
		rm -f "$candidate"
		local made=0
		if command -v xorriso >/dev/null; then
			xorriso -as mkisofs -quiet -J -R -o "$candidate" "$voldir" 2>/dev/null && made=1
		elif [[ "$allow_fallbacks" == "1" ]] && command -v genisoimage >/dev/null; then
			genisoimage -quiet -J -R -o "$candidate" "$voldir" 2>/dev/null && made=1
		fi
		if ((made == 1)) && media_iso_holds_fixtures "$candidate"; then
			media_publish "$candidate" "$ISO_DISK" "$iso_key"
		else
			rm -f "$candidate"
		fi
	fi

	local udf_key
	udf_key="$(media_key udf "$voldir" mkfs.udf)"
	UDF_DISK="$QEMU_BUILD_DIR/udf-media${suffix}.$udf_key.udf"
	media_sweep "$QEMU_BUILD_DIR/udf-media${suffix}." .udf "$UDF_DISK"
	if ! media_current "$UDF_DISK" "$udf_key" && command -v mkfs.udf >/dev/null; then
		candidate="$UDF_DISK.$$.candidate"
		rm -f "$candidate"
		dd if=/dev/zero of="$candidate" bs=1M count=8 status=none
		if mkfs.udf --media-type=hd --blocksize=2048 "$candidate" >/dev/null 2>&1 && media_populate_mounted "$candidate" "$QEMU_BUILD_DIR/udf-mnt${mount_suffix}" "$udf_mount_options" "$voldir"; then
			media_publish "$candidate" "$UDF_DISK" "$udf_key"
		else
			rm -f "$candidate"
			# SAID OUT LOUD, because this one failed silently for a long time.
			#
			# The cause was this file's own mount options: the x86_64 caller passed `loop,ro=0`,
			# and util-linux reads that as the `ro` flag and DISCARDS the `=0`, so the image was
			# mounted read-only and every copy failed with `cp: Read-only file system`. It is
			# `loop` now. Nothing is published on failure, so whatever was there stays.
			echo "qemu-run: the UDF fixture could not be populated (mount or copy failed); ${UDF_DISK##*/} is unchanged and holds no fixture files" >&2
		fi
	fi
}

qemu_prepare_usb_image() {
	local suffix="$1"
	local voldir="$QEMU_BOOT_DIR/../volume"
	USB_DISK="$QEMU_BUILD_DIR/usb-media${suffix}.img"
	local key candidate
	key="$(media_key usb "$voldir" mformat mcopy)"
	media_current "$USB_DISK" "$key" && return
	command -v mformat >/dev/null && command -v mcopy >/dev/null || {
		# A 16 MB file of zeros used to be left here when mtools was absent, and a zeroed image is
		# not a FAT filesystem - the guest reports it as unreadable media, which is a true statement
		# about a fixture that was never built.
		echo "qemu-run: mtools (mformat/mcopy) is required for the USB fixture; none was created" >&2
		return
	}
	candidate="$USB_DISK.$$.candidate"
	rm -f "$candidate"
	truncate -s 16M "$candidate"
	if mformat -i "$candidate" -F :: && mcopy -i "$candidate" "$voldir/hello.txt" ::hello.txt && mcopy -i "$candidate" "$voldir/motd.txt" ::motd.txt && media_fat_holds_fixtures "$candidate"; then
		media_publish "$candidate" "$USB_DISK" "$key"
	else
		rm -f "$candidate"
		echo "qemu-run: the USB fixture could not be built; the previous one (if any) is unchanged" >&2
	fi
}

# The per-device virtio options this machine's endpoints carry.
#
# `iommu_platform=on` is how a device tells the guest "the addresses you hand me are not physical
# ones" - `VIRTIO_F_ACCESS_PLATFORM`. It belongs on every endpoint behind a translating controller
# and on none in front of one: a device without it programs raw physical addresses, and under an
# IOMMU that has left bypass those are somebody else's memory. A driver that does not acknowledge
# the bit gets `FEATURES_OK` refused, which is the loud half; the quiet half is a device that was
# never told, and there is no message for that at all.
#
# One place, so an endpoint cannot be added to this profile without it.
qemu_virtio_opts() {
	local base="${1:-}"
	if [[ "${IOMMU:-0}" == "1" ]]; then
		[[ -n "$base" ]] && base="$base,"
		base="${base}iommu_platform=on"
	fi
	printf '%s' "$base"
}

qemu_attach_virtio_blk() {
	local -n arr=$1
	local file="$2"
	local drive_id="$3"
	local legacy="${4:-}"
	# READ-ONLY WHERE THE CALLER SAYS SO, AND THE FIXTURE MEDIA SAY SO.
	#
	# The FAT, ISO9660 and UDF fixtures were attached writable to every guest that took them, which
	# is what made sharing them unsafe rather than merely untidy: two guests of one architecture had
	# one writable image between them. The system already treats them as read-only volumes - the boot
	# hand-off routes the second, third and fourth disks up as the read-only media - so this makes
	# the attachment agree with what the machine above it already believes, and a test that writes to
	# one now fails loudly instead of leaving the next run a different fixture.
	local readonly_flag="${5:-}"
	if [[ "$readonly_flag" == "readonly" ]]; then
		arr+=(-drive "file=$file,if=none,id=$drive_id,format=raw,readonly=on")
	else
		arr+=(-drive "file=$file,if=none,id=$drive_id,format=raw")
	fi
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-blk-pci,drive=$drive_id,$legacy")
	else
		arr+=(-device "virtio-blk-pci,drive=$drive_id")
	fi
}

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

# The development channel: a SECOND single-port virtio-serial device, not a second port on
# the existing one. The guest driver already binds one console port per device and the
# device manager already binds a driver per device, so this needs no MULTIPORT negotiation,
# no control-queue port discovery and no change to the emergency console. Console traffic
# keeps its own device and stays usable if this one fails.
#
# The address is pinned, and high enough that auto-assignment never lands there, so the
# guest identifies the channel by PCI address rather than by enumeration order. Nothing
# above the port depends on that: the framing is carried by the port itself, so a later
# multiport driver can host the same protocol on a named port unchanged.
DEV_CHANNEL_PCI_SLOT="0x1e"

qemu_attach_dev_channel() {
	local -n arr=$1
	local socket_path="$2"
	local legacy="${3:-}"
	rm -f "$socket_path"
	# QEMU creates the socket under the ordinary umask, and this one carries a protocol that
	# publishes executable code into a running guest. Narrow every file QEMU creates from here
	# on to its owner: on a shared machine the alternative is a control channel anyone logged
	# in can speak to.
	umask 0077
	arr+=(-chardev "socket,id=devchan,path=$socket_path,server=on,wait=off")
	if [[ -n "$legacy" ]]; then
		arr+=(-device "virtio-serial-pci,id=devser,addr=$DEV_CHANNEL_PCI_SLOT,$legacy")
	else
		arr+=(-device "virtio-serial-pci,id=devser,addr=$DEV_CHANNEL_PCI_SLOT")
	fi
	# A console port, not a generic one. Without MULTIPORT there is no control queue to open
	# a generic port with, so a `virtserialport` here never opens and the guest's writes go
	# nowhere - measured, not assumed. The cost is that UEFI firmware writes its console
	# output to every console-class device it enumerates, so on the x86_64 UEFI path the
	# channel carries a firmware preamble before the guest owns the port. Guest console
	# traffic never appears on it, and framing above the port begins at the guest's first
	# byte, so the preamble is skipped rather than negotiated away with MULTIPORT.
	arr+=(-device "virtconsole,chardev=devchan,bus=devser.0")
}

qemu_attach_virt_interactive() {
	local -n arr=$1
	local suffix="$2"
	local legacy="${3:-}"
	# THE GUEST'S OWN OUTPUT, SO IT IS THIS RUN'S OWN FILE. It was one name per mode, which two
	# guests of one architecture shared - and a capture two guests write is a capture that describes
	# neither. Nothing outside this script looks it up by name, so per-run costs nothing; the sweep
	# below removes the ones whose run is gone.
	local vcon_out="$QEMU_BUILD_DIR/virtio-console${suffix}.$$.out"
	scratch_sweep "$QEMU_BUILD_DIR/virtio-console${suffix}" .out
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
	qemu_append_audio arr
	arr+=(-device "virtio-sound-pci,audiodev=snd0")
}

# PER-RUN SCRATCH, SWEPT BY OWNERSHIP.
#
# The mutable boot inputs - the ESP, the stripped kernel, the firmware variable store - were fixed
# per-architecture paths, deleted and repopulated in place with no lock. Two UEFI runs could
# therefore interleave: one deleted and reformatted the ESP while the other was populating it or
# about to attach it, so a guest could boot a partially written filesystem or the other run's
# kernel. The firmware store was worse - each run deleted ALL `ovmf-vars.*.fd` before making its
# own, so a run that had just created its copy, and had not yet opened it, found it gone.
#
# Every one of them now carries this shell's pid, and this shell is the process that BECOMES QEMU
# (the runner execs it) or waits for it. So the pid in the name is alive exactly as long as the file
# is in use, and a sweep can tell a leftover from a live run's file with certainty - no age
# heuristic, no wildcard that reaches into somebody else's run.
scratch_sweep() {
	local prefix="$1" suffix="$2" path owner
	for path in "$prefix".*"$suffix"; do
		[[ -e "$path" ]] || continue
		owner="${path#"$prefix".}"
		owner="${owner%"$suffix"}"
		if [[ ! "$owner" =~ ^[0-9]+$ ]]; then
			# A name from before this scheme - `mktemp` produced random suffixes, which cannot be
			# attributed to a run at all. Removed only when it is a day old, which no live run's
			# file can be.
			[[ -n "$(find "$path" -maxdepth 0 -mtime +1 2>/dev/null)" ]] && rm -f "$path"
			continue
		fi
		[[ "$owner" == "$$" ]] && continue
		kill -0 "$owner" 2>/dev/null && continue
		rm -f "$path"
	done
}

qemu_build_esp() {
	local arch="$1"
	local kernel="$2"
	local loader_efi="$3"
	local boot_name="$4"
	local strip="${STRIP:-all}"
	scratch_sweep "$QEMU_BUILD_DIR/esp-${arch}" .img
	# Left by runners from before the neutral `.staged` name; the pid-aware sweep never removes a
	# file still owned by a live older run.
	scratch_sweep "$QEMU_BUILD_DIR/kernel-${arch}" .stripped
	scratch_sweep "$QEMU_BUILD_DIR/kernel-${arch}" .staged
	ESP="$QEMU_BUILD_DIR/esp-${arch}.$$.img"
	STAGED_KERNEL="$QEMU_BUILD_DIR/kernel-${arch}.$$.staged"
	KERNEL_STRIP_TOOL=llvm-strip "$REPO_ROOT/src/tools/stage-kernel.sh" \
		"$strip" "$kernel" "$STAGED_KERNEL"
	local esp_mb=$((($(stat -c%s "$STAGED_KERNEL") + $(stat -c%s "$loader_efi")) / 1048576 + 16))
	rm -f "$ESP"
	truncate -s "${esp_mb}M" "$ESP"
	mformat -i "$ESP" ::
	mmd -i "$ESP" ::/EFI ::/EFI/BOOT
	mcopy -i "$ESP" "$loader_efi" "::/EFI/BOOT/$boot_name"
	mcopy -i "$ESP" "$STAGED_KERNEL" ::/kernel
	# The boot packages, so the loader has something to hand over. Without them a UEFI boot comes
	# up with no userspace at all - the device-tree architectures used to get theirs from an
	# archive the runner laid in RAM, which the loader path does not use.
	#
	# The bootstrap set as FILES, the same shape the system volume carries and the same shape the
	# shipping media use. Architecture-qualified: every build writes the unqualified name, so
	# taking that verbatim put x86_64 programs on a riscv64 ESP and the boot died at "failed to
	# load SystemManager", one step after a loader hand-off that had gone perfectly.
	local bootstrap="$QEMU_BUILD_DIR/bootstrap-${arch}"
	if [[ -d "$bootstrap" ]]; then
		mmd -i "$ESP" ::/etc ::/libexec
		mcopy -i "$ESP" "$bootstrap/etc/bootstrap.list" ::/etc/bootstrap.list
		mcopy -i "$ESP" "$bootstrap"/libexec/* ::/libexec/
	fi
	# AND THE MANIFEST FOR THIS MEDIUM. The loader checks what it reads against the manifest of the
	# source it read it from, so a medium with files and no manifest is one it refuses. The kernel
	# row is computed over the independently staged copy above. The volume's digest does not cover
	# this source, even when the selected staging policy happens to produce the same bytes.
	local manifest="$QEMU_BUILD_DIR/boot.manifest.$$"
	if [[ -d "$bootstrap" ]]; then
		cp "$bootstrap/etc/boot.manifest" "$manifest"
	else
		mmd -i "$ESP" ::/etc 2>/dev/null || true
		printf 'liberboot-manifest 1\n' >"$manifest"
	fi
	printf '%s  kernel\n' "$(sha256sum "$STAGED_KERNEL" | cut -d" " -f1)" >>"$manifest"
	mcopy -i "$ESP" "$manifest" ::/etc/boot.manifest
	rm -f "$manifest"
	# THE FACTORY ARCHIVE IS COVERED BY THE MANIFEST, because the loader hands it to the kernel.
	#
	# It is staged below and used to be staged with nothing vouching for it - a `package:` row was
	# what the manifest format has always had for exactly this, and no producer wrote one for this
	# medium. The loader now checks the row before publishing the archive as a boot module, so a
	# medium that carries the file and does not name it is a medium the loader refuses. Resolved
	# before the manifest is signed, because the row has to be in it.
	local volume_pkg="$QEMU_BUILD_DIR/volume-${arch}.pkg"
	stage_signed_boot_manifest "$ESP" "$bootstrap" "$arch" "$volume_pkg"
	# ONE BYTE OF THE SIGNED MANIFEST, FOR THE GATE THAT PROVES THE REFUSAL. The x86_64 signed-boot
	# gate builds its own medium out of the shipping ISO; these two ports have no shipping ISO, and
	# their medium is assembled here - so a gate that means to boot a TAMPERED one on them has to be
	# able to say so. Off unless asked for, named beside the other harness knobs at the top of this
	# file, and used by `check-signed-boot.sh` alone.
	if [[ "${DAMAGE_SIGNED_MANIFEST:-0}" == "1" ]]; then
		local damaged="$QEMU_BUILD_DIR/damaged.$$.manifest2"
		mcopy -i "$ESP" ::/etc/boot.manifest2 "$damaged" || die "there is no signed manifest on this ESP to damage"
		printf '\x01' | dd of="$damaged" bs=1 seek=40 count=1 conv=notrunc status=none
		mcopy -o -i "$ESP" "$damaged" ::/etc/boot.manifest2
		rm -f "$damaged"
		echo "qemu-run: the signed manifest on this ESP was DAMAGED on purpose (DAMAGE_SIGNED_MANIFEST=1)" >&2
	fi
	# The factory archive still travels for the tests that read it as a fixture.
	[[ -f "$volume_pkg" ]] && mcopy -i "$ESP" "$volume_pkg" ::/volume.pkg
	return 0
}

# The SIGNED manifest for the boot medium, beside the text one.
#
# THE KERNEL HERE IS THIS MEDIUM'S STAGED COPY, so this medium needs a manifest over what actually
# sits on it, exactly as the text one does. What the signature adds is who said so.
#
# Signed with the published test key, through the tool that owns it. A build that cannot sign is a
# build that stops here rather than staging a medium whose signed manifest is missing or stale.
stage_signed_boot_manifest() {
	local esp="$1" bootstrap="$2" arch="$3" package="${4:-}"
	local out="$QEMU_BUILD_DIR/boot.manifest2.$$"
	# The release the manifest names, out of the one file that holds it.
	local PRODUCT_VERSION_FOR_MANIFEST
	PRODUCT_VERSION_FOR_MANIFEST="$(sed -n 's/^PRODUCT_VERSION="\(.*\)"/\1/p;/^PRODUCT_VERSION=/q' "$HERE/../../product.conf")"
	local -a rows=(--row "kernel:kernel=$STAGED_KERNEL")
	if [[ -d "$bootstrap" ]]; then
		rows+=(--row "bootstrap-list:etc/bootstrap.list=$bootstrap/etc/bootstrap.list")
		local program
		for program in "$bootstrap"/libexec/*; do
			[[ -f "$program" ]] || continue
			rows+=(--row "program:libexec/$(basename "$program")=$program")
		done
	fi
	# The factory archive, under the destination it is staged at. A medium that carries it and does
	# not name it is one the loader refuses rather than one it takes on trust.
	if [[ -n "$package" && -f "$package" ]]; then
		rows+=(--row "package:volume.pkg=$package")
	fi
	(cd "$HERE/../tools/sign-manifest" && cargo run --quiet -- \
		--profile test-trust --product LiberSystem --arch "$arch" --source boot-medium \
		--release "$PRODUCT_VERSION_FOR_MANIFEST" \
		--volume-uuid 00000000000000000000000000000000 \
		"${rows[@]}" --out "$out") >&2 || {
		echo "qemu-run: the boot medium's manifest could not be signed" >&2
		exit 1
	}
	mcopy -i "$esp" "$out" ::/etc/boot.manifest2
	rm -f "$out"
}

normalize_arch() {
	case "$1" in
	x86_64) echo "x86_64" ;;
	aarch64 | arm64) echo "aarch64" ;;
	riscv64) echo "riscv64" ;;
	*)
		echo "qemu-run: unknown architecture '$1'" >&2
		return 1
		;;
	esac
}

detect_native_arch() {
	local host
	host="$(uname -m)"
	case "$host" in
	x86_64) echo "x86_64" ;;
	aarch64 | arm64) echo "aarch64" ;;
	riscv64) echo "riscv64" ;;
	*)
		echo "qemu-run: unsupported host architecture '$host'" >&2
		exit 1
		;;
	esac
}

qemu_select_cpu() {
	local -n args=$1
	local target="$2"
	local emulated_cpu="$3"
	local host
	case "$(uname -m)" in
	x86_64) host=x86_64 ;;
	aarch64 | arm64) host=aarch64 ;;
	riscv64) host=riscv64 ;;
	*) host=other ;;
	esac
	if [[ "${NOKVM:-0}" != "1" && "$target" == "$host" && -e /dev/kvm ]]; then
		args=(-enable-kvm -cpu host)
	else
		args=(-cpu "$emulated_cpu")
	fi
}

TARGET_ARCH=""
KERNEL_ELF=""

if [[ $# -eq 0 ]]; then
	TARGET_ARCH="$(detect_native_arch)"
elif [[ $# -eq 1 ]]; then
	if normalize_arch "$1" >/dev/null 2>&1; then
		TARGET_ARCH="$(normalize_arch "$1")"
	else
		TARGET_ARCH="$(detect_native_arch)"
		KERNEL_ELF="$1"
	fi
elif [[ $# -eq 2 ]]; then
	TARGET_ARCH="$(normalize_arch "$1")"
	KERNEL_ELF="$2"
else
	echo "usage: qemu-run.sh [x86_64|aarch64|riscv64] [kernel-elf]" >&2
	exit 1
fi

# Reject an unsupported development profile before any image work: the profile changes
# which host workflow owns the instance, so a request it cannot honour must fail loudly
# rather than boot an ordinary guest that merely looks like a development one.
if [[ "${DEV_PROFILE:-0}" == "1" && "${TEST:-0}" == "1" ]]; then
	echo "qemu-run: DEV_PROFILE and TEST are mutually exclusive" >&2
	exit 1
fi
# A PROFILE NAME THE KERNEL DOES NOT KNOW WOULD BOOT AN ORDINARY GUEST THAT LOOKS LIKE A DEVELOPMENT
# ONE, which is the failure this block already exists to prevent - so the two the kernel recognises
# are the two this accepts.
case "${LIBER_BOOT_PROFILE:-development}" in
development | development-trace) ;;
*)
	echo "qemu-run: LIBER_BOOT_PROFILE must be 'development' or 'development-trace'; the kernel recognises no other name and would boot as though none were named" >&2
	exit 1
	;;
esac

# AND REJECT A DRIVEN GUEST ON A DIRECT BOOT, on the two targets whose direct path can carry only
# one blob.
#
# `-kernel` on `virt` has no bootloader module hand-off: the machine takes an initrd and nothing
# else, so a direct boot can be handed the init package and NOT the system volume package beside it.
# That starts SystemManager and stops there - no shell, no development agent, no control channel -
# and `scenario-cold` then waited for a handshake that could never arrive while the guest looked
# alive. The UEFI path hands over both, which is what a driven guest needs, so a request for one on
# these targets says so instead of hanging.
if [[ "${DEV_PROFILE:-0}" == "1" && "${UEFI:-0}" != "1" && "$TARGET_ARCH" != "x86_64" ]]; then
	echo "qemu-run: DEV_PROFILE on $TARGET_ARCH needs UEFI=1 - a direct -kernel boot carries one" >&2
	echo "          module, so it can start SystemManager but not the shell or the dev agent" >&2
	exit 1
fi

# Where a development guest's control channel lives, by target. The persistent instance keeps
# the unsuffixed name it has always had, so nothing that owns one has to learn a new path; the
# other targets get their own so a one-shot run on one of them cannot be mistaken for it, or
# collide with it while it is up.
#
# A COLD RUN NEVER TAKES THE UNSUFFIXED NAME, whatever the target. `scenario-cold` on x86 selected
# exactly this path, unlinked it without checking whether the persistent instance was up, and then
# bound it - so a cold run and the persistent instance this milestone exists to provide destroyed
# each other. The suffix is what keeps them apart, and x86 was the one target that did not get one.
dev_channel_socket() {
	if [[ "${COLD:-0}" == "1" ]]; then
		printf '%s/dev-channel-cold-%s.sock' "$QEMU_BUILD_DIR" "$TARGET_ARCH"
	elif [[ "$TARGET_ARCH" == "x86_64" ]]; then
		printf '%s/dev-channel.sock' "$QEMU_BUILD_DIR"
	else
		printf '%s/dev-channel-%s.sock' "$QEMU_BUILD_DIR" "$TARGET_ARCH"
	fi
}

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
QEMU_BOOT_DIR="$HERE"
QEMU_BUILD_DIR="$REPO_ROOT/.build/boot"
mkdir -p "$QEMU_BUILD_DIR"

timing_event() {
	if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then printf '%s\t%s\t%s\n' "$(date +%s%N)" "$1" "$2" >>"$LIBER_TIMING_LOG"; fi
}

watch_test_timing() {
	local qemu_pid="$1"
	local serial_path="${SERIAL#file:}"
	local kernel_seen=0 tests_seen=0 complete_seen=0
	[[ "${SERIAL:-}" == file:* ]] || return 0
	while kill -0 "$qemu_pid" 2>/dev/null || [[ "$complete_seen" == 0 ]]; do
		if [[ "$kernel_seen" == 0 ]] && grep -q 'kernel is starting' "$serial_path" 2>/dev/null; then
			timing_event kernel start
			kernel_seen=1
		fi
		if [[ "$tests_seen" == 0 ]] && grep -q '^running [0-9].* tests' "$serial_path" 2>/dev/null; then
			timing_event scenario start
			tests_seen=1
		fi
		if [[ "$complete_seen" == 0 ]] && grep -q '^test suite complete:' "$serial_path" 2>/dev/null; then
			timing_event scenario end
			complete_seen=1
		fi
		if ! kill -0 "$qemu_pid" 2>/dev/null && [[ "$complete_seen" == 0 ]]; then break; fi
		sleep 0.01
	done
}

needs_kernel=1
if [[ "$TARGET_ARCH" == x86_64 && -n "${BOOT_IMAGE:-}" ]]; then
	# A shipping ISO contains its own kernel. Keeping a host-side ELF mandatory made `--image` unable
	# to boot an artifact copied into a clean tree, even though the runner never reads that ELF on this
	# path. `run.sh --gdb` checks for symbols separately because that command really does need them.
	needs_kernel=0
fi

if [[ -z "$KERNEL_ELF" && "$needs_kernel" == 1 ]]; then
	case "$TARGET_ARCH" in
	x86_64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/x86_64-unknown-none/debug/kernel" ;;
	aarch64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/aarch64-unknown-none/debug/kernel" ;;
	riscv64) KERNEL_ELF="$REPO_ROOT/.build/cargo/kernel/riscv64gc-unknown-none-elf/debug/kernel" ;;
	esac
fi

[[ "$needs_kernel" == 0 || -f "$KERNEL_ELF" ]] || {
	echo "qemu-run: kernel ELF not found: $KERNEL_ELF" >&2
	exit 1
}

qemu_run_x86_64() {
	local kernel="$1"
	# WHICH SET OF WRITABLE IMAGES THIS GUEST GETS.
	#
	# A guest writes to its system volume, its media and its USB stick, and QEMU takes a write lock
	# on each - so two guests sharing a set cannot both run, and if they somehow did they would
	# corrupt it. The test profile has had its own `-test` set for a long time. A COLD RUN DID NOT:
	# it took the persistent instance's unsuffixed images, which is the other half of the collision
	# whose control-socket half was separated first. The `-cold-<arch>` set gives it images of its
	# own, which is what makes the disk-conflict check below able to let it through honestly.
	local artifact_suffix=""
	[[ "${TEST:-0}" == "1" ]] && artifact_suffix="-test"
	[[ "${COLD:-0}" == "1" ]] && artifact_suffix="-cold-$TARGET_ARCH"
	timing_event runner start
	timing_event image start
	# Select an already-assembled ISO, or build the internal test/development medium for callers that
	# invoke this harness directly. The public run.sh always supplies BOOT_IMAGE and therefore never
	# enters the assembly path.
	#
	# The suite boots the TEST medium: it reads `volume.pkg` off it as its fixture source and as
	# the table of expected file contents, which the shipping ISO deliberately no longer carries.
	# BOOT_IMAGE names a medium to boot INSTEAD of building one.
	#
	# Without it this function always built an image, so "boot the thing I just built" was not
	# expressible: the build, the imaging and the run were one step, and what ended up on the medium
	# could not be inspected between them. That is how a disk image came to carry the TEST kernel -
	# nothing sat between assembling it and booting it.
	local iso
	if [[ -n "${BOOT_IMAGE:-}" ]]; then
		[[ -f "$BOOT_IMAGE" ]] || {
			echo "qemu-run: no image at $BOOT_IMAGE" >&2
			exit 1
		}
		iso="$BOOT_IMAGE"
		echo "qemu-run: booting $iso (built elsewhere; nothing was rebuilt)" >&2
	else
		# Direct harness callers, notably the x86_64 kernel suite, still own their test medium. Build the
		# loader only here because an already-assembled BOOT_IMAGE has no use for a fresh loader binary.
		#
		# UNDER THE SAME BUILD LOCK THE TEST KERNEL IS COMPILED WITH, and for the same reason
		# (2026-08-31). The loader shares one Cargo target directory with every other loader build in
		# this tree, and nothing held anything over it: two guests starting together both entered this
		# line, and two concurrent `cargo build`s over one target directory is the race M3's item 0
		# names - an intermediate replaced or removed while the other invocation is reading it. The
		# kernel half was fixed by building under a lock and staging a private copy; this is the other
		# producer that feeds the same medium, and it was left outside.
		#
		# The LOCK ONLY, not the medium: `mkimage.sh` is content-addressed and takes its own assembly
		# lock, so what this has to serialise is the compile.
		#
		# AND IT IS STAGED UNDER THAT LOCK, NOT MERELY COMPILED UNDER IT (2026-08-31). Locking the
		# compile left the result at the ONE shared output path, which `mkimage.sh` then hashes and
		# copies after the lock has been released - and other producers write that same path without
		# taking this lock at all: `build.sh` builds it, and the signed-boot and trust-profile gates
		# deliberately CYCLE trust profiles through it. So a concurrent producer could make image
		# assembly abort at its before/after check, and an A-to-B-to-A profile cycle could restore the
		# original hash while the copy had consumed B - a medium whose recorded identity names bytes
		# it was not built from, which the content key cannot detect because it agrees.
		#
		# A per-run copy made under the lock is immutable for this run by construction, which is
		# exactly what `test-kernel.sh` does for the kernel and for the same reason. It costs one copy
		# of a binary already on this disk.
		mkdir -p "$REPO_ROOT/.build/state"
		local staged_loader="$REPO_ROOT/.build/state/loader-$TARGET_ARCH.$$.efi"
		scratch_sweep "$REPO_ROOT/.build/state/loader-$TARGET_ARCH" .efi
		(
			flock 7
			cd "$HERE/../boot/loader" && cargo build || exit 1
			cp "$REPO_ROOT/.build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi" "$staged_loader"
		) 7>"$REPO_ROOT/.build/state/kernel-test-build.lock" >&2 || {
			echo "qemu-run: the loader did not build, or its run-private copy could not be staged" >&2
			exit 1
		}
		local iso_mode="iso"
		[[ "${TEST:-0}" == "1" ]] && iso_mode="testiso"
		# The medium is assembled from the STAGED loader, so nothing that happens to the shared path
		# between here and the copy can reach it.
		iso="$(LOADER_EFI="$staged_loader" "$HERE/mkimage.sh" "$iso_mode" "$kernel")"
	fi

	# UEFI firmware (OVMF): the platform boots through UEFI, not SeaBIOS - the ISO is
	# hybrid, and development deliberately exercises the UEFI path (the own UEFI-only
	# bootloader is the target; see the concept's bootloader choice). The CODE image is
	# read-only and shared; each run gets a private writable copy of the VARS store so
	# concurrent instances (a test suite next to a live run) never fight over NVRAM.
	# The script execs QEMU (no exit trap can clean up), so stale copies from earlier
	# runs are unlinked here instead - a still-running instance keeps its copy alive
	# through its open file descriptor.
	local ovmf_code="${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}"
	local ovmf_vars_src="${OVMF_VARS_SRC:-/usr/share/OVMF/OVMF_VARS_4M.fd}"
	[[ -f "$ovmf_code" && -f "$ovmf_vars_src" ]] || {
		echo "qemu-run: OVMF firmware not found (install the 'ovmf' package)" >&2
		exit 1
	}
	scratch_sweep "$QEMU_BUILD_DIR/ovmf-vars" .fd
	local ovmf_vars="$QEMU_BUILD_DIR/ovmf-vars.$$.fd"
	cp "$ovmf_vars_src" "$ovmf_vars"

	# THIS MACHINE HAS AN IOMMU IN IT, AND THAT IS THE DEFAULT: `./run.sh --no-iommu` takes it out.
	#
	# `IOMMU` is the plumbing, not the interface - the same shape as `SMP`, `MEM` and `SERIAL` above
	# it: `run.sh` owns the flag and exports the variable, and this layer is the one that speaks to
	# QEMU. Nothing outside `run.sh` and the gates sets it by hand.
	#
	# WHY IT IS THE DEFAULT. The isolation this system implements was exercised by one gate and by no
	# ordinary run, so every developer boot walked the degraded path and none walked the isolated one
	# - and a defect that stopped `virtio-net` receiving a single packet and `virtio-gpu` starting at
	# all lived there in the quiet for as long as that was true. A default is where the bugs are
	# found. This is the whole machine under translation: the controller, and every virtio endpoint
	# told `iommu_platform=on` so it asks the guest for `VIRTIO_F_ACCESS_PLATFORM`.
	#
	# It was opt-in until 2026-08-26 because `virtio-gpu` did not come up behind a translating
	# controller, which was recorded here as a measurement and was a real one. The cause was not the
	# display driver: this domain's IOVA allocator handed the first ring in each domain the address
	# ZERO, and a virtio device reads a queue whose descriptor table is at zero as a queue that was
	# never programmed. One allocator line, and both drivers work.
	#
	# THE SUITE DOES NOT FOLLOW, and that is deliberate rather than an oversight. `test.sh` runs
	# untranslated; `qemu-virtio-iommu-x86_64` owns the enforcing profile, boots the shipping image
	# under it and asserts the bypass-off transition, the hostile cases and a real DHCP lease through
	# the controller. Putting a controller under the whole suite would change what sixty tests are
	# testing without adding a claim that gate does not already make. So an ordinary RUN is
	# translated and the suite is not; `run.sh --help` says so, because a split nobody is told about
	# is a surprise rather than a decision.
	#
	# `boot-bypass=on` because the firmware's own drivers read the boot medium before this kernel
	# exists; the kernel takes it out of bypass and reads the byte back, which is what makes
	# `enforcing` a fact rather than a hope.
	local iommu
	if [[ -n "${IOMMU:-}" ]]; then
		# Asked for explicitly, by `--no-iommu` or by a gate that owns its own profile.
		iommu="$IOMMU"
	elif [[ "${TEST:-0}" == "1" ]]; then
		# The suite, which stays untranslated - see the note above.
		iommu=0
	else
		iommu=1
	fi
	# Two option strings, because not every virtio device here takes the same ones: `virtio-vga` and
	# the sound device are attached without `disable-legacy=on` and must not acquire it now.
	local virtio_opts virtio_plain
	virtio_opts="$(IOMMU="$iommu" qemu_virtio_opts disable-legacy=on)"
	virtio_plain="$(IOMMU="$iommu" qemu_virtio_opts)"

	# AND THE MACHINE REFUSES BUS BYPASS WHEN THERE IS A CONTROLLER TO BYPASS (added 2026-09-04).
	#
	# `default-bus-bypass-iommu=off` is the q35 property that stops a device being placed outside the
	# controller's reach by default. Without it the profile has an IOMMU that a device need not be
	# behind, which is not the machine an isolation claim is about - and the hostile fixture was
	# booting exactly that: the gate added the controller through `QEMU_EXTRA` while the machine
	# stayed plain `q35` and every endpoint was built WITHOUT `iommu_platform=on`, so the cases it
	# refused were refused on a topology the milestone does not describe.
	local machine="q35"
	[[ "$iommu" == "1" ]] && machine="q35,default-bus-bypass-iommu=off"
	local qemu_args=(
		-machine "$machine"
		-m "${MEM:-4G}"
		-drive "if=pflash,format=raw,readonly=on,file=$ovmf_code"
		-drive "if=pflash,format=raw,file=$ovmf_vars"
		-cdrom "$iso"
		-boot d
		-serial "${SERIAL:-stdio}"
	)
	# BEFORE THE ENDPOINTS IT TRANSLATES, which is the order the gate boots and the order QEMU
	# realizes devices in.
	[[ "$iommu" == "1" ]] && qemu_args+=(-device "virtio-iommu-pci,boot-bypass=on")

	# System volume disk: carries the LiberFS volume itself.
	local volume_image="$QEMU_BUILD_DIR/system-volume-x86_64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk${artifact_suffix}.img"
	if qemu_prepare_system_disk "$volume_image" "$virtio_disk"; then
		qemu_attach_virtio_blk qemu_args "$(qemu_run_disk "$virtio_disk")" vblk "$virtio_opts"
	fi

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images "$artifact_suffix" "$artifact_suffix" loop 1

	# An ad-hoc guest cannot run beside the persistent development instance, and the reason is
	# not the port - it is the disks. Both attach the same raw images, QEMU takes a write lock
	# on each, and the second guest dies on whichever it reaches first: a forwarding rule it
	# cannot bind, or `Failed to get "write" lock` naming an image. Neither message mentions the
	# instance that actually holds them, so the reader debugs QEMU instead of running one
	# command. Two guests writing one image is also the corruption this milestone refuses
	# outright, so the answer is to refuse early rather than to hand out parallel disks.
	# THE EXEMPTION IS FOR A GUEST WITH ITS OWN IMAGES, not for a profile name.
	#
	# The condition was "not TEST and not DEV_PROFILE", and `scenario-cold` runs with DEV_PROFILE=1 -
	# so the one mode that shared the persistent instance's writable images was the one excused from
	# the check that exists to stop exactly that. Now the exemption is `artifact_suffix`: a run that
	# has been given a set of its own may proceed, and a run reaching for the unsuffixed set while a
	# development instance holds it is refused, whatever profile it declares.
	#
	# AND THE INSTANCE'S OWN BOOT IS NOT AN AD-HOC GUEST (2026-09-01). `lab.py`'s `dev-up` takes this
	# very lock - `DEV_LOCK` is this file - and HOLDS it across the `run.sh` it starts, so the guest
	# being brought up was refused by the lock its own parent had taken: `dev.sh up` built the image,
	# wrote the ISO, launched the runner and died with "a development instance is running", naming
	# itself. Reproduced directly: hold the lock and the guard's own `flock -n` returns non-zero.
	# Nothing had noticed because nothing runs the dev-guest checks - `verify-history.json` has no
	# key for any of the three.
	#
	# The exemption is the bring-up itself, and only `cmd_dev_up` sets it. It is deliberately NOT the
	# profile name the comment above rejects: `scenario-cold` declares `DEV_PROFILE=1` too and is
	# covered by `artifact_suffix`, while this marker is set by exactly one caller for exactly the one
	# boot that IS the instance. A second `dev.sh up` cannot reach here - `cmd_dev_up` refuses on its
	# own `dev_state` and its own non-blocking lock long before it starts a runner - and every other
	# guest, having no marker, is refused exactly as it was.
	if [[ "${LIBER_DEV_INSTANCE_BRINGUP:-0}" != "1" ]] && [[ -z "$artifact_suffix" && -e "$QEMU_BUILD_DIR/dev-instance.lock" ]] && ! flock -n "$QEMU_BUILD_DIR/dev-instance.lock" true 2>/dev/null; then
		echo "qemu-run: a development instance is running and holds the system, media and USB images" >&2
		echo "qemu-run: release it with \`./dev.sh down\` (or \`./dev.sh status\` to see what it is)" >&2
		exit 1
	fi

	# Network: user-mode NIC with optional hostfwd for interactive runs.
	#
	# The persistent development instance forwards a different port from an ordinary run,
	# because otherwise the two cannot coexist: the port was hard-coded, so `./run.sh` while a
	# `dev-up` instance was alive died on
	#   Could not set up host forwarding rule 'tcp:127.0.0.1:5555-:80'
	# which names the port and not the reason. That is the same rule the instance already
	# follows for its serial, control and log paths - it owns its own names so an ad-hoc boot
	# cannot collide with it - and the port was the one thing left out.
	#
	# `HOSTFWD_PORT` overrides both, for a second ad-hoc guest or a host where 5555 is taken.
	local hostfwd=""
	if [[ "${TEST:-0}" != "1" ]]; then
		local default_port=5555
		[[ "${DEV_PROFILE:-0}" == "1" ]] && default_port=5556
		local port="${HOSTFWD_PORT:-$default_port}"
		# Fail on the cause rather than leaving QEMU to fail on a rule nobody can read.
		#
		# Capture before matching: `grep -q` stops at its first match and closes the pipe, so
		# `ss` takes SIGPIPE and `pipefail` makes that the status of the pipeline - a port
		# that IS in use could read as a failed check. The same shape the source-hygiene gate
		# refuses everywhere else, which is how it was found.
		local listeners
		listeners="$(ss -ltn "sport = :$port" 2>/dev/null || true)"
		if grep -q LISTEN <<<"$listeners"; then
			echo "qemu-run: host port $port is already in use, so this guest cannot forward it" >&2
			echo "qemu-run: a persistent development instance is the usual holder - check \`./dev.sh status\`, release it with \`./dev.sh down\`" >&2
			echo "qemu-run: or run this guest on another port with HOSTFWD_PORT=<port>" >&2
			exit 1
		fi
		hostfwd="hostfwd=tcp:127.0.0.1:$port-:80"
	fi
	qemu_attach_virtio_net qemu_args vnet0 "$hostfwd" "$virtio_opts"

	# THE DEDICATED DMA FIXTURE STOPS HERE, AND EVERY BUS MASTER BELOW IS WHY IT HAS TO EXIST
	# (added 2026-09-04). `DMA_FIXTURE=1` is the enforcing-IOMMU gate's machine: the firmware boot
	# medium, the system volume, virtio-net, the controller, and whatever `QEMU_EXTRA` adds - which
	# for that gate is the IOMMU and two `edu` functions.
	#
	# It exists because the gate used to add those three to the ORDINARY test machine, which brings
	# a virtio-serial console, an xHCI controller with a hub, a keyboard, a tablet and a USB stick,
	# three more virtio-blk media disks, a PCIe-to-PCI bridge with a device behind it, and a
	# virtio-sound card. Every one of them is a bus master, and the transition the gate is about
	# QUIESCES every non-controller endpoint before it turns bypass off - so those devices were not
	# bystanders, they were participants in the security-sensitive step, and the hostile cases were
	# passing on a topology the milestone does not describe. The bridge is named there too, in the
	# opposite sense: the fixture refuses a bridge alias rather than generalizing to it, and the
	# ordinary machine supplies one.
	#
	# Omission only. Nothing here changes what the ordinary machine is, so a run without the flag
	# gets exactly the machine it got before.
	local dma_fixture="${DMA_FIXTURE:-0}"

	# virtio-serial + virtconsole: mirrors a second console to a file.
	#
	# PER RUN, for the reason `qemu_attach_virt_interactive` gives about the same file: one name per
	# mode is a capture two guests of one architecture write, and a capture two guests write
	# describes neither. Nothing outside this script looks it up by name; the sweep removes the ones
	# whose run is gone.
	if [[ "$dma_fixture" != "1" ]]; then
		local vcon_out="$QEMU_BUILD_DIR/virtio-console${artifact_suffix}.$$.out"
		scratch_sweep "$QEMU_BUILD_DIR/virtio-console${artifact_suffix}" .out
		qemu_args+=(
			-device "virtio-serial-pci,$virtio_opts"
			-device virtconsole,chardev=vcon
			-chardev "file,id=vcon,path=$vcon_out"
		)
	fi

	# xHCI USB host controller + hub with keyboard, tablet, and optional storage.
	if [[ "$dma_fixture" != "1" ]]; then
		qemu_prepare_usb_image "$artifact_suffix"
		local usb_storage_id=""
		if [[ "${TEST:-0}" == "1" || -z "${USB_HOST:-}" ]]; then
			usb_storage_id="vusb"
			# THE USB FIXTURE IS ATTACHED WRITABLE, so this run gets its own copy.
			#
			# The other three fixture media are attached `readonly=on` and can be shared; this one is not, so
			# two runs of one architecture wrote into the same file - and the stray-guest guard even exempted
			# `usb-media*.img` as though it were read-only. `qemu_run_disk` is the same per-run copy the
			# system disk already takes, and `scratch_sweep` inside it is the cleanup.
			usb_run_disk="$(qemu_run_disk "$USB_DISK")" || {
				# A COPY THAT FAILED IS A RUN THAT CANNOT BE ISOLATED, AND IT FAILS (2026-08-31).
				#
				# This fell back to attaching the shared template WRITABLE - the exact arrangement the three
				# lines above exist to remove, reinstated by a defensive `||` on the one path where it matters.
				# So a full disk, a permission problem or any other copy failure silently turned isolation off,
				# and two guests of one architecture wrote into one fixture again. There is no degraded form of
				# "this run has its own copy": either it does or the run is not the thing that was asked for.
				echo "qemu-run: could not make this run's private copy of $USB_DISK - refusing to attach the shared template writable" >&2
				exit 1
			}
			qemu_args+=(-drive "file=$usb_run_disk,if=none,id=vusb,format=raw")
		fi
		qemu_attach_xhci qemu_args "$usb_storage_id"

		# Keep media disks after USB in PCI discovery order, matching the historical
		# runner and the volume/device inventory expected by the boot chain.
		#
		# AND `MEDIA_ORDER=swapped` PRESENTS THEM THE OTHER WAY ROUND, which is the profile M2's
		# format routing is written against (added 2026-09-04). Every machine this tree boots
		# presents FAT, then ISO, then UDF - exactly the order the positional assignment expected -
		# so a routing change is a NO-OP on all of them and a mistake in it would show up as a volume
		# silently not mounting on a machine nobody runs. The plan says so in as many words: building
		# the routing without the profile is how this item gets marked done for the third time
		# without the property it names.
		#
		# The three images are the same; only the bus addresses they take differ. That is the whole
		# fixture, and it is the whole point: position now says the wrong thing, so format has to
		# decide.
		if [[ "${MEDIA_ORDER:-}" == "swapped" ]]; then
			[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" vudf "$virtio_opts" readonly
			[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" viso "$virtio_opts" readonly
			[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" vmedia "$virtio_opts" readonly
		else
			[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" vmedia "$virtio_opts" readonly
			[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" viso "$virtio_opts" readonly
			[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" vudf "$virtio_opts" readonly
		fi
	fi

	# Display backends: parse DISPLAYS env for vnc/spice.
	qemu_parse_displays qemu-run
	qemu_args+=("${DISPLAY_ARGS[@]}")

	# CPU and SMP: KVM for a matching host, otherwise the emulated x86 model.
	local smp
	if [[ "${TEST:-0}" == "1" ]]; then smp="${SMP:-4}"; else smp="${SMP:-$(nproc)}"; fi
	local cpu_args=()
	qemu_select_cpu cpu_args x86_64 qemu64,+rdrand,+smep,+smap
	qemu_args+=("${cpu_args[@]}" -smp "$smp")

	qemu_append_debug_args qemu_args
	timing_event image end

	if [[ "${TEST:-0}" == "1" ]]; then
		# The development channel is present in the cold test configuration too: the same
		# second port on every target is what lets a scenario runner drive a boot over
		# identical framing, including where the persistent profile does not exist.
		[[ "$dma_fixture" == "1" ]] || qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-x86_64-test.$$.sock" "$virtio_opts"
		# A BRIDGE WITH SOMETHING BEHIND IT, so the PCI walk has a second bus to find. The x86
		# enumeration followed no bridges and the q35 default topology puts everything on bus 0,
		# so recursive enumeration could be written and never executed - it is the topology, not
		# the code, that decided the test passed. `pci-testdev` is inert: nothing in this kernel
		# binds it, so what it proves is exactly that the walk reached a bus firmware did not
		# place on the root.
		# AND THE FIXTURE REFUSES THE BRIDGE BY NAME. M2 says the first topology contains
		# direct-root-port endpoints only and that a bridge alias is refused rather than
		# generalized inside it, so the one machine that must not have this is the one the
		# enforcing gate boots.
		if [[ "$dma_fixture" != "1" ]]; then
			qemu_args+=(
				-device "pcie-pci-bridge,id=liberbr,bus=pcie.0,addr=0x1c"
				-device "pci-testdev,bus=liberbr,addr=0x1"
			)
		fi
		# A SOUND DEVICE THE SUITE CAN RECORD FROM. The audio path used to be tested against no
		# device at all - AudioService reporting not-found is a real case and it is the only one that
		# ran - so playback was exercised nowhere and capture could not be exercised at all.
		#
		# The `none` audio backend is a SYNTHETIC SOURCE, not a disabled one: it fills a capture
		# period with silence on the device's own clock, so the receive queue, the stream set-up and
		# the whole inverted used-ring path run exactly as they would with a microphone. What it
		# cannot prove is the sample values, which is why the recording test asserts that what it got
		# IS silence - a path returning stale playback data or uninitialised memory fails that.
		if [[ "$dma_fixture" != "1" ]]; then
			qemu_append_audio qemu_args
			qemu_args+=(-device "virtio-sound-pci,audiodev=snd0")
		fi
		qemu_args+=(-no-reboot -device isa-debug-exit,iobase=0xf4,iosize=0x04)
		timing_event qemu start
		set +e
		# `QEMU_EXTRA` reaches test mode too. It is documented at the top of this file as extra
		# QEMU arguments and did not apply here, which is the one configuration where it is
		# most wanted: diagnosing a guest that resets needs `-d int,cpu_reset` on the run that
		# reproduces it, and a test run is what reproduces it.
		if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then
			qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-} &
			local qemu_pid=$!
			watch_test_timing "$qemu_pid" &
			local watcher_pid=$!
			wait "$qemu_pid"
			local code=$?
			wait "$watcher_pid" || true
		else
			qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-}
			local code=$?
		fi
		set -e
		timing_event qemu end
		# 33 is the debug-exit device reporting a passing suite (the guest wrote 0x10 to
		# port 0xf4, and QEMU reports (0x10<<1)|1); 35 is the same device reporting failure.
		# Anything else is QEMU ending for a reason the guest never asked for. Mapping 33 to 0
		# here without saying which of the two happened is what made a broken run read as a
		# clean one: the caller sees 0 for a passing suite AND for a QEMU that quit on its own.
		echo "qemu-run: test guest ended with QEMU code $code (33 = suite passed, 35 = suite failed)" >&2
		[[ "$code" -eq 33 ]] && exit 0
		exit "$code"
	fi

	# Interactive-only devices: virtio-input keyboard/tablet, virtio-vga, virtio-sound.
	qemu_args+=(-device "virtio-keyboard-pci,$virtio_opts")
	qemu_args+=(-device "virtio-tablet-pci,$virtio_opts")
	qemu_args+=(-vga none -device "virtio-vga${virtio_plain:+,$virtio_plain}")
	qemu_append_audio qemu_args
	qemu_args+=(-device "virtio-sound-pci,audiodev=snd0${virtio_plain:+,$virtio_plain}")

	# USB passthrough: real USB device (interactive only).
	if [[ -n "${USB_HOST:-}" ]]; then
		qemu_args+=(-device "usb-host,bus=usb.0,vendorid=0x${USB_HOST%%:*},productid=0x${USB_HOST##*:}")
	fi

	# Development profile: name it over fw_cfg, which the guest reads at boot and prints.
	# This sits below the test early-exit above, so test mode cannot reach it by
	# construction, and it adds no device and rewrites no image, so the profile changes
	# nothing a normal or production boot is built from.
	if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
		qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=${LIBER_BOOT_PROFILE:-development}")
		qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" "$virtio_opts"
	fi

	# Interactive control sockets used by screenshot.sh and lab.py.
	# Same rule as `dev_channel_socket`: a cold run gets its own monitor and QMP names, so it cannot
	# remove or bind the persistent instance's.
	local monitor_socket="$QEMU_BUILD_DIR/qemu-monitor.sock"
	local qmp_socket="$QEMU_BUILD_DIR/qemu-qmp.sock"
	if [[ "${COLD:-0}" == "1" ]]; then
		monitor_socket="$QEMU_BUILD_DIR/qemu-monitor-cold-$TARGET_ARCH.sock"
		qmp_socket="$QEMU_BUILD_DIR/qemu-qmp-cold-$TARGET_ARCH.sock"
	fi
	rm -f "$monitor_socket" "$qmp_socket"
	qemu_args+=(-monitor "unix:$monitor_socket,server,nowait")
	qemu_args+=(-qmp "unix:$qmp_socket,server,nowait")

	exec qemu-system-x86_64 "${qemu_args[@]}" ${QEMU_EXTRA:-}
}

qemu_run_aarch64() {
	local kernel="$1"
	local serial="${SERIAL:-mon:stdio}"
	local smp="${SMP:-$(nproc | awk '{print ($1 > 8) ? 8 : $1}')}"
	local mem="${MEM:-512M}"
	local dtb_addr="${DTB_ADDR:-0x4A000000}"
	local uefi="${UEFI:-0}"
	local aavmf_code="${AAVMF_CODE:-/usr/share/AAVMF/AAVMF_CODE.fd}"
	local aavmf_vars="${AAVMF_VARS:-/usr/share/AAVMF/AAVMF_VARS.fd}"

	qemu_parse_displays qemu-run

	# The interrupt-controller profile. `its=off` is written out for the GICv3 core profile rather
	# than left to the default, because QEMU turns the ITS ON by default there - and a core profile
	# that quietly had an ITS would not be the profile it says it is.
	local machine
	case "${GIC:-2}" in
	2) machine="virt,gic-version=2" ;;
	3) machine="virt,gic-version=3,its=off" ;;
	3its) machine="virt,gic-version=3,its=on" ;;
	*)
		echo "qemu-run: GIC must be 2, 3 or 3its (got '${GIC}')" >&2
		exit 1
		;;
	esac
	# EL2, the level the specification says AArch64 firmware runs at. The loader reads `CurrentEL`
	# and drops to EL1 before entering the kernel; that path is written from the architecture manual
	# and, until this switch existed, had never been executed on any machine this project has run on.
	if [[ "${EL2:-0}" == "1" ]]; then
		machine="$machine,virtualization=on"
	fi
	local qemu_args=()
	local cpu_args=()
	qemu_select_cpu cpu_args aarch64 cortex-a72

	# System volume disk: virtio-blk holding the factory archive.
	#
	# A cold run gets writable images of its own, like every other target: it shares this machine's
	# `.build/boot` with whatever else is running, and two guests attaching one raw image is a write
	# lock collision at best and corruption at worst.
	local media_suffix="-aarch64"
	[[ "${COLD:-0}" == "1" ]] && media_suffix="-cold-aarch64"
	local volume_pkg="$QEMU_BUILD_DIR/system-volume-aarch64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk${media_suffix}.img"
	if qemu_prepare_system_disk "$volume_pkg" "$virtio_disk"; then
		qemu_attach_virtio_blk qemu_args "$(qemu_run_disk "$virtio_disk")" vol0 "disable-legacy=on"
	fi

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images "$media_suffix" -a64
	[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" med0 "disable-legacy=on" readonly
	[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" iso0 "disable-legacy=on" readonly
	[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" udf0 "disable-legacy=on" readonly

	# Network: user-mode virtio-net.
	qemu_attach_virtio_net qemu_args vnet0 "" "disable-legacy=on"

	# xHCI USB host controller + hub with keyboard, tablet, and storage.
	qemu_prepare_usb_image "$media_suffix"
	# THE USB FIXTURE IS ATTACHED WRITABLE, so this run gets its own copy.
	#
	# The other three fixture media are attached `readonly=on` and can be shared; this one is not, so
	# two runs of one architecture wrote into the same file - and the stray-guest guard even exempted
	# `usb-media*.img` as though it were read-only. `qemu_run_disk` is the same per-run copy the
	# system disk already takes, and `scratch_sweep` inside it is the cleanup.
	usb_run_disk="$(qemu_run_disk "$USB_DISK")" || {
		# A COPY THAT FAILED IS A RUN THAT CANNOT BE ISOLATED, AND IT FAILS (2026-08-31).
		#
		# This fell back to attaching the shared template WRITABLE - the exact arrangement the three
		# lines above exist to remove, reinstated by a defensive `||` on the one path where it matters.
		# So a full disk, a permission problem or any other copy failure silently turned isolation off,
		# and two guests of one architecture wrote into one fixture again. There is no degraded form of
		# "this run has its own copy": either it does or the run is not the thing that was asked for.
		echo "qemu-run: could not make this run's private copy of $USB_DISK - refusing to attach the shared template writable" >&2
		exit 1
	}
	qemu_args+=(-drive "if=none,id=vusb,format=raw,file=$usb_run_disk")
	qemu_attach_xhci qemu_args vusb

	# Test mode: enable Arm semihosting while retaining the selected serial backend.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		# The boot-chain test includes DisplayService and its Console/Shell dependents.
		# Unlike x86, the virt machine has no default VGA device, so test mode supplies
		# the same discoverable GPU path without enabling the interactive peripherals.
		qemu_args+=(-device virtio-gpu-pci,disable-legacy=on)
		qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-aarch64-test.$$.sock" "disable-legacy=on"
		# AND A SOUND DEVICE THE SUITE CAN RECORD FROM. The `none` audio backend is a SYNTHETIC
		# SOURCE rather than a disabled one: it fills a capture period with silence on the device's
		# own clock, so the receive queue, the input-stream search and the whole inverted used-ring
		# path run exactly as they would with a microphone. See the same block in the x86_64 test
		# arm for what the recording test can and cannot prove with it.
		qemu_append_audio qemu_args
		qemu_args+=(-device "virtio-sound-pci,audiodev=snd0,disable-legacy=on")
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -aarch64 "disable-legacy=on"
		# The development profile is not x86_64's alone: a scenario has to be runnable against
		# a cold boot of every target, and what that needs is a guest that names the profile
		# (so DeviceManager starts an agent) and a channel for the agent to answer on.
		if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
			qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=${LIBER_BOOT_PROFILE:-development}")
			qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" "disable-legacy=on"
			# The same discoverable GPU the test configuration supplies, and for the same
			# reason: the virt machine has no VGA device, the interactive set offers ramfb
			# instead, and nothing drives ramfb - so DisplayService never comes up and takes
			# ConsoleService and the shell down with it. A driven guest needs all three.
			qemu_args+=(-device "virtio-gpu-pci,disable-legacy=on")
			# The monitor and QMP sockets a driven guest needs: `key` and `pointer` steps go through
			# QMP, which is how a scenario reaches the emulated keyboard and tablet rather than the
			# console. Per target, so a one-shot run cannot be mistaken for the persistent instance's
			# or collide with it. Without these a `key` step reaches nothing and quietly does nothing.
			#
			# AND A COLD RUN TAKES THE COLD NAMES, which is what `scenario-cold` connects to. The x86
			# block below has taken them since the run that destroyed a persistent instance's sockets;
			# these two never did, so on these targets the guest listened on one path while the runner
			# dialled another - and `key` steps failed with "no QEMU QMP socket" against a guest that
			# was up and answering everything else.
			local dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-$TARGET_ARCH.sock"
			local dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-$TARGET_ARCH.sock"
			if [[ "${COLD:-0}" == "1" ]]; then
				dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-cold-$TARGET_ARCH.sock"
				dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-cold-$TARGET_ARCH.sock"
			fi
			rm -f "$dev_monitor" "$dev_qmp"
			qemu_args+=(-monitor "unix:$dev_monitor,server,nowait")
			qemu_args+=(-qmp "unix:$dev_qmp,server,nowait")
		fi
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under AAVMF.
		local loader_efi="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/aarch64-unknown-uefi/debug/libersystem-loader.efi}"
		[[ -f "$loader_efi" ]] || {
			echo "qemu-run: loader EFI not found: $loader_efi (run './build.sh --arch aarch64 --part loader')" >&2
			exit 1
		}
		[[ -f "$aavmf_code" && -f "$aavmf_vars" ]] || {
			echo "qemu-run: AAVMF firmware not found ($aavmf_code / $aavmf_vars)" >&2
			exit 1
		}
		qemu_build_esp aarch64 "$kernel" "$loader_efi" BOOTAA64.EFI
		# A private copy per run, like the OVMF path above. One shared file means two aarch64 runs
		# write each other's firmware variables, and the script `exec`s QEMU so no trap can clean up
		# afterwards - stale copies from earlier runs are unlinked here instead, while a still-
		# running instance keeps its own alive through its open descriptor.
		scratch_sweep "$QEMU_BUILD_DIR/aavmf-vars" .fd
		local vars="$QEMU_BUILD_DIR/aavmf-vars.$$.fd"
		cp "$aavmf_vars" "$vars"
		# ESP goes last so system volume enumerates ahead of it.
		qemu_attach_virtio_blk qemu_args "$ESP" esp "disable-legacy=on"
		exec qemu-system-aarch64 \
			-machine "$machine" \
			"${cpu_args[@]}" \
			-smp "$smp" \
			-m "$mem" \
			-drive "if=pflash,format=raw,file=$aavmf_code,readonly=on" \
			-drive "if=pflash,format=raw,file=$vars" \
			-serial "$serial" \
			"${DISPLAY_ARGS[@]}" \
			-no-reboot \
			"${test_args[@]}" \
			"${qemu_args[@]}" \
			${QEMU_EXTRA:-}
	fi

	# THE BOOT MODULES, and they are handed over here rather than not at all.
	#
	# This machine has no bootloader module hand-off, so a direct boot used to reach the kernel with
	# an empty boot archive: the comment that stood here said the archive arrived as an initrd whose
	# range the device tree carried, and neither the dump below nor the command under it had ever
	# had an `-initrd`. The kernel says what that costs - "no boot packages were handed over -
	# userspace is not started" - so every direct aarch64 boot came up with no userspace at all.
	#
	# IT CANNOT ARRIVE AS AN INITRD ON THIS PATH, which is why the archive is loaded at a fixed
	# address instead. The kernel is an ELF rather than a Linux Image, so QEMU enters it with x0 = 0
	# and places no device tree for it (measured: `arch: aarch64 | EL1 | DTB 0x0`) - hence the
	# separate dump loaded at DTB_ADDR. And that dump cannot be taken with the real arguments:
	# `dumpdtb` together with `-kernel` SEGFAULTS qemu-system-aarch64 10.0.11, with and without
	# `-initrd`. So the tree this guest reads is not the tree an initrd would have annotated, and
	# `/chosen/linux,initrd-start` can never appear in it. `-device loader` is the mechanism that is
	# left, it is the same one the DTB itself already uses on this line, and the address is fixed by
	# agreement with the kernel's `arch::aarch64::boot` rather than discovered - the archive names
	# its own length in its PKGARCH1 header, so only the start has to be agreed on.
	#
	# ONE BLOB, so this is the init package alone: the system volume package does not fit through a
	# single hand-off and a driven guest needs it, which is why DEV_PROFILE on this target is
	# refused above unless UEFI=1.
	local modules_addr="${MODULES_ADDR:-0x4B000000}"
	local module_args=()
	if [[ "${TEST:-0}" != "1" ]]; then
		local init_pkg="${INIT_PKG:-$QEMU_BUILD_DIR/init-aarch64.pkg}"
		[[ -f "$init_pkg" ]] || {
			echo "qemu-run: init package not found: $init_pkg (run 'just build --arch aarch64')" >&2
			exit 1
		}
		module_args+=(-device "loader,file=$init_pkg,addr=$modules_addr")
	fi

	# Direct -kernel boot: dump DTB and load it at DTB_ADDR.
	local dtb_file
	dtb_file="$(mktemp /tmp/qemu-virt-XXXXXX.dtb)"
	trap 'rm -f "$dtb_file"' EXIT
	# THE DUMPED TREE MUST DESCRIBE THE MACHINE THE GUEST ACTUALLY RUNS ON, so the dump carries the
	# same extra arguments the run below does. Without that, a machine given `-numa` boots with a
	# device tree dumped from a machine that was not - and the guest reads one memory node where its
	# hardware has two, with no way to tell that the tree and the machine disagree.
	qemu-system-aarch64 \
		-machine "$machine,dumpdtb=$dtb_file" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-display none ${QEMU_EXTRA:-} >/dev/null 2>&1

	# NOT `exec`, and the difference is the trap above. Bash does not run an EXIT trap when the
	# shell successfully replaces itself, so every direct aarch64 start since this path existed
	# abandoned its `/tmp/qemu-virt-XXXXXX.dtb`. QEMU runs as a child instead and this shell waits
	# for it, keeping the same foreground process group - a terminal interrupt still reaches QEMU -
	# and then removes exactly the file it created and exits with the child's status.
	local qemu_status=0
	qemu-system-aarch64 \
		-machine "$machine" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-kernel "$kernel" \
		-device "loader,file=$dtb_file,addr=$dtb_addr" \
		"${module_args[@]}" \
		-serial "$serial" \
		"${DISPLAY_ARGS[@]}" \
		-no-reboot \
		"${test_args[@]}" \
		"${qemu_args[@]}" \
		${QEMU_EXTRA:-} &
	local qemu_pid=$!
	wait "$qemu_pid" || qemu_status=$?
	rm -f "$dtb_file"
	trap - EXIT
	exit "$qemu_status"
}

qemu_run_riscv64() {
	local kernel="$1"
	local serial="${SERIAL:-mon:stdio}"
	local smp="${SMP:-$(nproc)}"
	local mem="${MEM:-512M}"
	local bios="${BIOS:-default}"
	local uefi="${UEFI:-0}"
	local uboot="${UBOOT:-/usr/lib/u-boot/qemu-riscv64_smode/u-boot.bin}"

	qemu_parse_displays qemu-run

	local qemu_args=()
	local cpu_args=()
	qemu_select_cpu cpu_args riscv64 rv64

	# System volume disk: virtio-blk holding the factory archive.
	#
	# A cold run gets writable images of its own, like every other target: it shares this machine's
	# `.build/boot` with whatever else is running, and two guests attaching one raw image is a write
	# lock collision at best and corruption at worst.
	local media_suffix="-riscv64"
	[[ "${COLD:-0}" == "1" ]] && media_suffix="-cold-riscv64"
	local volume_pkg="$QEMU_BUILD_DIR/system-volume-riscv64.img"
	local virtio_disk="$QEMU_BUILD_DIR/virtio-blk${media_suffix}.img"
	if qemu_prepare_system_disk "$volume_pkg" "$virtio_disk"; then
		qemu_attach_virtio_blk qemu_args "$(qemu_run_disk "$virtio_disk")" vol0 ""
	fi

	# Media volumes: FAT/ISO/UDF images seeded from volume/ directory.
	qemu_prepare_media_images "$media_suffix" -rv64
	[[ -f "$FAT_DISK" ]] && qemu_attach_virtio_blk qemu_args "$FAT_DISK" med0 "" readonly
	[[ -f "$ISO_DISK" ]] && qemu_attach_virtio_blk qemu_args "$ISO_DISK" iso0 "" readonly
	[[ -f "$UDF_DISK" ]] && qemu_attach_virtio_blk qemu_args "$UDF_DISK" udf0 "" readonly

	# Network: user-mode virtio-net (no disable-legacy for riscv64).
	qemu_attach_virtio_net qemu_args vnet0 "" ""

	# xHCI USB host controller + hub with keyboard, tablet, and storage.
	qemu_prepare_usb_image "$media_suffix"
	# THE USB FIXTURE IS ATTACHED WRITABLE, so this run gets its own copy.
	#
	# The other three fixture media are attached `readonly=on` and can be shared; this one is not, so
	# two runs of one architecture wrote into the same file - and the stray-guest guard even exempted
	# `usb-media*.img` as though it were read-only. `qemu_run_disk` is the same per-run copy the
	# system disk already takes, and `scratch_sweep` inside it is the cleanup.
	usb_run_disk="$(qemu_run_disk "$USB_DISK")" || {
		# A COPY THAT FAILED IS A RUN THAT CANNOT BE ISOLATED, AND IT FAILS (2026-08-31).
		#
		# This fell back to attaching the shared template WRITABLE - the exact arrangement the three
		# lines above exist to remove, reinstated by a defensive `||` on the one path where it matters.
		# So a full disk, a permission problem or any other copy failure silently turned isolation off,
		# and two guests of one architecture wrote into one fixture again. There is no degraded form of
		# "this run has its own copy": either it does or the run is not the thing that was asked for.
		echo "qemu-run: could not make this run's private copy of $USB_DISK - refusing to attach the shared template writable" >&2
		exit 1
	}
	qemu_args+=(-drive "if=none,id=vusb,format=raw,file=$usb_run_disk")
	qemu_attach_xhci qemu_args vusb

	# Test mode: enable RISC-V semihosting while retaining the selected serial backend.
	local test_args=()
	if [[ "${TEST:-0}" == "1" ]]; then
		test_args+=(-semihosting)
		# The RISC-V virt machine has no default VGA device, while the boot-chain test
		# requires DisplayService and its Console/Shell dependents.
		qemu_args+=(-device virtio-gpu-pci)
		qemu_attach_dev_channel qemu_args "$QEMU_BUILD_DIR/dev-channel-riscv64-test.$$.sock" ""
		# AND A SOUND DEVICE THE SUITE CAN RECORD FROM. The `none` audio backend is a SYNTHETIC
		# SOURCE rather than a disabled one: it fills a capture period with silence on the device's
		# own clock, so the receive queue, the input-stream search and the whole inverted used-ring
		# path run exactly as they would with a microphone. See the same block in the x86_64 test
		# arm for what the recording test can and cannot prove with it.
		qemu_append_audio qemu_args
		qemu_args+=(-device "virtio-sound-pci,audiodev=snd0")
	else
		# Interactive-only devices: ramfb, virtio-keyboard/tablet, sound, virtconsole.
		qemu_attach_virt_interactive qemu_args -riscv64 ""
		if [[ "${DEV_PROFILE:-0}" == "1" ]]; then
			qemu_args+=(-fw_cfg "name=opt/org.libersystem/profile,string=${LIBER_BOOT_PROFILE:-development}")
			qemu_attach_dev_channel qemu_args "$(dev_channel_socket)" ""
			# The same discoverable GPU the test configuration supplies, and for the same
			# reason: the virt machine has no VGA device, the interactive set offers ramfb
			# instead, and nothing drives ramfb - so DisplayService never comes up and takes
			# ConsoleService and the shell down with it. A driven guest needs all three.
			qemu_args+=(-device "virtio-gpu-pci")
			# The monitor and QMP sockets a driven guest needs: `key` and `pointer` steps go through
			# QMP, which is how a scenario reaches the emulated keyboard and tablet rather than the
			# console. Per target, so a one-shot run cannot be mistaken for the persistent instance's
			# or collide with it. Without these a `key` step reaches nothing and quietly does nothing.
			#
			# AND A COLD RUN TAKES THE COLD NAMES, which is what `scenario-cold` connects to. The x86
			# block below has taken them since the run that destroyed a persistent instance's sockets;
			# these two never did, so on these targets the guest listened on one path while the runner
			# dialled another - and `key` steps failed with "no QEMU QMP socket" against a guest that
			# was up and answering everything else.
			local dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-$TARGET_ARCH.sock"
			local dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-$TARGET_ARCH.sock"
			if [[ "${COLD:-0}" == "1" ]]; then
				dev_monitor="$QEMU_BUILD_DIR/qemu-monitor-cold-$TARGET_ARCH.sock"
				dev_qmp="$QEMU_BUILD_DIR/qemu-qmp-cold-$TARGET_ARCH.sock"
			fi
			rm -f "$dev_monitor" "$dev_qmp"
			qemu_args+=(-monitor "unix:$dev_monitor,server,nowait")
			qemu_args+=(-qmp "unix:$dev_qmp,server,nowait")
		fi
	fi
	qemu_append_debug_args qemu_args

	if [[ "$uefi" == "1" ]]; then
		# Boot through the own UEFI loader under U-Boot.
		#
		# U-Boot is clamped to at most 8 harts, and this is not tidiness - it is the whole reason
		# this path "produced no output at all". `-smp` defaults to `nproc`, and on a host with
		# enough cores that number is passed straight through: measured 2026-08-03, this U-Boot
		# build prints its banner at 50 harts and prints NOTHING at 51, so a 52-core host got a
		# silent failure that looked like the loader or the hand-rolled EFI image was at fault.
		# OpenSBI still ran and still logged, which is what made it read as a boot that got
		# further than it did.
		#
		# The clamp is announced rather than silent: overriding an explicit SMP= without saying so
		# is how a measurement gets attributed to the wrong core count.
		if ((smp > 8)); then
			echo "qemu-run: riscv64 UEFI: capping -smp $smp to 8 (U-Boot stops booting above ~50 harts)" >&2
			smp=8
		fi
		local loader_efi="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/riscv64gc-unknown-none-elf/debug/libersystem-loader.efi}"
		[[ -f "$loader_efi" ]] || {
			echo "qemu-run: loader EFI not found: $loader_efi (run './build.sh --arch riscv64 --part loader')" >&2
			exit 1
		}
		[[ -f "$uboot" ]] || {
			echo "qemu-run: U-Boot not found: $uboot (install the u-boot-qemu package)" >&2
			exit 1
		}
		qemu_build_esp riscv64 "$kernel" "$loader_efi" BOOTRISCV64.EFI
		# ESP is NVMe so U-Boot's default boot order tries nvme0 first.
		qemu_args+=(-drive "if=none,id=esp,format=raw,file=$ESP" -device "nvme,serial=libersystem-esp,drive=esp")
		exec qemu-system-riscv64 \
			-machine "virt,aia=aplic-imsic" \
			"${cpu_args[@]}" \
			-smp "$smp" \
			-m "$mem" \
			-bios "$bios" \
			-kernel "$uboot" \
			-serial "$serial" \
			"${DISPLAY_ARGS[@]}" \
			-no-reboot \
			"${qemu_args[@]}" \
			"${test_args[@]}" \
			${QEMU_EXTRA:-}
	fi

	# THE BOOT MODULES, as an initrd - the mechanism this machine actually has.
	#
	# Unlike aarch64 above, OpenSBI hands the kernel a device tree in a1, and it is the tree QEMU
	# generated for THIS invocation. So `-initrd` annotates the tree the kernel reads:
	# `/chosen/linux,initrd-start` and `-end` carry the archive's exact range, which is where
	# `arch::riscv64::boot` takes it from. Measured on qemu-system-riscv64 10.0.11: with
	# `-initrd init-riscv64.pkg` the dumped tree gains both properties, eight bytes each, and their
	# difference is the file's size to the byte.
	#
	# Without this the kernel came up with an empty boot archive and printed that it was starting no
	# userspace, on every direct riscv64 boot. ONE BLOB, as on aarch64: the init package, not the
	# system volume package with it - see the DEV_PROFILE refusal near the top.
	local initrd_args=()
	if [[ "${TEST:-0}" != "1" ]]; then
		local init_pkg="${INIT_PKG:-$QEMU_BUILD_DIR/init-riscv64.pkg}"
		[[ -f "$init_pkg" ]] || {
			echo "qemu-run: init package not found: $init_pkg (run 'just build --arch riscv64')" >&2
			exit 1
		}
		initrd_args+=(-initrd "$init_pkg")
	fi

	# Direct -kernel boot: OpenSBI jumps to kernel entry.
	exec qemu-system-riscv64 \
		-machine "virt,aia=aplic-imsic" \
		"${cpu_args[@]}" \
		-smp "$smp" \
		-m "$mem" \
		-bios "$bios" \
		-kernel "$kernel" \
		"${initrd_args[@]}" \
		-serial "$serial" \
		"${DISPLAY_ARGS[@]}" \
		-no-reboot \
		"${qemu_args[@]}" \
		"${test_args[@]}" \
		${QEMU_EXTRA:-}
}

case "$TARGET_ARCH" in
x86_64) qemu_run_x86_64 "$KERNEL_ELF" ;;
aarch64) qemu_run_aarch64 "$KERNEL_ELF" ;;
riscv64) qemu_run_riscv64 "$KERNEL_ELF" ;;
esac
