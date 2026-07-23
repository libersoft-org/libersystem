#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/user/services/manifest.txt"
report="$root/../docs/DYNAMIC_EXECUTABLES.tsv"
wave_report="$root/../docs/DYNAMIC_WAVES.tsv"
image_report="$root/../docs/DYNAMIC_IMAGE.tsv"
mode="${1:---check}"

case "$mode" in
--check | --write) ;;
*)
	echo "usage: $0 [--check|--write]" >&2
	exit 2
	;;
esac

command -v cmp >/dev/null
command -v diff >/dev/null
command -v llvm-readelf >/dev/null
command -v sha256sum >/dev/null
command -v stat >/dev/null

source_path() {
	awk -v owner="$1" '$1 == "source" && $2 == owner {print $3; count++} END {if (count != 1) exit 1}' "$manifest"
}

declare -A waves=()
declare -A tests=()
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
for tool in echo uname uptime dmesg free lscpu lsmem lsirq lspci ptyecho readln script; do waves[$tool]=1; done
for tool in cat write rm ls du mkdir rmdir snap volume lsvol lsblk; do waves[$tool]=2; done
for tool in date log config set lsdev lsusb lssvc usage ps run perm stop beep; do waves[$tool]=3; done
for tool in ping ip nslookup tcp nc arp httpd ss; do waves[$tool]=4; done
for tool in imgview imgconv play graphics_probe; do waves[$tool]=5; done
tests[1]='just test-tags service,process,storage'
tests[2]='just test-tags service,process,storage'
tests[3]='just test-tags service,process,storage'
tests[4]='just test-tags service,process'
tests[5]='just test-tags image,audio,service,process,storage'

manifest_tools="$(awk '$1 == "dynamic" && $3 == "tools" && $4 == "volume" {print $2}' "$manifest" | sort)"
wave_tools="$(printf '%s\n' "${!waves[@]}" | sort)"
if [[ "$manifest_tools" != "$wave_tools" ]]; then
	echo "dynamic-report: wave inventory differs from the manifest tools" >&2
	diff -u <(printf '%s\n' "$manifest_tools") <(printf '%s\n' "$wave_tools") >&2 || true
	exit 1
fi

library_file() {
	local target="$1"
	local provider="$2"
	local crate
	crate="$(awk -v provider="$provider" '$1 == "library" && $2 == provider {print $3; count++} END {if (count != 1) exit 1}' "$manifest")"
	printf '%s/%s/shared/%s/%s.lslib\n' "$root" "$(source_path "$crate")" "$target" "$provider"
}

canonical_manifest_order() {
	local roots="$1"
	local name provider candidate ready
	local -A present=()
	local -A edges=()
	local -A ordered=()
	local -a pending=($roots)
	local -a order=()
	while ((${#pending[@]})); do
		name="${pending[0]}"
		pending=("${pending[@]:1}")
		if [[ -n "${present[$name]:-}" ]]; then continue; fi
		edges[$name]="$(awk -v provider="$name" '$1 == "library" && $2 == provider {for (i = 6; i <= NF; i++) {if (i > 6) printf " "; printf "%s", $i} found++} END {if (found != 1) exit 1}' "$manifest")"
		present[$name]=1
		for provider in ${edges[$name]}; do pending+=("$provider"); done
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
	local directory="$root/boot/.build/image-artifacts-$target"
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
	llvm-readelf -h "$object" | grep -q 'Type:.*REL' || {
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
		while IFS= read -r provider; do provider_metrics "$target" "$provider" >/dev/null; done < <(awk '$1 == "library" {print $2}' "$manifest")
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
	local target wave key provider_key image_provider_key tool candidate row providers artifact order imports import_owners generic_residuals owner_record actual_needed declared transitive expected_transitive object_bytes pie_bytes provider_bytes private_bytes shared_bytes provider provider_size provider_private provider_shared
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
		for wave in 1 2 3 4 5; do
			key="$target|$wave"
			for tool in $(for candidate in "${!waves[@]}"; do if [[ "${waves[$candidate]}" == "$wave" ]]; then printf '%s\n' "$candidate"; fi; done | sort); do
				row="$(awk -v tool="$tool" '$1 == "dynamic" && $2 == tool && $3 == "tools" && $4 == "volume" {print; count++} END {if (count != 1) exit 1}' "$manifest")"
				providers="$(cut -d' ' -f5- <<<"$(tr -s ' ' <<<"$row")")"
				artifact="$root/$(source_path tools)/shared/$target/$tool"
				order="$artifact.order"
				[[ -f "$artifact" && -f "$order" ]] || {
					echo "dynamic-report: missing $target artifact or order for $tool" >&2
					return 1
				}
				imports="$(llvm-readelf --dyn-syms -W "$artifact" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u | join_lines)"
				actual_needed="$(llvm-readelf -dW "$artifact" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
				declared="$(for provider in $providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
				if [[ "$actual_needed" != "$declared" ]]; then
					echo "dynamic-report: $target $tool DT_NEEDED differs from its manifest" >&2
					return 1
				fi
				transitive="$(sed '/^$/d' "$order")"
				expected_transitive="$(canonical_manifest_order "$providers")"
				if [[ "$transitive" != "$expected_transitive" ]]; then
					echo "dynamic-report: $target $tool provider order differs from the manifest graph" >&2
					diff -u <(printf '%s\n' "$expected_transitive") <(printf '%s\n' "$transitive") >&2 || true
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
				printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$wave" "$target" "$tool" "$imports" "$import_owners" "$generic_residuals" "$(join_lines <<<"$declared")" "$(join_lines <<<"$actual_needed")" "$(join_lines <<<"$transitive")" "$object_bytes" "$pie_bytes" "$provider_bytes" "$private_bytes" "$shared_bytes" "${tests[$wave]}"
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
			printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "$wave" "${wave_tools_count[$key]}" "${wave_object_bytes[$key]}" "${wave_pie_bytes[$key]}" "${wave_provider_bytes[$key]}" "${wave_private_bytes[$key]}" "$shared_bytes" "${tests[$wave]}"
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
		printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$target" "${image_tools_count[$target]}" "${image_object_bytes[$target]}" "${image_pie_bytes[$target]}" "${image_provider_bytes[$target]}" "$staged_bytes" "${image_private_bytes[$target]}" "$shared_bytes" 'just dynamic-report-check'
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
if [[ "$(wc -l <"$temporary")" != 146 ]]; then
	echo "dynamic-report: expected format, header and 144 target/tool rows" >&2
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
	echo "dynamic-report: 48 tools x 3 targets, 15 waves and three whole images match"
fi
