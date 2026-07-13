#!/usr/bin/env bash
# Run the complete test suite on all architectures without overlapping TCG guests.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/libersystem-test-all.XXXXXX")"
trap 'rm -rf "$LOG_DIR"' EXIT

failed=0
for arch in x86_64 aarch64 riscv64; do
	if "$ROOT/boot/test-kernel.sh" "$arch" >"$LOG_DIR/$arch.log" 2>&1; then
		status=0
	else
		status=$?
		failed=1
	fi
	echo "===== $arch (exit $status) ====="
	cat "$LOG_DIR/$arch.log"
done
exit "$failed"
