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

fail() {
	echo "qemu-numa: $*" >&2
	exit 1
}

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

echo "qemu-numa: booting the two-node profile"
QEMU_EXTRA="$profile" ./test.sh --arch x86_64 --tags memory,smp --smp 4 >"$work/run.log" 2>&1 || {
	echo "qemu-numa: the memory suite failed on the two-node profile" >&2
	tail -20 "$work/run.log" >&2
	exit 1
}

shopt -s nullglob
logs=(.build/logs/test/x86_64-*-guest.log)
shopt -u nullglob
((${#logs[@]})) || fail "the run produced no guest log"
readarray -t logs < <(printf '%s\n' "${logs[@]}" | sort)
log="${logs[-1]}"

# 1. THE TOPOLOGY WAS READ, AND READ FROM FIRMWARE. "local/remote default" here would mean the SLIT
#    was absent or refused, which is a different machine from the one this profile describes.
grep -aq "numa: 2 node(s), distances from firmware" "$log" || {
	echo "qemu-numa: the kernel did not read a two-node topology with firmware distances" >&2
	grep -a -m 10 "numa:" "$log" >&2 || echo "    (the kernel printed nothing about topology)" >&2
	exit 1
}

# 2. BOTH NODES HAVE MEMORY AND PROCESSORS. A node reported with zero MiB is the shape of a parser
#    reading the proximity domain from the wrong offset - which is exactly what this found once, and
#    what the boot report showed while the processors looked perfectly correct.
for node in 0 1; do
	line="$(grep -a -m 1 "numa:   node $node:" "$log" || true)"
	[[ -n "$line" ]] || fail "node $node is missing from the report"
	case "$line" in
	*" 0 MiB"*) fail "node $node was reported with no memory: $line" ;;
	*"0 processor(s)"*) fail "node $node was reported with no processors: $line" ;;
	esac
	echo "qemu-numa:   ${line#*numa:   }"
done

# 3. THE ALLOCATOR WAS PARTITIONED. One pool per memory-bearing node plus the unaffiliated one.
for node in 0 1; do
	grep -aq "numa:   pool node $node:" "$log" || fail "node $node has no pool of its own"
done
grep -aq "numa:   pool unaffiliated:" "$log" || fail "there is no pool for memory no node owns"
if grep -aq "numa: WARNING" "$log"; then
	fail "the pools and the allocator disagree about how many frames are free"
fi

# 4. AND THE TESTS THAT STEER AN ALLOCATION RAN. On the one-node profile these report themselves
#    skipped, which is what makes requiring them here meaningful.
for name in strict_fails_where_preferred_falls_back a_contiguous_span_never_crosses_two_nodes every_frame_returns_to_the_pool_that_owns_its_address; do
	grep -aq "kernel.mem.numa.$name\.\.\..*\[ok\]" "$log" || fail "kernel.mem.numa.$name did not run or did not pass"
	echo "qemu-numa:   $name passed"
done
# AND THE PLACEMENT HALF: a thread asked for node 1 ran on a core whose normalized node is 1.
for name in only_cores_that_came_up_are_bound_to_a_node placement_names_a_core_of_the_node_it_was_asked_for a_thread_placed_on_a_node_runs_on_a_core_of_that_node; do
	grep -aq "kernel.smp.numa.$name\.\.\..*\[ok\]" "$log" || fail "kernel.smp.numa.$name did not run or did not pass"
	echo "qemu-numa:   $name passed"
done
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
weak_placement() {
	local where="$1" file="$2"
	if grep -aq "numa-fixture:" "$file"; then
		echo "qemu-numa: a placement test could not make its full claim on the $where profile" >&2
		grep -a "numa-fixture:" "$file" >&2
		exit 1
	fi
}
weak_placement "two-node" "$log"

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
	echo "qemu-numa: booting the two-node $port direct profile"
	UEFI=0 QEMU_EXTRA="$profile_dt" ./test.sh --arch "$port" --tags numa --smp 4 --timeout 1800 >"$work/$port.log" 2>&1 || {
		echo "qemu-numa: the numa tests failed on the two-node $port profile" >&2
		tail -20 "$work/$port.log" >&2
		exit 1
	}
	shopt -s nullglob
	port_logs=(.build/logs/test/$port-*-guest.log)
	shopt -u nullglob
	((${#port_logs[@]})) || fail "the $port run produced no guest log"
	readarray -t port_logs < <(printf '%s\n' "${port_logs[@]}" | sort)
	port_log="${port_logs[-1]}"
	grep -aq "numa: 2 node(s), distances from firmware" "$port_log" || {
		echo "qemu-numa: $port did not read a two-node topology with firmware distances" >&2
		grep -a -m 10 "numa:" "$port_log" >&2 || true
		exit 1
	}
	for node in 0 1; do
		grep -aq "numa:   pool node $node:" "$port_log" || fail "$port: node $node has no pool of its own"
	done
	weak_placement "$port" "$port_log"
	echo "qemu-numa:   $port: $(grep -a -m 1 -o 'numa:   node 0: .*' "$port_log")"
	echo "qemu-numa:   $port: $(grep -a -m 1 -o 'numa:   node 1: .*' "$port_log")"
done

echo "qemu-numa: three profiles - x86_64 ACPI, aarch64 and riscv64 device tree - each read two nodes, partitioned per node, and steered real allocations"
