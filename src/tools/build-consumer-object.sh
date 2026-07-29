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

object_header=""
if [[ -f "$object" ]]; then
	object_header="$(llvm-readelf -h "$object")" || object_header=""
fi
if [[ "$status" != 101 || ! -f "$object" ]] || ! grep -q 'Type:.*REL' <<<"$object_header"; then
	# Say which of the three expectations broke. They fail for different reasons and want
	# different answers: a status other than 101 means the build did not reach the final-link
	# shim collision this technique relies on, a missing object means cargo did not re-invoke
	# rustc so the `--emit` never ran, and a present object of the wrong kind means it did run
	# and produced something else. One message for all three sends the reader to the wrong one.
	if [[ "$status" != 101 ]]; then
		echo "build-consumer-object: $consumer exited $status, not the expected 101 from the final-link shim collision (see $errors)" >&2
	elif [[ ! -f "$object" ]]; then
		echo "build-consumer-object: $consumer stopped at the expected link failure but emitted no object at $object; cargo did not re-invoke rustc, so --emit never ran" >&2
	else
		echo "build-consumer-object: $consumer emitted $object, but it is $(awk '/Type:/{print $2}' <<<"$object_header") rather than ET_REL" >&2
	fi
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
