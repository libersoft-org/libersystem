#!/bin/bash
# The portable static target: compile the pinned loader configuration and archive it, for one target.
#
# WHY A SCRIPT AND NOT CMAKE'S OWN STATIC OPTION. Upstream's `BUILD_STATIC_LOADER` was renamed
# `APPLE_STATIC_LOADER` precisely because the name implied a portability it never had, and non-Apple
# use is made to fail. None of this system's three targets can obtain a static loader from it, so the
# portable static target is this milestone's own deliverable.
#
# WHAT "DONE" MEANS HERE, AND WHAT IT DOES NOT. An archive does not resolve its external symbols, so
# this proves COMPILATION and ARCHIVING and claims nothing about linking. The strict converging link
# needs the profile sysroot, the foreign-ABI substrate and the platform port - all of which come
# after pass 1 - and it belongs to pass 2, where the derived pin already records it.
#
# THE UNDEFINED SYMBOLS IN THE ARCHIVE ARE THE DELIVERABLE'S OTHER HALF. They are the candidate
# surface pass 1 measures: what this configuration asks of a system, stated by the objects rather
# than guessed from the sources.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

usage() {
	cat >&2 <<EOF
usage: build-foreign-static.sh --arch ARCH [--out DIR]

  --arch ARCH   x86_64 | aarch64 | riscv64
  --out DIR     where the objects and the archive go (default: .build/foreign/ARCH)

Needs the pinned sources unpacked under .build/foreign/src-loader and .build/foreign/src-headers.
The build FETCHES NOTHING: see docs/DEPENDENCY_POLICY.md.
EOF
	exit 2
}

arch=""
out=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--arch)
		[[ $# -ge 2 ]] || usage
		arch="$2"
		shift 2
		;;
	--out)
		[[ $# -ge 2 ]] || usage
		out="$2"
		shift 2
		;;
	-h | --help) usage ;;
	*) usage ;;
	esac
done
[[ -n "$arch" ]] || usage
out="${out:-$ROOT/.build/foreign/$arch}"

STORE="$ROOT/.build/foreign"
LOADER="$STORE/src-loader"
HEADERS="$STORE/src-headers"
for dir in "$LOADER" "$HEADERS"; do
	[[ -d "$dir" ]] || {
		echo "build-foreign-static: $dir is missing - unpack the pinned archives there once, by hand" >&2
		echo "build-foreign-static: the build fetches nothing; see docs/DEPENDENCY_POLICY.md" >&2
		exit 1
	}
done

# THE ABI COMES FROM THE SAME PLACE THE RUST SIDE'S DOES. Two descriptions of one ABI is how a C
# object and a Rust object come to disagree about a type, so these mirror the cross files exactly.
case "$arch" in
x86_64)
	target="x86_64-unknown-none-elf"
	abi=(-mno-mmx -msse -msse2 -mno-sse3 -mno-ssse3 -mno-sse4.1 -mno-sse4.2 -mno-avx -mno-avx2 -mfxsr -mno-red-zone)
	;;
aarch64)
	target="aarch64-unknown-none"
	abi=(-mgeneral-regs-only)
	;;
riscv64)
	target="riscv64-unknown-none-elf"
	abi=(-march=rv64gc -mabi=lp64d -mcmodel=medany)
	;;
*)
	echo "build-foreign-static: unsupported arch '$arch'" >&2
	exit 2
	;;
esac

# THE OPTION SET, AS THE BOOTSTRAP PIN FREEZES IT. Every WSI backend off because LiberSystem is none
# of those window systems; assembly off because per-architecture dispatch stubs are exactly the
# relocation surface this milestone measures; the unsafe file search off because the ambient search
# is what the discovery item replaces.
defines=(
	-D__LiberSystem__
	-DHAVE_ALLOCA_H
	-DVK_ENABLE_BETA_EXTENSIONS
	# THE QUOTES HAVE TO SURVIVE INTO THE COMPILER, which bare shell quoting does not do: the shell
	# strips them and the source then sees a bare path where it expects a string literal. These
	# values are only here because the upstream CMake would set them; what a configuration directory
	# means on this system is the discovery item's answer, not this script's.
	-DFALLBACK_CONFIG_DIRS='"/etc/xdg"'
	-DFALLBACK_DATA_DIRS='"/usr/local/share:/usr/share"'
	-DSYSCONFDIR='"/etc"'
	-DLOADER_ENABLE_LINUX_SORT=0
)

sources=(
	allocation cJSON debug_utils extension_manual loader_environment gpa_helper
	loader log loader_json settings terminator trampoline unknown_function_handling wsi
)

rm -rf "$out"
mkdir -p "$out"
objects=()
for name in "${sources[@]}"; do
	object="$out/$name.o"
	clang \
		--target="$target" \
		-ffreestanding -nostdlibinc -fPIC -O2 \
		"${abi[@]}" "${defines[@]}" \
		-isystem "$ROOT/src/foreign/bootstrap-sysroot/include" \
		-I "$LOADER/loader" -I "$LOADER/loader/generated" -I "$HEADERS/include" \
		-c "$LOADER/loader/$name.c" -o "$object"
	objects+=("$object")
done

archive="$out/libvulkan.a"
rm -f "$archive"
# `D` FOR DETERMINISTIC: zero timestamps, uids and modes in the archive members. Without it two
# archives of identical objects differ, and "builds reproducibly" becomes unprovable.
llvm-ar crsD "$archive" "${objects[@]}"

echo "build-foreign-static: $arch: ${#objects[@]} object(s) -> $archive"
echo "build-foreign-static: $arch: $(sha256sum "$archive" | cut -d' ' -f1)"
