#!/usr/bin/env bash
# Six deliberate defects, and the gate that must catch each one.
#
# A MODEL THAT HAS NEVER FAILED IS A MODEL NOBODY HAS TESTED. Every invariant in
# `docs/spec/capability` passes today, and passing is exactly what an invariant does when its
# dangerous action is disabled, when its quantifier ranges over nothing, or when it says something
# true of every state. So this breaks the specification on purpose, one rule at a time, and requires
# TLC to name the invariant that should have caught it.
#
# EACH MUTATION IS A LITERAL REPLACEMENT AND MUST MATCH EXACTLY ONCE. A mutation that matches
# nothing is a gate that has quietly stopped testing anything - which is the same failure it exists
# to catch - so a miss is an error rather than a skip.
#
# The mutations are on a COPY. Nothing here writes the specification.
set -euo pipefail

cd "$(dirname "$0")/../.."
SPEC="docs/spec/capability"
JAR=".build/tools/tla2tools.jar"

fail() {
	echo "model-mutations: $*" >&2
	exit 1
}

[[ -f "$JAR" ]] || fail "$JAR is not here - run ./bootstrap.sh tla2tools"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp "$SPEC"/*.tla "$SPEC"/*.cfg "$work/"

# Replace `old` with `new` in the copy, exactly once.
mutate() {
	python3 - "$work/Transfer.tla" "$1" "$2" <<'PY'
import io, sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = io.open(path).read()
found = text.count(old)
if found != 1:
	sys.stderr.write("the mutation matches %d places, and a mutation must match exactly one\n" % found)
	raise SystemExit(1)
io.open(path, "w").write(text.replace(old, new))
PY
}

run_mutation() {
	local name="$1" config="$2" expected="$3" old="$4" new="$5"
	cp "$SPEC/Transfer.tla" "$work/Transfer.tla"
	mutate "$old" "$new" || {
		echo "model-mutations: $name could not be applied - the specification no longer contains what it breaks" >&2
		return 1
	}
	local out="$work/$name.log"
	if java -XX:+UseParallelGC -cp "$JAR" tlc2.TLC -metadir "$work/meta-$name" -cleanup -workers 4 \
		-config "$work/$config.cfg" "$work/Transfer.tla" >"$out" 2>&1; then
		echo "model-mutations: $name PASSED the model - the gate does not catch it" >&2
		return 1
	fi
	if grep -aq "Invariant $expected is violated" "$out" || grep -aq "Temporal properties were violated" "$out"; then
		echo "model-mutations: $name is caught by $expected"
		return 0
	fi
	echo "model-mutations: $name failed, but not as $expected" >&2
	grep -aE "^Error" "$out" | head -3 >&2
	return 1
}

status=0

# 1. A slot's generation WRAPS instead of retiring, so a long-dead handle can name a later
#    capability. The step property is what notices a generation going backwards.
run_mutation generations-wrap spike GenerationsOnlyAdvance \
	'THEN [state |-> "Retired", cap |-> NoCap, gen |-> MaxGen]' \
	'THEN [state |-> "Free", cap |-> NoCap, gen |-> 1]' || status=1

# 2. A duplicate may be asked for rights the original does not have.
run_mutation duplicate-widens handles AuthorityNeverWidens \
	'    /\ r \subseteq table[p][i].cap.rights' \
	'    /\ TRUE' || status=1

# 3. A transfer CLONES rather than moves: the source slot keeps its capability.
run_mutation transfer-clones spike TransferIsLinear \
	"    /\\ table' = [table EXCEPT ![p][i] = [state |-> \"Reserved\", cap |-> NoCap, gen |-> table[p][i].gen]]" \
	'    /\ UNCHANGED table' || status=1

# 4. A rollback into a CLOSED table installs the capability anyway.
run_mutation closed-table-restores spike ClosedProcessCannotResurrect \
	'    /\ IF closed[p]' \
	'    /\ IF FALSE' || status=1

# 5. A receive takes whatever is at the head rather than the message it inspected.
run_mutation dequeue-by-position transactions-single MessageIdentityStable \
	'    /\ peeked = Head(queue).id' \
	'    /\ TRUE' || status=1

# 6. An install does not consume the booking it used, so one booking installs twice.
run_mutation install-keeps-booking spike QuotaConserved \
	"       /\\ table' = [table EXCEPT ![p][i] = [state |-> \"Live\", cap |-> Head(held.caps), gen |-> table[p][i].gen]]
       /\\ booked' = [booked EXCEPT ![p] = Tail(booked[p])]
       /\\ installed' = Append(installed, i)" \
	"       /\\ table' = [table EXCEPT ![p][i] = [state |-> \"Live\", cap |-> Head(held.caps), gen |-> table[p][i].gen]]
       /\\ UNCHANGED booked
       /\\ installed' = Append(installed, i)" || status=1

exit "$status"
