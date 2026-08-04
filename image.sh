#!/usr/bin/env bash
# Build bootable images.
#
# One command over three formats, because they are three outputs of one build rather than three
# procedures: the ISO is a live medium, the raw image an installed one, and QCOW2 the same raw
# image stored sparsely.

SCRIPT_NAME=image.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

FORMATS_ALL="iso img qcow2"

help() {
	usage_and_exit <<EOF
usage: image.sh [--format FMT[,FMT...]] [--size SIZE] [--strip debug|all]

Builds bootable images into .build/boot/. With no arguments: the ISO.

  --format FMT   iso | img | qcow2 | all      (default: iso)
  --size SIZE    disk size for img/qcow2, truncate-style: 128M, 1G   (default: 128M)
  --strip LEVEL  debug (drop DWARF, keep symbols) or all (smallest)  (default: debug)
  -h, --help     this text

formats:
  iso    a LiveCD: carries a LiberFS system volume that is copied into memory at boot, so the
         medium is never written and the machine needs no disk
  img    an installed system: a GPT disk with an ESP holding the loader and a recovery copy of the
         bootstrap programs, and a LiberFS system volume holding the kernel and everything else
  qcow2  the same disk, stored sparsely - a fraction of the raw size to keep or copy

examples:
  ./image.sh
  ./image.sh --format img --size 1G
  ./image.sh --format all --strip all

Note the size suffix is truncate's: 1G, not 1GB.
EOF
}

formats=()
size="128M"
strip="debug"

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
		[[ "$2" == debug || "$2" == all ]] || die "strip level must be 'debug' or 'all'"
		strip="$2"
		shift 2
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

[[ ${#formats[@]} -eq 0 ]] && formats=(iso)

# Every image needs the whole system built first, and the volume needs the kernel on it.
"$REPO_ROOT/build.sh" --arch x86_64 >&2

kernel="$BUILD_DIR/cargo/kernel/x86_64-unknown-none/debug/kernel"
[[ -f "$kernel" ]] || die "no kernel at $kernel"

for fmt in "${formats[@]}"; do
	case "$fmt" in
	iso) (cd "$SRC_DIR" && STRIP="$strip" boot/mkimage.sh iso "$kernel") ;;
	img) (cd "$SRC_DIR" && STRIP="$strip" boot/mkimage.sh img "$kernel" "$size") ;;
	qcow2)
		(cd "$SRC_DIR" && STRIP="$strip" boot/mkimage.sh img "$kernel" "$size")
		slug="$(grep -m1 '^PRODUCT_NAME=' "$REPO_ROOT/product.conf" | cut -d'"' -f2 | tr '[:upper:]' '[:lower:]')"
		raw="$BUILD_DIR/boot/$slug.img"
		[[ -f "$raw" ]] || die "no raw image at $raw"
		qemu-img convert -f raw -O qcow2 "$raw" "${raw%.img}.qcow2"
		note "wrote ${raw%.img}.qcow2"
		;;
	esac
done
