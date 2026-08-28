#!/usr/bin/env bash
# A MACHINE WITH MORE CORES THAN THIS KERNEL HOLDS IS BOOTED, NOT REASONED ABOUT.
#
# The kernel tracks one entry per core in two portable structures - the TLB shootdown's generation
# arrays and the online mask - and `smp::MAX_CPUS` is the smallest limit among them. A machine with
# more cores than that boots on the first `MAX_CPUS` and says so; the rest are parked and never
# started, so they hold no translation and a shootdown that reaches every started core is complete.
#
# WHAT THIS REFUSES TO LET COME BACK. Before the cap, x86_64 started every local APIC the firmware
# listed. On a 100-core host every shootdown then answered "cannot reach them all", every page freed
# after a page-table teardown was retired for good, and the boot never reached userspace: 3058 pages
# gone in 100 seconds. Nothing was wrong with the refusal - it is what stops a physical
# use-after-free - and everything was wrong with waking cores nobody could reach.
#
# THREE ASSERTIONS, AND THE LAST TWO ARE ABSENCES ON PURPOSE. That the cap happened is a line; that
# it WORKED is that neither of the two consequences of not capping appears anywhere in the boot.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
# shellcheck source=/dev/null
source "$HERE/result-logs.sh"

fail() {
	echo "smp-core-cap: $*" >&2
	exit 1
}

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"

# The supported count, read from the source rather than repeated here - a gate that carries its own
# copy of the number passes the day the kernel's changes and the gate's does not.
supported="$(sed -n 's/^pub const MAX_CPUS: usize = \([0-9]*\);.*/\1/p' src/kernel/smp/mod.rs)"
[[ -n "$supported" ]] || fail "src/kernel/smp/mod.rs does not declare MAX_CPUS where this gate reads it"
# EIGHT OVER, NOT A HUNDRED. The defect needs one core past the bound to appear; the rest is emulator
# time this gate would spend on every run to prove the same thing.
cores=$((supported + 8))

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "smp-core-cap: booting x86_64 with $cores cores against a supported count of $supported"
./test.sh --arch x86_64 --tags smoke --smp "$cores" >"$work/run.log" 2>&1 || {
	echo "smp-core-cap: the smoke suite failed at $cores cores" >&2
	tail -20 "$work/run.log" >&2
	exit 1
}

# THE LOGS THIS RUN WROTE, from the run. Not the newest guest log of this architecture: two runs in
# flight and that reads the other one's answer, which is the failure this gate is one of the users of.
mapfile -t logs < <(result_logs "$work/run.log") || fail "the run did not say which logs it wrote"
((${#logs[@]})) || fail "the run named no readable log"

parked=$((cores - supported))
want="smp: $parked declared core(s) past the $supported this kernel holds stay parked"
grep -qhF "$want" ${logs[@]} || fail "the boot did not say what it parked - expected '$want' in ${logs[*]}"
echo "smp-core-cap:     $want"

# THE SHOOTDOWN NEVER REFUSED. One line per boot when it does, so its absence is the assertion.
if grep -qh "cores are online and this tracks" ${logs[@]}; then
	grep -m1 -h "cores are online and this tracks" ${logs[@]} >&2
	fail "a shootdown could not reach every core, which is the cap failing to do its one job"
fi
echo "smp-core-cap:     no shootdown reported itself unable to reach every core"

# AND NOTHING WAS RETIRED - THE COUNTER, NOT A SENTENCE ABOUT IT.
#
# This grepped for one spelling of one warning: `its shootdown did not complete`. `frame::retire`
# prints `their shootdown did not complete` for the BATCHED case, and the retirement paths in
# `deallocate` and `note_retired_pages` increment the counter without printing either - so three of
# the four ways this machine can lose a page passed the check that exists to catch them.
#
# `test_runner` now prints the same retirement line the ordinary boot report prints, so the number
# itself is here to be asserted. Both warning spellings are still refused: they name WHY a page went,
# which the total cannot, and a run that prints one has already failed by the time the count is read.
if grep -qhE "could not be queued and (its|their) shootdown did not complete" ${logs[@]}; then
	grep -m1 -hE "could not be queued and (its|their) shootdown did not complete" ${logs[@]} >&2
	fail "pages were retired for an incomplete shootdown at $cores cores"
fi
retired="$(grep -ho "memory: [0-9]* page(s) retired for good" ${logs[@]} | tail -1 | sed 's/[^0-9]*\([0-9]*\).*/\1/')"
[[ -n "$retired" ]] || fail "the run printed no retirement count, so 'loses nothing' would be a claim with no evidence - expected a 'memory: N page(s) retired for good' line in ${logs[*]}"
[[ "$retired" == 0 ]] || fail "$retired page(s) were retired for good at $cores cores - the machine lost them, which is the consequence the cap exists to prevent"
echo "smp-core-cap:     0 page(s) retired for good, said by the run itself"
echo "smp-core-cap: a machine past the supported count boots on $supported cores, says so, and loses nothing"
