#!/usr/bin/env bash
# Explore the capability transfer model with TLC, offline.
#
# THE GATE DOES NOT FETCH ANYTHING. `toolchain.lock` pins the model checker by SHA-256 and
# `./bootstrap.sh` is the one command that fetches and verifies it; a gate that could reach the
# network is a gate whose answer depends on what a mirror served today. When the JAR is absent this
# says so and names the command, rather than skipping - a verification that quietly does not run is
# the failure this whole milestone is about.
set -euo pipefail

cd "$(dirname "$0")/../.."
SPEC_DIR="docs/spec/capability"
JAR=".build/tools/tla2tools.jar"
LOCK="toolchain.lock"

if [[ ! -f "$JAR" ]]; then
	echo "capability-model: $JAR is not here - run ./bootstrap.sh tla2tools" >&2
	exit 1
fi

# The pin, checked here too: the bootstrap verified what it fetched, and this verifies what it is
# about to RUN. They are different moments and a cache is writable in between.
want="$(awk '/^\[tla2tools\]/{inside=1;next} /^\[/{inside=0} inside && $1=="sha256"{gsub(/^[^=]*=[ \t]*|"/,"");print;exit}' "$LOCK")"
have="$(sha256sum "$JAR" | awk '{print $1}')"
if [[ "$want" != "$have" ]]; then
	echo "capability-model: $JAR does not match the pin in $LOCK" >&2
	echo "capability-model:   pinned $want" >&2
	echo "capability-model:   cached $have" >&2
	exit 1
fi

if ! command -v java >/dev/null; then
	echo "capability-model: java is required to run TLC (the lock names the minimum major version)" >&2
	exit 1
fi

status=0
for cfg in "$SPEC_DIR"/*.cfg; do
	name="$(basename "$cfg" .cfg)"
	meta=".build/tlc/$name"
	mkdir -p "$meta"
	out="$(mktemp)"
	# `-metadir` KEEPS TLC'S SCRATCH OUT OF THE WORKING TREE. Left to itself it writes a `states/`
	# directory beside the specification, one file per queue segment - gigabytes for a configuration
	# of any size, in a directory git reports as untracked and an editor's file watcher tries to
	# diff. `-cleanup` removes it afterwards; `-metadir` is what keeps it from being in the tree at
	# all while the run is going.
	if java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -metadir "$meta" -cleanup -workers 4 -config "$cfg" "$SPEC_DIR/Transfer.tla" >"$out" 2>&1; then
		# THE LAST ONE, NOT THE FIRST. TLC prints a progress line a minute into a long run and the
		# summary at the end, and both say "distinct states found" - so taking the first reported
		# the point the run had reached rather than where it finished, which looked like a
		# configuration that had shrunk by three orders of magnitude.
		printf 'capability-model: %s - %s, depth %s\n' "$name" \
			"$(grep -aoE '[0-9]+ distinct states found' "$out" | tail -1)" \
			"$(grep -aoE 'state graph search is [0-9]+' "$out" | grep -oE '[0-9]+$')"
	else
		echo "capability-model: $name FAILED" >&2
		# The counterexample is the whole value of the failure: print it rather than a summary.
		sed -n '/^Error:/,$p' "$out" >&2
		status=1
	fi
	rm -f "$out"
done
exit "$status"
