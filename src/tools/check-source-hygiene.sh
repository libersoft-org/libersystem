#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

mode="${1:---current}"
case "$mode" in
--added | --current | --history) ;;
*)
	echo "usage: $0 [--added|--current|--history]" >&2
	exit 2
	;;
esac

path_pattern='\.(lslib|lsexe|rlib|rmeta|wasm|o|a)$|(^|/)(\.build|target|shared)/'

check_added_paths() {
	local -a added_paths=()
	local path
	local type
	local user_root
	local manifest_pattern='(read_to_string|join).*user/services/manifest[.]toml'

	if git rev-parse --verify HEAD >/dev/null 2>&1; then
		mapfile -d '' -t added_paths < <(git diff --cached --name-only -z --diff-filter=A HEAD)
	else
		mapfile -d '' -t added_paths < <(git diff --cached --name-only -z --diff-filter=A --root)
	fi

	for path in "${added_paths[@]}"; do
		if [[ "$path" =~ $path_pattern ]]; then
			echo "source-hygiene: generated artifact is newly staged: $path" >&2
			exit 1
		fi

		[[ "$path" == src/* && -f "$path" ]] || continue

		type="$(file --brief --mime-type "$path")"
		case "$type" in
		application/wasm | application/x-archive | application/x-executable | application/x-object | application/x-pie-executable | application/x-sharedlib | application/x-dosexec | application/vnd.microsoft.portable-executable)
			echo "source-hygiene: compiled binary content is newly staged: $path ($type)" >&2
			exit 1
			;;
		esac

		if [[ "$path" == src/user/* ]]; then
			user_root="${path#src/user/}"
			user_root="${user_root%%/*}"
			case "$user_root" in
			.cargo | apps | drivers | libs | runtime | services | build.rs | rust-toolchain.toml | user.ld | user-aarch64.ld | user-riscv64.ld | x86_64-unknown-none.json) ;;
			*)
				echo "source-hygiene: undeclared src/user root is newly staged: $path" >&2
				exit 1
				;;
			esac
			if [[ "$path" =~ ^src/user/[^/]+/Cargo.toml$ ]]; then
				echo "source-hygiene: a Cargo crate is newly staged directly under src/user: $path" >&2
				exit 1
			fi
		fi

		case "$path" in
		src/tools/system-manifest/src/lib.rs | src/tools/system-manifest/src/main.rs) ;;
		*.rs | *.sh)
			if grep -Eq "$manifest_pattern" "$path"; then
				echo "source-hygiene: direct manifest reader is newly staged outside tools/system-manifest: $path" >&2
				exit 1
			fi
			;;
		esac
	done

	echo "source-hygiene: clean (--added; ${#added_paths[@]} new files)"
}

if [[ "$mode" == --added ]]; then
	check_added_paths
	exit 0
fi

manifest_json="$(src/tools/system-manifest.sh export-json)"

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

invalid_test_modules="$({
	find src/kernel -type f -path '*/tests.rs' ! -path 'src/kernel/tests.rs' -print0 |
		while IFS= read -r -d '' tests; do
			module_dir="$(dirname "$tests")"
			module_name="$(basename "$module_dir")"
			parent_dir="$(dirname "$module_dir")"
			if [[ ! -f "$module_dir/mod.rs" || -f "$parent_dir/$module_name.rs" ]]; then
				printf '%s\n' "$module_dir"
			fi
		done
} | sort -u)"
if [[ -n "$invalid_test_modules" ]]; then
	echo "source-hygiene: every non-root kernel tests.rs module requires mod.rs without a sibling module file:" >&2
	printf '%s\n' "$invalid_test_modules" >&2
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
allowed_manifest_readers="$(printf '%s\n' src/tools/system-manifest/src/lib.rs src/tools/system-manifest/src/main.rs src/user/services/core/build.rs | sort)"
if [[ "$manifest_readers" != "$allowed_manifest_readers" ]]; then
	echo "source-hygiene: direct manifest readers differ from the ownership allowlist:" >&2
	diff -u <(printf '%s\n' "$allowed_manifest_readers") <(printf '%s\n' "$manifest_readers") >&2 || true
	exit 1
fi

if [[ -n "$(find src/user -mindepth 2 -maxdepth 2 -name Cargo.toml -print -quit)" ]]; then
	echo "source-hygiene: a Cargo crate remains directly under src/user:" >&2
	find src/user -mindepth 2 -maxdepth 2 -name Cargo.toml -print >&2
	exit 1
fi

# A reader that stops at its first match closes the pipe. The producer's next write then fails
# with EPIPE, which the llvm tools report as exit 74, and `pipefail` makes that the status of the
# whole pipeline, so a successful match reads as a failed read. It is a race against the
# producer's final flush, so such a check passes review and most runs and then rejects a correct
# artifact under load. Match against captured output instead.
#
# `grep -q` was the shape this knew about, and it was not the only one. It missed an
# `llvm-readelf | awk '...; exit}'` in the injection checks, which is what made a gate exit 74
# with no output, a different gate each run.
#
# `head` and friends are the same hazard and are NOT scanned for yet: the tree has a dozen
# `find | sort | head -n1` pipelines whose producers would take SIGPIPE the same way, and
# converting them is its own piece of work. Recorded in M0142 rather than left implied.
#
# Comment lines are skipped, or this check flags the paragraph explaining itself.
# The awk alternative deliberately does not try to respect quoting: an awk program routinely
# contains BOTH quote characters - `awk '$1 == "DYNAMIC" {print $2; exit}'` is the very line
# this was written for - so a class excluding them stops at the first inner quote and matches
# nothing. That was this check's first version, and it passed the tree while the bug was in it.
early_close_pattern="^[^#]*[^|][|] *(grep -[A-Za-z]*q|grep -[A-Za-z]*m[0-9 ]|awk .*exit *}|sed -n .*[;']q)"
early_close_pipelines=""
while IFS= read -r script; do
	grep -q pipefail "$script" || continue
	script_matches="$(grep -nE "$early_close_pattern" "$script")" || continue
	early_close_pipelines+="$(sed "s#^#$script:#" <<<"$script_matches")"$'\n'
done < <(find src -name '*.sh' | sort)
if [[ -n "$early_close_pipelines" ]]; then
	echo "source-hygiene: a script using pipefail pipes into a reader that can stop early, where a match reads as a failed pipeline:" >&2
	printf '%s' "$early_close_pipelines" >&2
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
			if git grep --untracked -n -F "src/user/$owner/$suffix" -- ':(exclude,glob)docs/todo/**' ':!NOTES.md' >/dev/null; then
				echo "source-hygiene: stale pre-move path src/user/$owner/$suffix remains:" >&2
				git grep --untracked -n -F "src/user/$owner/$suffix" -- ':(exclude,glob)docs/todo/**' ':!NOTES.md' >&2
				exit 1
			fi
		done
	elif git grep --untracked -n -F "src/user/$owner/" -- ':(exclude,glob)docs/todo/**' ':!NOTES.md' >/dev/null; then
		echo "source-hygiene: stale pre-move path src/user/$owner/ remains:" >&2
		git grep --untracked -n -F "src/user/$owner/" -- ':(exclude,glob)docs/todo/**' ':!NOTES.md' >&2
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
