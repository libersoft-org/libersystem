#!/usr/bin/env bash
# Keep the custom test harness as the only source of #[test_case] descriptors.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ROOT_TESTS="$ROOT/kernel/tests.rs"
mapfile -t TEST_FILES < <(find "$ROOT/kernel" -type f -name tests.rs -print | sort)
count="$(grep -h '#\[test_case\]' "${TEST_FILES[@]}" | wc -l)"
if [[ "$count" -ne 1 ]]; then
	echo "test tag check: expected only tagged_test!'s generated #[test_case], found $count occurrences" >&2
	grep -n '#\[test_case\]' "${TEST_FILES[@]}" >&2 || true
	exit 1
fi
if grep -Eq 'tagged_test!\([^,]+, \[\s*\]\)' "${TEST_FILES[@]}"; then
	echo "test tag check: an empty tag list was found" >&2
	exit 1
fi

allowed="$(sed -n '/^define_test_tags! {/,/^}/p' "$ROOT_TESTS" | sed -n 's/^[[:space:]]*\([A-Za-z0-9_]*\) =>.*/\1/p')"
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
	for tests in "${TEST_FILES[@]}"; do
		awk '
			/^[[:space:]]*(crate::)?tagged_test!\(/ {
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
		' "$tests"
	done
)
if [[ "$descriptors" -eq 0 ]]; then
	echo "test tag check: no tagged tests found" >&2
	exit 1
fi
echo "test tag check: $descriptors kernel tests use canonical tagged descriptors"
