#!/usr/bin/env bash
# The wire format, the negotiation and the event parser - on a host, against answers that are wrong.
#
# WHAT THIS GATE OWNS. `qemu-virtio-iommu-x86_64` boots a real controller and proves the boundary
# holds; it cannot easily produce a device that answers with a status from a later specification, a
# truncated tail, or a fault record with no address in it. Those are this gate's, and they run in a
# second on a host.
#
# WHAT IT DOES NOT OWN: the generic interval, accounting and failure-order semantics. Those belong to
# the DMA contract's own fake backend, which is in the same crate and runs in the same command - the
# division is which SUBJECT each test has, not which process it runs in.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "virtio-iommu-protocol: $*" >&2
	exit 1
}

output="$(cd src/dma && cargo test --quiet --offline 2>&1)" || {
	echo "$output" >&2
	fail "the contract and codec suite failed"
}

# THE COUNT IS CHECKED, not just the exit status. A suite that stopped being compiled in - a renamed
# module, a `cfg` that stopped matching - exits zero having run nothing, which is the failure mode a
# green tick hides best.
# The largest count the run reported. `sort -rn` ends of its own accord, so nothing here is a reader
# that stops early - which under `pipefail` reads as a failed pipeline.
count="$(printf '%s\n' "$output" | sed -n 's/^running \([0-9]*\) tests$/\1/p' | sort -rn | sed -n '1p')"
[[ -n "$count" ]] || fail "the suite reported no test count at all"
((count >= 35)) || fail "only $count test(s) ran, and this suite has more than that - something stopped being compiled in"

echo "virtio-iommu-protocol: $count test(s) - the DMA contract, its fake backend, and the virtio-iommu codec"
