#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
	echo "usage: $0 <target> <crate>..." >&2
	exit 2
fi

target="$1"
shift
root="$(cd "$(dirname "$0")/.." && pwd)"
rust_min_stack="${RUST_MIN_STACK:-67108864}"
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
command -v jq >/dev/null

library_file() {
	case "$1" in
	lsrt) printf 'user/rt/shared/%s/lsrt.lslib' "$target" ;;
	proto) printf 'proto/shared/%s/proto.lslib' "$target" ;;
	wire) printf 'wire/shared/%s/wire.lslib' "$target" ;;
	*) printf 'user/%s/shared/%s/%s.lslib' "$1" "$target" "$1" ;;
	esac
}

manifest_library_row() {
	awk -v artifact="$1" '$1 == "library" && $2 == artifact {print; count++} END {if (count != 1) exit 1}' "$root/user/services/manifest.txt"
}

manifest_specs="$(awk '$1 == "library" {print $2 "=" $3}' "$root/user/services/manifest.txt" | sort)"
requested_specs="$(for spec in "$@"; do if [[ "$spec" == *=* ]]; then printf '%s\n' "$spec"; else printf '%s=%s\n' "$spec" "$spec"; fi; done | sort)"
if [[ "$requested_specs" != "$manifest_specs" ]]; then
	echo "build-shared: requested libraries differ from the manifest" >&2
	diff -u <(printf '%s\n' "$manifest_specs") <(printf '%s\n' "$requested_specs") >&2 || true
	exit 1
fi

image_graph=""
if printf '%s\n' "$@" | sed 's/=.*//' | grep -qx lsrt; then
	image_target="$root/boot/.build/image-cargo-$target"
	image_graph="$root/boot/.build/image-cargo-$target.jsonl"
	image_graph_errors="$root/boot/.build/image-cargo-$target.stderr"
	image_seed="$root/boot/.build/image-seed-$target.o"
	rm -rf "$image_target"
	rm -f "$image_seed"
	set +e
	(
		cd "$root/user/tools"
		CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --release --target "$cargo_target" --bin date --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$image_seed"
	) >"$image_graph" 2>"$image_graph_errors"
	graph_status=$?
	set -e
	if [[ "$graph_status" != 101 || ! -f "$image_seed" ]] || ! llvm-readelf -h "$image_seed" | grep -q 'Type:.*REL'; then
		echo "build-shared: Cargo image graph did not stop after emitting its ET_REL seed object" >&2
		exit 1
	fi
	if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$image_graph_errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$image_graph_errors"; then
		echo "build-shared: Cargo image graph failed outside the expected final-link shim boundary" >&2
		exit 1
	fi
fi

graph_archive() {
	local crate_dir="$1"
	local package_prefix="path+file://$root/$crate_dir#"
	local archives
	archives="$(jq -r --arg prefix "$package_prefix" 'select(.reason == "compiler-artifact" and (.package_id | startswith($prefix))) | .filenames[] | select(endswith(".rlib"))' "$image_graph" | sort -u)"
	if [[ "$(wc -l <<<"$archives")" != 1 || -z "$archives" ]]; then
		echo "build-shared: Cargo image graph has no unique archive for $crate_dir" >&2
		exit 1
	fi
	printf '%s' "$archives"
}

canonical_provider_order() {
	local roots="$1"
	local name dependency dependencies candidate ready
	local -A present=()
	local -A edges=()
	local -A visiting=()
	local -a pending=()
	local -a order=()
	read -r -a pending <<<"$roots"
	while ((${#pending[@]})); do
		name="${pending[0]}"
		pending=("${pending[@]:1}")
		[[ -n "$name" ]] || continue
		if [[ -n "${present[$name]:-}" ]]; then
			continue
		fi
		if ! printf '%s\n' "${artifacts[@]}" | grep -qx "$name"; then
			echo "build-shared: canonical graph names unavailable provider $name" >&2
			return 1
		fi
		dependencies="$(llvm-readelf -d "$(library_file "$name")" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sed 's/\.lslib$//' | sort -u)"
		present[$name]=1
		edges[$name]="$dependencies"
		for dependency in $dependencies; do
			pending+=("$dependency")
		done
	done
	while ((${#order[@]} < ${#present[@]})); do
		candidate=""
		while read -r name; do
			[[ -n "$name" ]] || continue
			if printf '%s\n' "${order[@]}" | grep -qx "$name"; then
				continue
			fi
			ready=1
			for dependency in ${edges[$name]}; do
				if ! printf '%s\n' "${order[@]}" | grep -qx "$dependency"; then
					ready=0
					break
				fi
			done
			if [[ "$ready" == 1 ]]; then
				candidate="$name"
				break
			fi
		done < <(printf '%s\n' "${!present[@]}" | sort)
		if [[ -z "$candidate" ]]; then
			echo "build-shared: provider graph contains a cycle" >&2
			return 1
		fi
		order+=("$candidate")
	done
	printf '%s.lslib\n' "${order[@]}"
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
	row="$(manifest_library_row "$artifact")" || {
		echo "build-shared: $artifact has no unique library manifest row" >&2
		exit 1
	}
	read -r row_kind row_artifact row_crate row_stage row_features row_providers <<<"$row"
	if [[ "$row_kind" != library || "$row_artifact" != "$artifact" || "$row_crate" != "$crate" || "$row_stage" != volume || -z "$row_features" ]]; then
		echo "build-shared: $artifact invocation differs from its library manifest row" >&2
		exit 1
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
	if [[ "$row_features" != - ]]; then
		if [[ ! "$row_features" =~ ^[A-Za-z0-9_-]+(,[A-Za-z0-9_-]+)*$ ]]; then
			echo "build-shared: $artifact has invalid feature set '$row_features'" >&2
			exit 1
		fi
		if [[ "$(tr ',' '\n' <<<"$row_features" | sort | uniq -d | head -n1)" != "" ]]; then
			echo "build-shared: $artifact repeats a build feature" >&2
			exit 1
		fi
		features=(--no-default-features --features "$row_features")
	fi
	if [[ -n "$image_graph" ]]; then
		deps="$image_target/$target/release/deps"
		rlib="$(graph_archive "$crate_dir")"
	else
		(cd "$crate_dir" && RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$cargo_target" --lib "${features[@]}")
		deps="$crate_dir/target/$target/release/deps"
		rlib="$(find "$deps" -maxdepth 1 -name "lib${crate_rust}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	fi
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
			archive_path="$(realpath "$archive")"
			archive_name="$(basename "$archive" .rlib)"
			mkdir -p "$object_root/$archive_name"
			(cd "$object_root/$archive_name" && llvm-ar x "$archive_path")
		done
		while IFS= read -r -d '' object; do
			llvm-objcopy --set-symbol-visibility=memcpy=default --set-symbol-visibility=memmove=default --set-symbol-visibility=memset=default --set-symbol-visibility=memcmp=default --set-symbol-visibility=__udivti3=default --set-symbol-visibility=__umodti3=default "$object"
			link_inputs+=("$object")
		done < <(find "$object_root" -name '*.o' -print0)
	else
		link_inputs=(--whole-archive "${archives[@]}" --no-whole-archive)
	fi
	case "$artifact" in
	deflate)
		miniz_archive="$(find "$deps" -maxdepth 1 -name 'libminiz_oxide-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		adler_archive="$(find "$deps" -maxdepth 1 -name 'libadler2-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$miniz_archive" || -z "$adler_archive" ]]; then
			echo "build-shared: missing miniz_oxide/adler2 archive for deflate.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$miniz_archive" "$adler_archive" --no-whole-archive)
		;;
	qoi)
		qoi_codec_archive="$(find "$deps" -maxdepth 1 -name 'libqoi-*.rlib' ! -samefile "$rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		bytemuck_archive="$(find "$deps" -maxdepth 1 -name 'libbytemuck-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$qoi_codec_archive" || -z "$bytemuck_archive" ]]; then
			echo "build-shared: missing qoi/bytemuck archive for qoi.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$qoi_codec_archive" "$bytemuck_archive" --no-whole-archive)
		;;
	gif)
		weezl_archive="$(find "$deps" -maxdepth 1 -name 'libweezl-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$weezl_archive" ]]; then
			echo "build-shared: missing weezl archive for gif.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$weezl_archive" --no-whole-archive)
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
		;;
	mp3)
		nanomp3_archive="$(find "$deps" -maxdepth 1 -name 'libnanomp3-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$nanomp3_archive" ]]; then
			echo "build-shared: missing nanomp3 archive for mp3.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$nanomp3_archive" --no-whole-archive)
		;;
	vorbis)
		libm_archive="$(find "$deps" -maxdepth 1 -name 'liblibm-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$libm_archive" ]]; then
			echo "build-shared: missing libm archive for vorbis.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$libm_archive" --no-whole-archive)
		;;
	esac
	expected_needed=""
	provider_count=0
	for provider in $row_providers; do
		if [[ "$provider" == "$artifact" || ! "$provider" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ || "$provider" == lib* ]] || ! printf '%s\n' "${artifacts[@]}" | grep -qx "$provider"; then
			echo "build-shared: library $artifact names invalid or unavailable provider $provider" >&2
			exit 1
		fi
		if grep -qx "$provider.lslib" <<<"$expected_needed"; then
			echo "build-shared: library $artifact repeats provider $provider" >&2
			exit 1
		fi
		link_deps+=("$(library_file "$provider")")
		expected_needed+="$provider.lslib"$'\n'
		provider_count=$((provider_count + 1))
	done
	if [[ "$artifact" != lsrt && "$provider_count" == 0 ]]; then
		echo "build-shared: library $artifact has no direct providers" >&2
		exit 1
	fi
	link_deps+=(--no-allow-shlib-undefined)
	"$lld" -flavor gnu -m "$emulation" -shared --hash-style=sysv "${symbolic_flags[@]}" --gc-sections "${export_flags[@]}" "${link_inputs[@]}" "${link_deps[@]}" -soname "$artifact.lslib" -o "$out"
	llvm-strip --strip-debug "$out"
	actual_needed="$(llvm-readelf -d "$out" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
	expected_needed="$(sort -u <<<"$expected_needed" | sed '/^$/d')"
	if [[ "$actual_needed" != "$expected_needed" ]]; then
		echo "build-shared: $out providers differ from its manifest: $actual_needed" >&2
		exit 1
	fi
	imports="$(llvm-readelf --wide --dyn-syms "$out" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u)"
	declare -A used_providers=()
	declare -A provider_closures=()
	closure_providers=""
	for provider in $row_providers; do
		provider_closures[$provider]="$(canonical_provider_order "$provider" | sed 's/\.lslib$//')"
		closure_providers+="${provider_closures[$provider]}"$'\n'
	done
	closure_providers="$(sort -u <<<"$closure_providers" | sed '/^$/d')"
	for symbol in $imports; do
		owner=""
		for provider in $closure_providers; do
			if llvm-readelf --wide --dyn-syms "$(library_file "$provider")" | awk -v symbol="$symbol" '$7 != "UND" && $8 == symbol {found=1} END {exit !found}'; then
				if [[ -n "$owner" ]]; then
					echo "build-shared: library $artifact import $symbol has duplicate providers $owner and $provider" >&2
					exit 1
				fi
				owner="$provider"
			fi
		done
		if [[ -z "$owner" ]]; then
			echo "build-shared: library $artifact import $symbol has no direct provider" >&2
			exit 1
		fi
		if grep -qw "$owner" <<<"$row_providers"; then
			used_providers[$owner]=1
		else
			owner_root=""
			for provider in $row_providers; do
				if grep -qx "$owner" <<<"${provider_closures[$provider]}"; then
					if [[ -n "$owner_root" ]]; then
						owner_root="ambiguous"
						break
					fi
					owner_root="$provider"
				fi
			done
			if [[ -n "$owner_root" && "$owner_root" != ambiguous ]]; then
				used_providers[$owner_root]=1
			fi
		fi
	done
	for provider in $row_providers; do
		if [[ -z "${used_providers[$provider]:-}" ]]; then
			echo "build-shared: library $artifact provider $provider satisfies no direct import" >&2
			exit 1
		fi
	done
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
	if [[ "$artifact" == "lsrt" ]]; then
		alloc_shim="$(llvm-readelf --wide --dyn-syms "$out" | awk '$8 == "_RNvCshfEkAwg4zv6_7___rustc35___rust_no_alloc_shim_is_unstable_v2" {print $4}')"
		if [[ "$alloc_shim" != "FUNC" ]]; then
			echo "build-shared: lsrt allocator shim alias is not one function" >&2
			exit 1
		fi
		for intrinsic in __udivti3 __umodti3; do
			if [[ "$(llvm-readelf --wide --dyn-syms "$out" | awk -v symbol="$intrinsic" '$7 != "UND" && $8 == symbol {count++} END {print count+0}')" != 1 ]]; then
				echo "build-shared: lsrt does not export exactly one $intrinsic compiler intrinsic" >&2
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

if [[ -n "$image_graph" ]]; then
	start_obj="$root/boot/.build/exe-start-$target.o"
	"$root/tools/build-exe-start.sh" "$target" "$start_obj"
	dynamic_rows="$(awk '$1 == "dynamic" && $3 == "tools" && $4 == "volume" {print}' "$root/user/services/manifest.txt" | sort -k2,2)"
	manifest_tools="$(awk '{print $2}' <<<"$dynamic_rows")"
	cargo_tools="$(cd "$root/user/tools" && cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "tools") | .targets[] | select(.kind == ["bin"]) | .name' | sort)"
	if [[ "$manifest_tools" != "$cargo_tools" ]]; then
		echo "build-shared: tools-package bins differ from dynamic volume manifest rows" >&2
		diff -u <(printf '%s\n' "$cargo_tools") <(printf '%s\n' "$manifest_tools") >&2 || true
		exit 1
	fi
	duplicate_consumer="$(awk '{print $2}' <<<"$dynamic_rows" | uniq -d | head -n1)"
	if [[ -n "$duplicate_consumer" ]]; then
		echo "build-shared: duplicate dynamic executable $duplicate_consumer" >&2
		exit 1
	fi
	while read -r kind consumer crate stage providers; do
		if [[ "$kind" != dynamic || "$crate" != tools || "$stage" != volume ]]; then
			continue
		fi
		if [[ -z "$providers" ]]; then
			echo "build-shared: dynamic $consumer has no direct providers" >&2
			exit 1
		fi
		provider_count="$(wc -w <<<"$providers")"
		providers="$(tr ' ' '\n' <<<"$providers" | sort -u | xargs)"
		if [[ "$(wc -w <<<"$providers")" != "$provider_count" ]]; then
			echo "build-shared: dynamic $consumer repeats a direct provider" >&2
			exit 1
		fi
		consumer_dir="$root/user/$crate"
		out_dir="$consumer_dir/shared/$target"
		consumer_obj="$out_dir/.$consumer.o"
		consumer_errors="$out_dir/.$consumer.stderr"
		out="$out_dir/$consumer"
		mkdir -p "$out_dir"
		rm -f "$consumer_obj"
		set +e
		(
			cd "$consumer_dir"
			CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --quiet --release --target "$cargo_target" --bin "$consumer" --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$consumer_obj"
		) >/dev/null 2>"$consumer_errors"
		consumer_status=$?
		set -e
		if [[ "$consumer_status" != 101 || ! -f "$consumer_obj" ]] || ! llvm-readelf -h "$consumer_obj" | grep -q 'Type:.*REL'; then
			echo "build-shared: $consumer did not stop after emitting its ET_REL object" >&2
			exit 1
		fi
		if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$consumer_errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$consumer_errors"; then
			echo "build-shared: $consumer failed outside the expected final-link shim boundary" >&2
			exit 1
		fi
		consumer_definitions="$(llvm-readelf --wide --symbols "$consumer_obj" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
		if [[ "$consumer_definitions" != "__user_main" ]]; then
			echo "build-shared: $consumer_obj defines globals outside __user_main: $consumer_definitions" >&2
			exit 1
		fi
		provider_inputs=()
		expected_needed=""
		for provider in $providers; do
			if ! printf '%s\n' "${artifacts[@]}" | grep -qx "$provider"; then
				echo "build-shared: dynamic $consumer names unavailable provider $provider" >&2
				exit 1
			fi
			provider_inputs+=("$(library_file "$provider")")
			expected_needed+="$provider.lslib"$'\n'
		done
		expected_needed="$(sort -u <<<"$expected_needed" | sed '/^$/d')"
		consumer_imports="$(llvm-readelf --wide --symbols "$consumer_obj" | awk '$5 == "GLOBAL" && $7 == "UND" && $8 != "" {print $8}' | sort -u)"
		declare -A used_consumer_providers=()
		for symbol in $consumer_imports; do
			count=0
			owner=""
			for provider in $providers; do
				provider_file="$(library_file "$provider")"
				matches="$(llvm-readelf --wide --dyn-syms "$provider_file" | awk -v symbol="$symbol" '$7 != "UND" && $8 == symbol { count++ } END { print count + 0 }')"
				count=$((count + matches))
				if [[ "$matches" == 1 ]]; then
					owner="$provider"
				fi
			done
			if [[ "$count" != 1 ]]; then
				echo "build-shared: $consumer import $symbol has $count declared providers (expected 1)" >&2
				exit 1
			fi
			used_consumer_providers[$owner]=1
		done
		for provider in $providers; do
			if [[ -z "${used_consumer_providers[$provider]:-}" ]]; then
				echo "build-shared: dynamic $consumer provider $provider satisfies no direct import" >&2
				exit 1
			fi
		done
		"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv --gc-sections --build-id=none -e _start "$start_obj" "$consumer_obj" "${provider_inputs[@]}" --no-allow-shlib-undefined -o "$out"
		if ! llvm-readelf -h "$out" | grep -q 'Type:.*DYN'; then
			echo "build-shared: $out is not ET_DYN" >&2
			exit 1
		fi
		actual_needed="$(llvm-readelf -d "$out" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
		if [[ "$actual_needed" != "$expected_needed" ]]; then
			echo "build-shared: $out providers differ from its manifest: $actual_needed" >&2
			exit 1
		fi
		if llvm-readelf -l "$out" | grep -q 'INTERP' || llvm-readelf -d "$out" | grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)'; then
			echo "build-shared: $out has a forbidden dynamic-loader contract" >&2
			exit 1
		fi
		if ! llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ { bad = 1 } END { exit bad }'; then
			echo "build-shared: $out contains a writable executable segment" >&2
			exit 1
		fi
		forbidden_definitions="$(llvm-readelf --wide --symbols "$out" | awk '$7 != "UND" && $8 ~ /^(__rust_alloc|__rust_dealloc|rust_begin_unwind|memcpy|memmove|memset|memcmp|liber_rt_start|print|inherit_stdout)$/ {print $8}')"
		if [[ -n "$forbidden_definitions" ]]; then
			echo "build-shared: $out contains runtime/provider definitions: $forbidden_definitions" >&2
			exit 1
		fi
		llvm-strip --strip-debug "$out"
		canonical_provider_order "$providers" >"$out.order"
		echo "build-shared: $out ($(stat -c %s "$out") bytes, PIE)"
	done <<<"$dynamic_rows"
fi

if printf '%s\n' "${artifacts[@]}" | grep -qx pix; then
	probe="user/dyn_probe"
	(cd "$probe" && RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib)
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
	canonical_provider_order "pix proto lsrt" >"$probe_out.order"
	echo "build-shared: $probe_out ($(stat -c %s "$probe_out") bytes)"
fi
