#!/usr/bin/env bash
# Keep the custom test harness as the only source of #[test_case] descriptors.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TESTS="$ROOT/kernel/tests.rs"
count="$(grep -c '#\[test_case\]' "$TESTS")"
if [[ "$count" -ne 1 ]]; then
	echo "test tag check: expected only tagged_test!'s generated #[test_case], found $count occurrences" >&2
	grep -n '#\[test_case\]' "$TESTS" >&2 || true
	exit 1
fi
if grep -Eq 'tagged_test!\([^,]+, \[\s*\]\)' "$TESTS"; then
	echo "test tag check: an empty tag list was found" >&2
	exit 1
fi

allowed="$(sed -n '/^define_test_tags! {/,/^}/p' "$TESTS" | sed -n 's/^[[:space:]]*\([A-Za-z0-9_]*\) =>.*/\1/p')"
descriptors=0
while IFS= read -r descriptor; do
	tags="$(printf '%s\n' "$descriptor" | grep -oE '\[[A-Za-z][A-Za-z0-9_, ]*\]' | tail -1 | tr -d '[],')"
	for tag in $tags; do
		if ! grep -qx "$tag" <<<"$allowed"; then
			echo "test tag check: unknown descriptor tag '$tag'" >&2
			exit 1
		fi
	done
	descriptors=$((descriptors + 1))
done < <(
	awk '
		/^[[:space:]]*tagged_test!\(/ {
			block = $0
			if ($0 ~ /\);/) print block
			else capture = 1
			next
		}
		capture {
			block = block " " $0
			if ($0 ~ /\);/) {
				print block
				capture = 0
			}
		}
	' "$TESTS"
)
if [[ "$descriptors" -eq 0 ]]; then
	echo "test tag check: no tagged tests found" >&2
	exit 1
fi
echo "test tag check: $descriptors kernel tests use canonical tagged descriptors"
