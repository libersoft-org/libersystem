#!/usr/bin/env bash
# Control surface for the persistent development guest.
#
# Twenty-three `dev-*` recipes lived in the Justfile, and twenty of them were the same line:
# `lab.py dev-<verb> <args>`. That is not a build system, and spelling each verb into its own
# recipe put them in the way of everything that is.

SCRIPT_NAME=dev.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# The three verbs that are NOT lab.py, kept here rather than hidden: each runs a different tool.
declare -A SPECIAL=(
	["baseline"]="tools/measure-dev-baseline.sh"
	["build"]="tools/dev-build.sh"
	["selftest"]="boot/dev-selftest.py"
)

LAB_VERBS=(up down status console log ping publish generations type reset reboot restart stop key pointer clean loop rollback test launch)

help() {
	usage_and_exit <<EOF
usage: dev.sh <verb> [arguments...]

Drives the persistent development guest. Everything after the verb is passed through unchanged.

verbs (through lab.py):
  ${LAB_VERBS[*]}

verbs (other tools):
  baseline <scenario> [tags]   measure a development baseline
  build [arguments...]         build inside the development flow
  selftest                     run the development self-test

examples:
  ./dev.sh up
  ./dev.sh log --follow
  ./dev.sh key ctrl-c
  ./dev.sh baseline boot smoke

Every other entry point in this directory answers --help the same way.
EOF
}

[[ $# -eq 0 ]] && help
case "${1:-}" in
-h | --help | help) help ;;
esac

verb="$1"
shift

if [[ -n "${SPECIAL[$verb]:-}" ]]; then
	cd "$SRC_DIR"
	exec "${SPECIAL[$verb]}" "$@"
fi

for known in "${LAB_VERBS[@]}"; do
	if [[ "$verb" == "$known" ]]; then
		cd "$SRC_DIR"
		exec boot/lab.py "dev-$verb" "$@"
	fi
done

die "unknown verb '$verb' (try --help)"
