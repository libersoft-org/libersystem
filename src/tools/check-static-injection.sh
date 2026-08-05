#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"
first="${1:-static}"
kind="static"
mode="all"
mutation=""
artifact=""
backup=""
baseline_log=""
failure_log=""
restore_log=""

case "$first" in
static | undeclared-edge | duplicate-edge | malformed-dynamic | malformed-symbol-relocation | identity-note)
	kind="$first"
	mode="${2:-all}"
	;;
all | x86_64 | aarch64 | riscv64) mode="$first" ;;
*)
	echo "usage: $0 [static|undeclared-edge|duplicate-edge|malformed-dynamic|malformed-symbol-relocation|identity-note] [all|x86_64|aarch64|riscv64]" >&2
	exit 2
	;;
esac

command -v dd >/dev/null
command -v flock >/dev/null
command -v llvm-readelf >/dev/null
command -v od >/dev/null
command -v sha256sum >/dev/null
command -v timeout >/dev/null

source_path() {
	jq -er --arg owner "$1" '.sources[$owner].path' <<<"$manifest_json"
}
mkdir -p "$build_root"
# The build directory has a shape; a script that writes into it makes sure its place exists.
mkdir -p "$build_root/state"
exec 8>"$build_root/state/build-x86_64-unknown-none.lock"
exec 9>"$build_root/state/build-aarch64-unknown-none.lock"
exec 10>"$build_root/state/build-riscv64gc-unknown-none-elf.lock"
flock 8
flock 9
flock 10

restore_artifact() {
	if [[ -n "$artifact" && -n "$backup" && -f "$backup" ]]; then cp "$backup" "$artifact"; fi
}

cleanup() {
	local status=$?
	restore_artifact
	rm -f "$backup" "$baseline_log" "$failure_log" "$restore_log"
	exit "$status"
}
trap cleanup EXIT

# Drive the step that reads the staged artifacts and refuses the malformed ones.
#
# That used to be the kernel's build.rs, so this gate rebuilt the kernel - with a `cargo clean -p
# kernel` in front, because a build.rs only reruns when cargo thinks it must. M0137 split compiling
# from packaging and moved every one of these rejections into `mkpackages`; the kernel's build.rs
# now says so itself. From that commit until this one the gate compiled a kernel that reads no
# artifacts, saw it succeed, and reported that an injection had been caught. Six gates in the
# `static-image` family did this. Nothing here needs a kernel: what is under test is packaging.
package_arch() {
	local arch="$1"
	local output="$2"
	local status
	pushd "$root/tools/mkpackages" >/dev/null
	if timeout 600 cargo run --quiet -- "$arch" >"$output" 2>&1; then status=0; else status=$?; fi
	popd >/dev/null
	return "$status"
}

write_u64_le() {
	local value="$1"
	local offset="$2"
	local byte byte_index
	for ((byte_index = 0; byte_index < 8; byte_index++)); do
		byte=$((value & 255))
		printf '%b' "\\$(printf '%03o' "$byte")" | dd of="$artifact" bs=1 seek="$((offset + byte_index))" conv=notrunc status=none
		value=$((value >> 8))
	done
}

write_u32_le() {
	local value="$1"
	local offset="$2"
	local byte byte_index
	for ((byte_index = 0; byte_index < 4; byte_index++)); do
		byte=$((value & 255))
		printf '%b' "\\$(printf '%03o' "$byte")" | dd of="$artifact" bs=1 seek="$((offset + byte_index))" conv=notrunc status=none
		value=$((value >> 8))
	done
}

dynamic_words() {
	local dynamic_offset_hex dynamic_size_hex dynamic_offset dynamic_size
	dynamic_offset_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $2; exit}')"
	dynamic_size_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $5; exit}')"
	if [[ -z "$dynamic_offset_hex" || -z "$dynamic_size_hex" ]]; then
		echo "image-injection-check: $mutation found no PT_DYNAMIC segment in $artifact" >&2
		return 1
	fi
	dynamic_offset=$((dynamic_offset_hex))
	dynamic_size=$((dynamic_size_hex))
	printf '%s %s\n' "$dynamic_offset" "$dynamic_size"
}

dynamic_value_offset() {
	local requested_tag="$1"
	local dynamic_offset dynamic_size word_index
	local -a words=()
	read -r dynamic_offset dynamic_size < <(dynamic_words)
	mapfile -t words < <(od -An -v -tu8 -j "$dynamic_offset" -N "$dynamic_size" "$artifact" | tr -s ' ' '\n' | sed '/^$/d')
	for ((word_index = 0; word_index + 1 < ${#words[@]}; word_index += 2)); do
		if [[ "${words[word_index]}" == "$requested_tag" ]]; then
			printf '%s\n' "$((dynamic_offset + (word_index / 2) * 16 + 8))"
			return
		fi
	done
	echo "image-injection-check: $mutation found no dynamic tag $requested_tag in $artifact" >&2
	return 1
}

virtual_file_offset() {
	local requested="$1"
	local file_offset virtual_address file_size
	while read -r file_offset virtual_address file_size; do
		if ((requested >= virtual_address && requested < virtual_address + file_size)); then
			printf '%s\n' "$((file_offset + requested - virtual_address))"
			return
		fi
	done < <(llvm-readelf -lW "$artifact" | awk '$1 == "LOAD" {print $2, $3, $5}')
	echo "image-injection-check: $mutation virtual address $requested is not file-backed in $artifact" >&2
	return 1
}

injection_cases() {
	case "$kind" in
	malformed-dynamic) printf '%s\n' duplicate-segment missing-terminator duplicate-singleton ;;
	malformed-symbol-relocation) printf '%s\n' bad-syment bad-hash-count bad-pltrelsz ;;
	*) printf '%s\n' "$kind" ;;
	esac
}

inject_artifact() {
	case "$mutation" in
	static)
		printf '\002\000' | dd of="$artifact" bs=1 seek=16 conv=notrunc status=none
		;;
	undeclared-edge)
		local -a offsets=()
		mapfile -t offsets < <(grep -aob 'lsrt.lslib' "$artifact" | cut -d: -f1)
		if [[ "${#offsets[@]}" != 1 ]]; then
			echo "image-injection-check: $kind expected one lsrt.lslib entry in $artifact" >&2
			return 1
		fi
		printf 'wire.lslib' | dd of="$artifact" bs=1 seek="${offsets[0]}" conv=notrunc status=none
		;;
	duplicate-edge)
		local dynamic_offset_hex dynamic_size_hex dynamic_offset dynamic_size value_offset word_index value
		local -a words=()
		local -a needed_indices=()
		local -a needed_values=()
		dynamic_offset_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $2; exit}')"
		dynamic_size_hex="$(llvm-readelf -lW "$artifact" | awk '$1 == "DYNAMIC" {print $5; exit}')"
		if [[ -z "$dynamic_offset_hex" || -z "$dynamic_size_hex" ]]; then
			echo "image-injection-check: $kind found no PT_DYNAMIC segment in $artifact" >&2
			return 1
		fi
		dynamic_offset=$((dynamic_offset_hex))
		dynamic_size=$((dynamic_size_hex))
		mapfile -t words < <(od -An -v -tu8 -j "$dynamic_offset" -N "$dynamic_size" "$artifact" | tr -s ' ' '\n' | sed '/^$/d')
		for ((word_index = 0; word_index + 1 < ${#words[@]}; word_index += 2)); do
			if [[ "${words[word_index]}" == 1 ]]; then
				needed_indices+=("$((word_index / 2))")
				needed_values+=("${words[word_index + 1]}")
			fi
		done
		if [[ "${#needed_indices[@]}" -lt 2 ]]; then
			echo "image-injection-check: $kind expected two DT_NEEDED entries in $artifact" >&2
			return 1
		fi
		value_offset=$((dynamic_offset + needed_indices[1] * 16 + 8))
		value="${needed_values[0]}"
		write_u64_le "$value" "$value_offset"
		;;
	duplicate-segment)
		local phoff phentsize phnum header_offset header_type index
		phoff="$(od -An -v -tu8 -j 32 -N 8 "$artifact" | tr -d ' ')"
		phentsize="$(od -An -v -tu2 -j 54 -N 2 "$artifact" | tr -d ' ')"
		phnum="$(od -An -v -tu2 -j 56 -N 2 "$artifact" | tr -d ' ')"
		for ((index = 0; index < phnum; index++)); do
			header_offset=$((phoff + index * phentsize))
			header_type="$(od -An -v -tu4 -j "$header_offset" -N 4 "$artifact" | tr -d ' ')"
			if [[ "$header_type" != 2 ]]; then
				write_u32_le 2 "$header_offset"
				return
			fi
		done
		echo "image-injection-check: $mutation found no non-dynamic program header in $artifact" >&2
		return 1
		;;
	missing-terminator)
		local dynamic_offset dynamic_size word_index entry_offset changed
		read -r dynamic_offset dynamic_size < <(dynamic_words)
		local -a words=()
		mapfile -t words < <(od -An -v -tu8 -j "$dynamic_offset" -N "$dynamic_size" "$artifact" | tr -s ' ' '\n' | sed '/^$/d')
		changed=0
		for ((word_index = 0; word_index + 1 < ${#words[@]}; word_index += 2)); do
			if [[ "${words[word_index]}" == 0 ]]; then
				entry_offset=$((dynamic_offset + (word_index / 2) * 16))
				write_u64_le 1879048191 "$entry_offset"
				changed=$((changed + 1))
			fi
		done
		if [[ "$changed" == 0 ]]; then
			echo "image-injection-check: $mutation found no DT_NULL entry in $artifact" >&2
			return 1
		fi
		;;
	duplicate-singleton)
		local dynamic_offset dynamic_size word_index entry_offset singleton_seen target_offset
		read -r dynamic_offset dynamic_size < <(dynamic_words)
		local -a words=()
		mapfile -t words < <(od -An -v -tu8 -j "$dynamic_offset" -N "$dynamic_size" "$artifact" | tr -s ' ' '\n' | sed '/^$/d')
		singleton_seen=0
		target_offset=""
		for ((word_index = 0; word_index + 1 < ${#words[@]}; word_index += 2)); do
			entry_offset=$((dynamic_offset + (word_index / 2) * 16))
			if [[ "${words[word_index]}" == 5 ]]; then singleton_seen=1; fi
			if [[ -z "$target_offset" && "${words[word_index]}" != 0 && "${words[word_index]}" != 5 ]]; then target_offset="$entry_offset"; fi
		done
		if [[ "$singleton_seen" == 0 || -z "$target_offset" ]]; then
			echo "image-injection-check: $mutation found no DT_STRTAB singleton pair in $artifact" >&2
			return 1
		fi
		write_u64_le 5 "$target_offset"
		;;
	bad-syment)
		write_u64_le 23 "$(dynamic_value_offset 11)"
		;;
	bad-hash-count)
		local hash_value_offset hash_address hash_file_offset
		hash_value_offset="$(dynamic_value_offset 4)"
		hash_address="$(od -An -v -tu8 -j "$hash_value_offset" -N 8 "$artifact" | tr -d ' ')"
		hash_file_offset="$(virtual_file_offset "$hash_address")"
		write_u32_le 4294967295 "$((hash_file_offset + 4))"
		;;
	bad-pltrelsz)
		write_u64_le 47 "$(dynamic_value_offset 2)"
		;;
	identity-note)
		local note_offset_hex note_size_hex note_offset note_size profile_offset
		read -r note_offset_hex note_size_hex < <(llvm-readelf -SW "$artifact" | awk '$2 == ".note.liber.identity" {print $5, $6; exit}')
		if [[ -z "$note_offset_hex" || -z "$note_size_hex" ]]; then
			echo "image-injection-check: $mutation found no identity note in $artifact" >&2
			return 1
		fi
		note_offset=$((0x$note_offset_hex))
		note_size=$((0x$note_size_hex))
		profile_offset="$(dd if="$artifact" bs=1 skip="$note_offset" count="$note_size" status=none | LC_ALL=C grep -aob 'profile=release' | cut -d: -f1)"
		if [[ "$(wc -w <<<"$profile_offset")" != 1 ]]; then
			echo "image-injection-check: $mutation found no unique identity profile in the note" >&2
			return 1
		fi
		printf 'x' | dd of="$artifact" bs=1 seek="$((note_offset + profile_offset + 8))" conv=notrunc status=none
		if dd if="$artifact" bs=1 skip="$note_offset" count="$note_size" status=none | LC_ALL=C grep -aq 'profile=release'; then
			echo "image-injection-check: $mutation did not corrupt the identity profile" >&2
			return 1
		fi
		;;
	esac
}

rejection_pattern() {
	case "$mutation" in
	static) printf '%s\n' 'dynamic echo is not ET_DYN' ;;
	undeclared-edge) printf '%s\n' 'dynamic echo DT_NEEDED providers differ from the manifest' ;;
	duplicate-edge) printf '%s\n' 'dynamic dyn_probe repeats a DT_NEEDED provider' ;;
	duplicate-segment | missing-terminator | duplicate-singleton) printf '%s\n' 'dynamic dyn_probe has no valid terminated PT_DYNAMIC' ;;
	bad-syment | bad-pltrelsz) printf '%s\n' 'dynamic dyn_probe has no valid terminated PT_DYNAMIC' ;;
	bad-hash-count) printf '%s\n' 'dynamic dyn_probe has malformed dynamic symbols' ;;
	identity-note) printf '%s\n' 'echo identity profile' ;;
	esac
}

check_target() {
	local label="$1"
	local target="$2"
	local volume_name="$3"
	local volume="$build_root/boot/$volume_name"
	local artifact_hash before after_failure after_restore
	if [[ "$kind" == duplicate-edge || "$kind" == malformed-dynamic || "$kind" == malformed-symbol-relocation ]]; then
		destination="$(jq -er '.programs.dyn_probe.destination | sub("\\.lsexe$"; "")' <<<"$manifest_json")"
	else
		destination="$(jq -er '.programs.echo.destination | sub("\\.lsexe$"; "")' <<<"$manifest_json")"
	fi
	artifact="$build_root/image/$target/$destination"
	[[ -f "$artifact" ]] || {
		echo "image-injection-check: missing staged $label artifact for $kind" >&2
		return 1
	}
	baseline_log="$(mktemp)"
	# Say so when the BASELINE cannot be built, because that is this gate's most likely failure and
	# it used to be its quietest. The gate mutates a staged artifact in place and restores it from
	# an EXIT trap, so a run that is killed rather than exiting leaves the artifact malformed - and
	# the next run's baseline then fails, correctly, because `mkpackages` refuses it. Unguarded
	# under `set -e` that was an empty log and exit 1: no message, no artifact named, nothing to
	# act on. It cost two wrong diagnoses in one night.
	if ! package_arch "$label" "$baseline_log"; then
		cat "$baseline_log" >&2
		echo "image-injection-check: $label baseline packaging failed before any injection" >&2
		echo "  A previous run of this gate may have been killed while $artifact was mutated." >&2
		echo "  Rebuild it and try again:  ./build.sh --arch $label --part user" >&2
		return 1
	fi
	[[ -f "$volume" ]] || {
		echo "image-injection-check: kernel build did not export $label volume package" >&2
		return 1
	}
	backup="$(mktemp)"
	failure_log="$(mktemp)"
	restore_log="$(mktemp)"
	cp "$artifact" "$backup"
	artifact_hash="$(sha256sum "$artifact" | awk '{print $1}')"
	before="$(sha256sum "$volume" | awk '{print $1}')"
	local -a mutations=()
	mapfile -t mutations < <(injection_cases)
	for mutation in "${mutations[@]}"; do
		cp "$backup" "$artifact"
		inject_artifact
		if package_arch "$label" "$failure_log"; then
			cat "$failure_log" >&2
			echo "image-injection-check: $label $mutation injection unexpectedly built" >&2
			return 1
		fi
		if ! grep -q "$(rejection_pattern)" "$failure_log"; then
			echo "image-injection-check: $label did not reject the injected $mutation artifact" >&2
			return 1
		fi
		after_failure="$(sha256sum "$volume" | awk '{print $1}')"
		if [[ "$before" != "$after_failure" ]]; then
			echo "image-injection-check: $label rewrote $volume_name after rejecting $mutation" >&2
			return 1
		fi
		restore_artifact
		if [[ "$(sha256sum "$artifact" | awk '{print $1}')" != "$artifact_hash" ]]; then
			echo "image-injection-check: $label failed to restore the dynamic artifact" >&2
			return 1
		fi
		if ! package_arch "$label" "$restore_log"; then
			cat "$restore_log" >&2
			echo "image-injection-check: $label could not rebuild after restoring $mutation" >&2
			return 1
		fi
		after_restore="$(sha256sum "$volume" | awk '{print $1}')"
		if [[ "$before" != "$after_restore" ]]; then
			echo "image-injection-check: $label rebuilt a different volume after artifact restoration" >&2
			return 1
		fi
	done
	rm -f "$backup" "$baseline_log" "$failure_log" "$restore_log"
	artifact=""
	backup=""
	baseline_log=""
	failure_log=""
	restore_log=""
	printf 'image-injection-check: %s %s passed\n' "$kind" "$label"
}

case "$mode" in
all)
	check_target x86_64 x86_64-unknown-none volume-x86_64.pkg
	check_target aarch64 aarch64-unknown-none volume-aarch64.pkg
	check_target riscv64 riscv64gc-unknown-none-elf volume-riscv64.pkg
	;;
x86_64) check_target x86_64 x86_64-unknown-none volume-x86_64.pkg ;;
aarch64) check_target aarch64 aarch64-unknown-none volume-aarch64.pkg ;;
riscv64) check_target riscv64 riscv64gc-unknown-none-elf volume-riscv64.pkg ;;
*)
	echo "usage: $0 [static|undeclared-edge|duplicate-edge|malformed-dynamic|malformed-symbol-relocation|identity-note] [all|x86_64|aarch64|riscv64]" >&2
	exit 2
	;;
esac

echo "image-injection-check: $kind $mode passed"
