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
)

help() {
	usage_and_exit <<EOF
usage: bench.sh [--suite NAME[,NAME...]] [--list]

Runs the optimized host measurement and hostile-input suites. With no arguments, runs all of them.

  --suite NAME   run these only ('all' for every suite)
  --list         print the names and exit
  -h, --help     this text

suites:
  audio         the staged MP3 decoder against real time - fails below it
  image         current image encode and decode profiles
  image-mutate  every image leaf and the central sniffer through deterministic hostile inputs

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
