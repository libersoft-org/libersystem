#!/usr/bin/env bash
# Shared by every entry-point script in this directory.
#
# The scripts are the build INTERFACE; the work still lives where it lived - `src/boot/mkimage.sh`,
# `src/boot/qemu-run.sh`, `src/boot/test-kernel.sh`, `src/boot/lab.py` and cargo. What these add is
# flags instead of names: the Justfile spelled every combination of architecture, mode and target
# into its own recipe and reached 123 of them, which is a discovery surface nobody reads.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$REPO_ROOT/src"
BUILD_DIR="$REPO_ROOT/.build"

ARCHS_ALL=(x86_64 aarch64 riscv64)

die() {
	echo "${SCRIPT_NAME:-$(basename "$0")}: $*" >&2
	exit 1
}

note() {
	echo "${SCRIPT_NAME:-$(basename "$0")}: $*" >&2
}

# Expand `all` and validate. Accepts repeated flags and comma-separated lists, so
# `--arch aarch64 --arch riscv64` and `--arch aarch64,riscv64` mean the same thing.
parse_list() {
	local raw="$1" what="$2" valid="$3" out=() item
	IFS=', ' read -r -a items <<<"$raw"
	for item in "${items[@]}"; do
		[[ -z "$item" ]] && continue
		if [[ "$item" == all ]]; then
			# shellcheck disable=SC2206
			out=($valid)
			break
		fi
		[[ " $valid " == *" $item "* ]] || die "unknown $what '$item' (valid: $valid, or 'all')"
		out+=("$item")
	done
	printf '%s\n' "${out[@]}"
}

# The rust target triple for an architecture. One place, because it was written out per recipe and
# a hard-coded x86_64 triple was still being passed for other architectures as recently as today.
target_triple() {
	case "$1" in
	x86_64) echo x86_64-unknown-none ;;
	aarch64) echo aarch64-unknown-none ;;
	riscv64) echo riscv64gc-unknown-none-elf ;;
	*) die "no target triple for '$1'" ;;
	esac
}

# Run a build step at most once per invocation.
#
# This replaces the Justfile's dependency graph, and it is the part of the move that has to be
# deliberate rather than incidental: `test-riscv64: test-preflight-riscv64 loader-riscv64` was a
# statement that both run first and exactly once. Getting it wrong is not theoretical - a disk
# image shipped carrying the TEST kernel because a volume was assembled before the file it read
# was written.
declare -A _ensure_done=()
ensure() {
	local key="$*"
	[[ -n "${_ensure_done[$key]:-}" ]] && return 0
	_ensure_done[$key]=1
	"$@"
}

# `--help` for every script, built from a here-doc the script supplies.
usage_and_exit() {
	cat
	exit "${1:-0}"
}
