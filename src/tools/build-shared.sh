#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
	echo "usage: $0 <target> <crate>..." >&2
	exit 2
fi

target="$1"
shift

case "$target" in
x86_64-unknown-none)
	emulation="elf_x86_64"
	rustflags="-C relocation-model=pic"
	;;
aarch64-unknown-none)
	emulation="aarch64elf"
	rustflags="-C relocation-model=pic"
	;;
riscv64gc-unknown-none-elf)
	emulation="elf64lriscv"
	rustflags="-C relocation-model=pic -C code-model=medium"
	;;
*)
	echo "build-shared: unsupported target '$target'" >&2
	exit 2
	;;
esac

lld="$(find "$(rustc --print sysroot)" -path '*/rust-lld' -type f -print -quit)"
if [[ -z "$lld" ]]; then
	echo "build-shared: rust-lld not found in the pinned toolchain" >&2
	exit 1
fi
command -v llvm-ar >/dev/null
command -v llvm-readelf >/dev/null
command -v llvm-strip >/dev/null

out_dir="user/shared/$target"
mkdir -p "$out_dir"

artifacts=()
for spec in "$@"; do
	if [[ "$spec" == *=* ]]; then
		artifact="${spec%%=*}"
		crate="${spec#*=}"
	else
		artifact="$spec"
		crate="$spec"
	fi
	if [[ "$crate" == "proto" ]]; then
		crate_dir="proto"
	else
		crate_dir="user/$crate"
	fi
	manifest="$crate_dir/Cargo.toml"
	if [[ ! -f "$manifest" ]]; then
		echo "build-shared: missing $manifest" >&2
		exit 1
	fi
	features=()
	if [[ "$artifact" == "liblsrt" ]]; then
		features=(--no-default-features --features shared-image)
	fi
	(cd "$crate_dir" && RUST_MIN_STACK=16777216 RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib "${features[@]}")
	deps="$crate_dir/target/$target/release/deps"
	rlib="$(find "$deps" -maxdepth 1 -name "lib${crate}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	if [[ -z "$rlib" ]]; then
		echo "build-shared: no rlib produced for $crate" >&2
		exit 1
	fi
	out="$out_dir/$artifact.so"
	link_deps=()
	export_flags=()
	symbolic_flags=(-Bsymbolic)
	archives=("$rlib")
	link_inputs=()
	if [[ "$artifact" == "liblsrt" ]]; then
		symbolic_flags=()
		archives=()
		for dependency in core alloc compiler_builtins abi rt; do
			archive="$(find "$deps" -maxdepth 1 -name "lib${dependency}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
			if [[ -z "$archive" ]]; then
				echo "build-shared: missing PIC archive $dependency for liblsrt" >&2
				exit 1
			fi
			archives+=("$archive")
		done
		object_root="$out_dir/.objects-liblsrt"
		rm -rf "$object_root"
		mkdir -p "$object_root"
		for archive in "${archives[@]}"; do
			archive_name="$(basename "$archive" .rlib)"
			mkdir -p "$object_root/$archive_name"
			(cd "$object_root/$archive_name" && llvm-ar x "$OLDPWD/$archive")
		done
		while IFS= read -r -d '' object; do
			llvm-objcopy --set-symbol-visibility=memcpy=default --set-symbol-visibility=memset=default --set-symbol-visibility=memcmp=default "$object"
			link_inputs+=("$object")
		done < <(find "$object_root" -name '*.o' -print0)
	else
		link_inputs=(--whole-archive "${archives[@]}" --no-whole-archive)
	fi
	case "$artifact" in
	libproto | libpix | libinflate | libpcm)
		link_deps=(-L "$out_dir" -l:liblsrt.so --no-allow-shlib-undefined)
		;;
	libbmp)
		link_deps=(-L "$out_dir" -l:libpix.so -l:liblsrt.so --no-allow-shlib-undefined)
		;;
	libpng)
		link_deps=(-L "$out_dir" -l:libpix.so -l:libinflate.so -l:liblsrt.so --no-allow-shlib-undefined)
		;;
	libkeys)
		link_deps=(-L "$out_dir" -l:libproto.so -l:liblsrt.so --no-allow-shlib-undefined)
		;;
	libsurface)
		link_deps=(-L "$out_dir" -l:libproto.so -l:libpix.so -l:liblsrt.so --no-allow-shlib-undefined)
		;;
	esac
	"$lld" -flavor gnu -m "$emulation" -shared --hash-style=sysv "${symbolic_flags[@]}" --gc-sections "${export_flags[@]}" "${link_inputs[@]}" "${link_deps[@]}" -soname "$artifact.so" -o "$out"
	llvm-strip --strip-debug "$out"
	if ! llvm-readelf -h "$out" | grep -q 'Type:.*DYN'; then
		echo "build-shared: $out is not ET_DYN" >&2
		exit 1
	fi
	if llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ { bad = 1 } END { exit bad }'; then
		:
	else
		echo "build-shared: $out contains a writable executable segment" >&2
		exit 1
	fi
	echo "build-shared: $out ($(stat -c %s "$out") bytes)"
	artifacts+=("$artifact")
done

if printf '%s\n' "${artifacts[@]}" | grep -qx libpix; then
	probe="user/dyn_probe"
	(cd "$probe" && RUST_MIN_STACK=16777216 RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib)
	probe_rlib="$(find "$probe/target/$target/release/deps" -maxdepth 1 -name 'libdyn_probe-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	probe_out="$out_dir/dyn_probe"
	"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv -e _start --whole-archive "$probe_rlib" --no-whole-archive -L "$out_dir" -l:libpix.so -l:libproto.so -l:liblsrt.so --no-allow-shlib-undefined -o "$probe_out"
	llvm-strip --strip-debug "$probe_out"
	if ! llvm-readelf -h "$probe_out" | grep -q 'Type:.*DYN' || ! llvm-readelf -d "$probe_out" | grep -q 'NEEDED.*libpix.so'; then
		echo "build-shared: $probe_out is not a libpix-linked ET_DYN" >&2
		exit 1
	fi
	echo "build-shared: $probe_out ($(stat -c %s "$probe_out") bytes)"
fi
