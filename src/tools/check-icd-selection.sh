#!/bin/bash
# The selection slot, exercised through the REAL launch path in a guest.
#
# WHAT A SLOT IS, AND WHY NOTHING SMALLER PROVES IT. A consumer built against a SET of
# interchangeable providers names none of them: there is no `DT_NEEDED` edge to name, so the
# candidates are carried by NAME AND DIGEST inside the authenticated identity record, and
# ProcessService chooses one and binds it into the verified closure BEFORE the first thread runs.
# Every part of that happens inside a launch. A host fixture can decide the rules - and eleven of
# them do, in `service-logic` - but only a running program can show that the provider it names is
# actually reachable through the slot.
#
# WHAT IS ASSERTED, AND WHY THE SET RATHER THAN ONE MEMBER OF IT:
#
#   the slot BINDS            `icdcheck` launches at all. With an unfilled slot the launch is
#                               refused outright, so reaching its first line is the binding.
#   the LOWEST and the        2 and 6 are the ends of the admitted range. A gate that negotiated one
#     HIGHEST admitted          convenient version would leave the substrate at every other admitted
#     version                   version undecided, which is the deferral this milestone refuses.
#   the FOUR REFUSALS         an ICD offering 0 and one offering only 1 are below the floor -
#                               negotiation does not exist there, so the two-export rule cannot be
#                               satisfied. One offering 7 is above the ceiling, where the interface
#                               functions MAY be queried rather than exported, so a conforming driver
#                               need not export what this substrate resolves. And the third export,
#                               `vk_icdGetPhysicalDeviceProcAddr`, is refused by NAME: admitting it
#                               would open a third symbol in a surface this milestone defines as
#                               closed.
#   the LOOKUP answers        the second export is reached and consulted, not merely present in a
#                               symbol table.
#
# THE THIRD-EXPORT REFUSAL IS A BUILD-TIME ONE and is asserted here as such: a slot declares the
# symbols its kind admits, and the build requires every candidate to export EXACTLY that set. A
# candidate carrying a third export fails to build, which is a stronger refusal than a run-time one -
# it cannot be staged at all.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."

work="$(mktemp -d)"
trap 'rm -rf "$work"; [[ -n "${driver_pid:-}" ]] && kill "$driver_pid" 2>/dev/null || true' EXIT
driver_pid=""

fail() {
	echo "icd-selection: $*" >&2
	exit 1
}

probe="$root/../.build/image/x86_64-unknown-none/lib/foreign/icdprobe.lslib"
consumer="$root/../.build/image/x86_64-unknown-none/libexec/icdcheck"
[[ -f "$probe" ]] || fail "the synthetic ICD is not staged at $probe - build the image first"
[[ -f "$consumer" ]] || fail "the slot consumer is not staged at $consumer - build the image first"

# THE CLOSED SURFACE, MEASURED ON THE ARTIFACT. Exactly two exports, and the third one by name.
exports="$(llvm-readelf --wide --dyn-syms "$probe" | awk '$7 != "UND" && $8 != "" && $8 != "Name" {print $8}' | sort -u)"
expected="$(printf '%s\n' vk_icdGetInstanceProcAddr vk_icdNegotiateLoaderICDInterfaceVersion)"
[[ "$exports" == "$expected" ]] || fail "the synthetic ICD does not export exactly the two admitted symbols: $exports"
if grep -q vk_icdGetPhysicalDeviceProcAddr <<<"$exports"; then
	fail "the synthetic ICD exports the physical-device function, which this profile refuses by name"
fi

# THE CONSUMER HAS NO EDGE TO IT. This is what a slot IS, and it is checkable on the file: an image
# where the candidate had quietly become an ordinary dependency would pass every behavioural
# assertion below while proving nothing about selection.
needed="$(llvm-readelf -d "$consumer" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
if grep -q '^icdprobe\.lslib$' <<<"$needed"; then
	fail "the consumer has a DT_NEEDED edge to the candidate; then it is a dependency and not a slot"
fi
record="$(llvm-objcopy --dump-section .note.liber.identity=/dev/stdout "$consumer" /dev/null 2>/dev/null | tr -d '\0')"
grep -q "^selection=vulkan-icd:icdprobe.lslib=[0-9a-f]\{64\}$" <<<"$record" || fail "the consumer record carries no selection slot naming the candidate by digest"

# THE LAUNCH ITSELF. The guest console types the probe into the shell and reads back what it said.
script="$work/script"
cat >"$script" <<'EOF'
icdcheck
EOF
guest="$work/guest"
socket="$work/console"
rm -f "$socket"
python3 src/harness/guest-console.py --socket "$socket" --log "$guest" --script "$script" --seconds 60 >"$work/driver" 2>&1 &
driver_pid=$!
SERIAL="unix:$socket,server=on,wait=off" timeout 180 ./run.sh --arch x86_64 --smp 2 >"$work/run" 2>&1 || true
wait "$driver_pid" 2>/dev/null || true
driver_pid=""
[[ -s "$guest" ]] || fail "the guest produced no console output"

expect() {
	local line="$1" why="$2"
	grep -qF "$line" "$guest" || {
		echo "icd-selection: expected \"$line\" - $why" >&2
		echo "--- guest log ---" >&2
		cat "$guest" >&2
		exit 1
	}
	echo "icd-selection: $line"
}

# The slot bound and the provider answered: negotiation at both ends of the admitted range.
expect "icd lowest asked=2 accepted agreed=2" "the lowest admitted version must be agreed as asked"
expect "icd highest asked=6 accepted agreed=6" "the highest admitted version must be agreed as asked"
# Below the floor, refused: version 1 exports one symbol and version 0 has another bootstrap shape.
expect "icd below asked=0 refused agreed=0" "an ICD offering 0 must be refused"
expect "icd below asked=1 refused agreed=1" "an ICD offering only 1 must be refused"
# Above the ceiling, lowered rather than accepted: at 7 the interface functions may be queried.
expect "icd above asked=7 accepted agreed=6" "a caller offering 7 must be brought down to 6"
# The second export is consulted rather than merely present.
expect "icd lookup known=address unknown=null" "the lookup must answer its own name and nothing else"

echo "icd-selection: the slot bound the synthetic ICD and the admitted range holds at both ends"
