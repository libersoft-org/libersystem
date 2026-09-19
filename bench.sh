#!/usr/bin/env bash
# The optimized host runs: measure a decoder against real time, profile the image codecs, drive
# every image leaf through deliberately hostile input, and put the file manager's core through the
# sizes a person actually meets.
#
# All of them are `cargo run --release` on a tool in src/tools, and all of them are things a person runs
# when they want the number - not part of `./check.sh`, which has to be runnable in a minute on a
# machine with nothing built. `image-mutate` is here rather than beside the gates for that reason
# alone: it is a robustness run and it is a release build of twelve codecs, so putting it in the
# default gate set would make "check it" mean something different from what it means today.

SCRIPT_NAME=bench.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

declare -A SUITES=(
	["audio"]="tools/audio-bench"
	["image"]="tools/image-bench"
	["image-mutate"]="tools/image-mutate"
	["lico"]="tools/lico-bench"
	["soft2d"]="tools/soft2d-bench"
	["soft3d"]="tools/soft3d-bench"
)

# WHAT EACH SUITE MEASURES, keyed by the SAME name as the map above.
#
# THE LIST IN THE HELP IS BUILT FROM THESE AND NOT WRITTEN OUT, because a hand-kept copy of a list
# drifts from it: `lico` and `soft2d` were runnable and undocumented, and `soft2d` is the benchmark
# the 2D profile's performance floor is measured by - so the one suite a reader most needed to find
# was the one the help did not mention. A suite with no line here is a hard error rather than a
# quiet omission, which is the only arrangement in which this cannot happen again.
declare -A SUITE_ABOUT=(
	["audio"]="the staged MP3 decoder against real time - fails below it"
	["image"]="current image encode and decode profiles"
	["image-mutate"]="every image leaf and the central sniffer through deterministic hostile inputs"
	["lico"]="the file manager's directory listing and its scrolling redraw"
	["soft2d"]="the 2D backend's four frozen scenes against the profile's frozen budgets"
	["soft3d"]="the 3D backend's frozen scene, stage by stage - and the interpreter on its own"
)

# PROVEN TO REFUSE BEFORE IT IS TRUSTED TO PRINT. A guard that has only ever seen a complete table
# is a guard nobody has tested: this adds a suite that has no description, requires the listing to
# fail, and takes it away again. Three lines, no files, and it cannot be forgotten.
self_test_suite_lines() {
	SUITES["__probe"]="tools/does-not-exist"
	if (suite_lines) >/dev/null 2>&1; then
		unset 'SUITES[__probe]'
		echo "bench.sh: SELF-TEST FAILED - a suite with no description was listed instead of refused" >&2
		exit 1
	fi
	unset 'SUITES[__probe]'
}

suite_lines() {
	local name
	for name in $(printf '%s\n' "${!SUITES[@]}" | sort); do
		if [[ -z "${SUITE_ABOUT[$name]:-}" ]]; then
			echo "bench.sh: the suite '$name' has no description - add one beside it in SUITE_ABOUT" >&2
			exit 1
		fi
		printf '  %-13s %s\n' "$name" "${SUITE_ABOUT[$name]}"
	done
}

help() {
	self_test_suite_lines
	usage_and_exit <<EOF
usage: bench.sh [--suite NAME[,NAME...]] [--list]

Runs the optimized host measurement and hostile-input suites. With no arguments, runs all of them.

  --suite NAME   run these only ('all' for every suite)
  --list         print the names and exit
  -h, --help     this text

suites:
$(suite_lines)

examples:
  ./bench.sh --suite audio
  ./bench.sh --suite image,image-mutate
  ./bench.sh
EOF
}

suites=()

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--list)
		echo "suites: ${!SUITES[*]}"
		exit 0
		;;
	--suite)
		[[ $# -ge 2 ]] || die "--suite needs a name"
		# Command substitution, not process substitution: `parse_list` refuses an unknown name by
		# exiting, and inside `< <(...)` that exit belongs to the subshell - the caller would carry
		# on with an empty selection and fall through to "nothing selected means everything".
		picked_raw="$(parse_list "$2" suite "${!SUITES[*]}")"
		mapfile -t picked <<<"$picked_raw"
		suites+=("${picked[@]}")
		shift 2
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

if [[ ${#suites[@]} -eq 0 ]]; then
	suites=("${!SUITES[@]}")
fi

for suite in "${suites[@]}"; do
	[[ -n "$suite" ]] || continue
	note "suite: $suite"
	(cd "$SRC_DIR" && cargo run --release --manifest-path "${SUITES[$suite]}/Cargo.toml")
done

note "all selected suites finished"
