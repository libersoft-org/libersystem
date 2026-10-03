#!/usr/bin/env bash
# OUTPUT PAST ONE STREAM CHUNK, WHOLE: a development guest booted cold, a counter's 5 KiB file written through a
# redirection, and its last line read back three ways - `cat` through the shell's relay, `cat | tail -n 1` through a
# pipe's edge, and `head`'s window. Each used to show only the first 4096 bytes: the writer sent the file as one
# message and every reader cut it to its buffer. See `harness/scenarios/shell-large-output.toml`.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
fail() {
	echo "shell-large-output: $*" >&2
	exit 1
}

case "${1:-}" in
"" | --arch)
	[[ "${2:-x86_64}" == x86_64 ]] || fail "this gate is the x86_64 one; the ports share the runtime and the shell it tests"
	;;
*) fail "unexpected argument '$1'" ;;
esac

"$repo/lab.sh" scenario-cold x86_64 "$root/harness/scenarios/shell-large-output.toml" || fail "the scenario failed (serial log: $repo/.build/boot/cold-x86_64.log)"
echo "shell-large-output: PASS - a 5 KiB file read back whole through the shell's relay, a pipe's edge and head's window"
