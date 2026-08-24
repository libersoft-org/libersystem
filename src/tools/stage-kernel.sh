#!/usr/bin/env bash
# Stage a kernel ELF with one well-defined stripping policy.

set -euo pipefail

if [[ $# -ne 3 ]]; then
	echo "usage: stage-kernel.sh {none|debug|all} <source-elf> <destination>" >&2
	exit 2
fi

mode="$1"
source_elf="$2"
destination="$3"
strip_tool="${KERNEL_STRIP_TOOL:-objcopy}"

case "$mode" in
none | debug | all) ;;
*)
	echo "stage-kernel.sh: invalid strip level '$mode' (expected 'none', 'debug' or 'all')" >&2
	exit 2
	;;
esac

[[ -f "$source_elf" ]] || {
	echo "stage-kernel.sh: kernel ELF not found: $source_elf" >&2
	exit 1
}

mkdir -p "$(dirname "$destination")"
candidate="$destination.$$.candidate"
cleanup() {
	rm -f -- "$candidate"
}
trap cleanup EXIT

# Copy first and transform the candidate in place. In `none` mode this is intentionally the only
# operation: even objcopy without a strip flag can rewrite ELF metadata, so it would not preserve
# the development kernel byte-for-byte.
cp -- "$source_elf" "$candidate"
case "$mode" in
none) ;;
debug | all)
	command -v "$strip_tool" >/dev/null 2>&1 || {
		echo "stage-kernel.sh: '$strip_tool' is required for --strip $mode" >&2
		exit 1
	}
	"$strip_tool" "--strip-$mode" "$candidate"
	;;
esac

mv -f -- "$candidate" "$destination"
trap - EXIT
