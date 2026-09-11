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
usage: build-foreign-static.sh --arch ARCH [--out DIR] [--sysroot WHICH]

  --arch ARCH      x86_64 | aarch64 | riscv64
  --out DIR        where the objects and the archive go (default: .build/foreign/ARCH)
  --sysroot WHICH  bootstrap (default) | profile

THE TWO SYSROOTS ARE NOT ALTERNATIVES AND THE DEFAULT IS NOT A PREFERENCE. The BOOTSTRAP one is what
the archives in the static-target pin were built against, so reproducing those digests has to name
it. The PROFILE one is sized by the inventory instead of by the option set, and building against it
is how the trim is PROVED to have removed only surface nobody required: the objects are the same
objects, or a declaration that was removed was one the configuration actually needed.

Needs the pinned sources unpacked under .build/foreign/src-loader and .build/foreign/src-headers.
The build FETCHES NOTHING: see docs/DEPENDENCY_POLICY.md.
EOF
	exit 2
}

arch=""
out=""
sysroot="bootstrap"
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
	--sysroot)
		[[ $# -ge 2 ]] || usage
		sysroot="$2"
		shift 2
		;;
	-h | --help) usage ;;
	*) usage ;;
	esac
done
[[ -n "$arch" ]] || usage
out="${out:-$ROOT/.build/foreign/$arch}"

case "$sysroot" in
bootstrap)
	include=("-isystem" "$ROOT/src/foreign/bootstrap-sysroot/include")
	;;
profile)
	# THE TARGET HEADER IS FORCE-INCLUDED AND NOT LEFT TO A SOURCE TO INCLUDE. Its static assertions
	# are the data model, and a model asserted in one translation unit describes one translation
	# unit; this way every object carries the check.
	include=("-isystem" "$ROOT/src/foreign/profile-sysroot/include" "-include" "$ROOT/src/foreign/profile-sysroot/arch/$arch.h")
	;;
*)
	echo "build-foreign-static: unknown sysroot '$sysroot' - bootstrap or profile" >&2
	exit 2
	;;
esac

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

# THE ABI COMES FROM THE SAME PLACE THE RUST SIDE'S DOES, and from the same place the image build's
# does. It was written out here until the image build needed it too; a third copy of one ABI is how a
# C object and a Rust object come to disagree about a type, so it moved to one file both read.
# shellcheck source=../foreign/profile-abi.sh
source "$ROOT/src/foreign/profile-abi.sh"
foreign_profile_abi "$arch" || exit 2
target="$foreign_triple"
abi=("${foreign_abi_flags[@]}")

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
	# LOADER_ENABLE_LINUX_SORT IS NOT DEFINED AT ALL, and `=0` was wrong: the source guards it with
	# `#if defined(...)`, so defining it to zero switched it ON. The inventory is what caught it - it
	# named `linux_read_sorted_physical_devices` and `linux_sort_physical_device_groups`, which live
	# in `loader_linux.c`, a source this configuration does not compile. A candidate surface holding
	# two symbols no object can ever provide is the inventory describing a build nobody made.
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
		"${include[@]}" \
		-I "$LOADER/loader" -I "$LOADER/loader/generated" -I "$HEADERS/include" \
		-c "$LOADER/loader/$name.c" -o "$object"
	objects+=("$object")
done

archive="$out/libvulkan.a"
rm -f "$archive"
# `D` FOR DETERMINISTIC: zero timestamps, uids and modes in the archive members. Without it two
# archives of identical objects differ, and "builds reproducibly" becomes unprovable.
llvm-ar crsD "$archive" "${objects[@]}"

echo "build-foreign-static: $arch: ${#objects[@]} object(s), $sysroot sysroot -> $archive"
echo "build-foreign-static: $arch: $(sha256sum "$archive" | cut -d' ' -f1)"
