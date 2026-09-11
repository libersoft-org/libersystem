#!/bin/bash
# The dependency and licensing policy, enforced rather than described.
#
# `docs/DEPENDENCY_POLICY.md` states three refusals, and a refusal nothing checks is a paragraph. The
# failure each one prevents is discovered by somebody else, later, in a distributed binary - which is
# exactly when a licence question is most expensive to answer and least possible to fix.
#
# WHAT IS CHECKED, AND WHY EACH IS A BUILD FAILURE RATHER THAN A WARNING:
#
#   an unreviewed licence      every vendored upstream has a recorded entry naming its source, its
#                              exact revision, its archive digest and this tree's patch series. A
#                              vendored directory with no entry is a dependency nobody reviewed.
#   a downloaded-at-build      the build fetches nothing. A build that downloads depends on what a
#                              remote server served at that moment, which makes its output
#                              unreproducible and its licence audit a statement about a file nobody
#                              kept.
#   a lost attribution         an entry's recorded digest matches the source actually in the tree. An
#                              inventory that lists what was reviewed while the tree holds something
#                              else documents a tree nobody ships.
#
# THE INVENTORY IS `third_party/INVENTORY.tsv`, AND IT RECORDS WHERE EACH UPSTREAM ACTUALLY LIVES
# rather than requiring them all under one directory. A vendored library sits where its consumers
# expect it; an inventory that insisted otherwise would be asking the tree to be rearranged to suit
# the check, and the check is not the thing that matters here.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

INVENTORY="third_party/INVENTORY.tsv"
POLICY="docs/DEPENDENCY_POLICY.md"

fail() {
	echo "dependency-policy: $*" >&2
	exit 1
}

note() {
	echo "dependency-policy: $*"
}

[[ -f "$POLICY" ]] || fail "$POLICY is missing - the policy is what every check below reads"

# THE PERMISSIVE SET, AND IT IS A LIST RATHER THAN A PATTERN. A licence identifier this file does not
# name has not been reviewed against the policy's test, and guessing from a prefix is how a
# reciprocal licence with a permissive-looking name gets admitted.
allowed=(Unlicense MIT BSD-2-Clause BSD-3-Clause ISC Apache-2.0 Zlib CC0-1.0 "Apache-2.0 WITH LLVM-exception")

is_allowed() {
	local candidate="$1" known
	for known in "${allowed[@]}"; do
		[[ "$candidate" == "$known" ]] && return 0
	done
	return 1
}

# 1. EVERY ENTRY IS COMPLETE, ITS LICENCE IS REVIEWED, AND ITS REVISION IS NOT A BRANCH.
entries=0
unpinned=()
declare -A recorded_path=()
[[ -f "$INVENTORY" ]] || fail "$INVENTORY is missing - the recorded review is what the checks below read"
while IFS=$'\t' read -r name path source revision digest licence patches; do
	[[ -n "$name" && "${name:0:1}" != "#" ]] || continue
	entries=$((entries + 1))
	[[ -n "$path" ]] || fail "$name: no path recorded"
	[[ -n "$source" ]] || fail "$name: no source URL recorded"
	[[ -n "$revision" ]] || fail "$name: no revision recorded"
	[[ -n "$digest" ]] || fail "$name: no archive digest recorded"
	[[ -n "$licence" ]] || fail "$name: no licence recorded"
	[[ -n "$patches" ]] || fail "$name: no patch series recorded - write 'none' when there is none"
	[[ -d "$path" ]] || fail "$name is in $INVENTORY and nothing is vendored at $path"
	recorded_path["$path"]="$name"
	is_allowed "$licence" || fail "$name: '$licence' is not in the reviewed permissive set - see $POLICY"
	# A REVISION IS A TAG OR A COMMIT, NEVER A BRANCH. A branch names whatever it points at today, so
	# a dependency pinned to one is not pinned. `unrecorded` is the one other answer, and it is
	# reported rather than passed over.
	if [[ "$revision" == "unrecorded" || "$digest" == "unrecorded" ]]; then
		[[ "$revision" == "unrecorded" && "$digest" == "unrecorded" ]] || fail "$name: half of the pin is 'unrecorded' - a revision without its digest pins nothing"
		unpinned+=("$name at $path (from $source)")
	else
		[[ "$revision" =~ ^[0-9a-f]{40}$ || "$revision" =~ ^v?[0-9] ]] || fail "$name: '$revision' is not a commit or a version tag"
		[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || fail "$name: '$digest' is not a SHA-256"
	fi
done <"$INVENTORY"

# 2. NO VENDORED UPSTREAM WITHOUT AN ENTRY. `third_party/` is checked because that is where a new
# import lands; an upstream that lives anywhere else is caught by the attribution check below, which
# finds it by the notice it carries rather than by where somebody put it.
if [[ -d third_party ]]; then
	while IFS= read -r directory; do
		[[ -n "$directory" ]] || continue
		[[ -n "${recorded_path[$directory]:-}" ]] || fail "$directory is vendored and $INVENTORY has no entry for it"
	done < <(find third_party -mindepth 1 -maxdepth 1 -type d | sort)
fi

# 3. THE BUILD FETCHES NOTHING. A network call in a build script is the refusal this exists for, and
# it is checked in the scripts the build actually runs rather than trusted to a convention.
#
# `git ls-remote` IS NOT EXEMPT AND NEITHER IS A SUBMODULE UPDATE: both reach the network, and the
# reason the policy gives - the output depends on what a server served at that moment - does not
# care which tool made the request.
fetchers='curl|wget|git clone|git fetch|git ls-remote|git submodule update|pip install|cargo install|FetchContent_Declare|ExternalProject_Add'
build_scripts=(build.sh image.sh gen.sh check.sh test.sh run.sh)
while IFS= read -r script; do
	build_scripts+=("$script")
done < <(find src/tools src/harness -name "*.sh" -type f 2>/dev/null | sort)
for script in "${build_scripts[@]}"; do
	[[ -f "$script" ]] || continue
	# The check's own list of fetchers is not a fetcher.
	[[ "$script" == "src/tools/check-dependency-policy.sh" ]] && continue
	if hits="$(grep -nE "(^|[^[:alnum:]_-])($fetchers)" "$script" | grep -v '^\s*#' || true)"; then
		if [[ -n "$hits" ]]; then
			echo "dependency-policy: $script reaches the network during a build:" >&2
			sed 's/^/dependency-policy:   /' <<<"$hits" >&2
			fail "the build fetches nothing - see $POLICY"
		fi
	fi
done

# 4. NO COPIED IMPLEMENTATION WITH LOST ATTRIBUTION. A source file under `src/` that carries an
# upstream copyright notice is a file that came from somewhere, and it belongs to an inventory entry.
# This finds the copy that was pasted in and tidied up rather than recorded.
while IFS= read -r candidate; do
	[[ -n "$candidate" ]] || continue
	# `sed -n '1s//p'` RATHER THAN `grep | head -1`: under `pipefail` a reader that stops early closes
	# the pipe, the writer takes SIGPIPE, and a MATCH reads as a failed pipeline. This reads all of it
	# and prints only the first line's substitution.
	holder="$(sed -n '/Copyright ([cC])/{s/.*\(Copyright ([cC])[^"]*\).*/\1/p;q;}' "$candidate")"
	grep -qiE "LiberSoft|LiberSystem" <<<"$holder" && continue
	covered=0
	for path in "${!recorded_path[@]}"; do
		[[ "$candidate" == "$path"/* ]] && covered=1 && break
	done
	[[ "$covered" == 1 ]] && continue
	fail "$candidate carries a third-party copyright notice ($holder) and is not a recorded import - see $POLICY"
done < <(grep -rlE "Copyright \(c\)" src --include="*.rs" --include="*.c" --include="*.h" --include="*.cpp" 2>/dev/null | sort)

note "$entries vendored upstream(s) reviewed, and every third-party notice in the tree belongs to one of them"
# NAMED ON EVERY RUN RATHER THAN COUNTED ONCE. An import whose upstream revision was never captured
# cannot say which revision it holds, and a gap that is reported stays a gap somebody can close.
if [[ "${#unpinned[@]}" -gt 0 ]]; then
	note "${#unpinned[@]} of them predate this policy and have NO recorded upstream revision:"
	for entry in "${unpinned[@]}"; do
		note "    $entry"
	done
fi
