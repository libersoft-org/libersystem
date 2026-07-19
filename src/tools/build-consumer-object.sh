#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 8 ]]; then
	echo "usage: $0 <consumer-dir> <cargo-target-dir> <rust-min-stack> <rustflags> <cargo-target> <consumer> <object> <errors> [cargo-target-flags...]" >&2
	exit 2
fi

consumer_dir="$1"
image_target="$2"
rust_min_stack="$3"
rustflags="$4"
cargo_target="$5"
consumer="$6"
object="$7"
errors="$8"
shift 8
cargo_target_flags=("$@")

rm -f "$object"
set +e
(
	cd "$consumer_dir"
	CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --quiet --release --target "$cargo_target" --bin "$consumer" --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$object"
) >/dev/null 2>"$errors"
status=$?
set -e

if [[ "$status" != 101 || ! -f "$object" ]] || ! llvm-readelf -h "$object" | grep -q 'Type:.*REL'; then
	echo "build-consumer-object: $consumer did not stop after emitting its ET_REL object" >&2
	exit 1
fi
if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$errors"; then
	echo "build-consumer-object: $consumer failed outside the expected final-link shim boundary" >&2
	exit 1
fi
definitions="$(llvm-readelf --wide --symbols "$object" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
if [[ "$definitions" != __user_main ]]; then
	echo "build-consumer-object: $object defines globals outside __user_main: $definitions" >&2
	exit 1
fi
