#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
	echo "usage: $0 <target> <crate>..." >&2
	exit 2
fi

target="$1"
shift
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo_target="$target"
cargo_target_flags=()

case "$target" in
x86_64-unknown-none)
	emulation="elf_x86_64"
	rustflags="-C relocation-model=pic"
	cargo_target="$root/user/x86_64-unknown-none.json"
	cargo_target_flags=(-Z json-target-spec)
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

library_file() {
	case "$1" in
	lsrt) printf 'user/rt/shared/%s/lsrt.lslib' "$target" ;;
	proto) printf 'proto/shared/%s/proto.lslib' "$target" ;;
	wire) printf 'wire/shared/%s/wire.lslib' "$target" ;;
	*) printf 'user/%s/shared/%s/%s.lslib' "$1" "$target" "$1" ;;
	esac
}

artifacts=()
for spec in "$@"; do
	if [[ "$spec" == *=* ]]; then
		artifact="${spec%%=*}"
		crate="${spec#*=}"
	else
		artifact="$spec"
		crate="$spec"
	fi
	crate_rust="${crate//-/_}"
	if [[ ! "$artifact" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ || "$artifact" == lib* ]]; then
		echo "build-shared: invalid LiberSystem library name '$artifact'" >&2
		exit 2
	fi
	case "$crate" in
	proto | wire) crate_dir="$crate" ;;
	*) crate_dir="user/$crate" ;;
	esac
	manifest="$crate_dir/Cargo.toml"
	if [[ ! -f "$manifest" ]]; then
		echo "build-shared: missing $manifest" >&2
		exit 1
	fi
	out_dir="$crate_dir/shared/$target"
	mkdir -p "$out_dir"
	features=()
	if [[ "$artifact" == "lsrt" ]]; then
		features=(--no-default-features --features shared-image)
	elif [[ "$artifact" == "ipc-client" ]]; then
		features=(--no-default-features --features shared-image)
	fi
	(cd "$crate_dir" && RUST_MIN_STACK=33554432 RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$cargo_target" --lib "${features[@]}")
	deps="$crate_dir/target/$target/release/deps"
	rlib="$(find "$deps" -maxdepth 1 -name "lib${crate_rust}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	if [[ -z "$rlib" ]]; then
		echo "build-shared: no rlib produced for $crate" >&2
		exit 1
	fi
	out="$out_dir/$artifact.lslib"
	link_deps=()
	export_flags=()
	symbolic_flags=(-Bsymbolic)
	archives=("$rlib")
	link_inputs=()
	if [[ "$artifact" == "lsrt" ]]; then
		symbolic_flags=()
		archives=()
		for dependency in core alloc compiler_builtins abi rt; do
			archive="$(find "$deps" -maxdepth 1 -name "lib${dependency}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
			if [[ -z "$archive" ]]; then
				echo "build-shared: missing PIC archive $dependency for lsrt.lslib" >&2
				exit 1
			fi
			archives+=("$archive")
		done
		object_root="$out_dir/.objects-lsrt"
		rm -rf "$object_root"
		mkdir -p "$object_root"
		for archive in "${archives[@]}"; do
			archive_name="$(basename "$archive" .rlib)"
			mkdir -p "$object_root/$archive_name"
			(cd "$object_root/$archive_name" && llvm-ar x "$OLDPWD/$archive")
		done
		while IFS= read -r -d '' object; do
			llvm-objcopy --set-symbol-visibility=memcpy=default --set-symbol-visibility=memmove=default --set-symbol-visibility=memset=default --set-symbol-visibility=memcmp=default "$object"
			link_inputs+=("$object")
		done < <(find "$object_root" -name '*.o' -print0)
	else
		link_inputs=(--whole-archive "${archives[@]}" --no-whole-archive)
	fi
	case "$artifact" in
	wire | pix | inflate | pcm | adpcm | ogg)
		link_deps=("$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	proto)
		link_deps=("$(library_file wire)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	ipc-client)
		link_deps=("$(library_file wire)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	deflate)
		miniz_archive="$(find "$deps" -maxdepth 1 -name 'libminiz_oxide-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		adler_archive="$(find "$deps" -maxdepth 1 -name 'libadler2-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$miniz_archive" || -z "$adler_archive" ]]; then
			echo "build-shared: missing miniz_oxide/adler2 archive for deflate.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$miniz_archive" "$adler_archive" --no-whole-archive)
		link_deps=("$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	bmp)
		link_deps=("$(library_file quantize)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	ppm | tga)
		link_deps=("$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	pcx)
		link_deps=("$(library_file quantize)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	qoi)
		qoi_codec_archive="$(find "$deps" -maxdepth 1 -name 'libqoi-*.rlib' ! -samefile "$rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		bytemuck_archive="$(find "$deps" -maxdepth 1 -name 'libbytemuck-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$qoi_codec_archive" || -z "$bytemuck_archive" ]]; then
			echo "build-shared: missing qoi/bytemuck archive for qoi.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$qoi_codec_archive" "$bytemuck_archive" --no-whole-archive)
		link_deps=("$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	png)
		link_deps=("$(library_file quantize)" "$(library_file pix)" "$(library_file inflate)" "$(library_file deflate)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	apng)
		link_deps=("$(library_file png)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	quantize)
		link_deps=("$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	gif)
		weezl_archive="$(find "$deps" -maxdepth 1 -name 'libweezl-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$weezl_archive" ]]; then
			echo "build-shared: missing weezl archive for gif.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$weezl_archive" --no-whole-archive)
		link_deps=("$(library_file quantize)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	ico)
		link_deps=("$(library_file png)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	icns)
		link_deps=("$(library_file png)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	jpeg)
		jpeg_encoder_archive="$(find "$deps" -maxdepth 1 -name 'libjpeg_encoder-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		zune_core_archive="$(find "$deps" -maxdepth 1 -name 'libzune_core-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		zune_jpeg_archive="$(find "$deps" -maxdepth 1 -name 'libzune_jpeg-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$jpeg_encoder_archive" || -z "$zune_core_archive" || -z "$zune_jpeg_archive" ]]; then
			echo "build-shared: missing JPEG engine archives for jpeg.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$jpeg_encoder_archive" "$zune_core_archive" "$zune_jpeg_archive" --no-whole-archive)
		link_deps=("$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	webp)
		webp_archives=()
		for dependency in ai_byteorder_lite ai_image_webp ai_quick_error allocator_api2 equivalent foldhash hashbrown memchr no_std_io; do
			archive="$(find "$deps" -maxdepth 1 -name "lib${dependency}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
			if [[ -z "$archive" ]]; then
				echo "build-shared: missing $dependency archive for webp.lslib" >&2
				exit 1
			fi
			webp_archives+=("$archive")
		done
		link_inputs=(--whole-archive "$rlib" "${webp_archives[@]}" --no-whole-archive)
		link_deps=("$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	keys)
		link_deps=("$(library_file proto)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	surface)
		link_deps=("$(library_file proto)" "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	aiff | flac | wavpack)
		link_deps=("$(library_file pcm)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	mp3)
		nanomp3_archive="$(find "$deps" -maxdepth 1 -name 'libnanomp3-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$nanomp3_archive" ]]; then
			echo "build-shared: missing nanomp3 archive for mp3.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$nanomp3_archive" --no-whole-archive)
		link_deps=("$(library_file pcm)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	vorbis)
		libm_archive="$(find "$deps" -maxdepth 1 -name 'liblibm-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$libm_archive" ]]; then
			echo "build-shared: missing libm archive for vorbis.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$libm_archive" --no-whole-archive)
		link_deps=("$(library_file ogg)" "$(library_file pcm)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	wav)
		link_deps=("$(library_file adpcm)" "$(library_file pcm)" "$(library_file lsrt)" --no-allow-shlib-undefined)
		;;
	esac
	"$lld" -flavor gnu -m "$emulation" -shared --hash-style=sysv "${symbolic_flags[@]}" --gc-sections "${export_flags[@]}" "${link_inputs[@]}" "${link_deps[@]}" -soname "$artifact.lslib" -o "$out"
	llvm-strip --strip-debug "$out"
	case "$artifact" in
	wire | ipc-client | proto)
		actual_needed="$(llvm-readelf -d "$out" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort)"
		case "$artifact" in
		wire) expected_needed="lsrt.lslib" ;;
		ipc-client | proto) expected_needed="$(printf '%s\n' lsrt.lslib wire.lslib | sort)" ;;
		esac
		if [[ "$actual_needed" != "$expected_needed" ]]; then
			echo "build-shared: $out has unexpected providers: $actual_needed" >&2
			exit 1
		fi
		;;
	esac
	if [[ "$artifact" == "ipc-client" ]]; then
		actual_imports="$(llvm-readelf --wide --dyn-syms "$out" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u)"
		expected_imports="$(printf '%s\n' recv_vec_blocking resolve | sort)"
		if [[ "$actual_imports" != "$expected_imports" ]]; then
			echo "build-shared: $out has unexpected runtime imports: $actual_imports" >&2
			exit 1
		fi
		for symbol in $actual_imports; do
			count="$(llvm-readelf --wide --dyn-syms "$(library_file lsrt)" | awk -v symbol="$symbol" '$7 != "UND" && $8 == symbol { count++ } END { print count + 0 }')"
			if [[ "$count" != 1 ]]; then
				echo "build-shared: $symbol has $count providers in lsrt.lslib (expected 1)" >&2
				exit 1
			fi
		done
	fi
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

if printf '%s\n' "${artifacts[@]}" | grep -qx pix; then
	probe="user/dyn_probe"
	(cd "$probe" && RUST_MIN_STACK=33554432 RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib)
	probe_rlib="$(find "$probe/target/$target/release/deps" -maxdepth 1 -name 'libdyn_probe-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	probe_dir="$probe/shared/$target"
	mkdir -p "$probe_dir"
	probe_out="$probe_dir/dyn_probe"
	"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv -e _start --whole-archive "$probe_rlib" --no-whole-archive "$(library_file pix)" "$(library_file proto)" "$(library_file lsrt)" --no-allow-shlib-undefined -o "$probe_out"
	llvm-strip --strip-debug "$probe_out"
	if ! llvm-readelf -h "$probe_out" | grep -q 'Type:.*DYN' || ! llvm-readelf -d "$probe_out" | grep -q 'NEEDED.*pix.lslib'; then
		echo "build-shared: $probe_out is not a pix.lslib-linked ET_DYN" >&2
		exit 1
	fi
	echo "build-shared: $probe_out ($(stat -c %s "$probe_out") bytes)"
fi
