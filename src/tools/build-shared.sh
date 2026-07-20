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
force_rebuild="${LIBER_IMAGE_REBUILD:-0}"
artifact_cache_dir="$root/boot/.build/image-artifacts-$target"
cargo_target="$target"
cargo_target_flags=()
build_started=$SECONDS
provider_cache_hits=0
provider_cache_misses=0
object_cache_hits=0
object_cache_misses=0
executable_cache_hits=0
executable_cache_misses=0

report_build_summary() {
	local status=$?
	find "$root" -path "*/shared/$target/*.$$.expected" -delete 2>/dev/null || true
	find "$artifact_cache_dir" -maxdepth 1 -type f -name "*.tmp.$$" -delete 2>/dev/null || true
	echo "build-shared: summary target=$target seconds=$((SECONDS - build_started)) providers=$provider_cache_hits/$provider_cache_misses objects=$object_cache_hits/$object_cache_misses executables=$executable_cache_hits/$executable_cache_misses status=$status"
}

trap report_build_summary EXIT

if [[ "$force_rebuild" != 0 && "$force_rebuild" != 1 ]]; then
	echo "build-shared: LIBER_IMAGE_REBUILD must be 0 or 1" >&2
	exit 2
fi

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
command -v sha256sum >/dev/null
command -v xxd >/dev/null

rustc_commit="$(rustc -vV | sed -n 's/^commit-hash: //p')"
if [[ ! "$rustc_commit" =~ ^[0-9a-f]{40}$ ]]; then
	echo "build-shared: rustc did not report one commit hash" >&2
	exit 1
fi

build_tool_digest="$({
	sha256sum "$root/tools/build-shared.sh" "$root/tools/build-exe-start.sh" "$root/tools/exe-start.rs" "$lld"
	for tool in llvm-objcopy llvm-readelf llvm-strip; do
		sha256sum "$(command -v "$tool")"
	done
} | sha256sum | awk '{print $1}')"
object_tool_digest="$(sha256sum "$root/tools/build-consumer-object.sh" | awk '{print $1}')"

library_file() {
	case "$1" in
	lsrt) printf 'user/rt/shared/%s/lsrt.lslib' "$target" ;;
	proto) printf 'proto/shared/%s/proto.lslib' "$target" ;;
	wire) printf 'wire/shared/%s/wire.lslib' "$target" ;;
	audio-client) printf 'user/audio-client-provider/shared/%s/audio-client.lslib' "$target" ;;
	config-client) printf 'user/config-client-provider/shared/%s/config-client.lslib' "$target" ;;
	device-client) printf 'user/device-client-provider/shared/%s/device-client.lslib' "$target" ;;
	log-client) printf 'user/log-client-provider/shared/%s/log-client.lslib' "$target" ;;
	network-client) printf 'user/network-client-provider/shared/%s/network-client.lslib' "$target" ;;
	observability-client) printf 'user/observability-client-provider/shared/%s/observability-client.lslib' "$target" ;;
	process-client) printf 'user/process-client-provider/shared/%s/process-client.lslib' "$target" ;;
	resources-client) printf 'user/resources-client-provider/shared/%s/resources-client.lslib' "$target" ;;
	security-client) printf 'user/security-client-provider/shared/%s/security-client.lslib' "$target" ;;
	time-client) printf 'user/time-client-provider/shared/%s/time-client.lslib' "$target" ;;
	volume-client) printf 'user/volume-client-provider/shared/%s/volume-client.lslib' "$target" ;;
	wasm) printf 'wasm/shared/%s/wasm.lslib' "$target" ;;
	term) printf 'term/shared/%s/term.lslib' "$target" ;;
	service-util) printf 'user/services/shared/%s/service-util.lslib' "$target" ;;
	*) printf 'user/%s/shared/%s/%s.lslib' "$1" "$target" "$1" ;;
	esac
}

library_identity_file() {
	printf '%s.identity' "$(library_file "$1")"
}

source_digest() {
	local crate_dir="$1"
	local api_dir=""
	if [[ "$crate_dir" == user/*-client-provider ]]; then
		api_dir="${crate_dir%-provider}"
		if [[ ! -f "$root/$api_dir/Cargo.toml" ]]; then
			echo "build-shared: $crate_dir has no public API crate $api_dir" >&2
			return 1
		fi
	fi
	(
		cd "$root"
		find "$crate_dir" ${api_dir:+"$api_dir"} -path '*/target' -prune -o -path '*/shared' -prune -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' \) -print0 |
			sort -z |
			while IFS= read -r -d '' source; do
				printf '%s\n' "$source"
				sha256sum "$source"
			done
	) | sha256sum | awk '{print $1}'
}

executable_source_digest() {
	local crate_dir="$1"
	local package="$2"
	local artifact="$3"
	if [[ "$package" == tools ]]; then
		(
			cd "$root"
			for source in "$crate_dir/Cargo.toml" "$crate_dir/Cargo.lock" "$crate_dir/src/lib.rs" "$crate_dir/src/$artifact.rs" user/build.rs user/user.ld user/user-aarch64.ld user/user-riscv64.ld ../product.conf; do
				if [[ ! -f "$source" ]]; then
					printf 'missing:%s\n' "$source"
					continue
				fi
				printf '%s\n' "$source"
				sha256sum "$source"
			done
		) | sha256sum | awk '{print $1}'
		return
	fi
	{
		printf 'dependency-closure=%s\n' "${package_source_digests[$package]:-$(source_digest "$crate_dir")}"
		for source in "$root/user/build.rs" "$root/user/user.ld" "$root/user/user-aarch64.ld" "$root/user/user-riscv64.ld" "$root/../product.conf"; do
			if [[ -f "$source" ]]; then
				sha256sum "$source"
			else
				printf 'missing:%s\n' "$source"
			fi
		done
		if [[ "$package" == services ]]; then
			sha256sum "$root/user/services/manifest.txt"
		fi
	} | sha256sum | awk '{print $1}'
}

local_dependency_source_digest() {
	local crate_dir="$1"
	local exclude_root="${2:-0}"
	local metadata
	metadata="$(cd "$root" && cargo metadata --format-version 1 --manifest-path "$crate_dir/Cargo.toml")"
	(
		cd "$root"
		jq -r --arg root "$root/$crate_dir/Cargo.toml" --arg exclude "$exclude_root" '.packages[] | select(.source == null and ($exclude != "1" or .manifest_path != $root)) | .manifest_path' <<<"$metadata" |
			while IFS= read -r manifest_path; do
				package_dir="$(dirname "$manifest_path")"
				package_dir="${package_dir#"$root/"}"
				find "$package_dir" -path '*/target' -prune -o -path '*/shared' -prune -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' \) -print
			done |
			sort -u |
			while IFS= read -r source; do
				printf '%s\n' "$source"
				sha256sum "$source"
			done
	) | sha256sum | awk '{print $1}'
}

write_identity_record() {
	local kind="$1"
	local artifact="$2"
	local package="$3"
	local source_sha="$4"
	local feature_set="$5"
	local providers="$6"
	local identity="$7"
	local provider digest
	{
		printf 'format=liber-image-identity-v1\n'
		printf 'kind=%s\n' "$kind"
		printf 'artifact=%s\n' "$artifact"
		printf 'package=%s\n' "$package"
		printf 'source-sha256=%s\n' "$source_sha"
		printf 'rustc-commit=%s\n' "$rustc_commit"
		printf 'target=%s\n' "$target"
		printf 'profile=release\n'
		printf 'rustflags=%s\n' "$rustflags"
		printf 'features=%s\n' "$feature_set"
		for provider in $(tr ' ' '\n' <<<"$providers" | sort); do
			[[ -n "$provider" ]] || continue
			if [[ ! -f "$(library_identity_file "$provider")" ]]; then
				echo "build-shared: $artifact has no identity for provider $provider" >&2
				return 1
			fi
			digest="$(sha256sum "$(library_identity_file "$provider")" | awk '{print $1}')"
			printf 'provider=%s:%s\n' "$provider" "$digest"
		done
	} >"$identity"
}

verify_identity_note() {
	local elf="$1"
	local identity="$2"
	local digest note dumped_note
	digest="$(sha256sum "$identity" | awk '{print $1}')"
	note="$elf.identity.note.$$.expected"
	dumped_note="$elf.identity.note.$$.dump"
	printf '0600000020000000010000004c49424552000000' | xxd -r -p >"$note"
	printf '%s' "$digest" | xxd -r -p >>"$note"
	if ! llvm-objcopy --dump-section .note.liber.identity="$dumped_note" "$elf" 2>/dev/null || ! cmp -s "$note" "$dumped_note"; then
		rm -f "$note" "$dumped_note"
		return 1
	fi
	rm -f "$note" "$dumped_note"
}

emit_identity() {
	local kind="$1"
	local artifact="$2"
	local package="$3"
	local source_sha="$4"
	local feature_set="$5"
	local providers="$6"
	local elf="$7"
	local identity="$elf.identity"
	local digest note dumped_note
	write_identity_record "$kind" "$artifact" "$package" "$source_sha" "$feature_set" "$providers" "$identity"
	digest="$(sha256sum "$identity" | awk '{print $1}')"
	note="$elf.identity.note"
	printf '0600000020000000010000004c49424552000000' | xxd -r -p >"$note"
	printf '%s' "$digest" | xxd -r -p >>"$note"
	llvm-objcopy --add-section .note.liber.identity="$note" --set-section-flags .note.liber.identity=alloc,readonly "$elf"
	dumped_note="$elf.identity.note.dump"
	llvm-objcopy --dump-section .note.liber.identity="$dumped_note" "$elf"
	if ! cmp -s "$note" "$dumped_note"; then
		echo "build-shared: $artifact identity note differs from its record" >&2
		exit 1
	fi
	rm -f "$note" "$dumped_note"
}

artifact_cache_key() {
	local kind="$1"
	local manifest_row="$2"
	local identity="$3"
	local extra="$4"
	{
		printf 'format=liber-image-artifact-cache-v1\n'
		printf 'build-tools=%s\n' "$build_tool_digest"
		printf 'kind=%s\n' "$kind"
		printf 'manifest=%s\n' "$manifest_row"
		printf 'extra=%s\n' "$extra"
		cat "$identity"
	} | sha256sum | awk '{print $1}'
}

artifact_cache_valid() {
	local out="$1"
	local cache_prefix="$2"
	local expected_key="$3"
	local expected_identity="$4"
	local expected_needed="$5"
	local actual_needed actual_hash identity_hash needed_hash audit_key
	[[ -f "$out" && -f "$out.identity" && -f "$cache_prefix.build-key" && -f "$cache_prefix.sha256" ]] || return 1
	[[ "$(cat "$cache_prefix.build-key")" == "$expected_key" ]] || return 1
	cmp -s "$expected_identity" "$out.identity" || return 1
	actual_hash="$(sha256sum "$out" | awk '{print $1}')" || return 1
	[[ "$(cat "$cache_prefix.sha256")" == "$actual_hash" ]] || return 1
	identity_hash="$(sha256sum "$out.identity" | awk '{print $1}')" || return 1
	needed_hash="$(printf '%s' "$expected_needed" | sha256sum | awk '{print $1}')"
	audit_key="$({
		printf 'format=liber-image-audit-cache-v1\n'
		printf 'schema=elf64-et-dyn-needed-wx-note-v1\n'
		printf 'build-key=%s\n' "$expected_key"
		printf 'elf=%s\n' "$actual_hash"
		printf 'identity=%s\n' "$identity_hash"
		printf 'needed=%s\n' "$needed_hash"
	} | sha256sum | awk '{print $1}')"
	if [[ -f "$cache_prefix.audit-key" && "$(cat "$cache_prefix.audit-key")" == "$audit_key" ]]; then
		return 0
	fi
	llvm-readelf -h "$out" | grep -q 'Type:.*DYN' || return 1
	actual_needed="$(llvm-readelf -d "$out" 2>/dev/null | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)" || return 1
	[[ "$actual_needed" == "$expected_needed" ]] || return 1
	! llvm-readelf -l "$out" | grep -q 'INTERP' || return 1
	! llvm-readelf -d "$out" | grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)' || return 1
	llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ {bad=1} END {exit bad}' || return 1
	verify_identity_note "$out" "$out.identity" || return 1
	printf '%s\n' "$audit_key" >"$cache_prefix.audit-key.tmp"
	mv "$cache_prefix.audit-key.tmp" "$cache_prefix.audit-key"
}

record_artifact_cache() {
	local out="$1"
	local cache_prefix="$2"
	local key="$3"
	mkdir -p "$artifact_cache_dir"
	printf '%s\n' "$key" >"$cache_prefix.build-key.tmp"
	sha256sum "$out" | awk '{print $1}' >"$cache_prefix.sha256.tmp"
	mv "$cache_prefix.build-key.tmp" "$cache_prefix.build-key"
	mv "$cache_prefix.sha256.tmp" "$cache_prefix.sha256"
	rm -f "$cache_prefix.audit-key"
}

object_cache_key() {
	local consumer="$1"
	local package="$2"
	local source_sha="$3"
	local providers="$4"
	local provider
	{
		printf 'format=liber-image-object-cache-v1\n'
		printf 'compile-tool=%s\n' "$object_tool_digest"
		printf 'cargo-config=%s\n' "$image_target_config_value"
		printf 'consumer=%s\n' "$consumer"
		printf 'package=%s\n' "$package"
		printf 'source=%s\n' "$source_sha"
		printf 'features=shared-image\n'
		for provider in $providers; do
			printf 'provider-api=%s:%s\n' "$provider" "${provider_compile_digests[$provider]}"
		done
	} | sha256sum | awk '{print $1}'
}

object_cache_valid() {
	local object="$1"
	local cache_prefix="$2"
	local expected_key="$3"
	local actual_hash definitions
	[[ -f "$object" && -f "$cache_prefix.build-key" && -f "$cache_prefix.sha256" ]] || return 1
	[[ "$(cat "$cache_prefix.build-key")" == "$expected_key" ]] || return 1
	actual_hash="$(sha256sum "$object" | awk '{print $1}')" || return 1
	[[ "$(cat "$cache_prefix.sha256")" == "$actual_hash" ]] || return 1
	llvm-readelf -h "$object" | grep -q 'Type:.*REL' || return 1
	definitions="$(llvm-readelf --wide --symbols "$object" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
	[[ "$definitions" == __user_main ]]
}

record_object_cache() {
	local object="$1"
	local cache_prefix="$2"
	local key="$3"
	printf '%s\n' "$key" >"$cache_prefix.build-key.tmp"
	sha256sum "$object" | awk '{print $1}' >"$cache_prefix.sha256.tmp"
	mv "$cache_prefix.build-key.tmp" "$cache_prefix.build-key"
	mv "$cache_prefix.sha256.tmp" "$cache_prefix.sha256"
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
if awk '$1 == "component" && $4 == "volume" {found=1} END {exit !found}' "$root/user/services/manifest.txt"; then
	echo "build-shared: volume components must use dynamic manifest rows" >&2
	exit 1
fi

dynamic_rows() {
	awk '
		$1 == "dynamic" && $2 != "dyn_probe" && $4 == "volume" {print; next}
		$1 == "dynamic-service" && $4 == "volume" {
			for (i = 5; i <= NF && $i != "--"; i++) {}
			if (i > NF) {exit 1}
			printf "dynamic %s %s %s", $2, $3, $4
			for (i = i + 1; i <= NF; i++) printf " %s", $i
			printf "\n"
		}
	' "$root/user/services/manifest.txt" | sort -k2,2
}

image_graph=""
if printf '%s\n' "$@" | sed 's/=.*//' | grep -qx lsrt; then
	image_target="$root/boot/.build/image-cargo-$target"
	image_target_config="$root/boot/.build/image-cargo-$target.config"
	image_graph="$root/boot/.build/image-cargo-$target.jsonl"
	image_graph_errors="$root/boot/.build/image-cargo-$target.stderr"
	image_seed="$root/boot/.build/image-seed-$target.o"
	target_spec_digest="$(if [[ -f "$cargo_target" ]]; then sha256sum "$cargo_target" | awk '{print $1}'; else printf '%s' "$cargo_target" | sha256sum | awk '{print $1}'; fi)"
	image_target_config_value="$({
		printf 'format=liber-image-cargo-cache-v1\n'
		printf 'workspace=%s\n' "$root"
		printf 'rustc=%s\n' "$(rustc -vV | sha256sum | awk '{print $1}')"
		printf 'cargo=%s\n' "$(cargo -V)"
		printf 'target=%s\n' "$target"
		printf 'target-spec=%s\n' "$target_spec_digest"
		printf 'profile=release\n'
		printf 'rustflags=%s\n' "$rustflags"
		printf 'cargo-target-flags=%s\n' "${cargo_target_flags[*]}"
		printf 'build-std=core,alloc,compiler_builtins\n'
		printf 'build-std-features=compiler-builtins-mem\n'
		printf 'features=shared-image\n'
		for config in "$root/user/.cargo/config.toml" "$root/user/rust-toolchain.toml"; do
			if [[ -f "$config" ]]; then
				printf 'config=%s:%s\n' "${config#"$root/"}" "$(sha256sum "$config" | awk '{print $1}')"
			fi
		done
		for variable in AR CARGO_BUILD_RUSTC CARGO_BUILD_RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CFLAGS RUSTC RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN; do
			printf 'env-%s=%s\n' "$variable" "${!variable-}"
		done
	} | sha256sum | awk '{print $1}')"
	if [[ "$force_rebuild" == 1 || ! -f "$image_target_config" || "$(cat "$image_target_config")" != "$image_target_config_value" ]]; then
		echo "build-shared: Cargo cache miss (global build configuration)"
		rm -rf "$image_target"
		mkdir -p "$(dirname "$image_target_config")"
		printf '%s\n' "$image_target_config_value" >"$image_target_config.tmp"
		mv "$image_target_config.tmp" "$image_target_config"
	else
		echo "build-shared: Cargo cache hit (global build configuration)"
	fi
	service_seed="$root/boot/.build/image-services-seed-$target.o"
	service_seed_errors="$root/boot/.build/image-services-seed-$target.stderr"
	image_graph_key_file="$root/boot/.build/image-cargo-$target.graph-key"
	image_graph_source_digest="$({
		while read -r crate; do
			case "$crate" in
			proto | wire | wasm | term) crate_dir="$crate" ;;
			*) crate_dir="user/$crate" ;;
			esac
			printf '%s=%s\n' "$crate_dir" "$(source_digest "$crate_dir")"
		done < <(awk '$1 == "library" {print $3}' "$root/user/services/manifest.txt" | sort -u)
		for source in "$root/user/build.rs" "$root/../product.conf"; do
			sha256sum "$source"
		done
	} | sha256sum | awk '{print $1}')"
	image_graph_key="$({
		printf 'format=liber-image-graph-cache-v1\n'
		printf 'build-tools=%s\n' "$build_tool_digest"
		printf 'cargo-config=%s\n' "$image_target_config_value"
		printf 'provider-sources=%s\n' "$image_graph_source_digest"
	} | sha256sum | awk '{print $1}')"
	image_graph_valid=0
	if [[ "$force_rebuild" == 0 && -f "$image_graph_key_file" && "$(cat "$image_graph_key_file")" == "$image_graph_key" && -f "$image_graph" && -f "$image_graph_errors" && -f "$image_seed" && -f "$service_seed" && -f "$service_seed_errors" ]] && llvm-readelf -h "$image_seed" | grep -q 'Type:.*REL' && llvm-readelf -h "$service_seed" | grep -q 'Type:.*REL' && grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$image_graph_errors" && grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$image_graph_errors" && grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$service_seed_errors" && grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$service_seed_errors"; then
		image_graph_valid=1
		echo "build-shared: Cargo image graph cache hit"
	else
		echo "build-shared: Cargo image graph cache miss"
		rm -f "$image_seed" "$service_seed"
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
		set +e
		(
			cd "$root/user/services"
			CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --release --target "$cargo_target" --bin component_host --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$service_seed"
		) >>"$image_graph" 2>"$service_seed_errors"
		service_seed_status=$?
		set -e
		if [[ "$service_seed_status" != 101 || ! -f "$service_seed" ]] || ! llvm-readelf -h "$service_seed" | grep -q 'Type:.*REL'; then
			echo "build-shared: services image graph did not stop after emitting its ET_REL seed object" >&2
			exit 1
		fi
		if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$service_seed_errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$service_seed_errors"; then
			echo "build-shared: services image graph failed outside the expected final-link shim boundary" >&2
			exit 1
		fi
		printf '%s\n' "$image_graph_key" >"$image_graph_key_file.tmp"
		mv "$image_graph_key_file.tmp" "$image_graph_key_file"
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

declare -A canonical_order_cache=()
declare -A provider_dependencies=()
declare -A provider_symbols=()
declare -A provider_symbols_indexed=()

build_provider_index() {
	local providers="$1"
	local provider file kind value
	for provider in $providers; do
		if [[ -n "${provider_symbols_indexed[$provider]:-}" ]]; then continue; fi
		file="$(library_file "$provider")"
		if [[ ! -v "provider_dependencies[$provider]" ]]; then provider_dependencies[$provider]=""; fi
		while IFS=$'\t' read -r kind value; do
			case "$kind" in
			D) provider_dependencies[$provider]+=" ${value%.lslib}" ;;
			S) provider_symbols["$provider|$value"]=1 ;;
			esac
		done < <(llvm-readelf --wide -d --dyn-syms "$file" | awk '
			/Shared library:/ {
				name = $0
				sub(/^.*Shared library: \[/, "", name)
				sub(/\].*$/, "", name)
				print "D\t" name
				next
			}
			$1 ~ /^[0-9]+:$/ && $7 != "UND" && $8 != "" {print "S\t" $8}
		')
		provider_dependencies[$provider]="$(tr ' ' '\n' <<<"${provider_dependencies[$provider]}" | sed '/^$/d' | sort -u | xargs)"
		provider_symbols_indexed[$provider]=1
	done
}

canonical_provider_order() {
	local roots="$1"
	local cache_key name dependency dependencies candidate ready result
	cache_key="$(tr ' ' '\n' <<<"$roots" | sort -u | xargs)"
	if [[ -n "${canonical_order_cache[$cache_key]:-}" ]]; then
		printf '%s' "${canonical_order_cache[$cache_key]}"
		return
	fi
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
		if [[ -v "provider_dependencies[$name]" ]]; then
			dependencies="${provider_dependencies[$name]}"
		else
			dependencies="$(llvm-readelf -d "$(library_file "$name")" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sed 's/\.lslib$//' | sort -u | xargs)"
			provider_dependencies[$name]="$dependencies"
		fi
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
	result="$(printf '%s.lslib\n' "${order[@]}")"$'\n'
	canonical_order_cache[$cache_key]="$result"
	printf '%s' "$result"
}

artifacts=()
declare -A provider_compile_digests=()
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
	proto | wire | wasm | term) crate_dir="$crate" ;;
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
	provider_source_sha="$(source_digest "$crate_dir")"
	if [[ "$crate_dir" == user/*-client-provider ]]; then
		provider_compile_source="$(source_digest "${crate_dir%-provider}")"
	else
		provider_compile_source="$provider_source_sha"
	fi
	provider_compile_digests[$artifact]="$({
		printf 'format=liber-provider-compile-identity-v1\n'
		printf 'source=%s\n' "$provider_compile_source"
		printf 'features=%s\n' "$row_features"
		for provider in $row_providers; do
			if [[ -z "${provider_compile_digests[$provider]:-}" ]]; then
				echo "build-shared: $artifact has no compile identity for provider $provider" >&2
				exit 1
			fi
			printf 'provider=%s:%s\n' "$provider" "${provider_compile_digests[$provider]}"
		done
	} | sha256sum | awk '{print $1}')"
	provider_expected_identity="$out.identity.$$.expected"
	write_identity_record library "$artifact" "$crate" "$provider_source_sha" "$row_features" "$row_providers" "$provider_expected_identity"
	provider_expected_needed="$(for provider in $row_providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
	provider_cache_key="$(artifact_cache_key library "$row" "$provider_expected_identity" "cargo=${image_target_config_value:-standalone} rlib=$(sha256sum "$rlib" | awk '{print $1}')")"
	provider_cache_prefix="$artifact_cache_dir/library-$artifact"
	if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$out" "$provider_cache_prefix" "$provider_cache_key" "$provider_expected_identity" "$provider_expected_needed"; then
		echo "build-shared: provider cache hit $artifact"
		((provider_cache_hits += 1))
		rm -f "$provider_expected_identity"
		artifacts+=("$artifact")
		continue
	fi
	echo "build-shared: provider cache miss $artifact"
	((provider_cache_misses += 1))
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
	emit_identity library "$artifact" "$crate" "$provider_source_sha" "$row_features" "$row_providers" "$out"
	record_artifact_cache "$out" "$provider_cache_prefix" "$provider_cache_key"
	rm -f "$provider_expected_identity"
	echo "build-shared: $out ($(stat -c %s "$out") bytes)"
	artifacts+=("$artifact")
done

if [[ -n "$image_graph" ]]; then
	start_obj="$root/boot/.build/exe-start-$target.o"
	"$root/tools/build-exe-start.sh" "$target" "$start_obj"
	dynamic_rows="$(dynamic_rows)"
	declare -A package_source_digests=()
	while read -r package; do
		[[ -n "$package" ]] || continue
		if [[ "$package" != tools ]]; then
			package_source_digests[$package]="$(local_dependency_source_digest "user/$package")"
		fi
	done < <(awk '{print $3}' <<<"$dynamic_rows" | sort -u)
	manifest_tools="$(awk '$3 == "tools" {print $2}' <<<"$dynamic_rows")"
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
		if [[ "$kind" != dynamic || "$stage" != volume ]]; then
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
		consumer_errors="$out_dir/.$consumer.stderr"
		out="$out_dir/$consumer"
		mkdir -p "$out_dir"
		consumer_source_sha="$(executable_source_digest "$consumer_dir" "$crate" "$consumer")"
		consumer_expected_identity="$out.identity.$$.expected"
		write_identity_record executable "$consumer" "$crate" "$consumer_source_sha" shared-image "$providers" "$consumer_expected_identity"
		consumer_expected_needed="$(for provider in $providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
		consumer_expected_order="$out.order.$$.expected"
		canonical_provider_order "$providers" >"$consumer_expected_order"
		consumer_cache_key="$(artifact_cache_key executable "$kind $consumer $crate $stage $providers" "$consumer_expected_identity" "cargo=$image_target_config_value start=$(sha256sum "$start_obj" | awk '{print $1}')")"
		consumer_cache_prefix="$artifact_cache_dir/executable-$consumer"
		if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$out" "$consumer_cache_prefix" "$consumer_cache_key" "$consumer_expected_identity" "$consumer_expected_needed" && [[ -f "$out.order" ]] && cmp -s "$consumer_expected_order" "$out.order"; then
			echo "build-shared: executable cache hit $consumer"
			((executable_cache_hits += 1))
			rm -f "$consumer_expected_identity" "$consumer_expected_order"
			continue
		fi
		echo "build-shared: executable cache miss $consumer"
		((executable_cache_misses += 1))
		object_key="$(object_cache_key "$consumer" "$crate" "$consumer_source_sha" "$providers")"
		object_cache_prefix="$artifact_cache_dir/object-$consumer-$object_key"
		consumer_obj="$object_cache_prefix.o"
		if [[ "$force_rebuild" == 0 ]] && object_cache_valid "$consumer_obj" "$object_cache_prefix" "$object_key"; then
			echo "build-shared: object cache hit $consumer"
			((object_cache_hits += 1))
		else
			echo "build-shared: object cache miss $consumer"
			((object_cache_misses += 1))
			consumer_obj_tmp="$consumer_obj.tmp.$$"
			rm -f "$consumer_obj_tmp"
			"$root/tools/build-consumer-object.sh" "$consumer_dir" "$image_target" "$rust_min_stack" "$rustflags" "$cargo_target" "$consumer" "$consumer_obj_tmp" "$consumer_errors" "${cargo_target_flags[@]}"
			mv "$consumer_obj_tmp" "$consumer_obj"
			record_object_cache "$consumer_obj" "$object_cache_prefix" "$object_key"
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
		build_provider_index "$providers"
		case "$consumer" in
		arp | httpd | ip | nc | nslookup | ping | ss | tcp)
			if ! grep -q '^liber_channel_liber_network_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete network client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_network_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete network client provider" >&2
				exit 1
			fi
			;;
		config | set)
			if ! grep -q '^liber_channel_liber_config_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete config client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_config_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete config client provider" >&2
				exit 1
			fi
			;;
		lsdev | lsusb)
			if ! grep -q '^liber_channel_liber_device_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete device client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_device_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete device client provider" >&2
				exit 1
			fi
			;;
		log)
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter' <<<"$consumer_imports"; then
				echo "build-shared: log contains a generic channel client implementation" >&2
				exit 1
			fi
			for domain in log time; do
				if ! grep -q "^liber_channel_liber_${domain}_" <<<"$consumer_imports"; then
					echo "build-shared: log does not import the concrete $domain client provider" >&2
					exit 1
				fi
				if grep -Eq "^liber_channel_impl_liber_${domain}_" <<<"$consumer_imports"; then
					echo "build-shared: log bypasses the concrete $domain client provider" >&2
					exit 1
				fi
			done
			;;
		date)
			if ! grep -q '^liber_channel_liber_time_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_time_' <<<"$consumer_imports"; then
				echo "build-shared: date bypasses the concrete time client provider" >&2
				exit 1
			fi
			;;
		lssvc)
			if ! grep -q '^liber_channel_liber_observability_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_observability_' <<<"$consumer_imports"; then
				echo "build-shared: lssvc bypasses the concrete observability client provider" >&2
				exit 1
			fi
			;;
		ps | run)
			if ! grep -q '^liber_channel_liber_process_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete process client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_process_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete process client provider" >&2
				exit 1
			fi
			if [[ "$consumer" == ps ]] && ! grep -q '^liber_channel_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: ps does not import the concrete resources client provider" >&2
				exit 1
			fi
			if [[ "$consumer" == ps ]] && grep -Eq '^liber_channel_impl_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: ps bypasses the concrete resources client provider" >&2
				exit 1
			fi
			;;
		usage)
			if ! grep -q '^liber_channel_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete resources client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete resources client provider" >&2
				exit 1
			fi
			;;
		beep)
			if ! grep -q '^liber_channel_liber_audio_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_audio_' <<<"$consumer_imports"; then
				echo "build-shared: beep bypasses the concrete audio client provider" >&2
				exit 1
			fi
			;;
		play)
			for symbol in audio_open_stream pcm_stream_write pcm_stream_close; do
				if ! grep -q "^liber_channel_liber_audio_${symbol}$" <<<"$consumer_imports"; then
					echo "build-shared: play does not import concrete audio symbol $symbol" >&2
					exit 1
				fi
			done
			if grep -Eq '^liber_channel_impl_liber_audio_' <<<"$consumer_imports"; then
				echo "build-shared: play bypasses the concrete audio client provider" >&2
				exit 1
			fi
			if ! grep -q '^liber_channel_liber_storage_volume_open$' <<<"$consumer_imports" || grep -Eq '^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: play bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		cat)
			if ! grep -q '^liber_channel_liber_storage_volume_open$' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: cat bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		rm | mkdir | rmdir)
			method="$consumer"
			if [[ "$consumer" == rm ]]; then method=remove; fi
			if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		write)
			for phase in begin finish; do
				if ! grep -q "^liber_channel_liber_storage_volume_write_stream_${phase}$" <<<"$consumer_imports"; then
					echo "build-shared: write does not import concrete write-stream $phase" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: write bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		perm)
			if ! grep -q '^liber_channel_liber_security_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_security_' <<<"$consumer_imports"; then
				echo "build-shared: perm bypasses the concrete security client provider" >&2
				exit 1
			fi
			;;
		esac
		declare -A used_consumer_providers=()
		for symbol in $consumer_imports; do
			count=0
			owner=""
			for provider in $providers; do
				if [[ -n "${provider_symbols["$provider|$symbol"]:-}" ]]; then
					((count += 1))
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
		emit_identity executable "$consumer" "$crate" "$consumer_source_sha" shared-image "$providers" "$out"
		mv "$consumer_expected_order" "$out.order"
		record_artifact_cache "$out" "$consumer_cache_prefix" "$consumer_cache_key"
		rm -f "$consumer_expected_identity"
		echo "build-shared: $out ($(stat -c %s "$out") bytes, PIE)"
	done <<<"$dynamic_rows"
fi

if printf '%s\n' "${artifacts[@]}" | grep -qx pix; then
	probe="user/dyn_probe"
	probe_dir="$probe/shared/$target"
	probe_out="$probe_dir/dyn_probe"
	mkdir -p "$probe_dir"
	probe_source_sha="$(source_digest "$probe")"
	probe_expected_identity="$probe_out.identity.$$.expected"
	write_identity_record executable dyn_probe dyn_probe "$probe_source_sha" - "pix proto lsrt" "$probe_expected_identity"
	probe_expected_needed="$(printf '%s\n' pix.lslib proto.lslib lsrt.lslib | sort -u)"
	probe_expected_order="$probe_out.order.$$.expected"
	canonical_provider_order "pix proto lsrt" >"$probe_expected_order"
	probe_cache_key="$(artifact_cache_key executable "dynamic dyn_probe dyn_probe volume pix proto lsrt" "$probe_expected_identity" "cargo=$image_target_config_value")"
	probe_cache_prefix="$artifact_cache_dir/executable-dyn_probe"
	if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$probe_out" "$probe_cache_prefix" "$probe_cache_key" "$probe_expected_identity" "$probe_expected_needed" && [[ -f "$probe_out.order" ]] && cmp -s "$probe_expected_order" "$probe_out.order"; then
		echo "build-shared: executable cache hit dyn_probe"
		rm -f "$probe_expected_identity" "$probe_expected_order"
		exit 0
	fi
	echo "build-shared: executable cache miss dyn_probe"
	(cd "$probe" && RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib)
	probe_rlib="$(find "$probe/target/$target/release/deps" -maxdepth 1 -name 'libdyn_probe-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv -e _start --whole-archive "$probe_rlib" --no-whole-archive "$(library_file pix)" "$(library_file proto)" "$(library_file lsrt)" --no-allow-shlib-undefined -o "$probe_out"
	llvm-strip --strip-debug "$probe_out"
	if ! llvm-readelf -h "$probe_out" | grep -q 'Type:.*DYN' || ! llvm-readelf -d "$probe_out" | grep -q 'NEEDED.*pix.lslib'; then
		echo "build-shared: $probe_out is not a pix.lslib-linked ET_DYN" >&2
		exit 1
	fi
	emit_identity executable dyn_probe dyn_probe "$probe_source_sha" - "pix proto lsrt" "$probe_out"
	mv "$probe_expected_order" "$probe_out.order"
	record_artifact_cache "$probe_out" "$probe_cache_prefix" "$probe_cache_key"
	rm -f "$probe_expected_identity"
	echo "build-shared: $probe_out ($(stat -c %s "$probe_out") bytes)"
fi
