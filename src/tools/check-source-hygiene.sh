#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

mode="${1:---current}"
case "$mode" in
--current | --history) ;;
*)
	echo "usage: $0 [--current|--history]" >&2
	exit 2
	;;
esac

path_pattern='\.(lslib|lsexe|rlib|rmeta|wasm|o|a)$|(^|/)(\.build|target|shared)/'
physical="$(find src \( -type d \( -name .build -o -name target -o -name shared \) -o -type f \( -name '*.lslib' -o -name '*.lsexe' -o -name '*.rlib' -o -name '*.rmeta' -o -name '*.wasm' -o -name '*.o' -o -name '*.a' \) \) -print)"
if [[ -n "$physical" ]]; then
	echo "source-hygiene: generated artifacts exist under src:" >&2
	printf '%s\n' "$physical" >&2
	exit 1
fi

magic="$(find src -type f -print0 | while IFS= read -r -d '' file; do
	type="$(file --brief --mime-type "$file")"
	case "$type" in
	application/wasm | application/x-archive | application/x-executable | application/x-object | application/x-pie-executable | application/x-sharedlib | application/x-dosexec | application/vnd.microsoft.portable-executable)
		printf '%s: %s\n' "$file" "$type"
		;;
	esac
done)"
if [[ -n "$magic" ]]; then
	echo "source-hygiene: compiled binary content exists under src:" >&2
	printf '%s\n' "$magic" >&2
	exit 1
fi

tracked="$(git ls-files | grep -E "$path_pattern" || true)"
if [[ -n "$tracked" ]]; then
	echo "source-hygiene: generated artifacts are tracked by Git:" >&2
	printf '%s\n' "$tracked" >&2
	exit 1
fi

if [[ "$mode" == --history ]]; then
	historical="$(git rev-list --objects HEAD | awk 'NF > 1 {sub(/^[^ ]+ /, ""); print}' | grep -E "$path_pattern" | sort -u || true)"
	if [[ -n "$historical" ]]; then
		echo "source-hygiene: generated artifacts remain in reachable history:" >&2
		printf '%s\n' "$historical" >&2
		exit 1
	fi
fi

echo "source-hygiene: clean ($mode)"
