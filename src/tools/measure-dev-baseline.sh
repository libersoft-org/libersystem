#!/usr/bin/env bash
# Record one development-loop timing sample without mutating source files.
set -euo pipefail

scenario="${1:?usage: measure-dev-baseline.sh <cold|warm|leaf|provider|kernel|loader|topology> [test-tags]}"
tags="${2:-smoke}"
# `kernel`, `loader` and `topology` label the three cold invalidation classes, the ones no
# publication can reach. Like `leaf` and `provider` they name what the operator edited before
# running this; nothing here mutates a source file.
case "$scenario" in
cold | warm | leaf | provider | kernel | loader | topology) ;;
*)
	echo "dev-baseline: unknown scenario '$scenario'" >&2
	exit 2
	;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(cd "$root/.." && pwd)"
baseline_root="$repo_root/.build/measure/dev-baseline"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
sample_dir="$baseline_root/$stamp-$scenario"
events="$sample_dir/events.tsv"
build_log="$sample_dir/shared-build.log"
test_log="$sample_dir/kernel-test.log"
samples="$baseline_root/samples.tsv"
# Bumped whenever a column is added, dropped or redefined.
SCHEMA="dev-baseline-v2"
mkdir -p "$sample_dir"
: >"$events"

run_started_ns="$(date +%s%N)"
set +e
# `./build.sh` FROM THE REPOSITORY ROOT, which is not where `$root` points.
#
# These two lines ran `just shared-libs`, and there has been no such recipe: `just --dry-run
# shared-libs` answers "justfile does not contain recipe". Nothing reported it because nothing runs
# this script on a schedule - it is the baseline measurement, taken by hand. `--rebuild` is what
# `LIBER_IMAGE_REBUILD=1` was: build.sh exports exactly that variable for it. The cold/warm
# distinction is the point of the measurement and is kept: cold discards every cache, warm does not.
if [[ "$scenario" == cold ]]; then
	(cd "$repo_root" && ./build.sh --part libs --rebuild) >"$build_log" 2>&1
else
	(cd "$repo_root" && ./build.sh --part libs) >"$build_log" 2>&1
fi
build_status=$?
set -e
if [[ "$build_status" != 0 ]]; then
	cat "$build_log" >&2
	echo "dev-baseline: shared build failed; logs: $sample_dir" >&2
	exit "$build_status"
fi

set +e
(cd "$root" && LIBER_TIMING_LOG="$events" harness/test-kernel.sh x86_64 "$tags") >"$test_log" 2>&1
test_status=$?
set -e
run_ended_ns="$(date +%s%N)"
if [[ "$test_status" != 0 ]]; then
	cat "$test_log" >&2
	echo "dev-baseline: kernel test failed; logs: $sample_dir" >&2
	exit "$test_status"
fi

first_event() {
	local phase="$1" event="$2"
	awk -F '\t' -v phase="$phase" -v event="$event" '$2 == phase && $3 == event { print $1; exit }' "$events"
}

last_event() {
	local phase="$1" event="$2"
	awk -F '\t' -v phase="$phase" -v event="$event" '$2 == phase && $3 == event { value = $1 } END { print value }' "$events"
}

duration_ms() {
	local start="$1" end="$2"
	if [[ -z "$start" || -z "$end" ]]; then printf '%s' -; else awk -v start="$start" -v end="$end" 'BEGIN { printf "%.3f", (end - start) / 1000000 }'; fi
}

summary="$(grep '^build-shared: summary ' "$build_log" | tail -1)"
field() {
	local name="$1"
	sed -n "s/.*${name}:\([0-9][0-9]*\).*/\1/p" <<<"$summary"
}

source_seconds="$(field source)"
graph_seconds="$(field graph)"
provider_seconds="$(field providers)"
consumer_seconds="$(field consumers)"
report_seconds="$(field reports)"
test_start="$(first_event test_driver start)"
runner_start="$(first_event runner start)"
package_init_start="$(first_event package_init start)"
package_init_end="$(last_event package_init end)"
package_volume_start="$(first_event package_volume start)"
package_volume_end="$(last_event package_volume end)"
image_start="$(first_event image start)"
image_end="$(last_event image end)"
qemu_start="$(first_event qemu start)"
kernel_start="$(first_event kernel start)"
scenario_start="$(first_event scenario start)"
scenario_end="$(last_event scenario end)"
qemu_end="$(last_event qemu end)"
package_init_ms="$(duration_ms "$package_init_start" "$package_init_end")"
package_volume_ms="$(duration_ms "$package_volume_start" "$package_volume_end")"
cargo_total_ms="$(duration_ms "$test_start" "$runner_start")"
kernel_compile_link_ms="$(awk -v total="$cargo_total_ms" -v init="$package_init_ms" -v volume="$package_volume_ms" 'BEGIN { if (init == "-") init = 0; if (volume == "-") volume = 0; printf "%.3f", total - init - volume }')"
image_ms="$(duration_ms "$image_start" "$image_end")"
qemu_startup_ms="$(duration_ms "$qemu_start" "$kernel_start")"
guest_boot_ms="$(duration_ms "$kernel_start" "$scenario_start")"
scenario_ms="$(duration_ms "$scenario_start" "$scenario_end")"
shutdown_ms="$(duration_ms "$scenario_end" "$qemu_end")"
total_ms="$(duration_ms "$run_started_ns" "$run_ended_ns")"
output_lines="$(($(wc -l <"$build_log") + $(wc -l <"$test_log")))"
output_bytes="$(($(wc -c <"$build_log") + $(wc -c <"$test_log")))"

header=$'schema\ttimestamp\tscenario\ttags\tsource_s\tgraph_s\tprovider_link_audit_stage_s\tconsumer_link_audit_stage_s\treport_s\tcargo_total_ms\tinit_package_ms\tvolume_package_ms\timage_assembly_ms\tqemu_startup_ms\tguest_boot_ms\tscenario_ms\tshutdown_ms\ttotal_ms\toutput_lines\toutput_bytes\tsample_dir'
# What a row means is the set of phases it measured, and that set changes as the loop changes.
# A column added, dropped or redefined turns older rows into different measurements wearing the
# same names - which is how a regression gets argued away against a number that never measured
# the same thing. So a row carries the schema it was recorded under, and appending to a file
# written under a different one is refused rather than reconciled: the operator moves the old
# file aside, and both remain readable as what they actually are.
if [[ -f "$samples" ]] && [[ "$(head -n 1 "$samples")" != "$header" ]]; then
	echo "dev-baseline: $samples was recorded under a different schema, so its rows measured different phases" >&2
	echo "dev-baseline: move it aside (for example to ${samples%.tsv}-previous.tsv) and rerun; do not merge the two" >&2
	exit 1
fi
if [[ ! -f "$samples" ]]; then printf '%s\n' "$header" >"$samples"; fi
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
	"$SCHEMA" "$stamp" "$scenario" "$tags" "${source_seconds:--}" "${graph_seconds:--}" "${provider_seconds:--}" "${consumer_seconds:--}" "${report_seconds:--}" \
	"$cargo_total_ms" "$package_init_ms" "$package_volume_ms" "$image_ms" "$qemu_startup_ms" "$guest_boot_ms" "$scenario_ms" "$shutdown_ms" "$total_ms" "$output_lines" "$output_bytes" "$sample_dir" >>"$samples"

sample_header=$'scenario\ttags\tsource_s\tgraph_s\tprovider_link_audit_stage_s\tconsumer_link_audit_stage_s\treport_s\tkernel_compile_link_ms\tinit_package_ms\tvolume_package_ms\timage_assembly_ms\tqemu_startup_ms\tguest_boot_ms\tscenario_ms\tshutdown_ms\ttotal_ms\toutput_lines\toutput_bytes'
printf '%s\n' "$sample_header" >"$sample_dir/summary.tsv"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
	"$scenario" "$tags" "${source_seconds:--}" "${graph_seconds:--}" "${provider_seconds:--}" "${consumer_seconds:--}" "${report_seconds:--}" \
	"$kernel_compile_link_ms" "$package_init_ms" "$package_volume_ms" "$image_ms" "$qemu_startup_ms" "$guest_boot_ms" "$scenario_ms" "$shutdown_ms" "$total_ms" "$output_lines" "$output_bytes" >>"$sample_dir/summary.tsv"

echo "dev-baseline: scenario=$scenario tags=$tags total=${total_ms}ms output=${output_lines}lines/${output_bytes}bytes"
echo "dev-baseline: shared source=${source_seconds}s graph=${graph_seconds}s providers=${provider_seconds}s consumers=${consumer_seconds}s reports=${report_seconds}s"
echo "dev-baseline: kernel=${kernel_compile_link_ms}ms init-pkg=${package_init_ms}ms volume-pkg=${package_volume_ms}ms image=${image_ms}ms qemu-start=${qemu_startup_ms}ms guest-boot=${guest_boot_ms}ms scenario=${scenario_ms}ms shutdown=${shutdown_ms}ms"
echo "dev-baseline: sample recorded in $sample_dir"
