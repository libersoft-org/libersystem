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

# The LOADER's triple, which is NOT the kernel's: it is a UEFI application on x86_64 and aarch64,
# and a hand-rolled ELF on riscv64. Checking for it under the kernel's triple is why a build that
# had just produced it was reported as missing.
loader_triple() {
	case "$1" in
	x86_64) echo x86_64-unknown-uefi ;;
	aarch64) echo aarch64-unknown-uefi ;;
	riscv64) echo riscv64gc-unknown-none-elf ;;
	*) die "no loader triple for '$1'" ;;
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

# The sources a built system volume reflects. Shared by build.sh, which records their digest, and
# test.sh, which refuses to run a suite against a build that does not match it. Defined HERE
# because both need it: keeping it in build.sh left test.sh computing a digest of nothing, which
# matched no stamp and refused every run.
# Which tool belongs to which wave, and what a wave is tested with.
#
# The waves came from the dynamic-linking migration and are the structure `check-dynamic-report.sh`
# organises docs/DYNAMIC_EXECUTABLES.tsv around: which tools were converted together, and what each
# group's shared-object footprint is.
#
# They are NOT a scoping mechanism, and used as one they were the measured problem M0148 opens with:
# wave 5 selects `image,audio,service,process,storage`, and because `service` and `process` are on a
# third of the suite each, that is 109 of 205 tests for a one-tool change. `./verify.sh` answers that
# question from the dependency graph instead.
declare -A TOOL_WAVES=()
for tool in echo uname uptime dmesg free lscpu lsmem lsirq lspci ptyecho readln script; do TOOL_WAVES[$tool]=1; done
for tool in cat write rm ls du mkdir rmdir snap volume lsvol lsblk; do TOOL_WAVES[$tool]=2; done
for tool in date log config set lsdev lsusb lssvc usage ps run perm start stop beep; do TOOL_WAVES[$tool]=3; done
for tool in ping ip nslookup tcp nc arp httpd ss; do TOOL_WAVES[$tool]=4; done
for tool in imgview imgconv audioconv play graphics_probe lico licoedit licoview; do TOOL_WAVES[$tool]=5; done
unset tool

declare -A WAVE_TAGS=()
WAVE_TAGS[1]='service,process,storage'
WAVE_TAGS[2]='service,process,storage'
WAVE_TAGS[3]='service,process,storage'
WAVE_TAGS[4]='service,process'
WAVE_TAGS[5]='image,audio,service,process,storage'

# Which part of the tree a change is verified with is NOT answered here any more.
#
# There was a hand-written table below this line - a path prefix mapped to a tag list - and it was
# wrong in both directions. It said `src/fs` was tested with `filesystem,storage,volume`, missing
# that the kernel and the loader both statically link LiberFS; and picking any tag from it that
# nearly every test carries (`process` is on 35% of the suite, `service` on 30%) collapsed a
# one-tool change to half the suite. A hand table cannot stay right as the tool count multiplies,
# and nothing tells you when it has stopped being right.
#
# `./verify.sh` answers the question now, from a model that is derived rather than written: crate
# directories and `[[bin]]` entries give ownership, the three Cargo dependency kinds and
# services/manifest.toml's providers give the edges, and what remains is declared once in
# src/tools/verify-model/model/registry.toml with a reason attached. See docs/todo/M0148.md.

VOLUME_SOURCES=(user fs wire abi proto idl tools/mkpackages)

# A digest of every source file a build reads, so "has it changed" is answered by CONTENT.
#
# Modification times cannot answer it. `mkpackages` skips a write whose bytes are unchanged, so an
# artifact can be older than a source it already reflects; and a `git checkout`, a commit hook or a
# formatter touches a source without changing a byte of it. Both happened in one day, and both made
# the suite refuse to run against a build that was already correct.
#
# Cheap enough to do on every check: a few thousand files hashed once, against test runs measured in
# tens of minutes.
source_digest() {
	local dir
	for dir in "$@"; do
		[[ -d "$SRC_DIR/$dir" ]] || continue
		find "$SRC_DIR/$dir" -name '*.rs' -o -name '*.toml' -o -name '*.lsidl' | sort
	done | xargs -r sha256sum 2>/dev/null | sha256sum | cut -d" " -f1
}

# `--help` for every script, built from a here-doc the script supplies.
usage_and_exit() {
	cat
	exit "${1:-0}"
}
