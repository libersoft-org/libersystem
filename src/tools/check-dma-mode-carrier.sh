#!/usr/bin/env bash
# The DMA-mode carrier: one frozen record, three inputs, a producer and a consumer that agree.
#
# WHAT THIS GATE IS. The harness writes the boot's DMA mode as eight bytes - into a `fw_cfg` file
# on x86_64, an ESP file on the UEFI ports, a device-tree property on a direct boot - and the loader
# and the kernel read them back through one shared codec. A frozen record that only one side has
# ever written is a record with one implementation, so this gate runs the PRODUCER, in the shell it
# is invoked from, and requires the CONSUMER's host tests to parse what it wrote: the same eight
# bytes on every carrier, found by `compatible` rather than by node name, and refused in the shapes
# that distinguish a frozen identity from a convention - the wrong `compatible`, the wrong length,
# and a provenance byte that claims `signed`.
#
# HOST WORK, MILLISECONDS. The carrier's boot-time halves are the DMA-mode boot gates; this is the
# format, which needs no guest.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../.."

fail() {
	echo "dma-mode-carrier: $*" >&2
	exit 1
}

command -v python3 >/dev/null || fail "python3 is not installed, and the harness producer is written in it"
producer="src/harness/dma-mode-record.py"
[[ -x "$producer" ]] || fail "no producer at $producer"

# 1. THE FROZEN BYTES, from the producer, compared with the literal the codec's tests carry. The
#    codec's own test asserts the same literal against its encoder, so the two implementations meet
#    at a byte string neither of them derives from the other.
enforcing="$(python3 "$producer" record enforcing-required | xxd -p)"
degraded="$(python3 "$producer" record no-iommu | xxd -p)"
[[ "$enforcing" == "4c53444d01010200" ]] || fail "the producer's enforcing-required record is $enforcing, not LSDM 01 01 02 00"
[[ "$degraded" == "4c53444d01020200" ]] || fail "the producer's no-iommu record is $degraded, not LSDM 01 02 02 00"
echo "dma-mode-carrier: the producer writes the frozen eight bytes for both modes"

# 2. THE PRODUCER REFUSES A MODE THIS FORMAT DOES NOT DEFINE - there is no encoding for absence, and
#    no third mode to fall into.
if python3 "$producer" record none >/dev/null 2>&1; then
	fail "the producer accepted a mode named 'none' - the record has no encoding for absence"
fi
if python3 "$producer" record enforcing >/dev/null 2>&1; then
	fail "the producer accepted an abbreviated mode name - the two names are the format's"
fi
echo "dma-mode-carrier: the producer refuses a mode the format does not define"

# 3. THE CONSUMERS. The device-tree reader's tests run the producer on every fixture tree in both
#    modes and parse the node back, and drive the negatives; the boot protocol's tests hold the codec
#    to the same literal and to every malformed shape. Both must pass, by name, so a test that
#    stopped running here is a gate that stopped testing.
run_named() {
	local crate="$1" pattern="$2" out
	out="$(cd "$crate" && cargo test --quiet "$pattern" 2>&1)" || {
		echo "$out" | tail -30 >&2
		fail "the $crate tests matching '$pattern' failed"
	}
	local passed
	# `sed -n 1p` rather than `head -1`: a reader that stops early closes the pipe on the producer,
	# and under `pipefail` a match then reads as a failed pipeline.
	passed="$(grep -o 'test result: ok\. [0-9]* passed' <<<"$out" | sed -n '1p' | grep -o '[0-9]* passed' | grep -o '[0-9]*' || true)"
	[[ -n "$passed" && "$passed" -gt 0 ]] || fail "the $crate tests matching '$pattern' ran nothing - the consumer half of this gate is missing"
	echo "dma-mode-carrier:   $crate: $passed test(s) matching '$pattern' passed"
}
run_named src/fdt "the_harness_record_reads_back_from_every_fixture_tree_in_both_modes"
run_named src/fdt "the_record_is_found_by_compatible_and_not_by_the_node_name"
run_named src/fdt "a_record_of_the_wrong_length_or_provenance_comes_back_as_it_stands_for_the_codec_to_refuse"
run_named src/fdt "the_fw_cfg_and_device_tree_carriers_share_one_record"
run_named src/boot/protocol "dma_mode::tests"

# 4. AND THE CONSUMER'S RULE ON THE PRODUCER'S OWN TREES, mirrored in the producer's `dtb-check`:
#    found by `compatible` under another node name, not found under the right name with the wrong
#    `compatible`, and reported at its real length when short.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
python3 "$producer" dtb src/fdt/tests/qemu-virt-aarch64.dtb "$work/other.dtb" no-iommu --shape other-node-name
[[ "$(python3 "$producer" dtb-check "$work/other.dtb" | xxd -p)" == "4c53444d01020200" ]] || fail "the record under another node name was not found by compatible"
python3 "$producer" dtb src/fdt/tests/qemu-virt-aarch64.dtb "$work/wrong.dtb" no-iommu --shape wrong-compatible
if python3 "$producer" dtb-check "$work/wrong.dtb" >/dev/null 2>&1; then
	fail "a node with the right name and the wrong compatible was read as the record"
fi
python3 "$producer" dtb src/fdt/tests/qemu-virt-riscv64.dtb "$work/short.dtb" no-iommu --shape short
if python3 "$producer" dtb-check "$work/short.dtb" >/dev/null 2>&1; then
	fail "a seven-byte property was accepted as the record"
fi
echo "dma-mode-carrier: the record is found by compatible, refused by length, and never by node name alone"

# 5. EVERY PRODUCER THE MATRIX NAMES WRITES A VALUE. The runner is the producer on every harness row;
#    it refuses an unset run mode, decides the value from the row, and every entry point sets the
#    mode only when it is unset. Read off the sources, because these are the rules that keep the rows
#    disjoint.
grep -q 'LIBER_RUN_MODE:-test' test.sh || fail "test.sh does not set LIBER_RUN_MODE=test when unset"
grep -q 'LIBER_RUN_MODE:-public' run.sh || fail "run.sh does not set LIBER_RUN_MODE=public when unset"
grep -q 'LIBER_RUN_MODE:-gate' check.sh || fail "check.sh does not set LIBER_RUN_MODE=gate when unset"
grep -q "LIBER_RUN_MODE='development'" src/harness/lab.py || fail "the lab does not boot the development instance under LIBER_RUN_MODE=development"
grep -q 'LIBER_RUN_MODE is unset' src/harness/qemu-run.sh || fail "the runner does not refuse an unset run mode"
for gate in src/tools/check-*.sh; do
	# This script names the runners in the pattern below and boots nothing.
	[[ "$gate" == src/tools/check-dma-mode-carrier.sh ]] && continue
	# A gate that INVOKES a runner - as a command, at the start of a line, after `env ...` or after
	# the verdict tool's `--` - must state its mode. A gate that only mentions one in a message it
	# prints, or reads the logs a run left behind, boots nothing and is not asked to.
	if grep -vE '^\s*#' "$gate" | grep -vE '^\s*(echo|fail|die|note|printf|#)' | grep -qE '(^\s*|\s(env\s[^|]*\s)?|-- )(\./test\.sh|\./run\.sh|(src/)?harness/qemu-run\.sh)(\s|$)'; then
		grep -q '^export LIBER_RUN_MODE="${LIBER_RUN_MODE:-gate}"' "$gate" || fail "$gate boots a guest and does not set LIBER_RUN_MODE=gate"
	fi
done
echo "dma-mode-carrier: every entry point sets the run mode only when unset, every booting gate says gate, and the runner refuses an unset mode"

echo "dma-mode-carrier: one record, three carriers, one codec - the producer and the consumer agree on every byte"
