#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
manifest_json="$(src/tools/system-manifest.sh export-json)"

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

expected_user_root="$({
	printf '%s\n' .cargo apps drivers libs runtime services
	printf '%s\n' build.rs rust-toolchain.toml user.ld user-aarch64.ld user-riscv64.ld x86_64-unknown-none.json
} | sort)"
actual_user_root="$(find src/user -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)"
if [[ "$actual_user_root" != "$expected_user_root" ]]; then
	echo "source-hygiene: src/user contains an undeclared role or infrastructure path:" >&2
	diff -u <(printf '%s\n' "$expected_user_root") <(printf '%s\n' "$actual_user_root") >&2 || true
	exit 1
fi

manifest_pattern='(read_to_string|join).*user/services/manifest[.]toml'
manifest_readers="$(grep -RIlE "$manifest_pattern" src --include='*.rs' --include='*.sh' | sort || true)"
allowed_manifest_readers="$(printf '%s\n' src/tools/system-manifest/src/lib.rs src/tools/system-manifest/src/main.rs | sort)"
if [[ "$manifest_readers" != "$allowed_manifest_readers" ]]; then
	echo "source-hygiene: direct manifest readers differ from the ownership allowlist:" >&2
	diff -u <(printf '%s\n' "$allowed_manifest_readers") <(printf '%s\n' "$manifest_readers") >&2 || true
	exit 1
fi

if find src/user -mindepth 2 -maxdepth 2 -name Cargo.toml -print -quit | grep -q .; then
	echo "source-hygiene: a Cargo crate remains directly under src/user:" >&2
	find src/user -mindepth 2 -maxdepth 2 -name Cargo.toml -print >&2
	exit 1
fi

source_rows="$(mktemp)"
physical_user_crates="$(mktemp)"
declared_user_crates="$(mktemp)"
trap 'rm -f "$source_rows" "$physical_user_crates" "$declared_user_crates"' EXIT
jq -r '.sources[] | [.owner, .path] | @tsv' <<<"$manifest_json" | sort >"$source_rows"
duplicate_owner="$(cut -f1 "$source_rows" | uniq -d | head -n1)"
duplicate_path="$(cut -f2 "$source_rows" | sort | uniq -d | head -n1)"
if [[ -n "$duplicate_owner" || -n "$duplicate_path" ]]; then
	echo "source-hygiene: duplicate manifest source owner or path: ${duplicate_owner:-$duplicate_path}" >&2
	exit 1
fi

while IFS=$'\t' read -r owner path; do
	if [[ -z "$owner" || -z "$path" || "$path" == /* || "$path" == *".."* || ! -f "src/$path/Cargo.toml" ]]; then
		echo "source-hygiene: invalid or missing manifest source path for $owner: $path" >&2
		exit 1
	fi
done <"$source_rows"

find src/user -mindepth 2 -name Cargo.toml -printf '%h\n' | sed 's#^src/##' | sort >"$physical_user_crates"
cut -f2 "$source_rows" | grep '^user/' | sort >"$declared_user_crates"
if ! cmp -s "$physical_user_crates" "$declared_user_crates"; then
	echo "source-hygiene: physical userspace Cargo roots differ from manifest ownership:" >&2
	diff -u "$declared_user_crates" "$physical_user_crates" >&2 || true
	exit 1
fi

while IFS=$'\t' read -r owner path; do
	[[ "$path" == user/*/* ]] || continue
	if [[ "$path" == "user/$owner/"* ]]; then
		for suffix in Cargo.toml Cargo.lock rust-toolchain.toml src/; do
			if git grep --untracked -n -F "src/user/$owner/$suffix" -- ':!TODO.md' ':!NOTES.md' >/dev/null; then
				echo "source-hygiene: stale pre-move path src/user/$owner/$suffix remains:" >&2
				git grep --untracked -n -F "src/user/$owner/$suffix" -- ':!TODO.md' ':!NOTES.md' >&2
				exit 1
			fi
		done
	elif git grep --untracked -n -F "src/user/$owner/" -- ':!TODO.md' ':!NOTES.md' >/dev/null; then
		echo "source-hygiene: stale pre-move path src/user/$owner/ remains:" >&2
		git grep --untracked -n -F "src/user/$owner/" -- ':!TODO.md' ':!NOTES.md' >&2
		exit 1
	fi
done <"$source_rows"

if [[ "$mode" == --history ]]; then
	historical="$(git rev-list --objects HEAD | awk 'NF > 1 {sub(/^[^ ]+ /, ""); print}' | grep -E "$path_pattern" | sort -u || true)"
	if [[ -n "$historical" ]]; then
		echo "source-hygiene: generated artifacts remain in reachable history:" >&2
		printf '%s\n' "$historical" >&2
		exit 1
	fi
fi

rm -f "$source_rows" "$physical_user_crates" "$declared_user_crates"
trap - EXIT
echo "source-hygiene: clean ($mode)"
