#!/usr/bin/env bash
# Build one signed-boot trust profile and acquire its immutable copy under the producer lock.
set -euo pipefail
profile="${1:?usage: build-loader-private.sh <test-trust|external-release> <private-output>}"
output="$(realpath -m "${2:?a private output path is required}")"
target="$(dirname "$output")/cargo-loader"
root="$(cd "$(dirname "$0")/../.." && pwd)"
case "$profile" in
test-trust | external-release) ;;
*)
	echo "build-loader-private: unknown profile: $profile" >&2
	exit 2
	;;
esac
mkdir -p "$root/.build/state" "$(dirname "$output")"
(
	flock 9
	cd "$root/src/boot/loader"
	# Both profiles reuse this run's dependencies without relinking the ordinary loader. Even
	# restoring its old profile changes PE timestamps/debug identity and invalidates image keys.
	LIBER_TRUST_PROFILE="$profile" CARGO_TARGET_DIR="$target" cargo build --quiet
	# Keep this copy inside the same lock as the build. The next producer may change profile.
	cp "$target/x86_64-unknown-uefi/debug/libersystem-loader.efi" "$output"
) 9>"$root/.build/state/kernel-test-build.lock"
