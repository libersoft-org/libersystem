#!/bin/bash
GUEST_GATE_NAME="foreign-facilities-guest"
# EVERY ADMITTED FOREIGN ABI FACILITY, CALLED ONCE IN A GUEST, WITH ITS ANSWER CHECKED.
#
# WHY THE STATIC GATES ARE NOT ENOUGH. `foreign-facilities` holds the substrate to exactly what the
# converged link resolved, and `foreign-audit-link` holds that set to the link itself - both read
# export tables and inventories. Neither shows that a facility WORKS. A symbol that is present and
# wrong is a link that succeeds and a driver that misbehaves later, which is the failure mode a C
# contract has: its violations are silent.
#
# THE ANSWERS ARE CHECKED AND NOT JUST THE CALLS MADE. `strncpy` pads to the full count, `snprintf`
# returns what it WOULD have written, `fputs` to anything but the diagnostic stream is refused rather
# than discarded, an `opendir` outside the record answers nothing, and an uncontended mutex taken
# twice in a row does not deadlock a process with one thread. Calling without checking would prove
# the symbol resolves, which the link already proved.
#
# THREE ARE RESOLVED AND DELIBERATELY NOT CALLED, and the probe says so rather than hiding it:
# `abort` and `__liber_assert_failed` diverge, so a probe that ended in one would report nothing
# about the facilities it checked first, and `vsnprintf` needs a `va_list` Rust cannot construct -
# it shares the rendering path `snprintf` exercises, and the host fixtures cover the difference.

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/.."
source "$root/tools/guest-gate.sh"

guest_gate_arch "$@"
guest_gate_require_quarantine abiprobe
guest_gate_run abiprobe abiprobe

expect_line() {
	grep -qxF "abiprobe: $1" "$GUEST_LINES" || {
		echo "foreign-facilities-guest: expected \"abiprobe: $1\" - $2" >&2
		cat "$GUEST_LINES" >&2
		exit 1
	}
}

# EVERY GROUP RAN. A probe that faulted half way would leave the later groups silent and the counts
# absent, which is what happened the first time this ran - so the groups are asserted individually
# rather than inferred from the total.
for group in allocator strings numbers formatting diagnostics record streams providers synchronisation; do
	expect_line "group $group" "the probe stopped before reaching the $group facilities"
done
expect_line "failures=0" "a facility answered wrongly; the probe names which"
checks="$(sed -n 's/^abiprobe: checks=\([0-9]*\)$/\1/p' "$GUEST_LINES" | tail -n 1)"
[[ -n "$checks" ]] || {
	echo "foreign-facilities-guest: the probe reported no check count" >&2
	exit 1
}
# THE COUNT IS A FLOOR AND NOT AN EQUALITY. Several facilities are checked more than once - a refusal
# as well as an answer - so the number is larger than the surface; what it must never do is shrink,
# because a probe that quietly stopped checking things would still report zero failures.
((checks >= 40)) || {
	echo "foreign-facilities-guest: only $checks check(s) ran; the probe is not exercising the surface" >&2
	exit 1
}
echo "foreign-facilities-guest: $GUEST_ARCH: $checks checks, 0 failures - every admitted facility answered"
