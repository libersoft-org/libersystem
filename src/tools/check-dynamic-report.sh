#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"
report="$root/../docs/DYNAMIC_EXECUTABLES.tsv"
wave_report="$root/../docs/DYNAMIC_WAVES.tsv"
image_report="$root/../docs/DYNAMIC_IMAGE.tsv"
source "$root/../lib.sh"
# Prove the comparison REFUSES before trusting it to approve.
#
# `--check` regenerates the report and compares it against the stored TSVs, and its whole value is
# that a difference fails. A `diff` that stopped comparing - a changed variable name, a redirection,
# an `exit 0` - would report a clean tree just as convincingly, and nothing about a currently-valid
# tree can tell the two apart. So the gate first corrupts a copy of what it compares against and
# requires itself to notice.
self_test() {
	local scratch original
	scratch="$(mktemp -d)"
	original="$(dirname "$0")/../../docs/DYNAMIC_EXECUTABLES.tsv"
	[[ -f "$original" ]] || return 0
	cp "$original" "$scratch/backup"
	# One byte of one row, in the last column, which is the command the report publishes.
	sed -i '2s/$/ MUTATED/' "$original"
	if DYNAMIC_REPORT_SELF_TEST=1 "$0" --check >/dev/null 2>&1; then
		cp "$scratch/backup" "$original"
		rm -rf "$scratch"
		echo "dynamic-report: SELF-TEST FAILED - a mutated report was accepted, so this gate is comparing nothing" >&2
		return 1
	fi
	cp "$scratch/backup" "$original"
	rm -rf "$scratch"
}

if [[ "${DYNAMIC_REPORT_SELF_TEST:-}" != "1" ]]; then
	self_test || exit 1
fi

mode="${1:---check}"

case "$mode" in
--check | --check-inventory | --write) ;;
*)
	echo "usage: $0 [--check|--check-inventory|--write]" >&2
	exit 2
	;;
esac

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

manifest_tools="$(jq -r '.programs[] | select(.role == "tool" and .linkage == "dynamic" and .stage == "volume") | .name' <<<"$manifest_json" | sort)"
wave_tools="$(printf '%s\n' "${!TOOL_WAVES[@]}" | sort)"
if [[ "$manifest_tools" != "$wave_tools" ]]; then
	echo "dynamic-report: wave inventory differs from the manifest tools" >&2
	diff -u <(printf '%s\n' "$manifest_tools") <(printf '%s\n' "$wave_tools") >&2 || true
	exit 1
fi
if [[ "$mode" == --check-inventory ]]; then
	[[ -f "$report" ]] || {
		echo "dynamic-report: missing checked report" >&2
		exit 1
	}
	checked_tools="$(awk -F '\t' '$1 ~ /^[0-9]+$/ && $2 == "x86_64-unknown-none" {print $3}' "$report" | sort)"
	if [[ "$manifest_tools" != "$checked_tools" ]]; then
		echo "dynamic-report: checked tool inventory differs from the manifest" >&2
		diff -u <(printf '%s\n' "$checked_tools") <(printf '%s\n' "$manifest_tools") >&2 || true
		exit 1
	fi
	checked_tool_count="$(wc -l <<<"$manifest_tools")"
	echo "dynamic-report: $checked_tool_count checked tools match the manifest"
	exit 0
fi

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

writable_load_bytes() {
	local image="$1"
	local total=0
	local kind offset address physical file_size memory_size flags
	while read -r kind offset address physical file_size memory_size flags; do
		[[ "$kind" == LOAD && "$flags" == *W* ]] || continue
		local start=$((address & -4096))
		local end=$(((address + memory_size + 4095) & -4096))
		total=$((total + end - start))
	done < <(llvm-readelf -lW "$image")
	printf '%s\n' "$total"
}

immutable_load_bytes() {
	local image="$1"
	local total=0
	local kind offset address physical file_size memory_size flags
	while read -r kind offset address physical file_size memory_size flags; do
		[[ "$kind" == LOAD && "$flags" != *W* ]] || continue
		local start=$((address & -4096))
		local end=$(((address + memory_size + 4095) & -4096))
		total=$((total + end - start))
	done < <(llvm-readelf -lW "$image")
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
	object_header="$(llvm-readelf -h "$object")" || object_header=""
	grep -q 'Type:.*REL' <<<"$object_header" || {
		echo "dynamic-report: current object is not ET_REL for $target $tool" >&2
		return 1
	}
	definitions="$(llvm-readelf --wide --symbols "$object" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
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
		provider_size_cache[$key]="$(stat -c %s "$provider_file")"
		provider_private_cache[$key]="$(writable_load_bytes "$provider_file")"
		provider_shared_cache[$key]="$(immutable_load_bytes "$provider_file")"
		while IFS= read -r symbol; do
			[[ -n "$symbol" ]] || continue
			provider_exports["$target|$symbol"]+="$provider "
		done < <(llvm-readelf --dyn-syms -W "$provider_file" | awk '$7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") && ($4 == "NOTYPE" || $4 == "OBJECT" || $4 == "FUNC") && ($6 == "DEFAULT" || $6 == "PROTECTED") && $8 != "" {print $8}' | sort -u)
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
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
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
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
		for wave in 1 2 3 4 5; do
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
				imports="$(llvm-readelf --dyn-syms -W "$artifact" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u | join_lines)"
				actual_needed="$(llvm-readelf -dW "$artifact" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
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
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
		for wave in 1 2 3 4 5; do
			key="$target|$wave"
			shared_bytes=$((${wave_shared_executable_bytes[$key]} + ${wave_provider_shared_bytes[$key]}))
			printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "$wave" "${wave_tools_count[$key]}" "${wave_object_bytes[$key]}" "${wave_pie_bytes[$key]}" "${wave_provider_bytes[$key]}" "${wave_private_bytes[$key]}" "$shared_bytes" "./test.sh --tags ${WAVE_TAGS[$wave]}"
		done
	done
}

generate_image_report() {
	printf 'format=liber-dynamic-image-report-v1\n'
	printf 'target\ttools\tobject_bytes\tpie_bytes\tunique_provider_bytes\tstaged_bytes\tprivate_bytes\tshared_bytes\ttest_command\n'
	local target staged_bytes shared_bytes
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
		staged_bytes=$((${image_pie_bytes[$target]} + ${image_provider_bytes[$target]}))
		shared_bytes=$((${image_shared_executable_bytes[$target]} + ${image_provider_shared_bytes[$target]}))
		printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "${image_tools_count[$target]}" "${image_object_bytes[$target]}" "${image_pie_bytes[$target]}" "${image_provider_bytes[$target]}" "$staged_bytes" "${image_private_bytes[$target]}" "$shared_bytes" './check.sh --gate dynamic-report'
	done
}

temporary="$(mktemp)"
wave_temporary="$(mktemp)"
image_temporary="$(mktemp)"
trap 'rm -f "$temporary" "$wave_temporary" "$image_temporary"' EXIT
preload_metrics
generate_report >"$temporary"
generate_wave_report >"$wave_temporary"
generate_image_report >"$image_temporary"
tool_count="$(wc -l <<<"$manifest_tools")"
target_count=3
expected_report_lines=$((2 + tool_count * target_count))
if [[ "$(wc -l <"$temporary")" != "$expected_report_lines" ]]; then
	echo "dynamic-report: expected format, header and $((tool_count * target_count)) target/tool rows" >&2
	exit 1
fi
if [[ "$(wc -l <"$wave_temporary")" != 17 ]]; then
	echo "dynamic-report: expected format, header and 15 target/wave rows" >&2
	exit 1
fi
if [[ "$(wc -l <"$image_temporary")" != 5 ]]; then
	echo "dynamic-report: expected format, header and three target image rows" >&2
	exit 1
fi

if [[ "$mode" == --write ]]; then
	mv "$temporary" "$report"
	mv "$wave_temporary" "$wave_report"
	mv "$image_temporary" "$image_report"
	trap - EXIT
	echo "dynamic-report: wrote $report, $wave_report and $image_report"
else
	[[ -f "$report" && -f "$wave_report" && -f "$image_report" ]] || {
		echo "dynamic-report: missing checked report; run $0 --write" >&2
		exit 1
	}
	if ! cmp -s "$temporary" "$report"; then
		echo "dynamic-report: $report is stale" >&2
		diff -u "$report" "$temporary" >&2 || true
		exit 1
	fi
	if ! cmp -s "$wave_temporary" "$wave_report"; then
		echo "dynamic-report: $wave_report is stale" >&2
		diff -u "$wave_report" "$wave_temporary" >&2 || true
		exit 1
	fi
	if ! cmp -s "$image_temporary" "$image_report"; then
		echo "dynamic-report: $image_report is stale" >&2
		diff -u "$image_report" "$image_temporary" >&2 || true
		exit 1
	fi
	echo "dynamic-report: $tool_count tools x $target_count targets, 15 waves and three whole images match"
fi
