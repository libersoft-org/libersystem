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

# What `MEASUREMENTS.md` records for one configuration, and whether this run matches it.
#
# The document is parsed rather than a second data file being kept beside it: the document is the
# thing that has to be true, and a machine-readable copy of it is one more place to drift.
recorded() {
	local name="$1" field="$2"
	awk -v want="## \`$name.cfg\`" -v field="$field" '
		/^## /  { inside = ($0 == want) }
		inside && index($0, "| " field " | ") == 1 {
			line = $0
			sub(/^\| [^|]* \| /, "", line)
			sub(/ \|$/, "", line)
			gsub(/`|…/, "", line)
			print line
			exit
		}
	' "$MEASUREMENTS"
}

compare_recorded() {
	local name="$1" states="$2" depth="$3" ok=0
	local want_states want_depth want_cfg have_cfg
	want_states="$(recorded "$name" "Distinct states")"
	want_depth="$(recorded "$name" "Search depth")"
	want_cfg="$(recorded "$name" "\`$name.cfg\`")"
	have_cfg="$(sha256sum "$SPEC_DIR/$name.cfg" | cut -c1-16)"
	if [[ -z "$want_states" || -z "$want_depth" || -z "$want_cfg" ]]; then
		echo "capability-model: $name is not recorded in $MEASUREMENTS - a configuration whose result is not published is one nobody can hold to it" >&2
		return 1
	fi
	if [[ "$states" != "$want_states" ]]; then
		echo "capability-model: $name explored $states distinct states, and $MEASUREMENTS records $want_states" >&2
		ok=1
	fi
	if [[ "$depth" != "$want_depth" ]]; then
		echo "capability-model: $name reached depth $depth, and $MEASUREMENTS records $want_depth" >&2
		ok=1
	fi
	if [[ "$have_cfg" != "$want_cfg" ]]; then
		echo "capability-model: $name.cfg is $have_cfg and $MEASUREMENTS records $want_cfg" >&2
		ok=1
	fi
	if ((ok)); then
		echo "capability-model:   a different result is a DIFFERENT RESULT: re-run, check what changed, and update $MEASUREMENTS deliberately" >&2
		return 1
	fi
	printf 'capability-model: %s - %s distinct states, depth %s, as recorded\n' "$name" "$states" "$depth"
}

# AND THE SPECIFICATION ITSELF, once rather than per configuration.
MEASUREMENTS="$SPEC_DIR/MEASUREMENTS.md"
[[ -f "$MEASUREMENTS" ]] || {
	echo "capability-model: $MEASUREMENTS is not here, so there is nothing to hold this run to" >&2
	exit 1
}
status=0
for file in Transfer.tla Capability.tla; do
	want="$(awk -v want="\`$file\`" '$0 ~ "^\\| " want " \\| " { gsub(/`|…/, ""); sub(/^\| [^|]* \| /, ""); sub(/ \|$/, ""); print; exit }' "$MEASUREMENTS")"
	have="$(sha256sum "$SPEC_DIR/$file" | cut -c1-16)"
	if [[ "$want" != "$have" ]]; then
		echo "capability-model: $file is $have and $MEASUREMENTS records $want - the published result describes a different specification" >&2
		status=1
	fi
done
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
		# COMPARED, NOT PRINTED.
		#
		# This formatted the numbers into a line and moved on, so every recorded digest in
		# `MEASUREMENTS.md` and three of its six state counts drifted away from the files in the tree
		# under a green gate - and the document's own first sentence says why that is not allowed: "a
		# later run that explores fewer states is a DIFFERENT result and may not quietly replace a
		# committed one". A number a gate prints is a number nobody is holding it to.
		states="$(grep -aoE '[0-9]+ distinct states found' "$out" | tail -1 | grep -oE '^[0-9]+')"
		depth="$(grep -aoE 'state graph search is [0-9]+' "$out" | grep -oE '[0-9]+$')"
		if ! compare_recorded "$name" "$states" "$depth"; then
			status=1
		fi
	else
		echo "capability-model: $name FAILED" >&2
		# The counterexample is the whole value of the failure: print it rather than a summary.
		sed -n '/^Error:/,$p' "$out" >&2
		status=1
	fi
	rm -f "$out"
done
exit "$status"
