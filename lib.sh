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
# This is the answer to "I changed one tool - what do I run": the tags its layer lives in, not the
# whole suite. `test.sh --for-tool NAME` resolves it, and `check-dynamic-report.sh` prints the same
# command into the last column of docs/DYNAMIC_EXECUTABLES.tsv, so the two cannot drift.
#
# The waves themselves came from the dynamic-linking migration and are kept because they group tools
# by the layer they actually touch, which is the same grouping a scoped test wants.
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

# The tags to test a tool with, or nothing if it is not a staged dynamic tool.
tool_tags() {
	local wave="${TOOL_WAVES[$1]:-}"
	[[ -n "$wave" ]] || return 1
	printf '%s\n' "${WAVE_TAGS[$wave]}"
}

# Which part of the tree a change lives in, and what that part is tested with.
#
# The same idea as the wave table above, for everything that is not a tool: a library, a service, a
# driver, the loader, the kernel. Longest matching prefix wins, and an EMPTY value means the whole
# suite - which is also what an unrecognised path gets, because the failure that matters here is
# testing too little.
#
# These are deliberately wider than "the thing that changed". A codec library is reached through a
# service and a volume, so its tags include those; an ABI or wire change is reached by everything,
# so it gets the lot. Narrower would be faster and would stop catching things.
declare -A COMPONENT_TAGS=(
	# Userspace libraries, by the layer their callers live in.
	["src/user/libs/audio"]="audio,service,process,storage"
	["src/user/libs/image"]="image,imgview,service,process,storage"
	["src/user/libs/compression"]="image,service,process,storage"
	["src/user/libs/display"]="display,image,service,process"
	["src/user/libs/input"]="input,mouse,display,service,process"
	["src/user/libs/terminal"]="lico,console,shell,service,process"
	# A protocol, a client or the IPC plumbing is spoken by everything that talks to a service.
	["src/user/libs/protocol"]="service,process,ipc,channel,storage,display,audio,network"
	["src/user/libs/clients"]="service,process,ipc,channel,storage,display,audio,network"
	["src/user/libs/ipc"]="service,process,ipc,channel"
	# Services.
	["src/user/services/storage"]="storage,filesystem,volume,service,process"
	["src/user/services/system_manager"]="service,process,permission,component"
	["src/user/services/core"]="service,process,config,console,display,input,audio,network,permission,storage"
	# Programs and the runtime they are linked against.
	["src/user/apps"]="shell,console,service,process,storage,image,audio"
	["src/user/drivers"]="drivers,pci,usb,dma,interrupt,service,process"
	["src/user/runtime"]="dynamic,component,service,process"
	# Everything else that is not the kernel.
	["src/fs"]="filesystem,storage,volume"
	["src/loader"]="boot,dynamic,volume"
	# `src/boot` is the harness: the image builder, the QEMU runner, the test runner and the boot
	# scenarios. A change there can alter what the guest is and how it is judged, so it is tested
	# with everything - and so is anything that generates or packages what ships.
	["src/boot"]=""
	["src/tools"]=""
	["src/volume"]=""
	["src/term"]="console,shell,display,lico"
	["src/sdk"]="component,service,process"
	["src/wasm"]="component,service,process"
	# The kernel, the ABI and the wire formats are reached by everything, so they are tested with
	# everything. This is the one place a full sweep is the right answer rather than a reflex.
	["src/kernel"]=""
	["src/abi"]=""
	["src/bootproto"]=""
	["src/wire"]=""
	["src/proto"]=""
	["src/idl"]=""
)

# The tags a changed path is tested with. Prints nothing and returns 0 for "the whole suite";
# returns 1 only for a path that needs no kernel test at all.
component_tags() {
	local path="${1#./}" best="" best_len=0 prefix
	# A tool's own source answers through the wave table, so the two never disagree.
	if [[ "$path" == src/user/apps/tools/src/*.rs ]]; then
		local tool="${path##*/}"
		tool_tags "${tool%.rs}" && return 0
	fi
	# What genuinely cannot change a built artifact, and so cannot be caught by booting one.
	#
	# This list was once `src/tools/* | docs/* | *.md | *.sh`, which was wrong in a way worth keeping
	# written down: it excused `src/boot/mkimage.sh` (builds the image), `src/boot/qemu-run.sh` and
	# `src/boot/test-kernel.sh` (boot and run the guest) and `src/tools/mkpackages` - which is in
	# VOLUME_SOURCES, so its output IS the system volume. "Change the thing that builds the system,
	# test nothing" is exactly the false green this whole mechanism exists to prevent, and it
	# contradicted M0148's own invariant that build and image tooling select everything.
	#
	# What is left here is documentation and the gates themselves: a gate that breaks fails loudly on
	# its own, and a Markdown file changes no byte of any artifact.
	case "$path" in
	docs/* | *.md) return 1 ;;
	src/tools/check-*.sh | src/boot/check-*.sh) return 1 ;;
	commit.sh | git-commits.sh | fix-rights.sh | dev.sh) return 1 ;;
	esac
	for prefix in "${!COMPONENT_TAGS[@]}"; do
		if [[ "$path" == "$prefix"/* || "$path" == "$prefix" ]] && ((${#prefix} > best_len)); then
			best="${COMPONENT_TAGS[$prefix]}"
			best_len=${#prefix}
		fi
	done
	# Unrecognised means unknown reach, and unknown reach is tested with everything.
	((best_len > 0)) || return 0
	printf '%s\n' "$best"
}

# The union of the tags for a set of changed paths. Prints nothing if any of them needs everything.
tags_for_paths() {
	local path tags all="" whole=0 any=0
	for path in "$@"; do
		if tags="$(component_tags "$path")"; then
			any=1
			[[ -n "$tags" ]] || whole=1
			all="$all,$tags"
		fi
	done
	((any)) || return 1
	((whole == 0)) || return 0
	printf '%s\n' "$all" | tr ',' '\n' | grep -v '^$' | sort -u | paste -sd,
}

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
