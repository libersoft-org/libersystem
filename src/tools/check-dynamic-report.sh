#!/usr/bin/env bash
set -euo pipefail

# The provider/consumer report for the dynamic command graph: what each tool imports, who owns each
# import, and what the whole image costs. Three tracked TSVs, one generator, three modes.
#
# THE EXIT CONTRACT, because the caller acts on it:
#
#   0  the requested check matched, or the write completed
#   1  tool, manifest, parser, I/O or other internal failure
#   2  invalid mode or surplus arguments
#   3  the name-only inventory differs from the tracked detailed report and needs a refresh
#   4  a full generated report differs from its tracked report
#
# Only 3 is a nonfatal post-build warning. `build-shared.sh` treats everything else as fatal, which
# is what makes the difference between "the report is out of date" and "the gate is broken" visible
# from outside.

usage() {
	echo "usage: $0 [--check|--check-inventory|--write]" >&2
}

# THE MODE IS DECIDED BEFORE ANY WORK, and that ordering is the point rather than tidiness.
#
# This used to load the manifest (a cargo subprocess), source `lib.sh`, locate five tools, create
# three temporary files and - worst - run a RECURSIVE SELF-TEST that invoked this script's own
# `--check`, generating all three reports and sweeping the whole ELF graph, before it looked at
# which mode had been asked for. So `--check-inventory`, the mode whose entire purpose is to be
# cheap enough to run after every build, paid for a full sweep of 2,352 `llvm-readelf` processes
# first. Measured on this tree: a warm shared-library build that compiled nothing spent 210 of its
# 210 seconds in that stage.
#
# A usage error now performs no manifest export, no temporary file and no ELF read at all.
mode="${1:---check}"
case "$mode" in
--check | --check-inventory | --write) ;;
*)
	usage
	exit 2
	;;
esac
if (($# > 1)); then
	echo "dynamic-report: $# arguments; this takes exactly one mode" >&2
	usage
	exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
report="$root/../docs/DYNAMIC_EXECUTABLES.tsv"
wave_report="$root/../docs/DYNAMIC_WAVES.tsv"
image_report="$root/../docs/DYNAMIC_IMAGE.tsv"
source "$root/../lib.sh"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"

# The shape of the reports, in one place. Every loop, row count and message below is derived from
# these rather than restated - the success line used to claim "15 waves" while the report holds six
# logical waves and 18 target-wave rows, which is what happens when a number is typed twice.
TARGETS=(x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf)
WAVES=(1 2 3 4 5 6)

manifest_tools="$(jq -r '.programs[] | select(.role == "tool" and .linkage == "dynamic" and .stage == "volume") | .name' <<<"$manifest_json" | sort)"
wave_tools="$(printf '%s\n' "${!TOOL_WAVES[@]}" | sort)"
if [[ "$manifest_tools" != "$wave_tools" ]]; then
	echo "dynamic-report: wave inventory differs from the manifest tools" >&2
	diff -u <(printf '%s\n' "$manifest_tools") <(printf '%s\n' "$wave_tools") >&2 || true
	exit 1
fi
tool_count="$(wc -l <<<"$manifest_tools")"

# NAME-ONLY, and it reads three things: the manifest's tool names, the wave table's keys, and the
# x86_64 tool column of the tracked detailed report.
#
# It does not locate `llvm-readelf`, open an artifact, generate a report or run a probe. A tool that
# has been added or removed since the report was written is the one thing this can see, and it is
# the thing a build wants to know about; anything deeper is what `--check` is for.
if [[ "$mode" == --check-inventory ]]; then
	[[ -f "$report" ]] || {
		echo "dynamic-report: no tracked report at $report; run (cd src && just dynamic-report-update)" >&2
		exit 3
	}
	checked_tools="$(awk -F '\t' '$1 ~ /^[0-9]+$/ && $2 == "x86_64-unknown-none" {print $3}' "$report" | sort)" || {
		echo "dynamic-report: could not read the tracked report" >&2
		exit 1
	}
	if [[ "$manifest_tools" != "$checked_tools" ]]; then
		echo "dynamic-report: the tracked report's tool inventory differs from the manifest" >&2
		diff -u <(printf '%s\n' "$checked_tools") <(printf '%s\n' "$manifest_tools") >&2 || true
		exit 3
	fi
	echo "dynamic-report: $tool_count checked tools match the manifest"
	exit 0
fi

command -v cmp >/dev/null
command -v diff >/dev/null
command -v llvm-readelf >/dev/null
command -v sha256sum >/dev/null
command -v stat >/dev/null

source_path() {
	jq -er --arg owner "$1" '.sources[$owner].path' <<<"$manifest_json"
}

declare -A wave_tools_count=()
declare -A wave_object_bytes=()
declare -A wave_pie_bytes=()
declare -A wave_private_bytes=()
declare -A wave_shared_executable_bytes=()
declare -A wave_provider_seen=()
declare -A wave_provider_bytes=()
declare -A wave_provider_shared_bytes=()
declare -A image_tools_count=()
declare -A image_object_bytes=()
declare -A image_pie_bytes=()
declare -A image_private_bytes=()
declare -A image_shared_executable_bytes=()
declare -A image_provider_seen=()
declare -A image_provider_bytes=()
declare -A image_provider_shared_bytes=()
declare -A object_bytes_cache=()
declare -A provider_size_cache=()
declare -A provider_private_cache=()
declare -A provider_shared_cache=()
declare -A provider_exports=()

library_file() {
	local target="$1"
	local provider="$2"
	local destination
	destination="$(jq -er --arg provider "$provider" '.libraries[$provider].destination' <<<"$manifest_json")"
	printf '%s/image/%s/%s\n' "$build_root" "$target" "$destination"
}

canonical_manifest_order() {
	local roots="$1"
	local name depth provider candidate ready
	local max_modules=64
	local max_depth=16
	local -A present=()
	local -A edges=()
	local -A depths=()
	local -A ordered=()
	local -a pending_names=()
	local -a pending_depths=()
	local -a order=()
	for name in $roots; do
		pending_names+=("$name")
		pending_depths+=(0)
	done
	while ((${#pending_names[@]})); do
		name="${pending_names[0]}"
		depth="${pending_depths[0]}"
		pending_names=("${pending_names[@]:1}")
		pending_depths=("${pending_depths[@]:1}")
		if ((depth >= max_depth)); then
			echo "dynamic-report: manifest provider graph exceeds dependency depth $max_depth" >&2
			return 1
		fi
		if [[ -n "${depths[$name]:-}" ]] && ((${depths[$name]} >= depth)); then continue; fi
		depths[$name]="$depth"
		if [[ -z "${present[$name]:-}" ]]; then
			if ((${#present[@]} >= max_modules)); then
				echo "dynamic-report: manifest provider graph exceeds module limit $max_modules" >&2
				return 1
			fi
			edges[$name]="$(jq -er --arg provider "$name" '.libraries[$provider].providers | join(" ")' <<<"$manifest_json")"
			present[$name]=1
		fi
		for provider in ${edges[$name]}; do
			pending_names+=("$provider")
			pending_depths+=($((depth + 1)))
		done
	done
	while ((${#order[@]} < ${#present[@]})); do
		candidate=""
		while IFS= read -r name; do
			if [[ -n "${ordered[$name]:-}" ]]; then continue; fi
			ready=1
			for provider in ${edges[$name]}; do
				if [[ -z "${ordered[$provider]:-}" ]]; then
					ready=0
					break
				fi
			done
			if [[ "$ready" == 1 ]]; then
				candidate="$name"
				break
			fi
		done < <(printf '%s\n' "${!present[@]}" | sort)
		[[ -n "$candidate" ]] || {
			echo "dynamic-report: manifest provider graph contains a cycle" >&2
			return 1
		}
		order+=("$candidate")
		ordered[$candidate]=1
	done
	printf '%s.lslib\n' "${order[@]}"
}

# EVERY `llvm-readelf` READ GOES THROUGH HERE, and the reason is a defect this script had after the
# milestone that was supposed to remove it.
#
# The segment and export readers fed their loops from `done < <(llvm-readelf ...)`. A process
# substitution's exit status is not the enclosing command's under any setting - `pipefail` does not
# reach it and `set -e` never sees it - so a reader that printed one plausible LOAD line and then
# died returned 0 with a metric computed from the part that arrived. Measured: a shim printing one
# `LOAD ... RW` line and exiting 37 produced status 0 and 4096 bytes, which is then published as a
# tracked report row.
#
# So the output is CAPTURED FIRST and the status checked before anything reads it. The caller gets a
# failure, and no partial metric exists to be defaulted, cached or written.
read_elf() {
	local what="$1"
	shift
	local output
	if ! output="$(llvm-readelf "$@" 2>&1)"; then
		echo "dynamic-report: llvm-readelf failed reading $what" >&2
		printf '%s\n' "$output" >&2
		return 1
	fi
	printf '%s\n' "$output"
}

writable_load_bytes() {
	local image="$1"
	local total=0
	local kind offset address physical file_size memory_size flags listing
	listing="$(read_elf "$image" -lW "$image")" || return 1
	while read -r kind offset address physical file_size memory_size flags; do
		[[ "$kind" == LOAD && "$flags" == *W* ]] || continue
		local start=$((address & -4096))
		local end=$(((address + memory_size + 4095) & -4096))
		total=$((total + end - start))
	done <<<"$listing"
	printf '%s\n' "$total"
}

immutable_load_bytes() {
	local image="$1"
	local total=0
	local kind offset address physical file_size memory_size flags listing
	listing="$(read_elf "$image" -lW "$image")" || return 1
	while read -r kind offset address physical file_size memory_size flags; do
		[[ "$kind" == LOAD && "$flags" != *W* ]] || continue
		local start=$((address & -4096))
		local end=$(((address + memory_size + 4095) & -4096))
		total=$((total + end - start))
	done <<<"$listing"
	printf '%s\n' "$total"
}

current_object_bytes() {
	local target="$1"
	local tool="$2"
	local directory="$build_root/cache/$target"
	local reference="$directory/executable-$tool.object"
	local key file expected_hash expected_bytes object prefix actual_hash definitions
	local -a record=()
	if [[ -n "${object_bytes_cache["$target|$tool"]:-}" ]]; then
		printf '%s\n' "${object_bytes_cache["$target|$tool"]}"
		return
	fi
	[[ -f "$reference" ]] || {
		echo "dynamic-report: missing current ET_REL reference for $target $tool" >&2
		return 1
	}
	mapfile -t record <"$reference"
	[[ "${#record[@]}" == 5 && "${record[0]}" == "format=liber-image-object-reference-v1" ]] || {
		echo "dynamic-report: malformed current ET_REL reference for $target $tool" >&2
		return 1
	}
	key="${record[1]#key=}"
	file="${record[2]#file=}"
	expected_hash="${record[3]#sha256=}"
	expected_bytes="${record[4]#bytes=}"
	[[ "${record[1]}" == "key=$key" && "${record[2]}" == "file=$file" && "${record[3]}" == "sha256=$expected_hash" && "${record[4]}" == "bytes=$expected_bytes" && "$key" =~ ^[0-9a-f]{64}$ && "$file" == "object-$tool-$key.o" && "$expected_hash" =~ ^[0-9a-f]{64}$ && "$expected_bytes" =~ ^[0-9]+$ ]] || {
		echo "dynamic-report: invalid current ET_REL fields for $target $tool" >&2
		return 1
	}
	object="$directory/$file"
	prefix="${object%.o}"
	[[ -f "$object" && -f "$prefix.build-key" && -f "$prefix.sha256" && "$(<"$prefix.build-key")" == "$key" && "$(<"$prefix.sha256")" == "$expected_hash" && "$(stat -c %s "$object")" == "$expected_bytes" ]] || {
		echo "dynamic-report: stale current ET_REL reference for $target $tool" >&2
		return 1
	}
	actual_hash="$(sha256sum "$object" | awk '{print $1}')"
	[[ "$actual_hash" == "$expected_hash" ]] || {
		echo "dynamic-report: current ET_REL hash differs for $target $tool" >&2
		return 1
	}
	object_header="$(read_elf "$object" -h "$object")" || return 1
	grep -q 'Type:.*REL' <<<"$object_header" || {
		echo "dynamic-report: current object is not ET_REL for $target $tool" >&2
		return 1
	}
	local symbols
	symbols="$(read_elf "$object" --wide --symbols "$object")" || return 1
	definitions="$(awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' <<<"$symbols" | sort -u)"
	[[ "$definitions" == __user_main ]] || {
		echo "dynamic-report: current ET_REL definitions differ for $target $tool" >&2
		return 1
	}
	object_bytes_cache["$target|$tool"]="$expected_bytes"
	printf '%s\n' "$expected_bytes"
}

provider_metrics() {
	local target="$1"
	local provider="$2"
	local key="$target|$provider"
	local provider_file
	if [[ -z "${provider_size_cache[$key]:-}" ]]; then
		provider_file="$(library_file "$target" "$provider")"
		[[ -f "$provider_file" ]] || {
			echo "dynamic-report: missing $target provider $provider" >&2
			return 1
		}
		local dyn_syms
		dyn_syms="$(read_elf "$provider_file" --dyn-syms -W "$provider_file")" || return 1
		provider_size_cache[$key]="$(stat -c %s "$provider_file")"
		provider_private_cache[$key]="$(writable_load_bytes "$provider_file")"
		provider_shared_cache[$key]="$(immutable_load_bytes "$provider_file")"
		while IFS= read -r symbol; do
			[[ -n "$symbol" ]] || continue
			provider_exports["$target|$symbol"]+="$provider "
		done <<<"$(awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") && ($4 == "NOTYPE" || $4 == "OBJECT" || $4 == "FUNC") && ($6 == "DEFAULT" || $6 == "PROTECTED") && $8 != "" {print $8}' <<<"$dyn_syms" | sort -u)"
	fi
	printf '%s %s %s\n' "${provider_size_cache[$key]}" "${provider_private_cache[$key]}" "${provider_shared_cache[$key]}"
}

resolve_import_owners() {
	local target="$1"
	local tool="$2"
	local imports="$3"
	local transitive="$4"
	local import provider owners count owner
	local -A closure=()
	local -a import_list=()
	while IFS= read -r provider; do closure["${provider%.lslib}"]=1; done <<<"$transitive"
	local result=""
	IFS=',' read -r -a import_list <<<"$imports"
	for import in "${import_list[@]}"; do
		[[ -n "$import" && "$import" != - ]] || continue
		owners="${provider_exports["$target|$import"]:-}"
		count=0
		owner=""
		for provider in $owners; do
			if [[ -n "${closure[$provider]:-}" ]]; then
				count=$((count + 1))
				owner="$provider"
			fi
		done
		[[ "$count" == 1 ]] || {
			echo "dynamic-report: $target $tool import $import has $count owners in its provider closure" >&2
			return 1
		}
		if [[ -n "$result" ]]; then result+=","; fi
		result+="$import=$owner"
		if [[ "$import" =~ liber_channel_impl_|ChannelClient ]]; then
			echo "dynamic-report: $target $tool imports private generated client implementation $import" >&2
			return 1
		fi
		if [[ "$import" =~ ChannelTransport|VecWriter ]]; then
			echo "dynamic-report: $target $tool has a generic transport residual $import=$owner" >&2
			return 1
		fi
	done
	printf '%s\t-\n' "${result:--}"
}

preload_metrics() {
	local target tool provider
	for target in "${TARGETS[@]}"; do
		while IFS= read -r tool; do current_object_bytes "$target" "$tool" >/dev/null; done <<<"$manifest_tools"
		while IFS= read -r provider; do provider_metrics "$target" "$provider" >/dev/null; done < <(jq -r '.libraries[].name' <<<"$manifest_json")
	done
}

join_lines() {
	local joined
	joined="$(sed '/^$/d' | paste -sd, -)"
	printf '%s' "${joined:--}"
}

generate_report() {
	printf 'format=liber-dynamic-executable-report-v4\n'
	printf 'wave\ttarget\ttool\tundefined_imports\timport_owners\tgeneric_residuals\tdeclared_providers\tdt_needed\ttransitive_providers\tobject_bytes\tpie_bytes\tprovider_bytes\tprivate_bytes\tshared_bytes\ttest_command\n'
	local target wave key provider_key image_provider_key tool candidate row providers artifact imports import_owners generic_residuals owner_record actual_needed declared transitive reversed_roots reversed_transitive object_bytes pie_bytes provider_bytes private_bytes shared_bytes provider provider_size provider_private provider_shared
	for target in "${TARGETS[@]}"; do
		for wave in "${WAVES[@]}"; do
			key="$target|$wave"
			for tool in $(for candidate in "${!TOOL_WAVES[@]}"; do if [[ "${TOOL_WAVES[$candidate]}" == "$wave" ]]; then printf '%s\n' "$candidate"; fi; done | sort); do
				row="$(jq -er --arg tool "$tool" '.programs[$tool] | select(.role == "tool" and .linkage == "dynamic" and .stage == "volume") | "dynamic \(.name) \(.owner) \(.stage) \(.providers | join(" "))"' <<<"$manifest_json")"
				providers="$(cut -d' ' -f5- <<<"$row")"
				destination="$(jq -er --arg tool "$tool" '.programs[$tool].destination | sub("\\.lsexe$"; "")' <<<"$manifest_json")"
				artifact="$build_root/image/$target/$destination"
				[[ -f "$artifact" ]] || {
					echo "dynamic-report: missing $target artifact for $tool" >&2
					return 1
				}
				local artifact_dyn artifact_dynamic
				artifact_dyn="$(read_elf "$artifact" --dyn-syms -W "$artifact")" || return 1
				artifact_dynamic="$(read_elf "$artifact" -dW "$artifact")" || return 1
				imports="$(awk '$7 == "UND" && $8 != "" {print $8}' <<<"$artifact_dyn" | sort -u | join_lines)"
				actual_needed="$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' <<<"$artifact_dynamic" | sort -u)"
				declared="$(for provider in $providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
				if [[ "$actual_needed" != "$declared" ]]; then
					echo "dynamic-report: $target $tool DT_NEEDED differs from its manifest" >&2
					return 1
				fi
				transitive="$(canonical_manifest_order "$providers")"
				reversed_roots=""
				for provider in $providers; do reversed_roots="$provider $reversed_roots"; done
				reversed_transitive="$(canonical_manifest_order "$reversed_roots")"
				if [[ "$transitive" != "$reversed_transitive" ]]; then
					echo "dynamic-report: $target $tool provider derivation depends on DT_NEEDED enumeration order" >&2
					diff -u <(printf '%s\n' "$transitive") <(printf '%s\n' "$reversed_transitive") >&2 || true
					return 1
				fi
				owner_record="$(resolve_import_owners "$target" "$tool" "$imports" "$transitive")"
				import_owners="${owner_record%%$'\t'*}"
				generic_residuals="${owner_record#*$'\t'}"
				pie_bytes="$(stat -c %s "$artifact")"
				object_bytes="$(current_object_bytes "$target" "$tool")"
				provider_bytes=0
				private_bytes="$(writable_load_bytes "$artifact")"
				shared_bytes="$(immutable_load_bytes "$artifact")"
				while IFS= read -r provider; do
					provider="${provider%.lslib}"
					read -r provider_size provider_private provider_shared < <(provider_metrics "$target" "$provider")
					provider_bytes=$((provider_bytes + provider_size))
					private_bytes=$((private_bytes + provider_private))
					shared_bytes=$((shared_bytes + provider_shared))
					provider_key="$key|$provider"
					if [[ -z "${wave_provider_seen[$provider_key]:-}" ]]; then
						wave_provider_seen[$provider_key]=1
						wave_provider_bytes[$key]=$((${wave_provider_bytes[$key]:-0} + provider_size))
						wave_provider_shared_bytes[$key]=$((${wave_provider_shared_bytes[$key]:-0} + provider_shared))
					fi
					image_provider_key="$target|$provider"
					if [[ -z "${image_provider_seen[$image_provider_key]:-}" ]]; then
						image_provider_seen[$image_provider_key]=1
						image_provider_bytes[$target]=$((${image_provider_bytes[$target]:-0} + provider_size))
						image_provider_shared_bytes[$target]=$((${image_provider_shared_bytes[$target]:-0} + provider_shared))
					fi
				done <<<"$transitive"
				wave_tools_count[$key]=$((${wave_tools_count[$key]:-0} + 1))
				wave_object_bytes[$key]=$((${wave_object_bytes[$key]:-0} + object_bytes))
				wave_pie_bytes[$key]=$((${wave_pie_bytes[$key]:-0} + pie_bytes))
				wave_private_bytes[$key]=$((${wave_private_bytes[$key]:-0} + private_bytes))
				wave_shared_executable_bytes[$key]=$((${wave_shared_executable_bytes[$key]:-0} + $(immutable_load_bytes "$artifact")))
				image_tools_count[$target]=$((${image_tools_count[$target]:-0} + 1))
				image_object_bytes[$target]=$((${image_object_bytes[$target]:-0} + object_bytes))
				image_pie_bytes[$target]=$((${image_pie_bytes[$target]:-0} + pie_bytes))
				image_private_bytes[$target]=$((${image_private_bytes[$target]:-0} + private_bytes))
				image_shared_executable_bytes[$target]=$((${image_shared_executable_bytes[$target]:-0} + $(immutable_load_bytes "$artifact")))
				printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$wave" "$target" "$tool" "$imports" "$import_owners" "$generic_residuals" "$(join_lines <<<"$declared")" "$(join_lines <<<"$actual_needed")" "$(join_lines <<<"$transitive")" "$object_bytes" "$pie_bytes" "$provider_bytes" "$private_bytes" "$shared_bytes" "./test.sh --tags ${WAVE_TAGS[$wave]}"
			done
		done
	done
}

generate_wave_report() {
	printf 'format=liber-dynamic-wave-report-v2\n'
	printf 'target\twave\ttools\tobject_bytes\tpie_bytes\tunique_provider_bytes\tprivate_bytes\tshared_bytes\ttest_command\n'
	local target wave key shared_bytes
	for target in "${TARGETS[@]}"; do
		for wave in "${WAVES[@]}"; do
			key="$target|$wave"
			# A WAVE WITH NO TOOLS IS ZERO, not an unbound variable.
			#
			# Every one of these was read without a default, and the accumulators are only created
			# when a tool lands in that wave - so a wave whose last tool is removed, or renamed into
			# another wave, made this line die with `wave_shared_executable_bytes[...]: unbound
			# variable` under `set -u`. In this tree all six waves have tools, so it has never
			# fired; a fixture with two waves finds it immediately. The values are identical
			# wherever a key exists, so no tracked report changes.
			shared_bytes=$((${wave_shared_executable_bytes[$key]:-0} + ${wave_provider_shared_bytes[$key]:-0}))
			printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "$wave" "${wave_tools_count[$key]:-0}" "${wave_object_bytes[$key]:-0}" "${wave_pie_bytes[$key]:-0}" "${wave_provider_bytes[$key]:-0}" "${wave_private_bytes[$key]:-0}" "$shared_bytes" "./test.sh --tags ${WAVE_TAGS[$wave]}"
		done
	done
}

generate_image_report() {
	printf 'format=liber-dynamic-image-report-v1\n'
	printf 'target\ttools\tobject_bytes\tpie_bytes\tunique_provider_bytes\tstaged_bytes\tprivate_bytes\tshared_bytes\ttest_command\n'
	local target staged_bytes shared_bytes
	for target in "${TARGETS[@]}"; do
		staged_bytes=$((${image_pie_bytes[$target]:-0} + ${image_provider_bytes[$target]:-0}))
		shared_bytes=$((${image_shared_executable_bytes[$target]:-0} + ${image_provider_shared_bytes[$target]:-0}))
		printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "${image_tools_count[$target]:-0}" "${image_object_bytes[$target]:-0}" "${image_pie_bytes[$target]:-0}" "${image_provider_bytes[$target]:-0}" "$staged_bytes" "${image_private_bytes[$target]:-0}" "$shared_bytes" './check.sh --gate dynamic-report'
	done
}

# --- validation and comparison ------------------------------------------------------------------
#
# Three local validators, one per report. Deliberately not a schema framework: there are three
# formats, they change when this file changes, and a generic validator would be more code than the
# thing it validates.
#
# BOTH SIDES ARE VALIDATED. Comparing a candidate against a tracked file establishes that they are
# equal; it establishes nothing about whether either is a report. Two identically malformed files
# compare equal and would have been reported as a match.

validate_detailed() {
	local file="$1" what="$2"
	local -a keys=()
	local -A seen=()
	awk -F '\t' -v what="$what" '
		NR == 1 { if ($0 != "format=liber-dynamic-executable-report-v4") { printf "dynamic-report: %s has the wrong format header\n", what > "/dev/stderr"; exit 1 } ; next }
		NR == 2 { if ($0 != "wave\ttarget\ttool\tundefined_imports\timport_owners\tgeneric_residuals\tdeclared_providers\tdt_needed\ttransitive_providers\tobject_bytes\tpie_bytes\tprovider_bytes\tprivate_bytes\tshared_bytes\ttest_command") { printf "dynamic-report: %s has the wrong column header\n", what > "/dev/stderr"; exit 1 } ; next }
		{
			if (NF != 15) { printf "dynamic-report: %s row %d has %d columns, expected 15\n", what, NR, NF > "/dev/stderr"; exit 1 }
			if ($1 !~ /^[0-9]+$/) { printf "dynamic-report: %s row %d has a non-numeric wave\n", what, NR > "/dev/stderr"; exit 1 }
			for (column = 10; column <= 14; column++) {
				if ($column !~ /^[0-9]+$/) { printf "dynamic-report: %s row %d column %d is not a byte count\n", what, NR, column > "/dev/stderr"; exit 1 }
			}
			key = $2 "|" $3
			if (key in row) { printf "dynamic-report: %s repeats the key %s\n", what, key > "/dev/stderr"; exit 1 }
			row[key] = 1
		}
	' "$file" || return 1
	# The key set is exactly targets x manifest tools, and every row's wave is the table's wave.
	local target tool wave line
	for target in "${TARGETS[@]}"; do
		while IFS= read -r tool; do keys+=("$target|$tool"); done <<<"$manifest_tools"
	done
	while IFS=$'\t' read -r wave target tool _rest; do
		[[ "$wave" =~ ^[0-9]+$ ]] || continue
		seen["$target|$tool"]=1
		if [[ "${TOOL_WAVES[$tool]:-}" != "$wave" ]]; then
			echo "dynamic-report: $what places $tool in wave $wave; the wave table says ${TOOL_WAVES[$tool]:-none}" >&2
			return 1
		fi
	done <"$file"
	for line in "${keys[@]}"; do
		[[ -n "${seen[$line]:-}" ]] || {
			echo "dynamic-report: $what is missing the row for $line" >&2
			return 1
		}
	done
	if ((${#seen[@]} != ${#keys[@]})); then
		echo "dynamic-report: $what has ${#seen[@]} rows, expected ${#keys[@]}" >&2
		return 1
	fi
	return 0
}

validate_wave() {
	local file="$1" what="$2"
	local -A seen=()
	local target wave rest
	awk -F '\t' -v what="$what" '
		NR == 1 { if ($0 != "format=liber-dynamic-wave-report-v2") { printf "dynamic-report: %s has the wrong format header\n", what > "/dev/stderr"; exit 1 } ; next }
		NR == 2 { if ($0 != "target\twave\ttools\tobject_bytes\tpie_bytes\tunique_provider_bytes\tprivate_bytes\tshared_bytes\ttest_command") { printf "dynamic-report: %s has the wrong column header\n", what > "/dev/stderr"; exit 1 } ; next }
		{
			if (NF != 9) { printf "dynamic-report: %s row %d has %d columns, expected 9\n", what, NR, NF > "/dev/stderr"; exit 1 }
			for (column = 2; column <= 8; column++) {
				if ($column !~ /^[0-9]+$/) { printf "dynamic-report: %s row %d column %d is not a number\n", what, NR, column > "/dev/stderr"; exit 1 }
			}
			key = $1 "|" $2
			if (key in row) { printf "dynamic-report: %s repeats the key %s\n", what, key > "/dev/stderr"; exit 1 }
			row[key] = 1
		}
	' "$file" || return 1
	while IFS=$'\t' read -r target wave rest; do
		[[ "$wave" =~ ^[0-9]+$ ]] || continue
		seen["$target|$wave"]=1
	done <"$file"
	local expected=$((${#TARGETS[@]} * ${#WAVES[@]}))
	for target in "${TARGETS[@]}"; do
		for wave in "${WAVES[@]}"; do
			[[ -n "${seen["$target|$wave"]:-}" ]] || {
				echo "dynamic-report: $what is missing the row for $target wave $wave" >&2
				return 1
			}
		done
	done
	((${#seen[@]} == expected)) || {
		echo "dynamic-report: $what has ${#seen[@]} target/wave rows, expected $expected" >&2
		return 1
	}
	return 0
}

validate_image() {
	local file="$1" what="$2"
	local -A seen=()
	local target rest
	awk -F '\t' -v what="$what" '
		NR == 1 { if ($0 != "format=liber-dynamic-image-report-v1") { printf "dynamic-report: %s has the wrong format header\n", what > "/dev/stderr"; exit 1 } ; next }
		NR == 2 { if ($0 != "target\ttools\tobject_bytes\tpie_bytes\tunique_provider_bytes\tstaged_bytes\tprivate_bytes\tshared_bytes\ttest_command") { printf "dynamic-report: %s has the wrong column header\n", what > "/dev/stderr"; exit 1 } ; next }
		{
			if (NF != 9) { printf "dynamic-report: %s row %d has %d columns, expected 9\n", what, NR, NF > "/dev/stderr"; exit 1 }
			for (column = 2; column <= 8; column++) {
				if ($column !~ /^[0-9]+$/) { printf "dynamic-report: %s row %d column %d is not a number\n", what, NR, column > "/dev/stderr"; exit 1 }
			}
			if ($1 in row) { printf "dynamic-report: %s repeats the row for %s\n", what, $1 > "/dev/stderr"; exit 1 }
			row[$1] = 1
		}
	' "$file" || return 1
	while IFS=$'\t' read -r target rest; do
		[[ "$target" == *-* && "$target" != format=* && "$target" != target ]] || continue
		seen["$target"]=1
	done <"$file"
	for target in "${TARGETS[@]}"; do
		[[ -n "${seen[$target]:-}" ]] || {
			echo "dynamic-report: $what is missing the row for $target" >&2
			return 1
		}
	done
	((${#seen[@]} == ${#TARGETS[@]})) || {
		echo "dynamic-report: $what has ${#seen[@]} target rows, expected ${#TARGETS[@]}" >&2
		return 1
	}
	return 0
}

# One report against its tracked file. Returns 4 - not 1 - so the caller can tell "the report is out
# of date" from "the gate could not run".
compare_one() {
	local candidate="$1" tracked="$2" name="$3" quiet="${4:-loud}"
	if cmp -s "$candidate" "$tracked"; then
		return 0
	fi
	if [[ "$quiet" != quiet ]]; then
		echo "dynamic-report: $name is stale" >&2
		diff -u "$tracked" "$candidate" >&2 || true
	fi
	return 4
}

# --- one generation, then compare -----------------------------------------------------------------

temporary="$(mktemp)"
wave_temporary="$(mktemp)"
image_temporary="$(mktemp)"
probe_dir="$(mktemp -d)"
trap 'rm -f "$temporary" "$wave_temporary" "$image_temporary"; rm -rf "$probe_dir"' EXIT
preload_metrics
generate_report >"$temporary"
generate_wave_report >"$wave_temporary"
generate_image_report >"$image_temporary"

validate_detailed "$temporary" "the generated report" || exit 1
validate_wave "$wave_temporary" "the generated wave report" || exit 1
validate_image "$image_temporary" "the generated image report" || exit 1

if [[ "$mode" == --write ]]; then
	mv "$temporary" "$report"
	mv "$wave_temporary" "$wave_report"
	mv "$image_temporary" "$image_report"
	trap 'rm -rf "$probe_dir"' EXIT
	echo "dynamic-report: wrote $report, $wave_report and $image_report"
	exit 0
fi

for tracked in "$report" "$wave_report" "$image_report"; do
	[[ -f "$tracked" ]] || {
		echo "dynamic-report: no tracked report at $tracked; run (cd src && just dynamic-report-update)" >&2
		exit 3
	}
done
validate_detailed "$report" "$report" || exit 1
validate_wave "$wave_report" "$wave_report" || exit 1
validate_image "$image_report" "$image_report" || exit 1

status=0
compare_one "$temporary" "$report" "$report" || status=$?
[[ "$status" == 0 ]] || exit "$status"
compare_one "$wave_temporary" "$wave_report" "$wave_report" || status=$?
[[ "$status" == 0 ]] || exit "$status"
compare_one "$image_temporary" "$image_report" "$image_report" || status=$?
[[ "$status" == 0 ]] || exit "$status"

# PROVE THE COMPARISON REFUSES, without generating anything a second time.
#
# The whole value of `--check` is that a difference fails, and a `diff` that stopped comparing - a
# changed variable name, a redirection, an `exit 0` - would report a clean tree just as convincingly.
# So the gate hands itself a corrupted report and requires itself to notice.
#
# It used to do that by RE-INVOKING ITSELF with an environment override, which regenerated every
# report and swept the whole ELF graph a second time, and accepted ANY nonzero status as proof -
# including the status of a script that failed to start. And it mutated only the detailed report, so
# the wave and image comparisons were never shown to refuse anything at all.
#
# Three probes now, one per report, against copies of the candidates that were just generated: each
# changes one non-key numeric value while leaving the format valid and the other two files
# byte-identical, and each must produce exactly status 4.
probe_report() {
	local which="$1" line_field="$2"
	local -a files=("$probe_dir/detailed.tsv" "$probe_dir/wave.tsv" "$probe_dir/image.tsv")
	cp "$temporary" "${files[0]}"
	cp "$wave_temporary" "${files[1]}"
	cp "$image_temporary" "${files[2]}"
	local target="${files[$which]}"
	# One numeric column of the last row, incremented. Valid format, different bytes.
	awk -F '\t' -v OFS='\t' -v field="$line_field" 'NR == FNR { last = FNR; next } { if (FNR == last) { $field = $field + 1 } ; print }' "$target" "$target" >"$target.mutated"
	mv "$target.mutated" "$target"
	local expected=("$report" "$wave_report" "$image_report")
	local candidates=("$temporary" "$wave_temporary" "$image_temporary")
	local index probe_status=0
	for index in 0 1 2; do
		local source_file="${candidates[$index]}"
		[[ "$index" != "$which" ]] || source_file="$target"
		local diagnostic
		diagnostic="$(compare_one "$source_file" "${expected[$index]}" "${expected[$index]}" 2>&1)" || probe_status=$?
		if [[ "$index" == "$which" ]]; then
			if [[ "$probe_status" != 4 ]]; then
				echo "dynamic-report: SELF-TEST FAILED - a mutated ${expected[$index]} was accepted with status $probe_status, so this gate is comparing nothing" >&2
				return 1
			fi
			# AND IT SAID WHICH REPORT. A refusal that named the wrong file, or named none, would
			# send a reader to the wrong place - and status alone cannot tell the three reports
			# apart, which is the whole reason there is a probe per report rather than one.
			if [[ "$diagnostic" != *"${expected[$index]} is stale"* ]]; then
				echo "dynamic-report: SELF-TEST FAILED - a mutated ${expected[$index]} was refused without saying which report is stale" >&2
				return 1
			fi
		elif [[ "$probe_status" != 0 ]]; then
			echo "dynamic-report: SELF-TEST FAILED - probing ${expected[$which]} disturbed ${expected[$index]}" >&2
			return 1
		fi
		probe_status=0
	done
	return 0
}

probe_report 0 10 || exit 1
probe_report 1 3 || exit 1
probe_report 2 2 || exit 1

echo "dynamic-report: $tool_count tools x ${#TARGETS[@]} targets, ${#WAVES[@]} waves and ${#TARGETS[@]} whole images match"
