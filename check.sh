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
	# P02M0151's gate: every symbol in an architecture's compiled contract is a path that
	# architecture can execute. Twenty `todo!()` bodies used to answer the x86 loader hand-off on the
	# two ports that never arrive through it, and nothing but a paragraph of prose separated "dormant
	# by construction" from "unfinished".
	["arch-surface"]="tools/check-arch-surface.sh"
	# P02M0154's gate: the capability transfer model, explored exhaustively under each published
	# configuration. It runs TLC from the JAR pinned in `toolchain.lock` with NO NETWORK, and names
	# `./bootstrap.sh tla2tools` when the artifact is absent rather than fetching one.
	["capability-model"]="tools/check-capability-model.sh"
	# P02M0150's gate: the two trust profiles differ in the BINARY. The test key's private half is
	# published on purpose, which is exactly why a release loader must contain none of it.
	["trust-profile"]="tools/check-trust-profile.sh"
	# P02M0150's other gate: the boot that must NOT happen. One successful signed boot proves the
	# pieces fit; only a refused one proves the check is load-bearing.
	["signed-boot"]="tools/check-signed-boot.sh"
	# P02M0150 M5: the firmware verifies the LOADER, or the loader does not run. Preflights its four
	# host tools by name and skips nothing when one is missing.
	["secure-boot"]="tools/check-secure-boot.sh"
	# The other configuration of the same source, compiled. `development-gate` above checks which
	# artifacts a configuration STAGES; it never builds the one it is guarding, and the profile it
	# guards stopped compiling twice in one release without anything noticing.
	["development-build"]="tools/check-development-build.sh"
	["artifact-metadata"]="tools/check-artifact-metadata.sh"
	["dynamic-report"]="tools/check-dynamic-report.sh --check"
	# The checker above decides whether the tracked reports still describe the tree, and it used to
	# prove itself by re-invoking itself - a second full ELF sweep that accepted any nonzero status
	# as proof. This tests its exit contract from outside, against a fixture, in seconds.
	["dynamic-report-regressions"]="tools/check-dynamic-report-regressions.sh"
	["test-tags"]="harness/check-test-tags.sh"
	# The harness that decides whether every other test passed, tested against fakes. An audit found
	# four of its oracles reporting success without measuring their subject; each looked correct on a
	# reading, which is why this is a gate rather than a review note.
	["boot-harness"]="harness/harness-test.py"
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
	# Generated or compiled artifacts below src, in the working tree and anywhere in reachable
	# history. Two Justfile recipes that nothing called; cheap enough to be part of "check it", and
	# a tree that has committed a build output does not un-commit it by nobody looking.
	["source-hygiene"]="tools/check-source-hygiene.sh --current"
	["source-history-hygiene"]="tools/check-source-hygiene.sh --history"
	["single-cap-receive"]="tools/check-single-cap-receive.sh"
	# A kernel allocation ring 3 can trigger must be able to refuse. Three audits closed that class
	# by enumeration and a fourth would have found the next member; this is the rule instead.
	["kernel-allocations"]="tools/check-kernel-allocations.sh"
	# A frame a page table ever pointed at goes back through `frame::retire`. The module's own doc
	# comment said so and the next round still wrote a rollback that unmapped a page and called
	# `deallocate` - a rule in a comment is a rule the next diff does not read.
	["frame-retirement"]="tools/check-frame-retirement.sh"
	# The hand-written bootstrap ladder and the generated role plan describe the same wiring while
	# P02M0141 migrates from one to the other, and two descriptions of one fact is what that
	# milestone exists to remove. Until the ladder is empty they are compared, tag by tag and in
	# order - a role in the wrong position has already displaced every read after it.
	["bootstrap-plan"]="tools/check-bootstrap-plan.py"
	# A hand-written `extern` declaration and the generated function it is forwarded to are joined
	# by a bare jump, so a signature that disagrees is a silent argument-register mismatch rather
	# than a link error. One such pair made every transactional write in the system return "no
	# answer", and it compiled without a warning.
	["forwarded-abi"]="tools/check-forwarded-abi.py"
	# A manifest role naming an LSIDL interface that does not exist. The field is a reference and
	# nothing resolved it, so four of the twenty names in the file were wrong from the day they
	# were written - harmlessly, because no generator reads it yet, which is exactly how a
	# declaration rots: it is read by people, who believe it.
	["declared-interfaces"]="tools/check-declared-interfaces.py"
	# A warning answered by switching the lint off rather than by fixing the code. Ninety-one such
	# attributes had accumulated, hiding a hundred and twenty more warnings than the build printed -
	# and hiding them UNEVENLY, so the same code was reported on one target and silent on another.
	["no-suppression"]="tools/check-no-suppression.sh"
	# P02M0143's M4 produced a manifest and a host-tested verifier, and the two defects it actually
	# had were in neither: the x86_64 boot medium carried no manifest at all, and the kernel read
	# from a boot medium was checked against nothing. A verifier that is right about files nobody
	# reads is not integrity - this asks the loader's question of the media on disk.
	["boot-manifest"]="tools/check-boot-manifest.sh"
	# The `--artifact` fast path knows a library's DEPENDENCIES, or it reports an artifact as
	# current after a crate it compiles against changed - which makes every test result taken on
	# that artifact meaningless. Its own family's `quick` and `provider` modes are not gates because
	# they rebuild the whole graph; this one is two targeted builds.
	["targeted-cache"]="tools/check-shared-cache.sh targeted"
)

FORMATS=(bmp gif ico icns jpeg pcx png ppm qoi tga webp)

# A gate that can also REGENERATE what it checks, and the command that does it.
#
# The dynamic report is three tracked TSVs describing the staged ELF graph; the gate compares them
# against the tree and `--write` produces them. Those were two Justfile recipes with nothing to say
# they were two halves of one thing - and the writing half is the one that must be reached
# deliberately, since it overwrites files under review.
declare -A REFRESH=(
	["dynamic-report"]="tools/check-dynamic-report.sh --write"
)

# Checks that take arguments, so they are flags rather than gate names - and, like --conformance,
# they run only when asked. Each rebuilds or re-stages something, which is why none belongs in the
# set `./check.sh` with no arguments runs.
#
#   --staged-image [T...]      are the staged images on disk the ones this tree produces
#   --cache-check MODE         quick | provider | targeted - build-cache invalidation, end to end
#   --fast-path [T] [A...]     the targeted build and the authoritative rebuild produce equal bytes

help() {
	usage_and_exit <<EOF
usage: check.sh [--gate NAME[,NAME...]] [--conformance [FORMAT[,FORMAT...]]]
                [--refresh NAME] [--staged-image [TARGET...]] [--cache-check MODE]
                [--fast-path [TARGET] [ARTIFACT...]] [--list]

Runs the build gates and the image conformance suites. With no arguments, runs everything.

  --gate NAME          run these gates only ('all' for every gate)
  --conformance [FMT]  run these conformance suites only (no value, or 'all', means every format)
  --refresh NAME       REGENERATE what a gate checks, instead of checking it
  --staged-image [T]   are the staged images the ones this tree produces (default: all staged)
  --cache-check MODE   build-cache invalidation end to end: quick | provider | targeted
  --fast-path [T] [A]  the targeted build and the authoritative rebuild produce equal bytes
  --list               print the names and exit
  -h, --help           this text

gates:
  ${!GATES[*]}

refreshable:
  ${!REFRESH[*]}

conformance formats:
  ${FORMATS[*]}

examples:
  ./check.sh                            # everything
  ./check.sh --gate volume-layout       # one gate
  ./check.sh --conformance png,webp     # two formats
  ./check.sh --conformance              # every format, no gates
  ./check.sh --refresh dynamic-report   # rewrite the tracked reports from the built tree

The four argument-taking checks rebuild or re-stage something, so they run only when named - never
as part of a bare ./check.sh.

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
refreshes=()
# Each is "did the caller ask" plus the arguments it gave, because all three take an OPTIONAL list.
staged_image=0
staged_image_targets=()
cache_check=""
fast_path=0
fast_path_args=()
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
	--refresh)
		[[ $# -ge 2 ]] || die "--refresh needs a name (refreshable: ${!REFRESH[*]})"
		[[ -n "${REFRESH[$2]:-}" ]] || die "'$2' cannot be refreshed (refreshable: ${!REFRESH[*]})"
		refreshes+=("$2")
		want_gates=0
		want_conformance=0
		shift 2
		;;
	--staged-image)
		staged_image=1
		want_gates=0
		want_conformance=0
		shift
		# Every following non-flag word is a target. With none, the script checks every staged image.
		while [[ $# -gt 0 && "$1" != -* ]]; do
			staged_image_targets+=("$1")
			shift
		done
		;;
	--cache-check)
		[[ $# -ge 2 ]] || die "--cache-check needs a mode (quick, provider or targeted)"
		cache_check="$2"
		want_gates=0
		want_conformance=0
		shift 2
		;;
	--fast-path)
		fast_path=1
		want_gates=0
		want_conformance=0
		shift
		# The target, then the artifacts to sample. With none, the script picks its own defaults.
		while [[ $# -gt 0 && "$1" != -* ]]; do
			fast_path_args+=("$1")
			shift
		done
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
for name in "${refreshes[@]:-}"; do
	[[ -n "$name" ]] || continue
	note "refresh: $name"
	(cd "$SRC_DIR" && eval "${REFRESH[$name]}")
done
if ((staged_image)); then
	note "staged-image: ${staged_image_targets[*]:-every staged target}"
	(cd "$SRC_DIR" && tools/check-staged-image.sh "${staged_image_targets[@]}")
fi
if [[ -n "$cache_check" ]]; then
	note "cache-check: $cache_check"
	(cd "$SRC_DIR" && tools/check-shared-cache.sh "$cache_check")
fi
if ((fast_path)); then
	# The script's own default target when none is named, so the flag alone means what the recipe
	# it replaces meant.
	set -- "${fast_path_args[@]}"
	[[ $# -ge 1 ]] || set -- x86_64
	note "fast-path: $*"
	(cd "$SRC_DIR" && tools/check-fast-path-parity.sh "$@")
fi

note "all selected checks passed"
