#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
# The build directory has a shape; a script that writes into it makes sure its place exists.
mkdir -p "$build_root/state"
crate="$root/tools/system-manifest"
target_dir="$build_root/cargo/system-manifest"
binary="$target_dir/debug/system-manifest"
key_file="$build_root/state/manifest-tool.key"
lock_file="$build_root/state/manifest-tool.lock"

# NAMED, for the reason build-shared gives at its own check: a bare `command -v` under `set -e`
# exits with status 1 and prints nothing, and this script runs before build-shared says anything -
# so a machine without `flock` failed a build with no output at all.
missing_tools=""
for tool in flock sha256sum find; do
	command -v "$tool" >/dev/null 2>&1 || missing_tools+=" $tool"
done
if [[ -n "$missing_tools" ]]; then
	echo "system-manifest: required tools not found:$missing_tools" >&2
	exit 1
fi
mkdir -p "$build_root"
exec 9>"$lock_file"
flock 9

key="$({
	find "$crate/src" -type f -name '*.rs' -print0
	printf '%s\0' "$crate/Cargo.toml" "$crate/Cargo.lock"
} | sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}')"
if [[ ! -x "$binary" || ! -f "$key_file" || "$(<"$key_file")" != "$key" ]]; then
	CARGO_TARGET_DIR="$target_dir" cargo build --quiet --manifest-path "$crate/Cargo.toml" --bin system-manifest
	printf '%s\n' "$key" >"$key_file.tmp"
	mv "$key_file.tmp" "$key_file"
fi

exec "$binary" "$@"
