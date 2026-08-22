#!/usr/bin/env bash
# A warning is answered by changing the code, never by switching the lint off.
#
# P02M0144 exists because that rule was not written down anywhere. Ninety-one attributes had
# accumulated under `src/` - fifty-five of them `#![allow(dead_code)]` at the top of a whole file -
# and they hid more than they showed: the x86_64 kernel printed twenty warnings and held a hundred
# and forty. Worse, the coverage was uneven, so the same code was reported on one target and silent
# on another, and nobody had decided that. `arch/x86_64/usermode.rs` had one and the aarch64
# usermode module, written inline in `mod.rs`, did not.
#
# WHAT THE SUPPRESSIONS WERE HIDING, as a sample: a duplicate of `read_isr` under a second name that
# nothing called, an `isr_ack` a driver was told it must call, a `Queue.handle` whose comment said it
# kept a DMA buffer alive and which no `Drop` ever read, `unmap_pages` declared by all three backends
# and called by none, a `msix_count` resolved for every PCI device and read by nothing, and two
# `cfg_attr(not(test), allow(dead_code))` attributes that had slid onto the wrong function.
#
# So the rule is mechanical now. The answer to "this warns" is a `cfg` that tells the truth about
# where the code is reached from, a deletion, or the missing caller - and if none of those is right,
# the answer is a decision recorded in a milestone, not an attribute.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# `allow(unused)` and its family, and `allow(dead_code)`, in any spelling including inside a
# `cfg_attr`. Nothing else is looked for: this gate is about lints that hide code nobody reaches,
# not about every attribute anyone might write.
pattern='allow\((dead_code|unused|unused_imports|unused_variables|unused_mut|unused_unsafe|unused_parens)\)'

found="$(grep -rnE "$pattern" "$root" --include='*.rs' || true)"
if [[ -n "$found" ]]; then
	echo "no-suppression: a lint is switched off instead of the code being fixed:" >&2
	echo "$found" >&2
	echo >&2
	echo "Answer the warning instead: a cfg that says where the code is reached from, a deletion, or the caller that is missing." >&2
	exit 1
fi

count="$(grep -rcE "$pattern" "$root" --include='*.rs' 2>/dev/null | grep -vc ':0$' || true)"
echo "no-suppression: clean (0 dead_code/unused allow attributes under src/)"
