#!/usr/bin/env bash
# Enforce the frozen native reference-host floor, retaining the runner's five scenes and timings.
set -euo pipefail
cd "$(dirname "$0")/.."
# Diagnostic modes must not replace the frozen workload when this is an acceptance gate.
unset SOFT2D_BENCH_PROBE SOFT2D_BENCH_FREEZE
exec cargo run --offline --release --manifest-path tools/soft2d-bench/Cargo.toml -- --check --workers 1
