#!/bin/bash
# THE PORTED LOADER ITSELF, RUNNING IN A GUEST, AGAINST A SYNTHETIC ICD.
#
# WHY THIS GATE AND NOT THE SELECTION-SLOT ONE. The slot gate proves a provider is bound and
# reachable; the surface gates prove what the pinned configuration asks a system for. Neither runs
# the thing being ported. A substrate that satisfies every undefined symbol and still cannot load an
# ICD has prepared nothing, and a gate that never runs the ported loader proves the wrong half.
#
# THE ARTIFACT IS QUARANTINED, NOT SHIPPED. Its link comes from an upstream that is deliberately not
# in this tree, so it is staged into the DEVELOPMENT image this gate builds and into no other - which
# is what "the gate's own test-only image" means. A tree without the upstream builds and tests
# exactly as before, and this gate says it was not performed.
#
# WHAT IS ASSERTED, AND WHY EACH IS THE PORT SHOWING THROUGH:
#
#   THE RECORD INSTALLS       the launch hands the substrate which provider was bound into its
#                               closure, by the two exports that provider has and by nothing else.
#   ZERO LAYERS               the port makes the layer scan answer an empty list, which is legal and
#                               true here: there is no layer directory and no way to load one.
#   A LAYER IS REFUSED BY     both a query naming one and an instance creation enabling one return
#     NAME                      `VK_ERROR_LAYER_NOT_PRESENT` - silently dropping a requested layer is
#                               how a validation build reports success while validating nothing.
#   THE ICD IS REACHED        the loader opens the provider through the substrate's replaced lookup
#                               and calls BOTH of its entry points. The counts are what say so: the
#                               ICD cannot report it itself, because its export surface is exactly
#                               two symbols by the kind's own rule and a counter would be a third.
#   A CANDIDATE THAT FAILS      is refused, and is watched where it can be: the guest suite mutates
#     IDENTITY                    each field of a candidate's record inside a real volume and requires
#                               ProcessService to refuse the launch, and the packager refuses a
#                               mutated candidate before an image can even be built. It is not
#                               repeated here because a mutated candidate cannot reach a booted
#                               guest - the refusal happens earlier, which is the stronger place.
#   AND THE INSTANCE FAILS    with `VK_ERROR_INCOMPATIBLE_DRIVER`, which is the honest outcome and
#                               not a shortfall: the synthetic ICD implements no Vulkan entry point,
#                               because this milestone exports no Vulkan ABI. The loader found a
#                               driver, talked to it, and got nothing - which is exactly what it
#                               should report.

set -euo pipefail
GUEST_GATE_NAME="foreign-loader-guest"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

fail() { guest_gate_fail "$@"; }

guest_gate_arch "$@"
guest_gate_require_quarantine vkprobe

staged="$root/../.build/image/$(guest_gate_triple)/libexec/vkprobe"

# IT CARRIES THE LOADER, checked on the artifact rather than assumed from its name. A consumer that
# had lost its link to the audit archive would pass every behavioural assertion below by answering
# from nothing at all.
# READ FROM THE SYMBOL TABLE AND NOT THE DYNAMIC ONE. An executable publishes nothing unless it is
# linked to, so the loader's entry points are DEFINED in it and absent from its dynamic exports -
# which is right, and which made the first version of this check report a correctly linked artifact
# as not linked at all.
exports="$(llvm-nm --defined-only "$staged" | awk '{print $NF}' | sort -u)"
for entry in vkCreateInstance vkEnumerateInstanceLayerProperties vkGetInstanceProcAddr; do
	grep -qx "$entry" <<<"$exports" || fail "the staged consumer does not carry $entry - it is not linked against the audit-linked loader"
done
# AND THE ICD IS BOUND BY A SLOT, not by an edge. The two ICD exports must be UNDEFINED in it: a
# `DT_NEEDED` to the candidate would make this a dependency and prove nothing about selection.
undefined="$(llvm-readelf --wide --dyn-syms "$staged" | awk '$7 == "UND" && $8 != "" && $8 != "Name" {print $8}' | sort -u)"
grep -qx "vk_icdGetInstanceProcAddr" <<<"$undefined" || fail "the consumer defines the ICD entry point itself; then nothing was bound"
record="$(llvm-objcopy --dump-section .note.liber.identity=/dev/stdout "$staged" /dev/null 2>/dev/null | tail -c +21 | tr -d '\0')"
grep -q "^selection=vulkan-icd:icdprobe\.lslib=[0-9a-f]\{64\}$" <<<"$record" || fail "the consumer record names no ICD candidate by digest"
grep -qx "licence=Apache-2.0" <<<"$record" || fail "the consumer record does not carry the upstream licence it links"
echo "foreign-loader-guest: the staged consumer carries the loader and binds its ICD through a slot"

guest_gate_run vkprobe vkprobe
lines="$GUEST_LINES"

expect() {
	grep -qxF "vkprobe: $1" "$lines" || {
		echo "foreign-loader-guest: expected \"vkprobe: $1\" - $2" >&2
		cat "$lines" >&2
		exit 1
	}
	echo "foreign-loader-guest: $1"
}

expect "record installed=0" "the launch must be able to hand the substrate the provider it bound"
expect "layers accepted count=0" "the ported layer scan answers an empty list, always"
expect "enabled layer refused by name" "a query naming a layer must be refused rather than answered empty"
expect "instance with a layer refused by name" "an instance creation enabling a layer must be refused by name"
expect "loader lookup address" "the loader must answer out of its own table, which is its own code running"
expect "instance result=4294967287" "with a driver that implements nothing, VK_ERROR_INCOMPATIBLE_DRIVER is the honest answer"

# THE COUNTS. Zero would mean the loader never reached the driver - which is exactly what happened
# the first time this ran, because its search path and the record's paths never met.
negotiations="$(sed -n 's/^vkprobe: icd negotiations=\([0-9]*\)$/\1/p' "$lines" | tail -n 1)"
lookups="$(sed -n 's/^vkprobe: icd lookups=\([0-9]*\)$/\1/p' "$lines" | tail -n 1)"
[[ -n "$negotiations" && -n "$lookups" ]] || fail "the consumer reported no call counts"
((negotiations > 0)) || fail "the loader never negotiated with the ICD - it reached no driver at all"
((lookups > 0)) || fail "the loader never asked the ICD for an entry point"
echo "foreign-loader-guest: $GUEST_ARCH: the loader reached the ICD through the replaced lookup: $negotiations negotiation(s), $lookups entry-point lookup(s)"
