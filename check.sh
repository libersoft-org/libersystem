#!/usr/bin/env bash
# Every gate and conformance suite, behind one command.
#
# These were 28 Justfile recipes - 16 `*-check` and 12 `*-conformance` - and the second group was a
# LIST OF DATA written as code: eleven recipes differing only in an image format's name. A caller
# wants all of them (CI) or one of them (a person chasing a failure), and neither wants to read 28
# names to find out which exist.

SCRIPT_NAME=check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# name -> command, run from src/. The static-injection family shares one script and differs by its
# argument, which is exactly the shape that became six recipe names.
declare -A GATES=(
	["development-gate"]="tools/check-development-gate.sh"
	["artifact-metadata"]="tools/check-artifact-metadata.sh"
	["dynamic-report"]="tools/check-dynamic-report.sh --check"
	# The checker above decides whether the tracked reports still describe the tree, and it used to
	# prove itself by re-invoking itself - a second full ELF sweep that accepted any nonzero status
	# as proof. This tests its exit contract from outside, against a fixture, in seconds.
	["dynamic-report-regressions"]="tools/check-dynamic-report-regressions.sh"
	["test-tags"]="boot/check-test-tags.sh"
	# The harness that decides whether every other test passed, tested against fakes. An audit found
	# four of its oracles reporting success without measuring their subject; each looked correct on a
	# reading, which is why this is a gate rather than a review note.
	["boot-harness"]="boot/harness-test.py"
	["host-tests"]="tools/check-host-tests.sh"
	["verify-model"]="cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- check"
	["verify-model-tests"]="cargo test --quiet --manifest-path tools/verify-model/Cargo.toml"
	["static-image"]="tools/check-static-injection.sh static"
	["undeclared-edge"]="tools/check-static-injection.sh undeclared-edge"
	["duplicate-edge"]="tools/check-static-injection.sh duplicate-edge"
	["malformed-dynamic"]="tools/check-static-injection.sh malformed-dynamic"
	["malformed-symbol-relocation"]="tools/check-static-injection.sh malformed-symbol-relocation"
	["identity-note"]="tools/check-static-injection.sh identity-note"
	["volume-layout"]="tools/check-volume-layout.sh ../.build/boot/volume-x86_64.pkg"
	["milestone-index"]="tools/check-milestone-index.sh"
	["single-cap-receive"]="tools/check-single-cap-receive.sh"
	# A kernel allocation ring 3 can trigger must be able to refuse. Three audits closed that class
	# by enumeration and a fourth would have found the next member; this is the rule instead.
	["kernel-allocations"]="tools/check-kernel-allocations.sh"
	# A frame a page table ever pointed at goes back through `frame::retire`. The module's own doc
	# comment said so and the next round still wrote a rollback that unmapped a page and called
	# `deallocate` - a rule in a comment is a rule the next diff does not read.
	["frame-retirement"]="tools/check-frame-retirement.sh"
	# The `--artifact` fast path knows a library's DEPENDENCIES, or it reports an artifact as
	# current after a crate it compiles against changed - which makes every test result taken on
	# that artifact meaningless. Its own family's `quick` and `provider` modes are not gates because
	# they rebuild the whole graph; this one is two targeted builds.
	["targeted-cache"]="tools/check-shared-cache.sh targeted"
)

FORMATS=(bmp gif ico icns jpeg pcx png ppm qoi tga webp)

help() {
	usage_and_exit <<EOF
usage: check.sh [--gate NAME[,NAME...]] [--conformance [FORMAT[,FORMAT...]]] [--list]

Runs the build gates and the image conformance suites. With no arguments, runs everything.

  --gate NAME          run these gates only ('all' for every gate)
  --conformance [FMT]  run these conformance suites only (no value, or 'all', means every format)
  --list               print the names and exit
  -h, --help           this text

gates:
  ${!GATES[*]}

conformance formats:
  ${FORMATS[*]}

examples:
  ./check.sh                          # everything
  ./check.sh --gate volume-layout     # one gate
  ./check.sh --conformance png,webp   # two formats
  ./check.sh --conformance            # every format, no gates

Gates that inspect built artifacts expect a build to exist; run ./build.sh first.
EOF
}

# Report HOW a gate failed, not just that the run stopped.
#
# `set -e` made a failing gate end the run with whatever the gate had printed - which for a gate
# that is KILLED is nothing at all. Three runs ended that way in one night, each at a different
# point, and the empty log left no way to tell a gate that refused its input from one that was
# taken out from under us. A status is not a diagnosis, but it separates those two: an ordinary
# non-zero exit means the gate decided, and a signal means something else decided for it.
run_gate() {
	local name="$1" cmd="${GATES[$1]:-}" status=0
	[[ -n "$cmd" ]] || die "unknown gate '$name' (--list to see them)"
	note "gate: $name"
	(cd "$SRC_DIR" && eval "$cmd") || status=$?
	if [[ "$status" -ne 0 ]]; then
		# Bash reports a killed child as 128 + the signal number.
		if [[ "$status" -gt 128 ]]; then
			note "gate '$name' was KILLED by signal $((status - 128)) - it did not fail, something stopped it"
		else
			note "gate '$name' failed (exit $status)"
		fi
		# And whatever execution trace the gate left behind.
		#
		# The injection gates write one as they go, precisely because their unexplained deaths are the
		# ones where nothing is left alive to report: a signal skips the gate's own EXIT trap, so the
		# gate cannot print its own trace and the only thing that can is out here. Printed and then
		# removed, so a later run is never read as this one's.
		local trace
		for trace in "${TMPDIR:-/tmp}"/liber-injection-trace.*.log; do
			[[ -s "$trace" ]] || continue
			note "the last commands '$name' ran, from $trace:"
			tail -n 15 "$trace" | sed 's/^/    /' >&2
			rm -f "$trace"
		done
		return "$status"
	fi
}

run_conformance() {
	local fmt="$1"
	[[ " ${FORMATS[*]} " == *" $fmt "* ]] || die "unknown conformance format '$fmt' (--list to see them)"
	note "conformance: $fmt"
	(cd "$SRC_DIR" && cargo run --release --manifest-path "tools/$fmt-conformance/Cargo.toml")
}

gates=()
formats=()
want_gates=1
want_conformance=1

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--list)
		echo "gates:       ${!GATES[*]}"
		echo "conformance: ${FORMATS[*]}"
		exit 0
		;;
	--gate)
		[[ $# -ge 2 ]] || die "--gate needs a name"
		# Command substitution, NOT process substitution: `parse_list` refuses an unknown name by
		# exiting, and inside `< <(...)` that exit is the subshell's alone - the script carried on
		# with an empty selection, which then fell through to "nothing selected means everything"
		# and ran every gate. A validation failure that ends up running MORE than was asked is
		# worse than one that runs nothing.
		picked_raw="$(parse_list "$2" gate "${!GATES[*]}")"
		mapfile -t picked <<<"$picked_raw"
		gates+=("${picked[@]}")
		want_conformance=0
		shift 2
		;;
	--conformance)
		# The value is optional: `--conformance` alone means every format.
		if [[ $# -ge 2 && "$2" != -* ]]; then
			picked_raw="$(parse_list "$2" format "${FORMATS[*]}")"
			mapfile -t picked <<<"$picked_raw"
			formats+=("${picked[@]}")
			shift 2
		else
			formats=("${FORMATS[@]}")
			shift
		fi
		want_gates=0
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

# Nothing selected means everything - the CI case, and the one a person means by "check it".
if [[ ${#gates[@]} -eq 0 && $want_gates -eq 1 ]]; then
	gates=("${!GATES[@]}")
fi
if [[ ${#formats[@]} -eq 0 && $want_conformance -eq 1 ]]; then
	formats=("${FORMATS[@]}")
fi

for gate in "${gates[@]:-}"; do
	[[ -n "$gate" ]] && run_gate "$gate"
done
for fmt in "${formats[@]:-}"; do
	[[ -n "$fmt" ]] && run_conformance "$fmt"
done

note "all selected checks passed"
