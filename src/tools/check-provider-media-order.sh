#!/usr/bin/env bash
# Exercise the real boot volume reads with FAT and UDF moved to the opposite bus positions.
set -euo pipefail
# THIS IS A NAMED GATE, AND IT SAYS SO BEFORE INVOKING ANYTHING. The run mode is the one carrier of
# which matrix row a boot is on, set by the outermost entry point that knows and left alone by the
# runners it invokes - so a gate's test-kernel phase runs under `gate` and not on the `test` row.
export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"
cd "$(dirname "$0")/../.."
MEDIA_ORDER=swapped ./test.sh --arch x86_64 --tags boot
