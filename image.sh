#!/usr/bin/env bash
# Build bootable images.
#
# One command over three formats, because they are three outputs of one build rather than three
# procedures: the ISO is a live medium, the raw image an installed one, and QCOW2 the same raw
# image stored sparsely.

SCRIPT_NAME=image.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
source "$SRC_DIR/tools/evidence.sh"
started=$SECONDS

# THE SHIPPING IMAGES ARE BUILT WORK. Inside a run, each ISO this script assembles as the release
# plan's image producer is stored in the run - immutable, with its build receipts - and published
# under its producer key, so the gates that boot it read the run's artifact and bind their result to
# its digest. A gate that assembles media of its own runs with `LIBER_GATE_KEY` set and is not the
# producer: it publishes nothing here and stores nothing over the run's artifact.
publish_image_evidence() {
	local mode="$1" iso="$2" id stored
	evidence_active || return 0
	[[ -z "${LIBER_GATE_KEY:-}" ]] || return 0
	case "$mode" in
	enforcing-required) id="image.libersystem-iso" ;;
	no-iommu) id="image.libersystem-no-iommu-iso" ;;
	harness) id="image.libersystem-dev-iso" ;;
	*) return 0 ;;
	esac
	stored="$(evidence_store_artifact "$(basename "$iso")" "$iso" "$iso.build-key" "$iso.build-digest")" || return 0
	local -a outputs=(--output "$stored")
	# The bootable system volume the medium was paired with, for the gate that boots it beside the
	# medium behind a translating controller: stored once, with the shipping image.
	if [[ "$mode" == enforcing-required && -f "$BUILD_DIR/boot/system-volume-bootable-x86_64.img" ]]; then
		local volume
		volume="$(evidence_store_artifact system-volume-bootable-x86_64.img "$BUILD_DIR/boot/system-volume-bootable-x86_64.img" "$BUILD_DIR/boot/system-volume-bootable-x86_64.uuid")" && outputs+=(--output "$volume")
	fi
	evidence_publish "$id / x86_64 / host / shared-image" "image.sh" passed "$((SECONDS - started))" --input "$kernel" "${outputs[@]}"
}

FORMATS_ALL="iso img qcow2"

help() {
	usage_and_exit <<EOF
usage: image.sh [--format FMT[,FMT...]] [--size SIZE] [--strip none|debug|all] [--dma-mode MODE]

Builds bootable images into .build/boot/. With no --format: all three formats.

  --format FMT   iso | img | qcow2 | all      (default: all)
  --size SIZE    disk size for img/qcow2, truncate-style: 128M, 1G   (default: 128M)
  --strip LEVEL  none (keep full debug kernel), debug (drop DWARF, keep symbols), or all (smallest)
                                                                  (default: all)
  --dma-mode M   enforcing-required | no-iommu | harness | all   (default: all)
                 Which DMA mode the medium is SIGNED for. A release is two distributable x86_64
                 artifacts, because the value is frozen when the image is assembled and
                 $(run.sh --no-iommu) selects an image rather than modifying one:
                   enforcing-required -> libersystem.iso / .img / .qcow2   (the default machine)
                   no-iommu           -> libersystem-no-iommu.*            (./run.sh --no-iommu)
                   harness            -> libersystem-dev.*   a development image signed with no
                                         mode; its boot takes the mode from the harness carrier
                 $(all) assembles the first two.
  -h, --help     this text

formats:
  iso    a LiveCD: carries a LiberFS system volume that is copied into memory at boot, so the
         medium is never written and the machine needs no disk
  img    an installed system: a GPT disk with an ESP holding the loader and a recovery copy of the
         bootstrap programs, and a LiberFS system volume holding the kernel and everything else
  qcow2  the same disk, stored sparsely - a fraction of the raw size to keep or copy

examples:
  ./image.sh                              # ISO, IMG and QCOW2
  ./image.sh --format img --size 1G
  ./image.sh --format iso --strip none    # development image with full kernel debug data

Note the size suffix is truncate's: 1G, not 1GB.
EOF
}

formats=()
size="128M"
strip="all"
dma_modes=()

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--format)
		[[ $# -ge 2 ]] || die "--format needs a value"
		picked_raw="$(parse_list "$2" format "$FORMATS_ALL")"
		mapfile -t picked <<<"$picked_raw"
		formats+=("${picked[@]}")
		shift 2
		;;
	--size)
		[[ $# -ge 2 ]] || die "--size needs a value"
		[[ "$2" =~ ^[0-9]+[KMGT]?$ ]] || die "size '$2' is not truncate-style (128M, 1G - no trailing B)"
		size="$2"
		shift 2
		;;
	--strip)
		[[ $# -ge 2 ]] || die "--strip needs a value"
		[[ "$2" == none || "$2" == debug || "$2" == all ]] || die "strip level must be 'none', 'debug' or 'all'"
		strip="$2"
		shift 2
		;;
	--dma-mode)
		[[ $# -ge 2 ]] || die "--dma-mode needs a value"
		case "$2" in
		enforcing-required | no-iommu | harness) dma_modes+=("$2") ;;
		all) dma_modes+=(no-iommu enforcing-required) ;;
		*) die "--dma-mode takes enforcing-required, no-iommu, harness or all, got '$2'" ;;
		esac
		shift 2
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

[[ ${#formats[@]} -eq 0 ]] && formats=(iso img qcow2)
# THE ENFORCING IMAGE IS ASSEMBLED LAST, so the bootable volume left in the tree is the one the
# default medium was paired with - which is what the gates that recompute the default image's key
# compare against.
[[ ${#dma_modes[@]} -eq 0 ]] && dma_modes=(no-iommu enforcing-required)

kernel="$BUILD_DIR/cargo/kernel/x86_64-unknown-none/debug/kernel"
slug="$(grep -m1 '^PRODUCT_NAME=' "$REPO_ROOT/product.conf" | cut -d'"' -f2 | tr '[:upper:]' '[:lower:]')"

for dma_mode in "${dma_modes[@]}"; do
	# Every image needs the whole system built first, and the volume needs the kernel on it - signed
	# for THIS mode, because the medium and the volume it carries are one signed set.
	# --kernel-on-volume: a shipping medium's loader reads the kernel off the system volume, which
	# is the point of shipping one. Builds leave it off so a test run's ESP kernel is the one that
	# boots.
	LIBER_KERNEL_STRIP="$strip" "$REPO_ROOT/build.sh" --arch x86_64 --kernel-on-volume --dma-mode "$dma_mode" >&2
	[[ -f "$kernel" ]] || die "no kernel at $kernel"
	case "$dma_mode" in
	enforcing-required) suffix="" ;;
	no-iommu) suffix="-no-iommu" ;;
	harness) suffix="-dev" ;;
	esac
	for fmt in "${formats[@]}"; do
		case "$fmt" in
		iso)
			(cd "$SRC_DIR" && LIBER_DMA_MODE="$dma_mode" STRIP="$strip" harness/mkimage.sh iso "$kernel")
			publish_image_evidence "$dma_mode" "$BUILD_DIR/boot/$slug$suffix.iso"
			;;
		img) (cd "$SRC_DIR" && LIBER_DMA_MODE="$dma_mode" STRIP="$strip" harness/mkimage.sh img "$kernel" "$size") ;;
		qcow2)
			(cd "$SRC_DIR" && LIBER_DMA_MODE="$dma_mode" STRIP="$strip" harness/mkimage.sh img "$kernel" "$size")
			raw="$BUILD_DIR/boot/$slug$suffix.img"
			[[ -f "$raw" ]] || die "no raw image at $raw"
			qemu-img convert -f raw -O qcow2 "$raw" "${raw%.img}.qcow2"
			note "wrote ${raw%.img}.qcow2"
			;;
		esac
	done
done
