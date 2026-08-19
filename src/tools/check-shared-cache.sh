#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"
mode="${1:-quick}"
output="$(mktemp)"
backup=""
source=""
stale_output=""
artifact=""
decision=""

source_path() {
	jq -er --arg owner "$1" '.sources[$owner].path' <<<"$manifest_json"
}

program_path() {
	jq -er --arg program "$1" '.programs[$program].destination | sub("\\.lsexe$"; "")' <<<"$manifest_json"
}

command -v flock >/dev/null
mkdir -p "$build_root"
# The build directory has a shape; a script that writes into it makes sure its place exists.
mkdir -p "$build_root/state"
exec 8>"$build_root/state/build-x86_64-unknown-none.lock"
flock 8

cleanup() {
	if [[ -n "$backup" && -n "$source" && -f "$backup" ]] && ! cmp -s "$backup" "$source"; then cp "$backup" "$source"; fi
	if [[ -n "$stale_output" ]]; then rm -f "$stale_output"; fi
	rm -f "$backup" "$output"
}
trap cleanup EXIT

# Every cache decision this guard asserts on is a verbose diagnostic: ordinary builds
# print one summary line instead. Ask for the detail explicitly rather than reading a
# quiet log for markers that are not in it.
run_graph() {
	(cd "$root" && LIBER_IMAGE_LOCK_HELD=1 LIBER_VERBOSE=1 ./build.sh --part libs --rebuild) >"$output" 2>&1
}

summary_value() {
	local name="$1"
	sed -n "s/.* $name=\([^ ]*\).*/\1/p" "$output" | tail -n1
}

expect_only_misses() {
	local kind="$1"
	shift
	local expected actual
	expected="$(printf '%s\n' "$@" | sort)"
	actual="$(sed -n "s/^build-shared: $kind cache miss //p" "$output" | sort)"
	if [[ "$actual" != "$expected" ]]; then
		echo "shared-cache-check: unexpected $kind misses" >&2
		diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
		exit 1
	fi
}

prime_graph() {
	run_graph
	if [[ "$(summary_value providers)" != */0 || "$(summary_value executables)" != */0 ]]; then
		run_graph
		if [[ "$(summary_value providers)" != */0 || "$(summary_value executables)" != */0 ]]; then
			echo "shared-cache-check: baseline graph did not reach a warm state" >&2
			exit 1
		fi
	fi
}

case "$mode" in
quick)
	prime_graph
	if [[ -n "$(find "$build_root/image/x86_64-unknown-none" -type f \( -name '*.identity' -o -name '*.order' \) -print -quit)" || -n "$(find "$build_root/cache/x86_64-unknown-none" -maxdepth 1 -type f -name '*.order.sha256' -print -quit)" ]]; then
		echo "shared-cache-check: obsolete identity or provider-order sidecar remains" >&2
		exit 1
	fi
	run_graph
	expect_only_misses provider
	expect_only_misses executable
	if ! grep -q '^build-shared: warm image snapshot hit$' "$output"; then
		echo "shared-cache-check: unchanged graph did not use the warm image snapshot" >&2
		exit 1
	fi
	rm -f "$build_root/image/x86_64-unknown-none/$(program_path echo)"
	run_graph
	expect_only_misses provider
	expect_only_misses executable echo
	if ! grep -q '^build-shared: object cache hit echo$' "$output"; then
		echo "shared-cache-check: missing echo output did not reuse its object" >&2
		exit 1
	fi
	run_graph
	if ! grep -q '^build-shared: warm image snapshot hit$' "$output"; then
		echo "shared-cache-check: restored output did not return to a snapshot hit" >&2
		exit 1
	fi
	stale_output="$build_root/image/x86_64-unknown-none/lib/stale-flat.lslib"
	cp "$build_root/image/x86_64-unknown-none/lib/runtime/lsrt.lslib" "$stale_output"
	if run_graph; then
		echo "shared-cache-check: stale flat provider passed the output audit" >&2
		exit 1
	fi
	if ! grep -q '^build-shared: staged library paths differ from the manifest$' "$output"; then
		echo "shared-cache-check: stale flat provider failed outside the destination audit" >&2
		exit 1
	fi
	rm -f "$stale_output"
	stale_output=""
	prime_graph
	run_graph
	if ! grep -q '^build-shared: warm image snapshot hit$' "$output"; then
		echo "shared-cache-check: stale-output recovery did not return to a snapshot hit" >&2
		exit 1
	fi
	rm -f "$build_root/cache/x86_64-unknown-none/executable-echo.build-key"
	run_graph
	expect_only_misses executable echo
	run_graph
	if [[ "$(summary_value providers)" != */0 || "$(summary_value executables)" != */0 ]]; then
		echo "shared-cache-check: echo baseline did not return to a warm state" >&2
		exit 1
	fi
	source="$root/$(source_path tools)/src/echo.rs"
	backup="$(mktemp)"
	cp "$source" "$backup"
	printf '\n// shared-cache-check-%s\n' "$$" >>"$source"
	run_graph
	expect_only_misses executable echo
	expect_only_misses object echo
	cp "$backup" "$source"
	run_graph
	expect_only_misses executable echo
	if ! grep -q '^build-shared: object cache hit echo$' "$output"; then
		echo "shared-cache-check: restored echo did not reuse its content-addressed object" >&2
		exit 1
	fi
	;;
provider)
	prime_graph
	source="$root/$(source_path volume-client-provider)/src/lib.rs"
	backup="$(mktemp)"
	cp "$source" "$backup"
	printf '\n// shared-cache-check-%s\n' "$$" >>"$source"
	run_graph
	expect_only_misses provider volume-client
	mapfile -t consumers < <(jq -r '.programs[] | select(.linkage == "dynamic" and .stage == "volume" and (.providers | index("volume-client"))) | .name' <<<"$manifest_json" | sort)
	expect_only_misses executable "${consumers[@]}"
	for consumer in "${consumers[@]}"; do
		if ! grep -q "^build-shared: object cache hit $consumer$" "$output"; then
			echo "shared-cache-check: provider-only change recompiled $consumer" >&2
			exit 1
		fi
	done
	cp "$backup" "$source"
	run_graph
	expect_only_misses provider volume-client
	expect_only_misses executable "${consumers[@]}"
	for consumer in "${consumers[@]}"; do
		if ! grep -q "^build-shared: object cache hit $consumer$" "$output"; then
			echo "shared-cache-check: provider restore recompiled $consumer" >&2
			exit 1
		fi
	done
	run_graph
	if [[ "$(summary_value providers)" != */0 || "$(summary_value executables)" != */0 ]]; then
		echo "shared-cache-check: provider baseline did not return to a warm state" >&2
		exit 1
	fi
	;;
targeted)
	# THE `--artifact` FAST PATH, and the one thing it has to know: a library's dependencies.
	#
	# `provider_closure_sha` made a change to `abi` invalidate `lsrt.lslib` on the full build path,
	# and `targeted_state_paths` recorded the owner's own sources only - then `build-shared.sh`
	# exits at the targeted state check, two hundred lines before the closure is computed. So
	# `dev-build lsrt` reported an artifact as current after a crate it compiles against had
	# changed, which makes every test result taken on that artifact meaningless.
	#
	# The full path had this asserted by measurement when it was written; the targeted path had
	# nothing, which is how it came to disagree. This is the assertion.
	targeted_decision() {
		(cd "$root" && LIBER_IMAGE_LOCK_HELD=1 tools/dev-build.sh --explain "$1" x86_64) >"$output" 2>&1 || {
			echo "shared-cache-check: dev-build $1 failed" >&2
			cat "$output" >&2
			exit 1
		}
		sed -n 's/^dev-build: explain decision=\([a-z]*\).*/\1/p' "$output" | tail -n1
	}

	artifact="${2:-lsrt}"
	# Two runs to reach a state: the first builds, the second must find it unchanged.
	targeted_decision "$artifact" >/dev/null
	decision="$(targeted_decision "$artifact")"
	if [[ "$decision" != "hit" ]]; then
		echo "shared-cache-check: an unchanged $artifact did not hit its targeted state (decision=$decision)" >&2
		cat "$output" >&2
		exit 1
	fi

	# A crate the artifact COMPILES AGAINST and does not contain. `abi` is under every library in
	# the tree and under none of their directories, which is exactly the relationship the owner's
	# own `find` could not see.
	#
	# Named literally rather than through `source_path`: `abi` is not a manifest SOURCE - it owns no
	# staged artifact - so the lookup returns `null`, and under `set -e` that killed this mode
	# silently with a zero exit. A guard that cannot fail is not a guard, so it is asserted here.
	source="$root/abi/src/lib.rs"
	[[ -f "$source" ]] || {
		echo "shared-cache-check: $source is not there - this mode needs a crate the artifact depends on and does not contain" >&2
		exit 1
	}
	backup="$(mktemp)"
	cp "$source" "$backup"
	printf '\n// shared-cache-check-%s\n' "$$" >>"$source"
	decision="$(targeted_decision "$artifact")"
	cp "$backup" "$source"
	if [[ "$decision" != "miss" ]]; then
		echo "shared-cache-check: $artifact hit its targeted state after a dependency changed - the closure is not in it" >&2
		exit 1
	fi
	# And back: the restored dependency must return the artifact to a hit rather than leaving it
	# permanently dirty.
	targeted_decision "$artifact" >/dev/null
	decision="$(targeted_decision "$artifact")"
	if [[ "$decision" != "hit" ]]; then
		echo "shared-cache-check: $artifact did not return to a targeted hit after the dependency was restored (decision=$decision)" >&2
		exit 1
	fi
	;;
*)
	echo "usage: $0 [quick|provider|targeted [ARTIFACT]]" >&2
	exit 2
	;;
esac

echo "shared-cache-check: $mode passed"
