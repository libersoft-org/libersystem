#!/usr/bin/env bash
# Keep the custom test harness as the only source of #[test_case] descriptors.
set -euo pipefail

# Prove the gate REFUSES before letting it approve.
#
# "A gate that breaks fails loudly on its own" is not true: `exit 0` at the top of this file breaks
# it catastrophically and silently, and so does a `grep` pattern that stops matching. A validator
# tested only by running its current version over a currently-valid tree is not tested at all - the
# tree is valid, so it passes, and it would pass just as well if it had stopped looking.
#
# So every run starts by feeding itself inputs it must reject. Cheap - three temporary files and no
# compilation - and it cannot be forgotten, because it is not a separate gate anybody has to
# remember to invoke.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	trap 'rm -rf "$scratch"' RETURN
	mkdir -p "$scratch/kernel/test_suites"
	# A minimal but VALID tree, so each rejection below is caused by the one defect it injects.
	cat >"$scratch/kernel/tests.rs" <<-'FIXTURE'
		define_test_tags! {
			Smoke => "smoke",
		}
		macro_rules! tagged_test {
			($name:ident, [$t:ident]) => { mod $name { #[test_case] static CASE: u8 = 0; } };
			($name:ident, [$t:ident], covers = [$($c:literal),*]) => { mod $name { #[test_case] static CASE: u8 = 0; } };
		}
	FIXTURE
	printf 'tagged_test!(a_test, [Smoke]);
' >"$scratch/kernel/test_suites/ok.rs"
	"$0" --root "$scratch" >/dev/null 2>&1 || {
		echo "test tag check: the self-test's own VALID fixture was rejected - the gate is broken in the direction that blocks work" >&2
		return 1
	}

	local case
	for case in hand-written-descriptor empty-tag-list unknown-tag; do
		cp "$scratch/kernel/test_suites/ok.rs" "$scratch/kernel/test_suites/bad.rs"
		case "$case" in
		hand-written-descriptor) printf '#[test_case]
static LOOSE: u8 = 0;
' >>"$scratch/kernel/test_suites/bad.rs" ;;
		empty-tag-list) printf 'tagged_test!(b_test, []);
' >>"$scratch/kernel/test_suites/bad.rs" ;;
		unknown-tag) printf 'tagged_test!(c_test, [NoSuchTag]);
' >>"$scratch/kernel/test_suites/bad.rs" ;;
		esac
		if "$0" --root "$scratch" >/dev/null 2>&1; then
			echo "test tag check: SELF-TEST FAILED - '$case' was accepted, so this gate is not checking what it claims to" >&2
			rm -f "$scratch/kernel/test_suites/bad.rs"
			return 1
		fi
		rm -f "$scratch/kernel/test_suites/bad.rs"
	done
}

if [[ "${1:-}" == "--root" ]]; then
	ROOT="$2"
	shift 2
else
	ROOT="$(cd "$(dirname "$0")/.." && pwd)"
	self_test || exit 1
fi
ROOT_TESTS="$ROOT/kernel/tests.rs"
mapfile -t TEST_FILES < <(find "$ROOT/kernel" -type f \( -name tests.rs -o -path "$ROOT/kernel/test_suites/*.rs" \) -print | sort)
# One #[test_case] per arm of `tagged_test!`, and nowhere else.
#
# The point is that the macro is the ONLY way a descriptor enters the suite - a hand-written
# `#[test_case]` would run with no tags, so no filter could ever select or skip it. The count is two
# because the macro has two arms: with and without a `covers` clause. Both arms live in
# kernel/tests.rs, so the rule is checked there by count and everywhere else by absence.
EXPECTED_IN_MACRO=2
in_macro="$(grep -c '#\[test_case\]' "$ROOT_TESTS")"
if [[ "$in_macro" -ne "$EXPECTED_IN_MACRO" ]]; then
	echo "test tag check: expected $EXPECTED_IN_MACRO #[test_case] in tagged_test!, found $in_macro in kernel/tests.rs" >&2
	grep -n '#\[test_case\]' "$ROOT_TESTS" >&2 || true
	exit 1
fi
outside="$(grep -h '#\[test_case\]' "${TEST_FILES[@]}" | wc -l)"
if [[ "$outside" -ne "$EXPECTED_IN_MACRO" ]]; then
	echo "test tag check: a #[test_case] outside tagged_test! would run with no tags, so no filter could select or skip it" >&2
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
