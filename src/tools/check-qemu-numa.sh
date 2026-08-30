#!/usr/bin/env bash
# Placement proved by addresses, not by timing - on all three ports.
#
# WHAT THIS GATE IS. A named two-node x86_64 QEMU profile - two memory backends, four processors
# split two and two, and a non-local distance greater than the local one - booted through the
# ordinary UEFI/ACPI path the suite already uses. The kernel reads the SRAT and the SLIT, partitions
# its frame allocator into one pool per node, and the in-guest tests then allocate on each node and
# check the RETURNED PHYSICAL ADDRESS against the range firmware assigned to it.
#
# NOT A PERFORMANCE GATE, and it could not be one: an emulated topology has no interconnect and no
# memory controllers. What it proves is routing and accounting, which is what placement correctness
# means here.
#
# A TEST THAT ONLY PRINTS TWO NODE IDS IS NOT EVIDENCE. This requires the node figures, the pools,
# and the tests that steer an allocation - by name - and it fails if any of them reports itself
# skipped, which is exactly what the same tests do on the one-node profile.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."
# shellcheck source=/dev/null
source "$HERE/result-logs.sh"

fail() {
	echo "qemu-numa: $*" >&2
	exit 1
}

# WHICH PROFILE, because three profiles are three costs.
#
# The three boots inside this gate are an x86_64 KVM run and two emulated ones, and the emulated pair
# is most of an hour. As one step they could not be scheduled against each other by the outer
# `--jobs`, could not be measured separately, and a change that only needs one of them paid for all
# three. `--only <arch>` runs one; no argument runs all three, which is what a person typing the gate
# name means. Same shape as `check-qemu-arch-profiles.sh --only`, and for the same reason.
only=""
while (($#)); do
	case "$1" in
	--only)
		only="${2:-}"
		shift 2 || fail "--only needs an architecture"
		;;
	*) fail "unknown argument: $1" ;;
	esac
done
case "$only" in
"" | x86_64 | aarch64 | riscv64) ;;
*) fail "--only takes x86_64, aarch64 or riscv64" ;;
esac
wanted() { [[ -z "$only" || "$only" == "$1" ]]; }

command -v qemu-system-x86_64 >/dev/null || fail "qemu-system-x86_64 is not installed"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# TWO BACKENDS SUMMING TO WHAT THE HARNESS GIVES THE GUEST. QEMU refuses a NUMA configuration whose
# nodes do not add up to `-m`, which is the check that keeps this profile honest about its own size.
profile="-object memory-backend-ram,id=m0,size=2G -object memory-backend-ram,id=m1,size=2G"
profile+=" -numa node,nodeid=0,memdev=m0,cpus=0-1 -numa node,nodeid=1,memdev=m1,cpus=2-3"
# ASYMMETRIC ON PURPOSE: the local distance is 10 by specification and this is what makes "further"
# a number the kernel read rather than a default it assumed.
profile+=" -numa dist,src=0,dst=1,val=21 -numa dist,src=1,dst=0,val=21"

# EVERY WEAKER OUTCOME, NOT ONE SPELLING OF IT.
#
# The placement test passes on a weaker claim when the target core never picked the thread up: it
# says so on a `numa-fixture:` line and returns success. The comment here said that line was what the
# check below refuses; the check looked for `numa-fixture: skipped`, and the line the test actually
# prints is `numa-fixture: the thread was queued on cpu N of node M and that core drains its own
# queue`. So the one outcome this gate exists to reject was the one it let through.
#
# Any `numa-fixture:` line at all is now the refusal. The tests print one only when they could not
# make their full claim - a run where every placement really ran produces none.
# EVERY LOG THE RUN NAMED, not the first of them. This took `$2` and greped that one file while both
# call sites hand it the whole `result_logs` array - run log first, guest log second. The oracle is in
# the GUEST log on x86_64 and aarch64, so the one check that rejects a weaker placement was reading
# the file that cannot contain it, and the false green this function exists to stop was still open.
weak_placement() {
	local where="$1"
	shift
	if grep -aqh "numa-fixture:" "$@"; then
		echo "qemu-numa: a placement test could not make its full claim on the $where profile" >&2
		grep -ah "numa-fixture:" "$@" >&2
		exit 1
	fi
}

# EVERY OUTCOME BUT THE FULL ONE. The matrix prints exactly one `numa-matrix:` line and the only
# spelling that means it made every claim is `complete`; `skipped` (no topology, one node) and
# `incomplete` (the other node's core never drained its queue, so the cross-node free was made here)
# are both a weaker run on a profile that was built to make the full one possible.
weak_matrix() {
	local where="$1"
	shift
	# A HERE-STRING, NOT A PIPE. `grep -q` closes its input on the first match, and under `pipefail`
	# that reads as a failed pipeline - which is a check whose success depends on how much output the
	# first stage had left to write. The lines are collected first and matched second.
	local said
	said="$(grep -ah "numa-matrix:" "$@" || true)"
	if [[ -n "$said" ]] && grep -aqv "numa-matrix: complete" <<<"$said"; then
		echo "qemu-numa: the placement matrix did not make its full claim on the $where profile" >&2
		grep -ah "numa-matrix:" "$@" >&2
		exit 1
	fi
	grep -aqh "numa-matrix: complete" "$@" || {
		echo "qemu-numa: the placement matrix printed nothing on the $where profile" >&2
		exit 1
	}
}

if wanted x86_64; then
	echo "qemu-numa: booting the two-node profile"
	# `--tags numa`, NOT `memory,smp`, AND THIS FILE ALREADY KNEW WHY.
	#
	# Three of its own assertions name `kernel.mem.numa.*` and `kernel.smp.numa.*`, every one of which
	# carries `Numa`; the rest are greps over what the BOOT printed and need no test at all. `memory,smp`
	# ran the whole memory suite to reach seven named tests - and `Memory` is also on the application
	# suite, so it pulled in `kernel.applications.imgconv_governed_working_set_is_measured`, which is an
	# image-conversion working-set measurement and takes 1149 seconds on an emulated target.
	#
	# The direct-boot ports below already do it this way, and say so in as many words: "The NUMA tests
	# carry their own tag for exactly this reason." That reasoning was never brought back up here.
	QEMU_EXTRA="$profile" ./test.sh --arch x86_64 --tags numa --smp 4 >"$work/run.log" 2>&1 || {
		echo "qemu-numa: the numa tests failed on the two-node profile" >&2
		tail -20 "$work/run.log" >&2
		exit 1
	}

	# THE LOGS THIS RUN WROTE, from the run rather than from the newest file of this architecture. Two
	# runs in flight and the glob reads the other one's answer; and it read only the guest log, which on
	# riscv64 below holds U-Boot and the loader and none of the suite's output.
	mapfile -t logs < <(result_logs "$work/run.log") || fail "the x86_64 run did not say which logs it wrote"
	((${#logs[@]})) || fail "the x86_64 run named no readable log"

	# 1. THE TOPOLOGY WAS READ, AND READ FROM FIRMWARE. "local/remote default" here would mean the SLIT
	#    was absent or refused, which is a different machine from the one this profile describes.
	grep -aqh "numa: 2 node(s), distances from firmware" ${logs[@]} || {
		echo "qemu-numa: the kernel did not read a two-node topology with firmware distances" >&2
		grep -ah -m 10 "numa:" ${logs[@]} >&2 || echo "    (the kernel printed nothing about topology)" >&2
		exit 1
	}

	# 2. BOTH NODES HAVE MEMORY AND PROCESSORS. A node reported with zero MiB is the shape of a parser
	#    reading the proximity domain from the wrong offset - which is exactly what this found once, and
	#    what the boot report showed while the processors looked perfectly correct.
	for node in 0 1; do
		line="$(grep -ah -m 1 "numa:   node $node:" ${logs[@]} || true)"
		[[ -n "$line" ]] || fail "node $node is missing from the report"
		case "$line" in
		*" 0 MiB"*) fail "node $node was reported with no memory: $line" ;;
		*"0 processor(s) described"*) fail "node $node was reported with no processors: $line" ;;
		esac
		# AND THE ONES THAT CAME UP, WHICH IS THE NUMBER THE SCHEDULER PLACES AGAINST. The report now
		# prints both because they are different facts; a node with two described cores and none
		# online is a node no thread can be placed on, and the count on its own hid that.
		case "$line" in
		*", 0 online"*) fail "node $node described processors and none of them came online: $line" ;;
		esac
		echo "qemu-numa:   ${line#*numa:   }"
	done

	# 2b. AND THE GRAPH IS THE ONE THIS PROFILE DESCRIBES, assignment by assignment.
	#
	#     Counting nodes and processors accepts a graph whose assignments are SWAPPED: this profile gives
	#     each node two cores and near-equal memory, so a machine that has them the wrong way round
	#     satisfies every count above. What the profile actually says is `cpus=0-1` on node 0, `cpus=2-3`
	#     on node 1, and a non-local distance of 21 against a local 10 - and the kernel now prints each
	#     of those, so they can be compared rather than assumed.
	for pair in "0 0" "0 1" "1 2" "1 3"; do
		set -- $pair
		# THE END OF THE LINE, ALLOWING THE CARRIAGE RETURN. A guest's serial log is CRLF, so a bare `$`
		# anchor never matches - and an unanchored one would let `cpu 1` match `cpu 10`.
		grep -aqhE "numa:     node $1 cpu $2[[:space:]]*$" ${logs[@]} || fail "the profile puts cpu $2 on node $1 and the kernel does not"
	done
	for triple in "0 0 10" "0 1 21" "1 0 21" "1 1 10"; do
		set -- $triple
		grep -aqhE "numa:   distance $1 -> $2: $3[[:space:]]*$" ${logs[@]} || fail "the profile declares distance $1 -> $2 as $3 and the kernel read something else"
	done
	echo "qemu-numa:   the normalized graph is the profile's: cpus 0,1 on node 0, cpus 2,3 on node 1, and 10/21 distances"

	# 3. THE ALLOCATOR WAS PARTITIONED. One pool per memory-bearing node plus the unaffiliated one.
	for node in 0 1; do
		grep -aqh "numa:   pool node $node:" ${logs[@]} || fail "node $node has no pool of its own"
	done
	grep -aqh "numa:   pool unaffiliated:" ${logs[@]} || fail "there is no pool for memory no node owns"
	if grep -aqh "numa: WARNING" ${logs[@]}; then
		fail "the pools and the allocator disagree about how many frames are free"
	fi

	# 4. AND THE TESTS THAT STEER AN ALLOCATION RAN. On the one-node profile these report themselves
	#    skipped, which is what makes requiring them here meaningful.
	# `[ok]` ON THE SAME LINE, WHICH IS ONLY TRUE OF A TEST THAT PRINTS NOTHING.
	#
	# The runner writes the test's name, then whatever the test printed, then `[ok]` - so for a test
	# with output the marker is on the NEXT line, and a single-line grep cannot see it. The two tests
	# that print are asserted by their own completion lines instead, which say more than `[ok]` does:
	# the matrix names every claim it made, and the comparison names the trace it walked.
	for name in strict_fails_where_preferred_falls_back a_contiguous_span_never_crosses_two_nodes every_frame_returns_to_the_pool_that_owns_its_address; do
		grep -aqh "kernel.mem.numa.$name\.\.\..*\[ok\]" ${logs[@]} || fail "kernel.mem.numa.$name did not run or did not pass"
		echo "qemu-numa:   $name passed"
	done
	grep -aqh "the model and the allocator agreed on every placement decision in the trace" ${logs[@]} || fail "the reference model and the allocator were not compared over a trace"
	echo "qemu-numa:   the_reference_model_and_the_allocator_agree_over_a_trace passed"
	# AND THE PLACEMENT HALF: a thread asked for node 1 ran on a core whose normalized node is 1.
	for name in only_cores_that_came_up_are_bound_to_a_node placement_names_a_core_of_the_node_it_was_asked_for a_thread_placed_on_a_node_runs_on_a_core_of_that_node; do
		grep -aqh "kernel.smp.numa.$name\.\.\..*\[ok\]" ${logs[@]} || fail "kernel.smp.numa.$name did not run or did not pass"
		echo "qemu-numa:   $name passed"
	done
	weak_placement "two-node" ${logs[@]}

	# AND THE MATRIX MADE ITS FULL CLAIM. It prints under a prefix of its own - `numa-matrix:` - because
	# it prints on success too, and the refusal above would then read a passing matrix as a weakened
	# placement. What is refused here is the matrix reporting itself SKIPPED, which on a profile that
	# demonstrably has two memory-bearing nodes would mean it never saw the topology the boot read.
	weak_matrix "two-node" ${logs[@]}
fi

# 5. AND THE TWO DEVICE-TREE PORTS, on their DIRECT-BOOT profiles.
#
# DIRECT BOOT AND NOT UEFI, because on these ports the loader hands the kernel a firmware memory map
# and the device tree's `/memory` is then not used at all - so a UEFI boot reads the topology from
# the tree and the banks from somewhere else, which is a machine describing itself two ways. The
# direct profile is the one the milestone names, and it is the one where both halves come from the
# same tree.
#
# `--tags numa` rather than the memory suite: a direct boot carries no volume package, so the
# application tests that need one cannot run there. The NUMA tests carry their own tag for exactly
# this reason.
profile_dt="-object memory-backend-ram,id=m0,size=256M -object memory-backend-ram,id=m1,size=256M"
profile_dt+=" -numa node,nodeid=0,memdev=m0,cpus=0-1 -numa node,nodeid=1,memdev=m1,cpus=2-3"
profile_dt+=" -numa dist,src=0,dst=1,val=21 -numa dist,src=1,dst=0,val=21"
for port in aarch64 riscv64; do
	wanted "$port" || continue
	echo "qemu-numa: booting the two-node $port direct profile"
	# NO `--timeout` OF THIS GATE'S OWN. 1800s is tighter than the harness picks for an emulated
	# target and tighter than the stall detector that decides whether a run is WEDGED (2400s of
	# silence) - so slow was always reported before wedged could be distinguished. Two authorities
	# over one window; the harness owns the emulated calibration.
	UEFI=0 QEMU_EXTRA="$profile_dt" ./test.sh --arch "$port" --tags numa --smp 4 >"$work/$port.log" 2>&1 || {
		echo "qemu-numa: the numa tests failed on the two-node $port profile" >&2
		tail -20 "$work/$port.log" >&2
		exit 1
	}
	mapfile -t port_logs < <(result_logs "$work/$port.log") || fail "the $port run did not say which logs it wrote"
	((${#port_logs[@]})) || fail "the $port run named no readable log"
	grep -aqh "numa: 2 node(s), distances from firmware" ${port_logs[@]} || {
		echo "qemu-numa: $port did not read a two-node topology with firmware distances" >&2
		grep -ah -m 10 "numa:" ${port_logs[@]} >&2 || true
		exit 1
	}
	for node in 0 1; do
		grep -aqh "numa:   pool node $node:" ${port_logs[@]} || fail "$port: node $node has no pool of its own"
	done
	# THE SAME GRAPH COMPARISON AS x86_64 - the distances, which the device tree declares the same
	# way.
	for triple in "0 0 10" "0 1 21" "1 0 21" "1 1 10"; do
		set -- $triple
		grep -aqhE "numa:   distance $1 -> $2: $3[[:space:]]*$" ${port_logs[@]} || fail "$port: the profile declares distance $1 -> $2 as $3 and the kernel read something else"
	done
	# AND THE CPU ASSIGNMENTS, BY THEIR SHAPE RATHER THAN BY THEIR INTEGERS.
	#
	# The x86 phase greps for `node 0 cpu 0` and the three lines beside it, and that cannot be done
	# here: a hart id and an MPIDR are the machine's own numbering while the profile names LOGICAL
	# cpus, so requiring them to be the same integers would be asserting a coincidence of QEMU's
	# mapping rather than a property of the kernel.
	#
	# But omitting the comparison entirely - which is what this did - accepts the one graph the whole
	# exact-graph work exists to reject: these profiles are symmetric 2+2 with near-equal memory, so a
	# SWAPPED assignment satisfies every count, every pool and every distance above. What is
	# numbering-independent is the SHAPE, and it is enough: this profile puts the first two cpus on
	# node 0 and the last two on node 1, so however the machine numbers them, node 0's identifiers
	# are two, node 1's are two, and every one of node 0's is below every one of node 1's. A swap
	# fails on the last of those.
	node0_cpus="$(grep -ahoE "numa:     node 0 cpu [0-9]+" ${port_logs[@]} | awk '{print $NF}' | sort -n -u)"
	node1_cpus="$(grep -ahoE "numa:     node 1 cpu [0-9]+" ${port_logs[@]} | awk '{print $NF}' | sort -n -u)"
	[[ "$(wc -l <<<"$node0_cpus")" == 2 && "$(wc -l <<<"$node1_cpus")" == 2 ]] || fail "$port: the profile gives each node two processors and the kernel reported node 0: $(tr '\n' ' ' <<<"$node0_cpus"), node 1: $(tr '\n' ' ' <<<"$node1_cpus")"
	if [[ "$(tail -n 1 <<<"$node0_cpus")" -ge "$(head -n 1 <<<"$node1_cpus")" ]]; then
		fail "$port: this profile puts the FIRST two processors on node 0 and the last two on node 1, and the kernel reported node 0: $(tr '\n' ' ' <<<"$node0_cpus"), node 1: $(tr '\n' ' ' <<<"$node1_cpus") - the assignments are swapped or interleaved"
	fi
	echo "qemu-numa:   $port: node 0 holds the first two processors and node 1 the last two, so the assignment is not swapped"
	weak_placement "$port" ${port_logs[@]}
	# AND THE PLACEMENT HALF, WHICH ONLY x86_64 WAS ASKED FOR. M5 wants node-placement execution on
	# all three named profiles, and a tag or selection regression that stopped running these on the
	# device-tree ports would have been invisible: the phase asserted pools, a report and the memory
	# tests, none of which needs a core to have been bound to a node at all.
	for name in only_cores_that_came_up_are_bound_to_a_node placement_names_a_core_of_the_node_it_was_asked_for a_thread_placed_on_a_node_runs_on_a_core_of_that_node; do
		grep -aqh "kernel.smp.numa.$name\.\.\..*\[ok\]" ${port_logs[@]} || fail "$port: kernel.smp.numa.$name did not run or did not pass"
	done
	# THE SAME NAMED TESTS AS x86_64, on the profiles that used to be checked for pools and a report
	# and nothing else. Three profiles were the milestone's evidence and only one of them was asked
	# whether an allocation had been steered.
	for name in strict_fails_where_preferred_falls_back a_contiguous_span_never_crosses_two_nodes every_frame_returns_to_the_pool_that_owns_its_address; do
		grep -aqh "kernel.mem.numa.$name\.\.\..*\[ok\]" ${port_logs[@]} || fail "$port: kernel.mem.numa.$name did not run or did not pass"
	done
	# The two that print are asserted by their own lines - see the x86_64 phase.
	grep -aqh "the model and the allocator agreed on every placement decision in the trace" ${port_logs[@]} || fail "$port: the reference model and the allocator were not compared over a trace"
	weak_matrix "$port" ${port_logs[@]}
	echo "qemu-numa:   $port: $(grep -a -m 1 -o 'numa:   node 0: .*' ${port_logs[@]})"
	echo "qemu-numa:   $port: $(grep -a -m 1 -o 'numa:   node 1: .*' ${port_logs[@]})"
done

if [[ -n "$only" ]]; then
	echo "qemu-numa: the $only profile read two nodes, partitioned per node, and steered real allocations"
else
	echo "qemu-numa: three profiles - x86_64 ACPI, aarch64 and riscv64 device tree - each read two nodes, partitioned per node, and steered real allocations"
fi
