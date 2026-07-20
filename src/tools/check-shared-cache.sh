#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mode="${1:-quick}"
output="$(mktemp)"
backup=""
source=""

command -v flock >/dev/null
mkdir -p "$root/boot/.build"
exec 8>"$root/boot/.build/image-build-x86_64-unknown-none.lock"
flock 8

cleanup() {
	if [[ -n "$backup" && -n "$source" && -f "$backup" ]]; then cp "$backup" "$source"; fi
	rm -f "$backup" "$output"
}
trap cleanup EXIT

run_graph() {
	(cd "$root" && LIBER_IMAGE_LOCK_HELD=1 just shared-libs) >"$output" 2>&1
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
	rm -f "$root/boot/.build/image-artifacts-x86_64-unknown-none/executable-echo.build-key"
	run_graph
	expect_only_misses executable echo
	run_graph
	if [[ "$(summary_value providers)" != */0 || "$(summary_value executables)" != */0 ]]; then
		echo "shared-cache-check: echo baseline did not return to a warm state" >&2
		exit 1
	fi
	source="$root/user/tools/src/echo.rs"
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
	source="$root/user/volume-client-provider/src/lib.rs"
	backup="$(mktemp)"
	cp "$source" "$backup"
	printf '\n// shared-cache-check-%s\n' "$$" >>"$source"
	run_graph
	expect_only_misses provider volume-client
	mapfile -t consumers < <(awk '$1 == "dynamic" && $4 == "volume" {for (i = 5; i <= NF; i++) if ($i == "volume-client") {print $2; break}}' "$root/user/services/manifest.txt" | sort)
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
*)
	echo "usage: $0 [quick|provider]" >&2
	exit 2
	;;
esac

echo "shared-cache-check: $mode passed"
