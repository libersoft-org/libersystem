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
			(@build $name:ident, [$t:ident]) => { mod $name { #[test_case] static CASE: u8 = 0; } };
			($name:ident, [$t:ident]) => { tagged_test!(@build $name, [$t]); };
			($name:ident, [$t:ident], covers = [$($c:literal),*]) => { tagged_test!(@build $name, [$t]); };
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
	# The fixture tree has no harness in it, and the real `test.sh` reads the real tree rather than
	# this one - so the harness rule below is checked once, in the real path, with its own fixtures.
	CHECK_HARNESS_LIST=0
else
	ROOT="$(cd "$(dirname "$0")/.." && pwd)"
	CHECK_HARNESS_LIST=1
	self_test || exit 1
fi
ROOT_TESTS="$ROOT/kernel/tests.rs"
# EVERY `.rs` under the kernel, not `tests.rs` and `test_suites/` alone.
#
# The narrow `find` was a blind spot of exactly the shape this gate exists to remove: a
# `tagged_test!` written beside the code it tests was invisible to it, and eleven of them were -
# five in `elf.rs`, four in `mem/frame/buddy.rs`, two in `mem/vapool.rs`. All eleven were
# well-formed, which is why nothing showed; the point is that the gate that checks descriptors was
# not looking at them, and putting a test next to its subject is a habit this tree encourages.
#
# The arithmetic that found it: this gate counted 220 descriptors, the verification model knew 228,
# and the tree held 231 invocations. Three counters over one macro, three answers.
mapfile -t TEST_FILES < <(find "$ROOT/kernel" -type f -name '*.rs' -print | sort)
# One #[test_case] per arm of `tagged_test!`, and nowhere else.
#
# The point is that the macro is the ONLY way a descriptor enters the suite - a hand-written
# `#[test_case]` would run with no tags, so no filter could ever select or skip it. The count is ONE
# because the macro funnels its two public shapes - with and without `covers`, both requiring
# `id` - through ONE internal rule. That rule lives in kernel/tests.rs, so the rule is checked there
# by count and everywhere else by absence.
#
# This comment said "four public shapes - with and without `id`" until 2026-08-12, describing the
# macro as it was before `id` became mandatory. That was this milestone's own fix and the gate's
# prose predated it - a gate whose explanation names shapes the tree does not have is a gate nobody
# can check against the tree.
EXPECTED_IN_MACRO=1
# Comment lines are excluded rather than the marker being anchored: this file explains the rule in
# prose that names the attribute, and a line-start anchor also misses the one-line `mod` form.
in_macro="$(grep '#\[test_case\]' "$ROOT_TESTS" | grep -vc '^[[:space:]]*//')"
if [[ "$in_macro" -ne "$EXPECTED_IN_MACRO" ]]; then
	echo "test tag check: expected $EXPECTED_IN_MACRO #[test_case] in tagged_test!, found $in_macro in kernel/tests.rs" >&2
	grep -n '#\[test_case\]' "$ROOT_TESTS" >&2 || true
	exit 1
fi
outside="$(grep -h '#\[test_case\]' "${TEST_FILES[@]}" | grep -vc '^[[:space:]]*//')"
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

# THE LIST THE HARNESS ADVERTISES IS THE LIST THE KERNEL PARSES, and nothing compared them.
#
# `./test.sh --list-tags` exists so a caller can discover what `--tags` accepts, and it reads THIS
# table. It read it with a character class that stopped at a hyphen, so it advertised four names the
# kernel refuses - `arch`, `capability`, `permission`, `volume` - and hid eleven it accepts, among
# them `volume-layout`, which is the only way to select the booted-system test on its own. FOUR OF
# THE ELEVEN TRUNCATED ONTO A DIFFERENT REAL TAG - `audio`, `dynamic`, `lico`, `process` - so the
# output looked complete, and `--tags permission` answered `unknown tag 'permission'` from the
# kernel that the harness had just recommended it.
#
# Nothing above can catch that. Those checks prove every tag a test USES is declared here; this is
# the only one about what a caller is TOLD, and the two sides are exactly where they can drift.
tag_lists_agree() {
	[[ "$(printf '%s\n' $1 | sort -u)" == "$(printf '%s\n' $2 | sort -u)" ]]
}
if [[ "$CHECK_HARNESS_LIST" == 1 ]]; then
	# PROVEN TO REFUSE BEFORE IT IS ALLOWED TO APPROVE, like the fixtures at the top of this file:
	# a truncated name and an extra one, which are the two ways these lists can differ, and then an
	# agreeing pair in a different order so the rule is not passing by refusing everything.
	if tag_lists_agree "a b-c" "a b"; then
		echo "test tag check: SELF-TEST FAILED - a truncated tag was accepted as agreement" >&2
		exit 1
	fi
	if tag_lists_agree "a b-c" "a b-c d"; then
		echo "test tag check: SELF-TEST FAILED - an advertised name the kernel does not have was accepted" >&2
		exit 1
	fi
	if ! tag_lists_agree "a b-c" "b-c a"; then
		echo "test tag check: SELF-TEST FAILED - two lists that agree were reported as differing" >&2
		exit 1
	fi
	declared_wire="$(sed -n '/^define_test_tags! {/,/^}/p' "$ROOT_TESTS" | sed -n 's/^[[:space:]]*[A-Za-z0-9_]* => "\([^"]*\)".*/\1/p')"
	# Its verdict line goes to stderr and is this gate's noise, not its output; a run that fails
	# instead of printing is caught by the comparison below, which an empty list cannot pass.
	printed_wire="$(bash "$ROOT/../test.sh" --list-tags 2>/dev/null || true)"
	if ! tag_lists_agree "$declared_wire" "$printed_wire"; then
		echo "test tag check: ./test.sh --list-tags and the kernel's tag table disagree" >&2
		echo "  advertised and refused by the kernel: $(comm -13 <(printf '%s\n' $declared_wire | sort -u) <(printf '%s\n' $printed_wire | sort -u) | tr '\n' ' ')" >&2
		echo "  accepted by the kernel and not shown: $(comm -23 <(printf '%s\n' $declared_wire | sort -u) <(printf '%s\n' $printed_wire | sort -u) | tr '\n' ' ')" >&2
		exit 1
	fi
	wire_count="$(printf '%s\n' $declared_wire | sort -u | grep -c .)"
	echo "test tag check: $descriptors kernel tests use canonical tagged descriptors, and --list-tags advertises the $wire_count tags the kernel parses"
else
	echo "test tag check: $descriptors kernel tests use canonical tagged descriptors"
fi
