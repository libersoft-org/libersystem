#!/usr/bin/env bash
# mkimage.sh - assemble bootable OS images from the kernel ELF.
#
# Subcommands:
#   mkimage.sh iso <kernel-elf>          build the shipping CD image (.iso): system volume, no
#                                        factory archive
#   mkimage.sh testiso <kernel-elf>      build the test CD image (-test.iso): the factory archive
#                                        the kernel suite needs as its fixture
#   mkimage.sh img <kernel-elf> [size]   build a raw UEFI-only disk image (.img)
#
# The platform is UEFI-only and boots through the own loader. Both images carry a
# FAT boot filesystem holding the loader at /EFI/BOOT/BOOTX64.EFI plus the files it
# reads from that same volume: the kernel at /kernel and the init/volume packages
# at their product.conf names. The ISO exposes the FAT image as its UEFI El Torito
# boot entry (OVMF has no ISO9660 driver, so the loader can only read the FAT
# volume); the disk image is a GPT disk with a single EFI System Partition. No
# root or loop mount is needed.
#
# `size` (img only) accepts truncate-style suffixes (e.g. 64M, 1G); default 64M.
#
# STRIP env var selects how much is stripped from the staged kernel:
#   STRIP=debug  (default) drop only the DWARF debug info (keeps the symbol table)
#   STRIP=all              also drop the symbol table for the smallest image
# Both only remove non-loadable sections, so booting is unaffected either way.
#
# The artifact is written to .build/boot/<product-slug>.{iso,img}; its path is
# printed to stdout (progress goes to stderr) so callers can capture it.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
# The own UEFI loader's EFI binary, staged into the boot image as BOOTX64.EFI.
LOADER_EFI="${LOADER_EFI:-$REPO_ROOT/.build/cargo/loader/x86_64-unknown-uefi/debug/libersystem-loader.efi}"

# product metadata (single source of truth)
# shellcheck source=/dev/null
source "$REPO_ROOT/product.conf"

BUILD="$REPO_ROOT/.build/boot"
SLUG="$(echo "$PRODUCT_NAME" | tr '[:upper:]' '[:lower:]')"

# THE CANDIDATE THIS RUN IS ACTUALLY WRITING, recorded as it is chosen.
#
# Cleanup used to name `$SLUG.iso.$$.candidate` and `$SLUG.img.$$.candidate` by hand, and the test
# ISO builder chooses `$SLUG-test.iso.$$.candidate` - a third name neither line matched. Every
# failed or interrupted test-image build therefore left a full-size candidate under `.build/boot`,
# accumulating silently, while the comment said cleanup covered it. One variable, set where the name
# is decided, cannot drift from the name that was decided.
CANDIDATES=()

manifest_rows=""

cleanup() {
	local path
	for path in "${CANDIDATES[@]}"; do
		rm -f "$path"
	done
	rm -f "$BUILD/$SLUG.iso.build-key.tmp.$$" "$BUILD/$SLUG.img.build-key.tmp.$$"
}
trap cleanup EXIT

# operate on raw partition offsets without tripping mtools' geometry checks
export MTOOLS_SKIP_CHECK=1

# WHAT MAKES TWO BUILDS OF THE SAME TREE THE SAME BYTES.
#
# Filesystem metadata, timestamps and GUIDs are generated fresh on every run unless they are told
# not to be: mtools picks a random FAT volume serial and stamps the host clock, `sgdisk` draws random
# disk and partition GUIDs, and `xorriso` records the moment it ran. So two clean builders could not
# be expected to produce equivalent media even with identical payloads, and a cached image could not
# be traced to the environment that made it - binary comparison, provenance and delta updates all
# rest on that and none of them was available.
#
# Each of these pins ONE variable field. What they deliberately do not pin is per-installed-machine
# identity: the system volume's own UUID is drawn when the volume is formatted, and two installed
# machines are supposed to differ there. Reproducible MEDIA identity and unique INSTALLED identity
# are different properties, and this file is about the first.
#
# `SOURCE_DATE_EPOCH` is the cross-project spelling of "the timestamp to record"; it is honoured when
# set and otherwise fixed, so an unset environment is deterministic rather than merely usual.
export SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1735689600}" # 2025-01-01T00:00:00Z
export TZ=UTC
# mtools reads these: a fixed FAT volume serial and no host clock in the directory entries.
MTOOLS_FAT_SERIAL="0x4C696265" # "Libe", so a hexdump of a serial says where it came from
# The GPT identities, derived from a declared seed rather than drawn. They name the MEDIUM layout,
# which is the same on every copy of the same build, and not the installed system.
GPT_DISK_GUID="4C424653-0000-4000-8000-000000000000"
GPT_ESP_GUID="4C424653-0000-4000-8000-000000000001"
GPT_SYSTEM_GUID="4C424653-0000-4000-8000-000000000002"

# Stamp a staged file with the build epoch before it is copied into a filesystem that records
# mtimes. mtools copies the source file's timestamp, so the recorded time is whatever the build
# happened to write - which differs between two builds of identical content.
# Make a directory on a FAT image, and DO NOT ASK WHEN IT IS ALREADY THERE.
#
# `mmd` on an existing name asks what to do about the collision - and it asks on `/dev/tty`, which
# it opens itself. Neither redirecting stdin nor discarding stderr reaches that: the question is
# invisible and the answer can only come from a person sitting at the terminal that started the
# build. `./run.sh` on a fresh machine therefore stopped dead right after `kernel: stripped`,
# printing nothing further and using no CPU, for as long as anyone let it. It reproduces ONLY with
# a terminal on stdin, which is why every scripted run and every CI run of the same command passed:
# with no controlling terminal the open fails and mtools gives up instead of waiting.
#
# Asking first is the fix, not silencing the answer. `::/etc` legitimately exists already whenever a
# medium stages the bootstrap set before its manifest - two callers here do exactly that - so the
# collision is expected and creating the directory is what is conditional.
ensure_dir() {
	local image="$1" dir="$2"
	mdir -i "$image" "$dir" >/dev/null 2>&1 || mmd -i "$image" "$dir"
}

stamp_epoch() {
	touch -d "@$SOURCE_DATE_EPOCH" "$@"
}

# The tool identities this image's bytes depend on. Two builders with different mtools, xorriso or
# sgdisk can produce different media from the same inputs, and the cache key said nothing about
# which ones made the image it holds.
tool_identity() {
	local tool version
	for tool in objcopy llvm-strip mformat mcopy sgdisk xorriso; do
		version="$("$tool" --version 2>&1 || echo unknown)"
		printf 'tool=%s version=%s\n' "$tool" "${version%%$'\n'*}"
	done
}

info() { echo "mkimage: $*" >&2; }
die() {
	echo "mkimage: $*" >&2
	exit 1
}

# resolve the strip level (STRIP=debug|all) to an objcopy flag, once and up front
STRIP="${STRIP:-debug}"
case "$STRIP" in
debug) STRIP_FLAG="--strip-debug" ;;
all) STRIP_FLAG="--strip-all" ;;
*) die "invalid STRIP='$STRIP' (expected 'debug' or 'all')" ;;
esac

# stage the kernel for an image: strip it per STRIP_FLAG. The loader loads only
# the PT_LOAD segments and GDB reads symbols from the on-disk build, so the
# stripped sections are dead weight in a bootable image. Prints the staged path on stdout.
stage_kernel() {
	local src="$1" out="$BUILD/kernel"
	mkdir -p "$BUILD"
	objcopy "$STRIP_FLAG" "$src" "$out"
	info "kernel: stripped ($STRIP) $(stat -c %s "$src") -> $(stat -c %s "$out") bytes"
	echo "$out"
}

# Check every boot artifact the manifest names against what is actually on disk. The manifest is
# the single statement of what an image contains; this is how packaging enforces it rather than
# restating the list. Packaging compiles nothing - it verifies and assembles - so a missing
# artifact is an error here, unlike the compile phase where a missing optional library warns and
# the build carries on.
verify_boot_artifacts() {
	local staged_kernel="$1"
	local kind name destination source missing=0
	# THE MANIFEST IS READ FROM A CHECKED SUBPROCESS, not through `< <(...)`.
	#
	# Bash does not propagate a process substitution's exit status to the `while` that reads it, so a
	# `cargo run -- boot-artifacts` that failed and printed nothing left this loop with no
	# iterations and `missing=0` - and the function then declared every required artifact present.
	# That is the authoritative list of what an image must contain, verified by not being read.
	#
	# THROUGH THE SCRIPT THAT OWNS THAT TOOL, not through a second `cargo run` of its crate. This
	# ran `cargo run --quiet` from the crate directory, which builds it AGAIN into the shared target
	# directory - and `--quiet` means a first run on a fresh machine compiles `serde`, `serde_json`
	# and `toml` while printing NOTHING AT ALL. Reported as a hang: `./run.sh` on a new VM stopped
	# dead after `kernel: stripped` and sat there. `tools/system-manifest.sh` is the tree's one way
	# to reach this tool - it keys the binary against the crate's sources, builds it once under a
	# lock, and `exec`s it - so this costs nothing after `./build.sh` has run.
	local rows
	rows="$("$REPO_ROOT/src/tools/system-manifest.sh" boot-artifacts)" ||
		die "the system manifest could not be exported, so nothing here knows what this image must contain"
	[[ -n "$rows" ]] || die "the system manifest exported no boot artifacts, which is not a manifest this builder can act on"
	# Published for the cache key: the artifact list and its destinations ARE part of what an image
	# is, and the key did not contain them in any form.
	manifest_rows="$rows"
	while read -r kind name destination; do
		[[ -n "$kind" ]] || continue
		case "$kind" in
		kernel) source="$staged_kernel" ;;
		loader) source="$LOADER_EFI" ;;
		# The manifest names these by their DESTINATION on the medium (`init.pkg`); in the build
		# directory they carry the architecture, because every architecture writes them and an
		# unqualified name holds whichever ran last.
		init-package | volume-package) source="$BUILD/${destination%.pkg}-x86_64.pkg" ;;
		*) die "manifest names boot artifact kind '$kind', which this image builder cannot stage" ;;
		esac
		if [[ ! -f "$source" ]]; then
			echo "mkimage: missing $kind '$name': $source" >&2
			missing=1
		fi
	done <<<"$rows"
	((missing == 0)) || die "packaging needs every artifact built first - run './build.sh'"
}

# build a hybrid ISO (BIOS El Torito + UEFI), bootable as a CD or off a USB stick
# build a UEFI-only ISO, bootable as a CD or off a USB stick
# Copy the loader's fallback bootstrap set onto a FAT image, in the layout the loader reads.
stage_bootstrap_files() {
	local image="$1" root="$BUILD/bootstrap-${2:-x86_64}"
	[[ -d "$root" ]] || die "no fallback bootstrap set at $root (run \`just system-volume\`)"
	mmd -i "$image" ::/etc ::/libexec
	# Stamped before the copy: mtools records the source file's mtime, so an identical build made a
	# minute later produced different directory entries.
	stamp_epoch "$root/etc/bootstrap.list" "$root"/libexec/*
	mcopy -i "$image" "$root/etc/bootstrap.list" ::/etc/bootstrap.list
	mcopy -i "$image" "$root"/libexec/* ::/libexec/
}

# The `etc/boot.manifest` for a BOOT MEDIUM, which is not the system volume's.
#
# The loader checks every file it reads against the manifest of THE SOURCE IT READ IT FROM, so a
# medium the loader may boot needs one of its own - and it cannot simply be the volume's copy,
# because the kernel staged here is the STRIPPED build. That is a different sequence of bytes from
# the volume's kernel, so the volume's digest for it would refuse every boot from this medium.
#
# The kernel row is therefore computed here, over the file that was actually copied in. The
# bootstrap rows come from the set's own manifest when the medium carries the set; a medium that
# carries only a kernel - the shipping ISO, whose programs live on the volume beside it - gets a
# manifest with the kernel alone, which is still a source that can be checked rather than one the
# loader has to take on trust.
stage_boot_manifest() {
	local image="$1" staged_kernel="$2" arch="${3:-}" payload="${4:-}" payload_name="${5:-}"
	local out="$BUILD/boot.manifest.$$"
	if [[ -n "$arch" ]]; then
		cp "$BUILD/bootstrap-${arch}/etc/boot.manifest" "$out"
	else
		printf 'liberboot-manifest 1\n' >"$out"
	fi
	printf '%s  kernel\n' "$(sha256sum "$staged_kernel" | cut -d" " -f1)" >>"$out"
	# `::/etc` already exists when a bootstrap set was staged into it.
	ensure_dir "$image" ::/etc
	stamp_epoch "$out"
	mcopy -i "$image" "$out" ::/etc/boot.manifest
	rm -f "$out"
	stage_signed_boot_manifest "$image" "$staged_kernel" "$arch" "$payload" "$payload_name"
}

# The SIGNED manifest for this medium, beside the text one.
#
# THE SAME FILES, AND WHO SAYS SO. The text manifest proves the content matches what is next to it;
# this proves it came from a build holding the key. Signed through the tool that owns the key, over
# the kernel that was actually copied in - which is the stripped one, and a different sequence of
# bytes from the volume's.
#
# A medium without a bootstrap set gets a manifest with the kernel alone: still a source that can be
# checked rather than one the loader has to take on trust.
stage_signed_boot_manifest() {
	local image="$1" staged_kernel="$2" arch="$3" payload="${4:-}" payload_name="${5:-}"
	local out="$BUILD/boot.manifest2.$$"
	local release
	release="$(sed -n 's/^PRODUCT_VERSION="\(.*\)"/\1/p' "$REPO_ROOT/product.conf" | head -1)"
	local -a rows=(--row "kernel:kernel=$staged_kernel")
	# THE PAYLOAD THIS MEDIUM CARRIES, whichever of the two it is. A boot medium that hands the
	# kernel a system volume as a module is a medium whose manifest has to cover that whole image -
	# otherwise the one artifact the loader publishes untouched is the one nothing vouched for.
	if [[ -n "$payload" && -f "$payload" ]]; then
		case "$payload_name" in
		system-volume.img) rows+=(--row "system-volume:$payload_name=$payload") ;;
		*) rows+=(--row "package:$payload_name=$payload") ;;
		esac
	fi
	if [[ -n "$arch" && -d "$BUILD/bootstrap-${arch}" ]]; then
		rows+=(--row "bootstrap-list:etc/bootstrap.list=$BUILD/bootstrap-${arch}/etc/bootstrap.list")
		local program
		for program in "$BUILD/bootstrap-${arch}"/libexec/*; do
			[[ -f "$program" ]] || continue
			rows+=(--row "program:libexec/$(basename "$program")=$program")
		done
	fi
	(cd "$REPO_ROOT/src/tools/sign-manifest" && cargo run --quiet -- \
		--profile test-trust --product LiberSystem --arch "${arch:-x86_64}" --source boot-medium \
		--release "$release" --volume-uuid 00000000000000000000000000000000 \
		"${rows[@]}" --out "$out") >&2 || die "the boot medium's manifest could not be signed"
	ensure_dir "$image" ::/etc
	stamp_epoch "$out"
	mcopy -i "$image" "$out" ::/etc/boot.manifest2
	rm -f "$out"
}

# WHICH volume this boot medium is paired with.
#
# The loader has read `etc/system-volume.uuid` since P02M0129 and nothing ever wrote it, so every
# image this tree built took the "the boot medium names no system volume" fallback and said so in
# its own boot log - a mechanism that looked implemented and was dead. Two LiberSystem disks in one
# machine then let the firmware's block-handle order decide which one booted.
#
# The value comes from the sidecar `mkpackages` writes from the same `FormatOpts.uuid` the
# superblock got, and `assert_pairing_matches_volume` checks it against the superblock actually on
# the image - because a pairing file naming a volume the image does not contain is WORSE than none:
# the loader then declines the volume that is really there.
stage_volume_pairing() {
	local image="$1" arch="${2:-x86_64}"
	local uuid_file="$BUILD/system-volume-${arch}.uuid"
	local volume="$BUILD/system-volume-${arch}.img"
	if [[ ! -f "$uuid_file" ]]; then
		# A MEDIUM CARRYING A VOLUME AND NO PAIRING IS A BUILD ERROR, not a warning.
		#
		# That combination is the one the loader cannot resolve safely: it finds a LiberFS volume, has
		# nothing naming which one it should be, and falls back to the firmware's block-handle order -
		# which is the defect this whole mechanism exists to remove. Advisory was the wrong strength
		# for the one case it was written for.
		#
		# A medium that deliberately names nothing - the shipping ISO stages no set - has no volume
		# either, and that stays permitted and says so.
		if [[ -f "$volume" ]]; then
			die "the medium carries $volume and no pairing file at $uuid_file - the loader would fall back to firmware enumeration order, which is what the pairing exists to stop"
		fi
		echo "mkimage: no system volume on this medium, so it names none" >&2
		return 0
	fi
	assert_pairing_matches_volume "$uuid_file" "$BUILD/system-volume-${arch}.img"
	# `::/etc` already exists when the bootstrap set was staged, and when the boot manifest was
	# written before this - which is every medium, since P02M0143 gave them all one.
	ensure_dir "$image" ::/etc
	stamp_epoch "$uuid_file"
	mcopy -i "$image" "$uuid_file" ::/etc/system-volume.uuid
}

# The pairing file and the volume's own superblock must agree. Checked HERE, in the image gate,
# rather than at boot: at boot the only thing the loader can do about a mismatch is decline the
# volume, which is the failure this is meant to prevent.
#
# The uuid is 16 raw bytes at offset 80 of the LiberFS superblock, which is block 0 of the image.
assert_pairing_matches_volume() {
	local uuid_file="$1" volume="$2"
	# A pairing naming a volume that is not on THIS medium is legitimate - that is the multi-disk
	# case the mechanism is for, an ESP naming a volume on another disk - so there is nothing to
	# compare and nothing wrong. Said out loud rather than left as a bare `return 0`, which reads as
	# neither a decision nor an oversight.
	[[ -f "$volume" ]] || return 0
	local declared actual
	declared="$(tr -d '[:space:]-' <"$uuid_file" | tr 'A-F' 'a-f')"
	actual="$(dd if="$volume" bs=1 skip=80 count=16 status=none | od -An -tx1 | tr -d ' \n')"
	if [[ "$declared" != "$actual" ]]; then
		die "the pairing file names volume $declared but the system volume's superblock says $actual - the loader would decline the volume that is actually on this image"
	fi
}

make_iso() {
	local kernel="$1" test_medium="${2:-0}"
	local final="$BUILD/$SLUG.iso" out="$BUILD/$SLUG.iso.$$.candidate"
	if [[ "$test_medium" == "1" ]]; then
		final="$BUILD/$SLUG-test.iso"
		out="$BUILD/$SLUG-test.iso.$$.candidate"
	fi
	CANDIDATES+=("$out")
	local iso_root="$BUILD/iso_root"
	rm -f "$out"

	local staged
	staged="$(stage_kernel "$kernel")"
	verify_boot_artifacts "$staged"

	# The FAT El Torito boot image. OVMF has no ISO9660 driver, so everything the
	# loader reads (the kernel and the packages) must live on this FAT filesystem -
	# the volume the loader is booted from - alongside the loader itself. The ISO
	# around it only carries this image as its UEFI El Torito boot entry.
	local efi_img="$BUILD/efiboot.img"
	local bytes total
	# TWO media, one builder. The SHIPPING ISO carries the system VOLUME - a LiberFS image the
	# running system copies into memory, because a CD cannot be written - and no factory archive.
	# The TEST ISO carries the archive instead, because the x86_64 kernel suite boots this exact
	# artifact and reads `volume.pkg` twice: as the source its fixture volume is built from, and as
	# the table of expected file contents.
	#
	# The split is the point of the item, and it was briefly collapsed on the belief that nothing
	# booted the archive-carrying ISO. The device-tree runners do build their own ESP - but the
	# x86_64 runner calls this script and boots what it returns, which is how a shipping medium and
	# a test medium had become one artifact in the first place.
	local payload="$BUILD/system-volume-x86_64.img" payload_name="system-volume.img"
	if [[ "$test_medium" == "1" ]]; then
		# Architecture-qualified in the build directory, staged under the plain name the kernel
		# looks it up by. The unqualified BUILD file no longer exists: every architecture wrote it,
		# so it held whichever ran last.
		payload="$BUILD/volume-x86_64.pkg"
		payload_name="$VOLUME_PACKAGE"
		[[ -f "$payload" ]] || die "testiso: no volume package at $payload (run \`just packages\`)"
	else
		[[ -f "$payload" ]] || die "iso: no system volume at $payload (run \`just system-volume\`)"
	fi
	# The init package is counted whether or not it is staged: it is the smaller of the two payloads
	# and over-sizing a FAT image by a few megabytes is cheaper than getting it wrong.
	bytes=$(($(stat -c%s "$staged") + $(stat -c%s "$BUILD/init-x86_64.pkg") + $(stat -c%s "$payload") + $(stat -c%s "$LOADER_EFI")))
	# FAT overhead + slack, rounded up to a whole MiB (min 32 MiB).
	total=$(((bytes + 16 * 1024 * 1024) / (1024 * 1024) + 1))
	((total < 32)) && total=32
	rm -f "$efi_img"
	truncate -s "${total}M" "$efi_img"
	mformat -i "$efi_img" -N "${MTOOLS_FAT_SERIAL#0x}" ::
	mmd -i "$efi_img" ::/EFI ::/EFI/BOOT
	stamp_epoch "$LOADER_EFI" "$staged"
	mcopy -i "$efi_img" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$efi_img" "$staged" ::/kernel
	# The TEST medium carries the bootstrap set as FILES, the same way the disk image's ESP does
	# and the same way the system volume does. It needs its own copy because it carries the factory
	# archive rather than a volume, so there is no volume list to read - but it gets one by the
	# same mechanism rather than as a packaged `init.pkg`.
	#
	# The SHIPPING medium needs none: its system volume is right there on the same filesystem and
	# names its own programs, and a medium whose volume is unreadable has nothing else to boot.
	if [[ "$test_medium" == "1" ]]; then
		stage_bootstrap_files "$efi_img" x86_64
		stage_boot_manifest "$efi_img" "$staged" x86_64 "$payload" "$payload_name"
	else
		stage_boot_manifest "$efi_img" "$staged" "" "$payload" "$payload_name"
		# THE SHIPPING MEDIUM ONLY. It carries the system volume as a file on this same filesystem,
		# so naming that volume is true of it. The TEST medium carries the factory archive and no
		# volume at all - a pairing file there would name a volume the medium does not have, which
		# is the one case worse than no pairing: the loader would decline whatever volume it did
		# find rather than fall back cleanly.
		stage_volume_pairing "$efi_img" x86_64
	fi
	stamp_epoch "$payload"
	mcopy -i "$efi_img" "$payload" "::/$payload_name"

	rm -rf "$iso_root"
	mkdir -p "$iso_root/boot"
	cp "$efi_img" "$iso_root/boot/efiboot.img"

	# UEFI-only El Torito: no BIOS boot entry. The EFI image is also exposed as a
	# GPT partition so the ISO boots when dd'd to a USB stick.
	# Every date in the volume descriptors set from the build epoch rather than from the clock, and
	# the file dates with them - otherwise two builds of identical content differ in the seconds
	# they were made.
	local iso_date
	iso_date="$(date -u -d "@$SOURCE_DATE_EPOCH" +%Y%m%d%H%M%S00)"
	# The mkisofs-compatible spellings; `-volume_date` is xorriso's own command syntax and is not
	# accepted under `-as mkisofs`, which fails the whole invocation. Measured on xorriso 1.5.6:
	# with these two, two builds of the same tree produce byte-identical ISOs even when the source
	# files' mtimes have moved in between.
	xorriso -as mkisofs -quiet \
		--efi-boot boot/efiboot.img \
		-efi-boot-part --efi-boot-image \
		--protective-msdos-label \
		--set_all_file_dates "=$SOURCE_DATE_EPOCH" \
		--modification-date="$iso_date" \
		"$iso_root" -o "$out" 2>/dev/null ||
		die "xorriso could not build the ISO"
	[[ -s "$out" ]] || die "xorriso produced no ISO at $out"
	mv "$out" "$final"

	info "wrote $final"
	echo "$final"
}

# build a raw GPT disk image for a USB stick / SD card / hard disk
make_img() {
	local kernel="$1" size="${2:-64M}" final="$BUILD/$SLUG.img" out="$BUILD/$SLUG.img.$$.candidate"

	mkdir -p "$BUILD"
	rm -f "$out"
	truncate -s "$size" "$out"

	# GPT with two partitions: an EFI System Partition (ef00, FAT) holding the loader and the
	# boot fallback copies, and a LiberFS system volume holding everything else.
	#
	# The second partition is what makes this an installed system rather than a boot medium. The
	# storage service finds it by the LiberFS type GUID rather than by device order, and the
	# loader finds it by its superblock - firmware exposes a partition as its own block handle, so
	# LBA 0 of that handle is the volume's own start (P02M0108).
	#
	# The ESP is fixed at 32 MiB: it needs the loader, the kernel and the init fallback, and every
	# byte beyond that is a byte the system volume does not get.
	local esp_end=$((2048 + 32 * 1024 * 1024 / 512 - 1))
	sgdisk "$out" -n "1:2048:$esp_end" -t 1:ef00 -c 1:ESP -u "1:$GPT_ESP_GUID" >/dev/null
	sgdisk "$out" -n 2:0:0 -t 2:4C424653-0001-4000-8000-4C6962657246 -c 2:system -u "2:$GPT_SYSTEM_GUID" >/dev/null
	# The disk GUID last, so it is not re-drawn by the partition edits above.
	sgdisk "$out" -U "$GPT_DISK_GUID" >/dev/null

	# read back the ESP's exact start and length (mtools cannot parse GPT, so we
	# build the FAT filesystem as a standalone image and splice it into place).
	local esp_start esp_sectors
	esp_start="$(sgdisk -i 1 "$out" | awk '/^First sector:/ {print $3}')"
	esp_sectors="$(sgdisk -i 1 "$out" | awk '/^Partition size:/ {print $3}')"

	local esp="$BUILD/esp.img"
	rm -f "$esp"
	truncate -s "$((esp_sectors * 512))" "$esp"

	local staged
	staged="$(stage_kernel "$kernel")"
	verify_boot_artifacts "$staged"

	mformat -i "$esp" -N "${MTOOLS_FAT_SERIAL#0x}" ::
	mmd -i "$esp" ::/EFI ::/EFI/BOOT
	stamp_epoch "$LOADER_EFI" "$staged"
	mcopy -i "$esp" "$LOADER_EFI" ::/EFI/BOOT/BOOTX64.EFI
	mcopy -i "$esp" "$staged" ::/kernel
	# The bootstrap set as FILES, not as a packaged archive.
	#
	# This is the recovery path for a machine whose system volume is missing or unreadable, and it
	# is now the same shape as the volume's own: `etc/bootstrap.list` naming programs under
	# `libexec/`, assembled by the same loader code. `init.pkg` was the second mechanism for this
	# one job, and the only one of the two whose programs could not be replaced individually.
	stage_bootstrap_files "$esp" x86_64
	stage_boot_manifest "$esp" "$staged" x86_64
	stage_volume_pairing "$esp" x86_64
	# No volume archive: the system volume is a filesystem in partition 2, and the archive exists
	# only as the kernel test suite's fixture (P02M0108).

	# splice the populated FAT filesystem into the ESP region of the disk
	dd if="$esp" of="$out" bs=512 seek="$esp_start" conv=notrunc status=none
	rm -f "$esp"

	# Lay the system volume into partition 2. Built by `just system-volume`, which runs after the
	# kernel so the image carries the kernel that was just linked.
	local volume="$BUILD/system-volume-x86_64.img"
	if [[ -f "$volume" ]]; then
		local sys_start sys_sectors
		sys_start="$(sgdisk -i 2 "$out" | awk '/^First sector:/ {print $3}')"
		sys_sectors="$(sgdisk -i 2 "$out" | awk '/^Partition size:/ {print $3}')"
		local volume_sectors=$(($(stat -c%s "$volume") / 512))
		if ((volume_sectors > sys_sectors)); then
			echo "mkimage: the system volume ($((volume_sectors / 2048)) MiB) does not fit the partition ($((sys_sectors / 2048)) MiB); build a larger image" >&2
			exit 1
		fi
		dd if="$volume" of="$out" bs=512 seek="$sys_start" conv=notrunc status=none
	else
		echo "mkimage: no system volume at $volume; the image has an empty system partition" >&2
	fi
	mv "$out" "$final"

	info "wrote $final ($size, GPT: ESP)"
	echo "$final"
}

cmd="${1:-}"
[[ $# -ge 2 ]] || die "usage: mkimage.sh {iso|img} <kernel-elf> [size]"
kernel="$2"
[[ -f "$kernel" ]] || die "kernel ELF not found: $kernel"
kernel="$(realpath -m "$kernel")"

mkdir -p "$BUILD"
command -v flock >/dev/null
command -v sha256sum >/dev/null
exec 9>"$BUILD/$SLUG.image.lock"
flock 9

case "$cmd" in
iso)
	output="$BUILD/$SLUG.iso"
	mode_input="iso"
	;;
testiso)
	output="$BUILD/$SLUG-test.iso"
	mode_input="testiso"
	;;
img)
	output="$BUILD/$SLUG.img"
	mode_input="img:${3:-64M}"
	;;
*) die "unknown subcommand '$cmd' (expected 'iso', 'testiso' or 'img')" ;;
esac

# The same manifest-driven check as the image builders below, run before the cache key is
# computed - hashing inputs that do not all exist would cache a decision made on a partial tree.
# The kernel is checked here as the ELF it was given; the builders re-check the stripped copy.
verify_boot_artifacts "$kernel"
key_file="$output.build-key"
digest_file="$output.build-digest"

# EVERY COPIED BYTE, not the subset that was easy to name.
#
# The key hashed the builder, the product configuration, the kernel, the loader, two packages and
# the system volume - and NOT the manifest that decides which artifacts go where, nor the fallback
# `bootstrap.list` and `libexec/` files this copies onto the ESP, nor the volume UUID sidecar. So a
# destination could move, the fallback set could change, or the pairing file could be corrected, and
# the key stayed equal: a cache HIT returning an image that does not implement the current manifest.
image_input_key() {
	printf 'format=liber-boot-image-input-v3\n'
	printf 'mode=%s\n' "$mode_input"
	printf 'strip=%s\n' "$STRIP"
	printf 'epoch=%s\n' "$SOURCE_DATE_EPOCH"
	tool_identity
	# The manifest's own bytes AND its normalized projection: the file catches an edit, the export
	# catches a generator change that produces a different layout from the same file.
	printf 'manifest=%s\n' "$(sha256sum "$REPO_ROOT/src/user/services/manifest.toml" | awk '{print $1}')"
	printf 'layout=%s\n' "$(printf '%s' "$manifest_rows" | sha256sum | awk '{print $1}')"
	# The fallback bootstrap set, file by file in a stable order - it is copied onto the ESP and was
	# in no key at all.
	local bootstrap_root="$BUILD/bootstrap-x86_64"
	if [[ -d "$bootstrap_root" ]]; then
		find "$bootstrap_root" -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum
	else
		printf 'bootstrap=absent\n'
	fi
	# The pairing sidecar, which decides which volume the medium declares itself paired with.
	local uuid_file="$BUILD/system-volume-x86_64.uuid"
	if [[ -f "$uuid_file" ]]; then
		sha256sum "$uuid_file"
	else
		printf 'pairing=absent\n'
	fi
	# BOTH payloads are hashed, whichever medium is being built: the shipping ISO carries the
	# system volume and the test ISO the archive, and keying on only one would serve a stale image
	# whenever the other changed. `mode=` above already separates the two outputs.
	sha256sum "$0" "$REPO_ROOT/product.conf" "$kernel" "$LOADER_EFI" "$BUILD/init-x86_64.pkg" "$BUILD/volume-x86_64.pkg" "$BUILD/system-volume-x86_64.img"
}

key="$(image_input_key | sha256sum | awk '{print $1}')"
# AND THE OUTPUT IS VERIFIED ON A HIT. A matching key used to return the existing file without
# looking at it, so a truncated, hand-edited or half-written image was served as current - the key
# describes the INPUTS and says nothing about the bytes that were produced from them.
if [[ -f "$output" && -f "$key_file" && -f "$digest_file" && "$(<"$key_file")" == "$key" ]]; then
	actual_digest="$(sha256sum "$output" | awk '{print $1}')"
	if [[ "$actual_digest" == "$(<"$digest_file")" ]]; then
		info "cache hit $output"
		echo "$output"
		exit 0
	fi
	info "cache key matches but $output does not match its recorded digest - rebuilding"
fi
info "cache miss $output; rebuilding"

case "$cmd" in
iso) make_iso "$kernel" 0 ;;
testiso) make_iso "$kernel" 1 ;;
img) make_img "$kernel" "${3:-64M}" ;;
esac
# The key is recomputed AFTER assembly and must still agree. Producers are not covered by this
# script's lock, so an input can be replaced while the image is being written - and the record would
# then describe bytes the image was not built from.
after="$(image_input_key | sha256sum | awk '{print $1}')"
[[ "$after" == "$key" ]] || die "an input changed while the image was being assembled; nothing was cached - build again with the tree still"
printf '%s\n' "$(sha256sum "$output" | awk '{print $1}')" >"$digest_file.tmp.$$"
mv "$digest_file.tmp.$$" "$digest_file"
printf '%s\n' "$key" >"$key_file.tmp.$$"
mv "$key_file.tmp.$$" "$key_file"
