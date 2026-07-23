#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/user/services/manifest.txt"
report="$root/../docs/DYNAMIC_EXECUTABLES.tsv"
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
command -v stat >/dev/null

declare -A waves=()
declare -A tests=()
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
	case "$crate" in
	proto | wire | wasm | term) printf '%s/%s/shared/%s/%s.lslib\n' "$root" "$crate" "$target" "$provider" ;;
	*) printf '%s/user/%s/shared/%s/%s.lslib\n' "$root" "$crate" "$target" "$provider" ;;
	esac
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

join_lines() {
	local joined
	joined="$(sed '/^$/d' | paste -sd, -)"
	printf '%s' "${joined:--}"
}

generate_report() {
	printf 'format=liber-dynamic-executable-report-v1\n'
	printf 'wave\ttarget\ttool\tundefined_imports\tdeclared_providers\tdt_needed\ttransitive_providers\tpie_bytes\tprivate_bytes\ttest_command\n'
	local target wave tool candidate row providers artifact order imports actual_needed declared transitive expected_transitive pie_bytes private_bytes provider provider_file
	for target in x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf; do
		for wave in 1 2 3 4 5; do
			for tool in $(for candidate in "${!waves[@]}"; do if [[ "${waves[$candidate]}" == "$wave" ]]; then printf '%s\n' "$candidate"; fi; done | sort); do
				row="$(awk -v tool="$tool" '$1 == "dynamic" && $2 == tool && $3 == "tools" && $4 == "volume" {print; count++} END {if (count != 1) exit 1}' "$manifest")"
				providers="$(cut -d' ' -f5- <<<"$(tr -s ' ' <<<"$row")")"
				artifact="$root/user/tools/shared/$target/$tool"
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
				pie_bytes="$(stat -c %s "$artifact")"
				private_bytes="$(writable_load_bytes "$artifact")"
				while IFS= read -r provider; do
					provider="${provider%.lslib}"
					provider_file="$(library_file "$target" "$provider")"
					[[ -f "$provider_file" ]] || {
						echo "dynamic-report: missing $target transitive provider $provider for $tool" >&2
						return 1
					}
					private_bytes=$((private_bytes + $(writable_load_bytes "$provider_file")))
				done <<<"$transitive"
				printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$wave" "$target" "$tool" "$imports" "$(join_lines <<<"$declared")" "$(join_lines <<<"$actual_needed")" "$(join_lines <<<"$transitive")" "$pie_bytes" "$private_bytes" "${tests[$wave]}"
			done
		done
	done
}

temporary="$(mktemp)"
trap 'rm -f "$temporary"' EXIT
generate_report >"$temporary"
if [[ "$(wc -l <"$temporary")" != 146 ]]; then
	echo "dynamic-report: expected format, header and 144 target/tool rows" >&2
	exit 1
fi

if [[ "$mode" == --write ]]; then
	mv "$temporary" "$report"
	trap - EXIT
	echo "dynamic-report: wrote $report"
else
	[[ -f "$report" ]] || {
		echo "dynamic-report: missing $report; run $0 --write" >&2
		exit 1
	}
	if ! cmp -s "$temporary" "$report"; then
		echo "dynamic-report: $report is stale" >&2
		diff -u "$report" "$temporary" >&2 || true
		exit 1
	fi
	echo "dynamic-report: 48 tools x 3 targets match"
fi
