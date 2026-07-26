#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
	echo "usage: source-path.sh <logical-owner>" >&2
	exit 2
fi

owner="$1"
root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
crate="$root/tools/system-manifest"
manifest="$root/user/services/manifest.toml"
cache="$build_root/system-manifest-source-paths.tsv"
state_file="$cache.state"
inputs_file="$cache.inputs"
lock_file="$cache.lock"

command -v flock >/dev/null
command -v jq >/dev/null
command -v sha256sum >/dev/null
mkdir -p "$build_root"
exec 9>"$lock_file"
flock 9

state=""
if [[ -f "$inputs_file" ]]; then
	mapfile -t inputs <"$inputs_file"
	if ((${#inputs[@]} != 0)); then state="$(stat -Lc $'input\t%n\t%s\t%i\t%y\t%z' "${inputs[@]}")"; fi
fi

if [[ -z "$state" || ! -f "$cache" || ! -f "$state_file" || "$(<"$state_file")" != "$state" ]]; then
	temporary="$cache.tmp.$$"
	inputs_temporary="$inputs_file.tmp.$$"
	{
		printf '%s\n' "$manifest" "$root/tools/system-manifest.sh" "$crate/Cargo.toml" "$crate/Cargo.lock" "$crate/src"
		find "$crate/src" \( -type d -o -type f \) -print
	} | sort -u >"$inputs_temporary"
	mapfile -t inputs <"$inputs_temporary"
	state="$(stat -Lc $'input\t%n\t%s\t%i\t%y\t%z' "${inputs[@]}")"
	"$root/tools/system-manifest.sh" export-json | jq -r '.sources[] | [.owner, .path] | @tsv' >"$temporary"
	printf '%s\n' "$state" >"$state_file.tmp.$$"
	mv "$temporary" "$cache"
	mv "$inputs_temporary" "$inputs_file"
	mv "$state_file.tmp.$$" "$state_file"
fi

path="$(awk -F '\t' -v owner="$owner" '$1 == owner { print $2; found = 1; exit } END { exit !found }' "$cache")" || exec "$root/tools/system-manifest.sh" source-path "$owner"
printf '%s\n' "$path"
