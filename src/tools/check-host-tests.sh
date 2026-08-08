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
	# The audio leaves. Their suites existed and nothing ran them either - the decoders were pinned
	# down by tests no gate invoked, which is the same suite-in-name-only this script was written
	# for. The encoders beside them are the reason it was noticed.
	"user/libs/audio/pcm"
	"user/libs/audio/adpcm"
	"user/libs/audio/wav"
	"user/libs/audio/aiff"
	"user/libs/audio/flac"
	"user/libs/audio/wavpack"
	"user/libs/audio/audioconv"
)

status=0
for crate in "${CRATES[@]}"; do
	echo "host-tests: $crate"
	# Run from here rather than from inside the crate. `src/user/.cargo/config.toml` names a
	# bare-metal target and builds `core` from source, which is right for the volume and fatal for a
	# host test: the harness needs `std` and `test`, and a second `core` collides with the one `std`
	# already carries. Cargo picks its config up from the working directory, so staying out of that
	# subtree is what selects the host.
	if ! cargo test --quiet --manifest-path "$crate/Cargo.toml"; then
		echo "host-tests: $crate FAILED" >&2
		status=1
	fi
done
exit "$status"
