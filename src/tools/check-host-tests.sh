#!/usr/bin/env bash
# Run the host test suites of the crates that can be exercised without booting anything.
#
# LiberFS and the partition probe are pure over their input - a `Vec` of blocks, a map of
# sectors - so their behaviour can be pinned down on the host far more finely than through a
# QEMU boot: a forged superblock, a refused allocation, a partition table with one wrong
# checksum. Those suites existed and nothing ran them, which is a suite in name only. This
# gate is what makes them part of `./check.sh`.
#
# The in-kernel suite (test.sh) is the other half and not a substitute: it answers what the
# SYSTEM does with these crates, and this answers what the crates do.
set -euo pipefail

cd "$(dirname "$0")/.."

# crate directory -> what its host suite is for.
CRATES=(
	"fs/liberfs"
	"fs/partition"
)

status=0
for crate in "${CRATES[@]}"; do
	echo "host-tests: $crate"
	if ! (cd "$crate" && cargo test --quiet); then
		echo "host-tests: $crate FAILED" >&2
		status=1
	fi
done
exit "$status"
