#!/usr/bin/env bash
set -euo pipefail

verbose="${LIBER_VERBOSE:-0}"
if [[ "${1:-}" == "--verbose" ]]; then
	verbose=1
	shift
fi
explain=0
if [[ "${1:-}" == "--explain" ]]; then
	explain=1
	shift
fi
selected_artifact=""
selected_kind=""
if [[ "${1:-}" == "--artifact" ]]; then
	selected_artifact="${2:-}"
	if [[ -z "$selected_artifact" ]]; then
		echo "usage: $0 [--verbose] [--explain] [--artifact <artifact>] <target> <crate>..." >&2
		exit 2
	fi
	shift 2
	case "$selected_artifact" in
	*.lsexe)
		selected_artifact="${selected_artifact%.lsexe}"
		selected_kind="program"
		;;
	*.lslib)
		selected_artifact="${selected_artifact%.lslib}"
		selected_kind="library"
		;;
	esac
fi
if [[ -z "$selected_artifact" && $# -lt 2 ]] || [[ -n "$selected_artifact" && $# -lt 1 ]]; then
	echo "usage: $0 [--verbose] [--explain] [--artifact <artifact>] <target> <crate>..." >&2
	exit 2
fi
if [[ "$verbose" != 0 && "$verbose" != 1 ]]; then
	echo "build-shared: LIBER_VERBOSE must be 0 or 1" >&2
	exit 2
fi

verbose_log() {
	if [[ "$verbose" == 1 ]]; then echo "$*"; fi
}

# Machine-readable phase events, in the format the kernel test driver and the QEMU runner
# already emit: one host-nanosecond timestamp, a phase and an event, appended to the file the
# caller names in `LIBER_TIMING_LOG`. Costed in whole seconds the summary line cannot answer
# where a three-second build went, and a reader that has to parse prose is a reader that stops
# being run. Per-unit events carry their own decision, so what was compiled and what was reused
# is a fact in the record rather than an inference from the totals.
#
# A unit event is `<kind>:<hit|miss>:<name>` and marks the DECISION, not the work: a miss is
# emitted where the cache is found wanting, before the compile it causes. Timing the work is
# what the surrounding phase boundaries are for, and naming these after the work would put a
# `built` event before the building - which is what the first version of them did.
timing_event() {
	if [[ -n "${LIBER_TIMING_LOG:-}" ]]; then printf '%s\t%s\t%s\n' "$(date +%s%N)" "$1" "$2" >>"$LIBER_TIMING_LOG"; fi
}

target="$1"
shift
root="$(cd "$(dirname "$0")/.." && pwd)"
build_root="$root/../.build"
manifest_json="$("$root/tools/system-manifest.sh" export-json)"
manifest_digest="$(sha256sum <<<"$manifest_json" | awk '{print $1}')"
if [[ -n "$selected_artifact" ]]; then
	if [[ -z "$selected_kind" ]]; then
		# Test the fields directly rather than through `select`. A `select` that rejects emits
		# nothing, and an `if` whose condition emits nothing produces nothing at all, so the
		# `elif` was unreachable and every library resolved as unknown. Indexing a missing
		# program yields null, which compares false and leaves exactly one boolean.
		selected_kind="$(jq -r --arg artifact "$selected_artifact" '
			if (.programs[$artifact].linkage == "dynamic" and .programs[$artifact].stage == "volume") then "program"
			elif .libraries[$artifact] then "library"
			else empty
			end
		' <<<"$manifest_json")"
	elif ! jq -e --arg artifact "$selected_artifact" --arg kind "$selected_kind" '
		if $kind == "program" then
			.programs[$artifact] | select(.linkage == "dynamic" and .stage == "volume")
		else
			.libraries[$artifact]
		end
	' <<<"$manifest_json" >/dev/null; then
		selected_kind=""
	fi
	if [[ -z "$selected_kind" ]]; then
		echo "build-shared: unknown dynamic volume artifact '$selected_artifact'" >&2
		exit 2
	fi
	mapfile -t selected_specs < <(jq -r --arg artifact "$selected_artifact" --arg kind "$selected_kind" '
		def dependencies($root; $name):
			($root.libraries[$name].providers[]? | dependencies($root; .)), $name;
		def depends_on($root; $name; $wanted):
			any($root.libraries[$name].providers[]?;
				. == $wanted or depends_on($root; .; $wanted));
		. as $root |
		(if $kind == "program" then
			($root.programs[$artifact].providers[] | dependencies($root; .))
		else
			(($root.libraries | keys[] as $name |
				select($name == $artifact or depends_on($root; $name; $artifact)) |
				dependencies($root; $name)),
			($root.programs[] |
				select(.linkage == "dynamic" and .stage == "volume") |
				select(any(.providers[]?; depends_on($root; .; $artifact))) |
				.providers[] | dependencies($root; .)))
		end) as $name |
		"\($name)=\($root.libraries[$name].owner)"
	' <<<"$manifest_json" | awk '!seen[$0]++')
	mapfile -t selected_programs < <(jq -r --arg artifact "$selected_artifact" --arg kind "$selected_kind" '
		def depends_on($root; $name; $wanted):
			$name == $wanted or any($root.libraries[$name].providers[]?;
				depends_on($root; .; $wanted));
		. as $root | .programs[] |
		select(.linkage == "dynamic" and .stage == "volume" and .name != "dyn_probe") |
		select(($kind == "program" and .name == $artifact) or
			($kind == "library" and any(.providers[]?; depends_on($root; .; $artifact)))) |
		.name
	' <<<"$manifest_json" | sort -u)
	set -- "${selected_specs[@]}"
fi
declare -A source_owners=()
declare -A source_paths=()
declare -A library_rows=()
declare -A library_destinations=()
declare -A program_destinations=()
declare -A program_owners=()
while IFS=$'\t' read -r record_kind name owner destination features providers; do
	case "$record_kind" in
	source)
		source_owners[$name]="$owner"
		source_paths[$owner]="$name"
		;;
	library)
		library_rows[$name]="library"$'\t'"$name"$'\t'"$owner"$'\t'"volume"$'\t'"$destination"$'\t'"$features"$'\t'"$providers"
		library_destinations[$name]="$destination"
		;;
	program)
		program_owners[$name]="$owner"
		program_destinations[$name]="$destination"
		;;
	esac
done < <(jq -r '
	(.sources[] | ["source", .owner, .path, "", "", ""]),
	(.libraries[] | ["library", .name, .owner, .destination,
		(if (.features | length) == 0 then "-" else (.features | join(",")) end),
		(.providers | join(" "))]),
	(.programs[] | ["program", .name, .owner, .destination, "", ""]) |
	@tsv
' <<<"$manifest_json")
requested_arguments=("$@")
artifact_output_root="$build_root/system-image/$target"
provider_output_dir="$artifact_output_root"
artifact_log_dir="$artifact_output_root/logs"
rust_min_stack="${RUST_MIN_STACK:-67108864}"
force_rebuild="${LIBER_IMAGE_REBUILD:-0}"
artifact_cache_dir="$build_root/image-artifacts-$target"
provider_cargo_target="$build_root/cargo/provider-$target"
cargo_target="$target"
cargo_target_flags=()
build_started=$SECONDS
timing_event build start
provider_cache_hits=0
provider_cache_misses=0
object_cache_hits=0
object_cache_misses=0
executable_cache_hits=0
executable_cache_misses=0
source_inventory_file=""
source_metadata_dir=""
source_inventory_seconds=0
image_graph_seconds=0
provider_seconds=0
consumer_seconds=0
report_seconds=0
warm_snapshot_file=""
warm_snapshot_hit=0
targeted_state_file=""
targeted_state_hit=0
targeted_state_reason=""
object_inputs=""

# `grep -q` stops at its first match and closes the pipe, so the producer's next write fails
# with EPIPE. llvm-readelf reports that as exit 74, and `pipefail` makes it the status of the
# whole pipeline, so a successful match read as a failed read and rejected an artifact of
# exactly the kind that was being asked for. It is a race against the producer's final flush,
# so it struck a different check on each run and never the same one twice. Match against
# captured output instead: a producer that really failed still fails here, on the capture,
# which is the case a pipeline cannot tell apart from a match.
matches_output() {
	local pattern="$1"
	local output
	shift
	output="$("$@")" || return 2
	grep -Eq -- "$pattern" <<<"$output"
}

matches_line() {
	local value="$1"
	local output
	shift
	output="$("$@")" || return 2
	grep -Fqx -- "$value" <<<"$output"
}

check_dynamic_report_inventory() {
	local report_started=$SECONDS
	local diagnostics
	if diagnostics="$("$root/tools/check-dynamic-report.sh" --check-inventory 2>&1)"; then
		verbose_log "build-shared: $diagnostics"
	else
		echo "build-shared: checked dynamic reports need refresh after all target graphs are current" >&2
		echo "build-shared: refresh with: cd $root && just dynamic-report-update" >&2
		if [[ "$verbose" == 1 ]]; then printf '%s\n' "$diagnostics" >&2; fi
	fi
	report_seconds=$((SECONDS - report_started))
}

report_build_summary() {
	local status=$?
	find "$artifact_output_root" -type f -name "*.$$.expected" -delete 2>/dev/null || true
	find "$artifact_output_root" -type f -name "*.$$.candidate" -delete 2>/dev/null || true
	find "$artifact_cache_dir" -maxdepth 1 -type f -name "*.tmp.$$" -delete 2>/dev/null || true
	find "$artifact_cache_dir" -maxdepth 1 -type f -name "*.$$.expected" -delete 2>/dev/null || true
	if [[ -n "$object_inputs" ]]; then rm -f "$object_inputs"; fi
	rm -f "$build_root/image-warm-$target.state.inputs.current.$$" "$build_root/image-warm-$target.state.inputs.tmp.$$" "$build_root/image-warm-$target.state.tmp.$$"
	rm -f "$build_root/package-dirs.$$.tmp"
	if [[ $status == 0 && $targeted_state_hit == 0 && -n "$targeted_state_file" ]] && declare -F write_targeted_state >/dev/null; then
		write_targeted_state || rm -f "$targeted_state_file.tmp.$$"
	elif [[ $status != 0 && -n "$targeted_state_file" ]]; then
		rm -f "$targeted_state_file.tmp.$$"
	fi
	if [[ -n "$source_inventory_file" ]]; then rm -f "$source_inventory_file"; fi
	if [[ -n "$source_metadata_dir" ]]; then rm -rf "$source_metadata_dir"; fi
	if [[ $status == 0 && $warm_snapshot_hit == 0 && -n "$warm_snapshot_file" ]] && declare -F write_warm_snapshot >/dev/null; then
		write_warm_snapshot || rm -f "$warm_snapshot_file"
	elif [[ $status != 0 && -n "$warm_snapshot_file" ]]; then
		rm -f "$warm_snapshot_file"
	fi
	if [[ $status == 0 && -z "$selected_artifact" ]]; then check_dynamic_report_inventory; fi
	timing_event build end
	echo "build-shared: summary target=$target seconds=$((SECONDS - build_started)) stages=source:$source_inventory_seconds,graph:$image_graph_seconds,providers:$provider_seconds,consumers:$consumer_seconds,reports:$report_seconds providers=$provider_cache_hits/$provider_cache_misses objects=$object_cache_hits/$object_cache_misses executables=$executable_cache_hits/$executable_cache_misses status=$status"
}

trap report_build_summary EXIT

if [[ "$force_rebuild" != 0 && "$force_rebuild" != 1 ]]; then
	echo "build-shared: LIBER_IMAGE_REBUILD must be 0 or 1" >&2
	exit 2
fi

command -v flock >/dev/null
mkdir -p "$build_root" "$provider_output_dir" "$artifact_log_dir"
if [[ "${LIBER_IMAGE_LOCK_HELD:-0}" != 1 ]]; then
	exec 9>"$build_root/image-build-$target.lock"
	flock 9
fi
if [[ -z "$selected_artifact" ]]; then
	find "$artifact_output_root" -type f \( -name '*.identity' -o -name '*.order' \) -delete 2>/dev/null || true
	find "$artifact_cache_dir" -maxdepth 1 -type f -name '*.order.sha256' -delete 2>/dev/null || true
fi

case "$target" in
x86_64-unknown-none)
	emulation="elf_x86_64"
	rustflags="-C relocation-model=pic"
	cargo_target="$root/user/x86_64-unknown-none.json"
	cargo_target_flags=(-Z json-target-spec)
	;;
aarch64-unknown-none)
	emulation="aarch64elf"
	rustflags="-C relocation-model=pic"
	;;
riscv64gc-unknown-none-elf)
	emulation="elf64lriscv"
	rustflags="-C relocation-model=pic -C code-model=medium"
	;;
*)
	echo "build-shared: unsupported target '$target'" >&2
	exit 2
	;;
esac

lld="$(find "$(rustc --print sysroot)" -path '*/rust-lld' -type f -print -quit)"
if [[ -z "$lld" ]]; then
	echo "build-shared: rust-lld not found in the pinned toolchain" >&2
	exit 1
fi
command -v llvm-ar >/dev/null
command -v llvm-readelf >/dev/null
command -v llvm-strip >/dev/null
command -v jq >/dev/null
command -v sha256sum >/dev/null
command -v stat >/dev/null
command -v xxd >/dev/null

targeted_plan_record() {
	local library_names program_names
	library_names="$(printf '%s\n' "${selected_specs[@]}" | sed 's/=.*//')"
	program_names="$(printf '%s\n' "${selected_programs[@]}")"
	printf 'target\t%s\n' "$target"
	printf 'artifact\t%s\n' "$selected_artifact"
	printf 'kind\t%s\n' "$selected_kind"
	printf 'rustflags\t%s\n' "$rustflags"
	printf 'cargo-target\t%s\n' "$cargo_target"
	printf 'spec\t%s\n' "${requested_arguments[*]}"
	printf 'tool:rust-lld\t%s\n' "$lld"
	for tool in cargo rustc llvm-ar llvm-objcopy llvm-readelf llvm-strip; do
		printf 'tool:%s\t%s\n' "$tool" "$(command -v "$tool")"
	done
	for variable in AR CARGO_BUILD_RUSTC CARGO_BUILD_RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CFLAGS RUSTC RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN; do
		printf 'environment:%s\t%s\n' "$variable" "${!variable-}"
	done
	jq -r --arg libraries "$library_names" --arg programs "$program_names" '
		($libraries | split("\n") | map(select(. != ""))) as $library_names |
		($programs | split("\n") | map(select(. != ""))) as $program_names |
		(.libraries[] | select(.name as $name | $library_names | index($name)) |
			["manifest-library:" + .name, (tojson)] | @tsv),
		(.programs[] | select(.name as $name | $program_names | index($name)) |
			["manifest-program:" + .name, (tojson)] | @tsv)
	' <<<"$manifest_json"
}

targeted_state_stat() {
	local path="$1"
	stat -Lc $'entry\t%n\t%F\t%s\t%i\t%y\t%z' "$path"
}

targeted_state_valid() {
	local index key expected current path
	local -a expected_plan=() current_plan=() expected_entries=() current_entries=()
	local -a paths=()
	targeted_state_reason=""
	if [[ ! -f "$targeted_state_file" ]]; then
		targeted_state_reason="state missing"
		return 1
	fi
	if [[ "$(sed -n '1p' "$targeted_state_file")" != "format=liber-targeted-state-v2" ]]; then
		targeted_state_reason="state format changed"
		return 1
	fi
	mapfile -t expected_plan < <(sed -n 's/^plan-entry\t//p' "$targeted_state_file")
	mapfile -t current_plan < <(targeted_plan_record)
	for ((index = 0; index < ${#expected_plan[@]} || index < ${#current_plan[@]}; index += 1)); do
		if [[ "${expected_plan[$index]:-}" != "${current_plan[$index]:-}" ]]; then
			key="${current_plan[$index]%%$'\t'*}"
			if [[ -z "$key" ]]; then key="${expected_plan[$index]%%$'\t'*}"; fi
			targeted_state_reason="plan edge changed: $key"
			return 1
		fi
	done
	mapfile -t paths < <(sed -n 's/^entry\t\([^\t]*\)\t.*$/\1/p' "$targeted_state_file")
	if ((${#paths[@]} == 0)); then
		targeted_state_reason="state has no input edges"
		return 1
	fi
	mapfile -t expected_entries < <(sed -n '/^entry\t/p' "$targeted_state_file")
	if ! mapfile -t current_entries < <(stat -Lc $'entry\t%n\t%F\t%s\t%i\t%y\t%z' "${paths[@]}" 2>/dev/null); then
		for path in "${paths[@]}"; do
			if [[ ! -e "$path" ]]; then
				targeted_state_reason="input missing: $path"
				return 1
			fi
		done
		targeted_state_reason="input stat failed"
		return 1
	fi
	for ((index = 0; index < ${#expected_entries[@]}; index += 1)); do
		if [[ "${expected_entries[$index]}" != "${current_entries[$index]:-}" ]]; then
			IFS=$'\t' read -r _ path _ <<<"${expected_entries[$index]}"
			targeted_state_reason="input changed: $path"
			return 1
		fi
	done
	return 0
}

targeted_state_paths() {
	local artifact owner package package_dir source_dir
	{
		printf '%s\n' "$root/tools/build-shared.sh" "$root/tools/build-consumer-object.sh"
		printf '%s\n' "$root/tools/build-exe-start.sh" "$root/tools/exe-start.rs"
		printf '%s\n' "$root/tools/system-manifest.sh" "$root/../product.conf"
		printf '%s\n' "$root/user/build.rs" "$root/user/user.ld"
		printf '%s\n' "$root/user/user-aarch64.ld" "$root/user/user-riscv64.ld"
		printf '%s\n' "$(command -v cargo)" "$(command -v rustc)" "$lld"
		for tool in llvm-ar llvm-objcopy llvm-readelf llvm-strip; do command -v "$tool"; done
		for spec in "${selected_specs[@]}"; do
			artifact="${spec%%=*}"
			owner="${spec#*=}"
			source_dir="$root/$(source_path "$owner")"
			find "$source_dir" -path '*/target' -prune -o -path '*/shared' -prune -o \( -type d -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' -o -name '*.ld' \) \) -print
			if [[ "$owner" == *-client-provider ]]; then
				source_dir="$root/$(source_path "${owner%-provider}")"
				find "$source_dir" -path '*/target' -prune -o -path '*/shared' -prune -o \( -type d -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' -o -name '*.ld' \) \) -print
			fi
			printf '%s\n' "$(library_file "$artifact")"
			find "$artifact_cache_dir" -maxdepth 1 -type f -name "library-$artifact.*" -print
		done
		for program in "${selected_programs[@]}"; do
			package="$(jq -er --arg program "$program" '.programs[$program].owner' <<<"$manifest_json")"
			if [[ "$package" == tools ]]; then
				source_dir="$root/$(source_path tools)"
				printf '%s\n' "$source_dir/Cargo.toml" "$source_dir/Cargo.lock"
				printf '%s\n' "$source_dir/src/lib.rs" "$source_dir/src/$program.rs"
			elif [[ -f "$source_metadata_dir/$package.dirs" ]]; then
				while read -r package_dir; do
					find "$root/$package_dir" -path '*/target' -prune -o -path '*/shared' -prune -o \( -type d -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' -o -name '*.ld' \) \) -print
				done <"$source_metadata_dir/$package.dirs"
			fi
			printf '%s\n' "$(program_file "$program")"
			find "$artifact_cache_dir" -maxdepth 1 -type f \( -name "executable-$program.*" -o -name "object-$program-*" \) -print
		done
		printf '%s\n' "$build_root/exe-start-$target.o" "$build_root/exe-start-$target.o.build-key"
		if [[ "$selected_kind" == library ]] && matches_output '^pix=' printf '%s\n' "${selected_specs[@]}"; then
			printf '%s\n' "$(program_file dyn_probe)"
			find "$artifact_cache_dir" -maxdepth 1 -type f -name 'executable-dyn_probe.*' -print
		fi
	} | while read -r path; do
		[[ -e "$path" ]] && printf '%s\n' "$path"
	done | sort -u
}

write_targeted_state() {
	local path temporary
	temporary="$targeted_state_file.tmp.$$"
	{
		printf 'format=liber-targeted-state-v2\n'
		targeted_plan_record | sed 's/^/plan-entry\t/'
		while read -r path; do targeted_state_stat "$path"; done < <(targeted_state_paths)
	} >"$temporary"
	mv "$temporary" "$targeted_state_file"
}

if [[ -n "$selected_artifact" ]]; then
	targeted_state_file="$artifact_cache_dir/targeted-$selected_kind-$selected_artifact.state"
	if [[ "$force_rebuild" == 0 ]] && targeted_state_valid; then
		provider_cache_hits="${#selected_specs[@]}"
		executable_cache_hits="${#selected_programs[@]}"
		if [[ "$selected_kind" == library ]] && matches_output '^pix=' printf '%s\n' "${selected_specs[@]}"; then
			((executable_cache_hits += 1))
		fi
		targeted_state_hit=1
		verbose_log "build-shared: targeted state hit $selected_kind $selected_artifact"
		if [[ "$explain" == 1 ]]; then echo "dev-build: explain decision=hit edge=none reason=unchanged"; fi
		exit 0
	fi
	verbose_log "build-shared: targeted state miss $selected_kind $selected_artifact"
	if [[ "$explain" == 1 ]]; then echo "dev-build: explain decision=miss reason=$targeted_state_reason"; fi
fi

prune_stale_program_outputs() {
	local file relative
	declare -A expected=()
	while IFS= read -r relative; do
		expected["$relative"]=1
	done < <(jq -r '.programs[] | select(.linkage == "dynamic" and .stage == "volume") | .destination | sub("\\.lsexe$"; "")' <<<"$manifest_json")
	while IFS= read -r -d '' file; do
		relative="${file#"$artifact_output_root/"}"
		if [[ -z "${expected[$relative]:-}" ]]; then
			rm -f "$file"
		fi
	done < <(find "$artifact_output_root" -type f \( -path "$artifact_output_root/bin/*" -o -path "$artifact_output_root/libexec/*" \) -print0 2>/dev/null)
	find "$artifact_output_root/bin" "$artifact_output_root/libexec" -depth -type d -empty -delete 2>/dev/null || true
}

if [[ -z "$selected_artifact" ]]; then prune_stale_program_outputs; fi

warm_input_inventory_file=""
if [[ -z "$selected_artifact" ]]; then
	warm_snapshot_file="$build_root/image-warm-$target.state"
	warm_input_inventory_file="$warm_snapshot_file.inputs"
fi

warm_input_inventory() {
	{
		printf 'format=liber-image-warm-input-v1\n'
		printf 'target=%s\n' "$target"
		printf 'manifest=%s\n' "$manifest_digest"
		printf 'spec=%s\n' "${requested_arguments[@]}"
		printf 'rustflags=%s\n' "$rustflags"
		printf 'cargo-target=%s\n' "$cargo_target"
		printf 'rustc=%s\n' "$(rustc -vV | sha256sum | awk '{print $1}')"
		for tool in "$(command -v cargo)" "$(command -v rustc)" "$(command -v llvm-mc)" "$lld" "$(command -v llvm-ar)" "$(command -v llvm-objcopy)" "$(command -v llvm-readelf)" "$(command -v llvm-strip)"; do
			stat -c 'tool\t%n\t%s\t%y' "$tool"
		done
		for variable in AR CARGO_BUILD_RUSTC CARGO_BUILD_RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CFLAGS RUSTC RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN; do
			printf 'env-%s=%s\n' "$variable" "${!variable-}"
		done
		find "$root/user" "$root/abi" "$root/bootproto" "$root/fs" "$root/proto" "$root/term" "$root/wasm" "$root/wire" "$root/tools/system-manifest" -type f -printf 'input\t%p\t%s\t%T@\n'
		for input in "$root/tools/build-shared.sh" "$root/tools/build-consumer-object.sh" "$root/tools/build-exe-start.sh" "$root/tools/exe-start.rs" "$root/tools/system-manifest.sh" "$root/../product.conf"; do
			stat -c 'input\t%n\t%s\t%y' "$input"
		done
	} | sort
}

warm_input_fingerprint() {
	warm_input_inventory | sha256sum | awk '{print $1}'
}

warm_output_fingerprint() {
	{
		find "$artifact_output_root" -type f ! -path "$artifact_log_dir/*" -printf 'output\t%p\t%s\t%T@\n' 2>/dev/null || true
		find "$artifact_cache_dir" -type f -printf 'cache\t%p\t%s\t%T@\n' 2>/dev/null || true
		for output in "$build_root/exe-start-$target.o" "$build_root/exe-start-$target.o.build-key"; do
			if [[ -f "$output" ]]; then stat -c 'output\t%n\t%s\t%y' "$output"; fi
		done
	} | sort | sha256sum | awk '{print $1}'
}

write_warm_snapshot() {
	local input output temporary temporary_inputs
	temporary_inputs="$warm_input_inventory_file.tmp.$$"
	warm_input_inventory >"$temporary_inputs"
	input="$(sha256sum "$temporary_inputs" | awk '{print $1}')"
	output="$(warm_output_fingerprint)"
	temporary="$warm_snapshot_file.tmp.$$"
	{
		printf 'format=liber-image-warm-state-v1\n'
		printf 'input=%s\n' "$input"
		printf 'output=%s\n' "$output"
	} >"$temporary"
	mv "$temporary" "$warm_snapshot_file"
	mv "$temporary_inputs" "$warm_input_inventory_file"
}

find "$artifact_output_root" -type f -name '*.expected' -delete 2>/dev/null || true
find "$artifact_cache_dir" -maxdepth 1 -type f -name '*.tmp.*' -delete 2>/dev/null || true
if [[ -z "$selected_artifact" && "$force_rebuild" == 0 && -f "$warm_snapshot_file" ]]; then
	warm_expected_input="$(sed -n 's/^input=//p' "$warm_snapshot_file")"
	warm_expected_output="$(sed -n 's/^output=//p' "$warm_snapshot_file")"
	warm_actual_inputs="$warm_input_inventory_file.current.$$"
	warm_input_inventory >"$warm_actual_inputs"
	warm_actual_input="$(sha256sum "$warm_actual_inputs" | awk '{print $1}')"
	warm_actual_output="$(warm_output_fingerprint)"
	if [[ -n "$warm_expected_input" && -n "$warm_expected_output" && "$warm_expected_input" == "$warm_actual_input" && "$warm_expected_output" == "$warm_actual_output" ]]; then
		provider_cache_hits="$(jq '.libraries | length' <<<"$manifest_json")"
		executable_cache_hits="$(jq '[.programs[] | select(.linkage == "dynamic" and .stage == "volume" and .name != "dyn_probe")] | length' <<<"$manifest_json")"
		warm_snapshot_hit=1
		verbose_log "build-shared: warm image snapshot hit"
		rm -f "$warm_actual_inputs"
		exit 0
	fi
	if [[ "$warm_expected_input" != "$warm_actual_input" ]]; then
		verbose_log "build-shared: warm image snapshot miss (inputs)"
		if [[ "$verbose" == 1 && -f "$warm_input_inventory_file" ]]; then diff -u "$warm_input_inventory_file" "$warm_actual_inputs" || true; fi
	fi
	if [[ "$warm_expected_output" != "$warm_actual_output" ]]; then verbose_log "build-shared: warm image snapshot miss (outputs)"; fi
	rm -f "$warm_actual_inputs"
fi

source_path() {
	local owner="$1"
	[[ -n "${source_owners[$owner]:-}" ]] || return 1
	printf '%s\n' "${source_owners[$owner]}"
}

declare -A source_file_hashes=()
declare -A build_file_hashes=()
timing_event source start
source_inventory_started=$SECONDS
source_inventory_file="$build_root/image-sources.$$.inventory"
source_metadata_dir="$build_root/image-source-metadata.$$"
source_roots_file="$source_metadata_dir/roots"
mkdir -p "$(dirname "$source_inventory_file")"
mkdir -p "$source_metadata_dir"

if [[ -n "$selected_artifact" ]]; then
	printf '%s\n' "${selected_specs[@]}" | sed 's/^[^=]*=//'
else
	jq -r '.libraries[].owner' <<<"$manifest_json" | sort -u
fi | while read -r crate; do
	crate_dir="$(source_path "$crate")" || {
		echo "build-shared: library owner $crate has no unique source path" >&2
		exit 1
	}
	printf '%s\n' "$crate_dir" >>"$source_roots_file"
	if [[ "$crate" == *-client-provider ]]; then printf '%s\n' "$(source_path "${crate%-provider}")" >>"$source_roots_file"; fi
done

if [[ -n "$selected_artifact" ]]; then
	for program in "${selected_programs[@]}"; do printf '%s\n' "${program_owners[$program]}"; done | sort -u
else
	jq -r '.programs[] | select(.linkage == "dynamic" and .stage == "volume") | .owner' <<<"$manifest_json" | sort -u
fi | while read -r package; do
	[[ -n "$package" ]] || continue
	package_dir="$(source_path "$package")" || {
		echo "build-shared: executable owner $package has no unique source path" >&2
		exit 1
	}
	package_dirs="$source_metadata_dir/$package.dirs"
	printf '%s\n' "$package_dir" >>"$source_roots_file"
	(cd "$root" && cargo metadata --format-version 1 --manifest-path "$package_dir/Cargo.toml") |
		jq -r --arg root "$root" '.packages[] | select(.source == null) | .manifest_path | sub("/Cargo.toml$"; "") | sub("^" + $root + "/"; "")' |
		sort -u >"$package_dirs"
	cat "$package_dirs" >>"$source_roots_file"
done
sort -u -o "$source_roots_file" "$source_roots_file"

while IFS= read -r -d '' record; do
	hash="${record%% *}"
	source="${record#* }"
	source="${source# }"
	source="${source#./}"
	source_file_hashes[$source]="$hash"
	printf '%s\t%s\n' "$source" "$hash" >>"$source_inventory_file"
done < <(
	cd "$root"
	{
		mapfile -t source_roots <"$source_roots_file"
		find "${source_roots[@]}" -path '*/target' -prune -o -path '*/shared' -prune -o -type f \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' -o -name 'rust-toolchain.toml' -o -name '*.ld' \) -print0
		printf 'user/build.rs\0user/user.ld\0user/user-aarch64.ld\0user/user-riscv64.ld\0'
		printf '../product.conf\0'
	} | sort -z | xargs -0 sha256sum --zero
)
source_inventory_seconds=$((SECONDS - source_inventory_started))
timing_event source end

source_file_digest() {
	local source="$1"
	local display="${2:-$source}"
	local key="${source#"$root/"}"
	if [[ "$source" == "$root/../product.conf" ]]; then key="../product.conf"; fi
	if [[ -z "${source_file_hashes[$key]:-}" ]]; then
		printf 'missing:%s\n' "$key"
		return
	fi
	printf '%s  %s\n' "${source_file_hashes[$key]}" "$display"
}

build_file_digest() {
	local file="$1"
	if [[ -n "${build_file_hashes[$file]:-}" ]]; then
		printf '%s\n' "${build_file_hashes[$file]}"
	else
		sha256sum "$file" | awk '{print $1}'
	fi
}

rustc_commit="$(rustc -vV | sed -n 's/^commit-hash: //p')"
if [[ ! "$rustc_commit" =~ ^[0-9a-f]{40}$ ]]; then
	echo "build-shared: rustc did not report one commit hash" >&2
	exit 1
fi

build_tool_digest="$({
	sha256sum "$root/tools/build-shared.sh" "$root/tools/build-exe-start.sh" "$root/tools/exe-start.rs" "$lld"
	for tool in llvm-objcopy llvm-readelf llvm-strip; do
		sha256sum "$(command -v "$tool")"
	done
} | sha256sum | awk '{print $1}')"
object_tool_digest="$(sha256sum "$root/tools/build-consumer-object.sh" | awk '{print $1}')"

library_file() {
	local artifact="$1"
	[[ -n "${library_destinations[$artifact]:-}" ]] || return 1
	printf '%s/%s' "$provider_output_dir" "${library_destinations[$artifact]}"
}

program_file() {
	local artifact="$1"
	local destination="${program_destinations[$artifact]:-}"
	[[ "$destination" == *.lsexe ]] || return 1
	printf '%s/%s' "$artifact_output_root" "${destination%.lsexe}"
}

declare -A crate_source_digests=()
declare -A provider_identity_digests=()

compute_source_digest() {
	local crate_dir="$1"
	local api_dir=""
	local owner="${source_paths[$crate_dir]:-}"
	if [[ "$owner" == *-client-provider ]]; then
		api_dir="$(source_path "${owner%-provider}")"
		if [[ ! -f "$root/$api_dir/Cargo.toml" ]]; then
			echo "build-shared: $crate_dir has no public API crate $api_dir" >&2
			return 1
		fi
	fi
	awk -F '\t' -v crate="$crate_dir/" -v api="${api_dir:+$api_dir/}" '
		function source_file(path) {
			return path ~ /\.rs$/ || path ~ /(^|\/)Cargo\.toml$/ || path ~ /(^|\/)Cargo\.lock$/ || path ~ /(^|\/)rust-toolchain\.toml$/
		}
		source_file($1) && (index($1, crate) == 1 || (api != "" && index($1, api) == 1)) {
			print $1
			print $2 "  " $1
		}
	' "$source_inventory_file" | sha256sum | awk '{print $1}'
}

source_digest() {
	local crate_dir="$1"
	if [[ -n "${crate_source_digests[$crate_dir]:-}" ]]; then
		printf '%s\n' "${crate_source_digests[$crate_dir]}"
	else
		compute_source_digest "$crate_dir"
	fi
}

executable_source_digest() {
	local crate_dir="$1"
	local package="$2"
	local artifact="$3"
	if [[ "$package" == tools ]]; then
		(
			for source in "$crate_dir/Cargo.toml" "$crate_dir/Cargo.lock" "$crate_dir/src/lib.rs" "$crate_dir/src/$artifact.rs" user/build.rs user/user.ld user/user-aarch64.ld user/user-riscv64.ld ../product.conf; do
				printf '%s\n' "$source"
				if [[ "$source" == /* ]]; then
					source_file_digest "$source" "$source"
				else
					source_file_digest "$root/$source" "$source"
				fi
			done
		) | sha256sum | awk '{print $1}'
		return
	fi
	{
		printf 'dependency-closure=%s\n' "${package_source_digests[$package]:-$(source_digest "$crate_dir")}"
		for source in "$root/user/build.rs" "$root/user/user.ld" "$root/user/user-aarch64.ld" "$root/user/user-riscv64.ld" "$root/../product.conf"; do
			source_file_digest "$source"
		done
		if [[ "$package" == services ]]; then
			printf 'manifest=%s\n' "$manifest_digest"
		fi
	} | sha256sum | awk '{print $1}'
}

local_dependency_source_digest() {
	local crate_dir="$1"
	local package="$2"
	local exclude_root="${3:-0}"
	local package_dirs package_dir
	package_dirs="$build_root/package-dirs.$$.tmp"
	while IFS= read -r package_dir; do
		if [[ "$exclude_root" == 1 && "$package_dir" == "$crate_dir" ]]; then continue; fi
		printf '%s/\n' "$package_dir"
	done <"$source_metadata_dir/$package.dirs" >"$package_dirs"
	awk -F '\t' '
		NR == FNR {prefix[$1] = 1; next}
		{
			if (!($1 ~ /\.rs$/ || $1 ~ /(^|\/)Cargo\.toml$/ || $1 ~ /(^|\/)Cargo\.lock$/ || $1 ~ /(^|\/)rust-toolchain\.toml$/)) next
			for (dir in prefix) {
				if (index($1, dir) == 1) {
					print $1
					print $2 "  " $1
					break
				}
			}
		}
	' "$package_dirs" "$source_inventory_file" | sha256sum | awk '{print $1}'
	rm -f "$package_dirs"
}

source_digest_roots="$source_metadata_dir/digest-roots"
if [[ -n "$selected_artifact" ]]; then
	printf '%s\n' "${selected_specs[@]}" | sed 's/^[^=]*=//'
else
	jq -r '.libraries[].owner' <<<"$manifest_json" | sort -u
fi | while read -r crate; do
	crate_dir="$(source_path "$crate")"
	printf '%s\n' "$crate_dir" >>"$source_digest_roots"
	if [[ "$crate" == *-client-provider ]]; then printf '%s\n' "$(source_path "${crate%-provider}")" >>"$source_digest_roots"; fi
done
if [[ -z "$selected_artifact" ]] || { [[ "$selected_kind" == library ]] && matches_output '^pix=' printf '%s\n' "${selected_specs[@]}"; }; then
	printf '%s\n' "$(source_path dyn_probe)" >>"$source_digest_roots"
fi
sort -u -o "$source_digest_roots" "$source_digest_roots"
while IFS= read -r crate_dir; do
	crate_source_digests[$crate_dir]="$(compute_source_digest "$crate_dir")"
done <"$source_digest_roots"

write_identity_record() {
	local kind="$1"
	local artifact="$2"
	local package="$3"
	local source_sha="$4"
	local feature_set="$5"
	local providers="$6"
	local identity="$7"
	local provider digest
	{
		printf 'format=liber-image-identity-v1\n'
		printf 'kind=%s\n' "$kind"
		printf 'artifact=%s\n' "$artifact"
		printf 'package=%s\n' "$package"
		printf 'source-sha256=%s\n' "$source_sha"
		printf 'rustc-commit=%s\n' "$rustc_commit"
		printf 'target=%s\n' "$target"
		printf 'profile=release\n'
		printf 'rustflags=%s\n' "$rustflags"
		printf 'features=%s\n' "$feature_set"
		for provider in $(tr ' ' '\n' <<<"$providers" | sort); do
			[[ -n "$provider" ]] || continue
			if [[ -z "${provider_identity_digests[$provider]:-}" ]]; then
				echo "build-shared: $artifact has no identity for provider $provider" >&2
				return 1
			fi
			digest="${provider_identity_digests[$provider]}"
			printf 'provider=%s:%s\n' "$provider" "$digest"
		done
	} >"$identity"
}

write_identity_note() {
	local record="$1"
	local note="$2"
	local record_len padding record_len_le
	record_len="$(stat -c %s "$record")"
	if ((record_len == 0 || record_len > 8192)); then
		echo "build-shared: identity record has invalid length $record_len" >&2
		return 1
	fi
	record_len_le="$(printf '%08x' "$record_len" | sed -E 's/(..)(..)(..)(..)/\4\3\2\1/')"
	{
		printf '06000000%s010000004c49424552000000' "$record_len_le"
	} | xxd -r -p >"$note"
	cat "$record" >>"$note"
	padding=$(((4 - record_len % 4) % 4))
	if ((padding != 0)); then head -c "$padding" /dev/zero >>"$note"; fi
}

verify_identity_note() {
	local elf="$1"
	local identity="$2"
	local note dumped_note
	note="$(mktemp "$build_root/identity-note.XXXXXX")"
	dumped_note="$(mktemp "$build_root/identity-note.XXXXXX")"
	rm -f "$dumped_note"
	# Name an explicit output. Given one file, llvm-objcopy edits it in place, so verifying a
	# staged artifact would republish identical bytes under a new inode and mtime and defeat
	# every stat-keyed cache downstream of it.
	if ! write_identity_note "$identity" "$note" || ! llvm-objcopy --dump-section .note.liber.identity="$dumped_note" "$elf" /dev/null 2>/dev/null || ! cmp -s "$note" "$dumped_note"; then
		rm -f "$note" "$dumped_note"
		return 1
	fi
	rm -f "$note" "$dumped_note"
}

emit_identity() {
	local elf="$1"
	local identity="$2"
	local note
	note="$(mktemp "$build_root/identity-note.XXXXXX")"
	if ! write_identity_note "$identity" "$note" || ! llvm-objcopy --add-section .note.liber.identity="$note" --set-section-flags .note.liber.identity=alloc,readonly "$elf" || ! verify_identity_note "$elf" "$identity"; then
		rm -f "$note"
		echo "build-shared: $elf identity note differs from its record" >&2
		exit 1
	fi
	rm -f "$note"
}

artifact_cache_record() {
	local kind="$1"
	local manifest_row="$2"
	local identity="$3"
	local extra="$4"
	printf 'format=liber-image-artifact-inputs-v1\n'
	printf 'build-tools=%s\n' "$build_tool_digest"
	printf 'kind=%s\n' "$kind"
	printf 'manifest=%s\n' "$manifest_row"
	printf 'extra=%s\n' "$extra"
	cat "$identity"
}

artifact_audit_cache_valid() {
	local record_file="$1"
	local expected_key="$2"
	local actual_hash="$3"
	local identity_hash="$4"
	local expected_needed="$5"
	local needed index last_index
	local -a record=()
	local -a expected_needed_lines=()
	[[ -f "$record_file" ]] || return 1
	mapfile -t record <"$record_file" || return 1
	if ((${#record[@]} < 7)); then return 1; fi
	last_index=$((${#record[@]} - 1))
	if [[ "${record[0]}" != "format=liber-image-audit-cache-v3" || "${record[1]}" != "schema=elf64-et-dyn-needed-wx-identity-record-v1" || "${record[2]}" != "build-key=$expected_key" || "${record[3]}" != "elf=$actual_hash" || "${record[4]}" != "identity=$identity_hash" || "${record[5]}" != "needed" || "${record[$last_index]}" != "end" ]]; then return 1; fi
	while IFS= read -r needed; do
		[[ -n "$needed" ]] || continue
		expected_needed_lines+=("$needed")
	done <<<"$expected_needed"
	if ((${#expected_needed_lines[@]} != last_index - 6)); then return 1; fi
	for ((index = 0; index < ${#expected_needed_lines[@]}; index += 1)); do
		if [[ "${record[$((index + 6))]}" != "${expected_needed_lines[$index]}" ]]; then return 1; fi
	done
}

record_artifact_audit() {
	local record_file="$1"
	local expected_key="$2"
	local actual_hash="$3"
	local identity_hash="$4"
	local expected_needed="$5"
	{
		printf 'format=liber-image-audit-cache-v3\n'
		printf 'schema=elf64-et-dyn-needed-wx-identity-record-v1\n'
		printf 'build-key=%s\n' "$expected_key"
		printf 'elf=%s\n' "$actual_hash"
		printf 'identity=%s\n' "$identity_hash"
		printf 'needed\n'
		if [[ -n "$expected_needed" ]]; then printf '%s\n' "$expected_needed"; fi
		printf 'end\n'
	} >"$record_file.tmp.$$"
	mv "$record_file.tmp.$$" "$record_file"
}

artifact_cache_valid() {
	local out="$1"
	local cache_prefix="$2"
	local expected_key="$3"
	local expected_identity="$4"
	local expected_needed="$5"
	local actual_needed actual_hash identity_hash program_headers dynamic_section
	[[ -f "$out" && -f "$cache_prefix.build-key" && -f "$cache_prefix.sha256" ]] || return 1
	[[ "$(<"$cache_prefix.build-key")" == "$expected_key" ]] || return 1
	actual_hash="$(build_file_digest "$out")" || return 1
	[[ "$(<"$cache_prefix.sha256")" == "$actual_hash" ]] || return 1
	verify_identity_note "$out" "$expected_identity" || return 1
	identity_hash="$(build_file_digest "$expected_identity")" || return 1
	if artifact_audit_cache_valid "$cache_prefix.audit" "$expected_key" "$actual_hash" "$identity_hash" "$expected_needed"; then return 0; fi
	matches_output 'Type:.*DYN' llvm-readelf -h "$out" || return 1
	actual_needed="$(llvm-readelf -d "$out" 2>/dev/null | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)" || return 1
	[[ "$actual_needed" == "$expected_needed" ]] || return 1
	program_headers="$(llvm-readelf -l "$out")" || return 1
	! grep -q 'INTERP' <<<"$program_headers" || return 1
	dynamic_section="$(llvm-readelf -d "$out")" || return 1
	! grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)' <<<"$dynamic_section" || return 1
	llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ {bad=1} END {exit bad}' || return 1
	verify_identity_note "$out" "$expected_identity" || return 1
	record_artifact_audit "$cache_prefix.audit" "$expected_key" "$actual_hash" "$identity_hash" "$expected_needed"
}

record_artifact_cache() {
	local out="$1"
	local cache_prefix="$2"
	local key="$3"
	local inputs="$4"
	mkdir -p "$artifact_cache_dir"
	printf '%s\n' "$key" >"$cache_prefix.build-key.tmp"
	build_file_hashes[$out]="$(sha256sum "$out" | awk '{print $1}')"
	printf '%s\n' "${build_file_hashes[$out]}" >"$cache_prefix.sha256.tmp"
	mv "$cache_prefix.build-key.tmp" "$cache_prefix.build-key"
	mv "$cache_prefix.sha256.tmp" "$cache_prefix.sha256"
	mv "$inputs" "$cache_prefix.inputs"
	rm -f "$cache_prefix.audit-key" "$cache_prefix.audit"
}

object_cache_record() {
	local consumer="$1"
	local package="$2"
	local source_sha="$3"
	local providers="$4"
	local provider
	printf 'format=liber-image-object-inputs-v1\n'
	printf 'compile-tool=%s\n' "$object_tool_digest"
	printf 'cargo-config=%s\n' "$image_target_config_value"
	printf 'consumer=%s\n' "$consumer"
	printf 'package=%s\n' "$package"
	printf 'source=%s\n' "$source_sha"
	printf 'features=shared-image\n'
	for provider in $providers; do
		printf 'provider-api=%s:%s\n' "$provider" "${provider_compile_digests[$provider]}"
	done
}

object_cache_valid() {
	local object="$1"
	local cache_prefix="$2"
	local expected_key="$3"
	local actual_hash definitions
	[[ -f "$object" && -f "$cache_prefix.build-key" && -f "$cache_prefix.sha256" ]] || return 1
	[[ "$(cat "$cache_prefix.build-key")" == "$expected_key" ]] || return 1
	actual_hash="$(build_file_digest "$object")" || return 1
	[[ "$(cat "$cache_prefix.sha256")" == "$actual_hash" ]] || return 1
	matches_output 'Type:.*REL' llvm-readelf -h "$object" || return 1
	definitions="$(llvm-readelf --wide --symbols "$object" | awk '$5 == "GLOBAL" && $7 != "UND" && $8 != "" {print $8}' | sort -u)"
	[[ "$definitions" == __user_main ]]
}

record_object_cache() {
	local object="$1"
	local cache_prefix="$2"
	local key="$3"
	local inputs="$4"
	printf '%s\n' "$key" >"$cache_prefix.build-key.tmp"
	build_file_hashes[$object]="$(sha256sum "$object" | awk '{print $1}')"
	printf '%s\n' "${build_file_hashes[$object]}" >"$cache_prefix.sha256.tmp"
	mv "$cache_prefix.build-key.tmp" "$cache_prefix.build-key"
	mv "$cache_prefix.sha256.tmp" "$cache_prefix.sha256"
	mv "$inputs" "$cache_prefix.inputs"
}

object_reference_valid() {
	local reference="$1"
	local object="$2"
	local key="$3"
	local cache_prefix="$4"
	local object_hash object_bytes
	local -a record=()
	[[ -f "$reference" && -f "$object" && -f "$cache_prefix.build-key" && -f "$cache_prefix.sha256" ]] || return 1
	[[ "$(<"$cache_prefix.build-key")" == "$key" ]] || return 1
	object_hash="$(<"$cache_prefix.sha256")"
	object_bytes="$(stat -c %s "$object")"
	mapfile -t record <"$reference" || return 1
	[[ "${#record[@]}" == 5 && "${record[0]}" == "format=liber-image-object-reference-v1" && "${record[1]}" == "key=$key" && "${record[2]}" == "file=$(basename "$object")" && "${record[3]}" == "sha256=$object_hash" && "${record[4]}" == "bytes=$object_bytes" ]]
}

record_object_reference() {
	local reference="$1"
	local object="$2"
	local key="$3"
	local cache_prefix="$4"
	{
		printf 'format=liber-image-object-reference-v1\n'
		printf 'key=%s\n' "$key"
		printf 'file=%s\n' "$(basename "$object")"
		printf 'sha256=%s\n' "$(<"$cache_prefix.sha256")"
		printf 'bytes=%s\n' "$(stat -c %s "$object")"
	} >"$reference.tmp.$$"
	mv "$reference.tmp.$$" "$reference"
}

# Per-artifact fast state for the full graph. Proving a warm graph from scratch dumps the
# identity note out of every staged ELF, re-derives every cache key and re-audits the whole
# provider export graph, so concluding that nothing moved costs tens of seconds of process
# spawns. A state records the plan edges that decide one artifact next to a stat signature
# for each file feeding it, so an unchanged artifact is settled by string comparison against
# a single batched stat of the entire graph. The targeted path keeps its own state; this one
# only serves the full build, where the whole-image snapshot is all-or-nothing and any single
# edit drops every artifact back onto the slow proof.
declare -A artifact_state_header=()
declare -A artifact_state_entries=()
declare -A artifact_state_result=()
declare -A artifact_state_stat=()
declare -A artifact_state_hit=()
artifact_state_plan=""
artifact_state_enabled=0
artifact_state_misses=0

load_artifact_states() {
	local key rest
	local -a paths=()
	artifact_state_enabled=1
	# Split on the first tab by hand. Manifest rows carry tabs and can end in one, and `read`
	# with a tab IFS would trim the trailing field away and never match the row it recorded.
	while IFS= read -r rest; do
		key="${rest%%$'\t'*}"
		rest="${rest#*$'\t'}"
		case "$rest" in
		entry$'\t'*) artifact_state_entries[$key]+="${rest#entry$'\t'}"$'\n' ;;
		result$'\t'*) artifact_state_result[$key]="${rest#result$'\t'}" ;;
		*) artifact_state_header[$key]+="$rest"$'\n' ;;
		esac
	done < <(awk 'FNR == 1 { key = FILENAME; sub(/^.*\/state-/, "", key) } { print key "\t" $0 }' "$artifact_cache_dir"/state-* 2>/dev/null)
	if ((${#artifact_state_entries[@]} == 0)); then return 0; fi
	mapfile -t paths < <(printf '%s' "${artifact_state_entries[@]}" | cut -f1 | sort -u)
	while IFS= read -r rest; do
		artifact_state_stat[${rest%%$'\t'*}]="$rest"
	done < <(stat -Lc $'%n\t%F\t%s\t%i\t%y\t%z' -- "${paths[@]}" 2>/dev/null)
	return 0
}

# A provider that did not prove itself invalidates its consumers: a relinked provider changes
# the identity digest its consumers embed, and this comparison never reads the staged bytes.
artifact_state_valid() {
	local key="$1"
	local header="$2"
	local providers="$3"
	local line provider
	((artifact_state_enabled == 1)) || return 1
	# A forced rebuild still records fresh state, so the run after it is fast again; it just
	# refuses to trust any of it.
	[[ "$force_rebuild" == 0 ]] || return 1
	[[ -n "${artifact_state_entries[$key]:-}" ]] || return 1
	[[ "${artifact_state_header[$key]:-}" == "$header" ]] || return 1
	for provider in $providers; do
		[[ -n "${artifact_state_hit[$provider]:-}" ]] || return 1
	done
	while IFS= read -r line; do
		[[ -n "$line" ]] || continue
		[[ "${artifact_state_stat[${line%%$'\t'*}]:-}" == "$line" ]] || return 1
	done <<<"${artifact_state_entries[$key]}"
	return 0
}

write_artifact_state() {
	local key="$1"
	local header="$2"
	local result="$3"
	shift 3
	local file temporary path
	((artifact_state_enabled == 1)) || return 0
	file="$artifact_cache_dir/state-$key"
	temporary="$file.tmp.$$"
	{
		printf '%s' "$header"
		if [[ -n "$result" ]]; then printf 'result\t%s\n' "$result"; fi
		for path in "$@"; do
			if [[ -n "$path" && -e "$path" ]]; then printf '%s\n' "$path"; fi
		done | sort -u | xargs -r -d '\n' stat -Lc $'entry\t%n\t%F\t%s\t%i\t%y\t%z' --
	} >"$temporary"
	mv "$temporary" "$file"
}

manifest_library_row() {
	[[ -n "${library_rows[$1]:-}" ]] || return 1
	printf '%s\n' "${library_rows[$1]}"
}

audit_library_destinations() {
	local expected actual
	expected="$(jq -r '.libraries[].destination' <<<"$manifest_json" | sort)"
	actual="$(find "$provider_output_dir/lib" -type f -name '*.lslib' -printf 'lib/%P\n' 2>/dev/null | sort)"
	if [[ "$actual" != "$expected" ]]; then
		echo "build-shared: staged library paths differ from the manifest" >&2
		diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
		exit 1
	fi
}

audit_program_destinations() {
	local expected actual
	expected="$(jq -r '.programs[] | select(.linkage == "dynamic" and .stage == "volume") | .destination | sub("\\.lsexe$"; "")' <<<"$manifest_json" | sort)"
	actual="$(find "$artifact_output_root" -type f \( -path "$artifact_output_root/bin/*" -o -path "$artifact_output_root/libexec/*" \) -printf '%P\n' 2>/dev/null | sort)"
	if [[ "$actual" != "$expected" ]]; then
		echo "build-shared: staged program paths differ from the manifest" >&2
		diff -u <(printf '%s\n' "$expected") <(printf '%s\n' "$actual") >&2 || true
		exit 1
	fi
}

manifest_specs="$(jq -r '.libraries[] | "\(.name)=\(.owner)"' <<<"$manifest_json" | sort)"
requested_specs="$(for spec in "$@"; do if [[ "$spec" == *=* ]]; then printf '%s\n' "$spec"; else printf '%s=%s\n' "$spec" "$spec"; fi; done | sort)"
if [[ -z "$selected_artifact" && "$requested_specs" != "$manifest_specs" ]]; then
	echo "build-shared: requested libraries differ from the manifest" >&2
	diff -u <(printf '%s\n' "$manifest_specs") <(printf '%s\n' "$requested_specs") >&2 || true
	exit 1
fi
dynamic_rows() {
	jq -r --arg artifact "$selected_artifact" --arg kind "$selected_kind" '
		def depends_on($root; $name; $wanted):
			$name == $wanted or any($root.libraries[$name].providers[]?;
				depends_on($root; .; $wanted));
		. as $root | .programs[] |
		select(.linkage == "dynamic" and .stage == "volume" and .name != "dyn_probe") |
		select($artifact == "" or
			($kind == "program" and .name == $artifact) or
			($kind == "library" and any(.providers[]?; depends_on($root; .; $artifact)))) |
		["dynamic", .name, .owner, "volume", .destination, (.providers | join(" "))] | @tsv
	' <<<"$manifest_json" | sort -k2,2
}

build_file_hash_inventory() {
	local artifact file kind consumer crate stage providers record hash
	if [[ -n "$selected_artifact" ]]; then
		printf '%s\n' "${selected_specs[@]}" | sed 's/=.*//'
	else
		jq -r '.libraries[].name' <<<"$manifest_json"
	fi | while read -r artifact; do
		file="$(library_file "$artifact")"
		if [[ -f "$file" ]]; then printf '%s\0' "$file"; fi
	done
	while read -r kind consumer crate stage destination providers; do
		file="$artifact_output_root/${destination%.lsexe}"
		if [[ -f "$file" ]]; then printf '%s\0' "$file"; fi
	done < <(dynamic_rows)
	if [[ -z "$selected_artifact" ]] || { [[ "$selected_kind" == library ]] && matches_output '^pix=' printf '%s\n' "${selected_specs[@]}"; }; then
		file="$(program_file dyn_probe)"
		if [[ -f "$file" ]]; then printf '%s\0' "$file"; fi
	fi
	if [[ -n "$selected_artifact" ]]; then
		for consumer in "${selected_programs[@]}"; do
			find "$artifact_cache_dir" -maxdepth 1 -type f -name "object-$consumer-*.o" -print0 2>/dev/null || true
		done
	else
		find "$artifact_cache_dir" -maxdepth 1 -type f -name 'object-*.o' -print0 2>/dev/null || true
	fi
}

while IFS= read -r -d '' record; do
	hash="${record%% *}"
	file="${record#* }"
	file="${file# }"
	build_file_hashes[$file]="$hash"
done < <(build_file_hash_inventory | sort -z -u | xargs -0 -r sha256sum --zero)

timing_event graph start
image_graph_started=$SECONDS
image_graph=""
requested_artifact_names="$(printf '%s\n' "$@" | sed 's/=.*//')"
if grep -Fqx -- lsrt <<<"$requested_artifact_names"; then
	image_target="$build_root/image-cargo-$target"
	image_target_config="$build_root/image-cargo-$target.config"
	image_graph="$build_root/image-cargo-$target.jsonl"
	image_graph_errors="$build_root/image-cargo-$target.stderr"
	image_seed="$build_root/image-seed-$target.o"
	target_spec_digest="$(if [[ -f "$cargo_target" ]]; then sha256sum "$cargo_target" | awk '{print $1}'; else printf '%s' "$cargo_target" | sha256sum | awk '{print $1}'; fi)"
	image_target_config_value="$({
		printf 'format=liber-image-cargo-cache-v1\n'
		printf 'workspace=%s\n' "$root"
		printf 'rustc=%s\n' "$(rustc -vV | sha256sum | awk '{print $1}')"
		printf 'cargo=%s\n' "$(cargo -V)"
		printf 'target=%s\n' "$target"
		printf 'target-spec=%s\n' "$target_spec_digest"
		printf 'profile=release\n'
		printf 'rustflags=%s\n' "$rustflags"
		printf 'cargo-target-flags=%s\n' "${cargo_target_flags[*]}"
		printf 'build-std=core,alloc,compiler_builtins\n'
		printf 'build-std-features=compiler-builtins-mem\n'
		printf 'features=shared-image\n'
		for config in "$root/user/.cargo/config.toml" "$root/user/rust-toolchain.toml"; do
			if [[ -f "$config" ]]; then
				printf 'config=%s:%s\n' "${config#"$root/"}" "$(sha256sum "$config" | awk '{print $1}')"
			fi
		done
		for variable in AR CARGO_BUILD_RUSTC CARGO_BUILD_RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CC CFLAGS RUSTC RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN; do
			printf 'env-%s=%s\n' "$variable" "${!variable-}"
		done
	} | sha256sum | awk '{print $1}')"
	if [[ "$force_rebuild" == 1 || ! -f "$image_target_config" || "$(cat "$image_target_config")" != "$image_target_config_value" ]]; then
		verbose_log "build-shared: Cargo cache miss (global build configuration)"
		rm -rf "$image_target"
		mkdir -p "$(dirname "$image_target_config")"
		printf '%s\n' "$image_target_config_value" >"$image_target_config.tmp"
		mv "$image_target_config.tmp" "$image_target_config"
	else
		verbose_log "build-shared: Cargo cache hit (global build configuration)"
	fi
	service_seed="$build_root/image-services-seed-$target.o"
	service_seed_errors="$build_root/image-services-seed-$target.stderr"
	if [[ -n "$selected_artifact" ]]; then
		image_graph_key_file="$build_root/image-cargo-$target.graph-key-$selected_kind-$selected_artifact"
	else
		image_graph_key_file="$build_root/image-cargo-$target.graph-key"
	fi
	image_graph_source_digest="$({
		if [[ -n "$selected_artifact" ]]; then
			printf '%s\n' "${selected_specs[@]}" | sed 's/^[^=]*=//'
		else
			jq -r '.libraries[].owner' <<<"$manifest_json" | sort -u
		fi | while read -r crate; do
			crate_dir="$(source_path "$crate")"
			printf '%s=%s\n' "$crate_dir" "$(source_digest "$crate_dir")"
		done
		for source in "$root/user/build.rs" "$root/../product.conf"; do
			source_file_digest "$source"
		done
	} | sha256sum | awk '{print $1}')"
	image_graph_key="$({
		printf 'format=liber-image-graph-cache-v1\n'
		printf 'build-tools=%s\n' "$build_tool_digest"
		printf 'cargo-config=%s\n' "$image_target_config_value"
		printf 'provider-sources=%s\n' "$image_graph_source_digest"
	} | sha256sum | awk '{print $1}')"
	image_graph_valid=0
	if [[ "$force_rebuild" == 0 && -f "$image_graph_key_file" && "$(cat "$image_graph_key_file")" == "$image_graph_key" && -f "$image_graph" && -f "$image_graph_errors" && -f "$image_seed" && -f "$service_seed" && -f "$service_seed_errors" ]] && matches_output 'Type:.*REL' llvm-readelf -h "$image_seed" && matches_output 'Type:.*REL' llvm-readelf -h "$service_seed" && grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$image_graph_errors" && grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$image_graph_errors" && grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$service_seed_errors" && grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$service_seed_errors"; then
		image_graph_valid=1
		verbose_log "build-shared: Cargo image graph cache hit"
	else
		verbose_log "build-shared: Cargo image graph cache miss"
		rm -f "$image_seed" "$service_seed"
		set +e
		(
			cd "$root/$(source_path tools)"
			CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --release --target "$cargo_target" --bin date --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$image_seed"
		) >"$image_graph" 2>"$image_graph_errors"
		graph_status=$?
		set -e
		if [[ "$graph_status" != 101 || ! -f "$image_seed" ]] || ! matches_output 'Type:.*REL' llvm-readelf -h "$image_seed"; then
			echo "build-shared: Cargo image graph did not stop after emitting its ET_REL seed object" >&2
			exit 1
		fi
		if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$image_graph_errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$image_graph_errors"; then
			echo "build-shared: Cargo image graph failed outside the expected final-link shim boundary" >&2
			exit 1
		fi
		set +e
		(
			cd "$root/$(source_path services)"
			CARGO_TARGET_DIR="$image_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem rustc --release --target "$cargo_target" --bin component_host --no-default-features --features shared-image --message-format=json-render-diagnostics -- --emit="obj=$service_seed"
		) >>"$image_graph" 2>"$service_seed_errors"
		service_seed_status=$?
		set -e
		if [[ "$service_seed_status" != 101 || ! -f "$service_seed" ]] || ! matches_output 'Type:.*REL' llvm-readelf -h "$service_seed"; then
			echo "build-shared: services image graph did not stop after emitting its ET_REL seed object" >&2
			exit 1
		fi
		if ! grep -q 'duplicate symbol: __rustc::__rust_alloc_error_handler' "$service_seed_errors" || ! grep -q 'duplicate symbol: __rustc::__rust_no_alloc_shim_is_unstable_v2' "$service_seed_errors"; then
			echo "build-shared: services image graph failed outside the expected final-link shim boundary" >&2
			exit 1
		fi
		printf '%s\n' "$image_graph_key" >"$image_graph_key_file.tmp"
		mv "$image_graph_key_file.tmp" "$image_graph_key_file"
	fi
fi
image_graph_seconds=$((SECONDS - image_graph_started))
timing_event graph end

if [[ -z "$selected_artifact" ]]; then
	artifact_state_plan="$({
		printf 'format=liber-artifact-state-v1\n'
		printf 'target=%s\n' "$target"
		printf 'rustflags=%s\n' "$rustflags"
		printf 'cargo-target=%s\n' "$cargo_target"
		printf 'manifest=%s\n' "$manifest_digest"
		printf 'build-tools=%s\n' "$build_tool_digest"
		printf 'object-tool=%s\n' "$object_tool_digest"
		printf 'rustc-commit=%s\n' "$rustc_commit"
		printf 'cargo-config=%s\n' "${image_target_config_value:-standalone}"
	} | sha256sum | awk '{print $1}')"
	load_artifact_states
fi

graph_archive() {
	local crate_dir="$1"
	local package_prefix="path+file://$root/$crate_dir#"
	local archives
	archives="$(jq -r --arg prefix "$package_prefix" 'select(.reason == "compiler-artifact" and (.package_id | startswith($prefix))) | .filenames[] | select(endswith(".rlib"))' "$image_graph" | sort -u)"
	if [[ "$(wc -l <<<"$archives")" != 1 || -z "$archives" ]]; then
		echo "build-shared: Cargo image graph has no unique archive for $crate_dir" >&2
		exit 1
	fi
	printf '%s' "$archives"
}

declare -A canonical_order_cache=()
declare -A provider_dependencies=()
declare -A provider_symbols=()
declare -A provider_exports=()
declare -A provider_symbols_indexed=()
declare -A provider_export_audit_cache=()

build_provider_index() {
	local providers="$1"
	local provider file kind value symbol_key
	for provider in $providers; do
		if [[ -n "${provider_symbols_indexed[$provider]:-}" ]]; then continue; fi
		file="$(library_file "$provider")"
		if [[ ! -v "provider_dependencies[$provider]" ]]; then provider_dependencies[$provider]=""; fi
		while IFS=$'\t' read -r kind value; do
			case "$kind" in
			D) provider_dependencies[$provider]+=" ${value%.lslib}" ;;
			S)
				symbol_key="$provider|$value"
				if [[ -z "${provider_symbols[$symbol_key]:-}" ]]; then
					provider_symbols[$symbol_key]=1
					provider_exports[$provider]+="$value"$'\n'
				fi
				;;
			esac
		done < <(llvm-readelf --wide -d --dyn-syms "$file" | awk '
			/Shared library:/ {
				name = $0
				sub(/^.*Shared library: \[/, "", name)
				sub(/\].*$/, "", name)
				print "D\t" name
				next
			}
			$1 ~ /^[0-9]+:$/ && $7 != "UND" && ($5 == "GLOBAL" || $5 == "WEAK") && ($4 == "NOTYPE" || $4 == "OBJECT" || $4 == "FUNC") && ($6 == "DEFAULT" || $6 == "PROTECTED") && $8 != "" {print "S\t" $8}
		')
		provider_dependencies[$provider]="$(tr ' ' '\n' <<<"${provider_dependencies[$provider]}" | sed '/^$/d' | sort -u | xargs)"
		provider_symbols_indexed[$provider]=1
	done
}

canonical_provider_order() {
	local roots="$1"
	local cache_key name depth dependency dependencies candidate ready result
	local max_modules=64
	local max_depth=16
	cache_key="$(tr ' ' '\n' <<<"$roots" | sort -u | xargs)"
	if [[ -n "${canonical_order_cache[$cache_key]:-}" ]]; then
		printf '%s' "${canonical_order_cache[$cache_key]}"
		return
	fi
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
		[[ -n "$name" ]] || continue
		if ((depth >= max_depth)); then
			echo "build-shared: provider graph exceeds dependency depth $max_depth" >&2
			return 1
		fi
		if [[ -n "${depths[$name]:-}" ]] && ((${depths[$name]} >= depth)); then
			continue
		fi
		depths[$name]="$depth"
		if [[ -z "${artifact_available[$name]:-}" ]]; then
			echo "build-shared: canonical graph names unavailable provider $name" >&2
			return 1
		fi
		if [[ -v "provider_dependencies[$name]" ]]; then
			dependencies="${provider_dependencies[$name]}"
		else
			dependencies="$(llvm-readelf -d "$(library_file "$name")" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sed 's/\.lslib$//' | sort -u | xargs)"
			provider_dependencies[$name]="$dependencies"
		fi
		if [[ -z "${present[$name]:-}" ]]; then
			if ((${#present[@]} >= max_modules)); then
				echo "build-shared: provider graph exceeds module limit $max_modules" >&2
				return 1
			fi
			present[$name]=1
			edges[$name]="$dependencies"
		fi
		for dependency in $dependencies; do
			pending_names+=("$dependency")
			pending_depths+=($((depth + 1)))
		done
	done
	while ((${#order[@]} < ${#present[@]})); do
		candidate=""
		while read -r name; do
			[[ -n "$name" ]] || continue
			if [[ -n "${ordered[$name]:-}" ]]; then
				continue
			fi
			ready=1
			for dependency in ${edges[$name]}; do
				if [[ -z "${ordered[$dependency]:-}" ]]; then
					ready=0
					break
				fi
			done
			if [[ "$ready" == 1 ]]; then
				candidate="$name"
				break
			fi
		done < <(printf '%s\n' "${!present[@]}" | sort)
		if [[ -z "$candidate" ]]; then
			echo "build-shared: provider graph contains a cycle" >&2
			return 1
		fi
		order+=("$candidate")
		ordered[$candidate]=1
	done
	result="$(printf '%s.lslib\n' "${order[@]}")"$'\n'
	canonical_order_cache[$cache_key]="$result"
	printf '%s' "$result"
}

audit_provider_export_ownership() {
	local roots="$1"
	local cache_key order provider symbol owner
	cache_key="$(tr ' ' '\n' <<<"$roots" | sort -u | xargs)"
	if [[ -n "${provider_export_audit_cache[$cache_key]:-}" ]]; then return; fi
	order="$(canonical_provider_order "$roots")" || return 1
	# Index the closure this audit walks, not just its roots. Providers reached through NEEDED
	# entries export symbols too, and the graph is no longer indexed in full up front.
	local -a closure=()
	mapfile -t closure < <(sed 's/\.lslib$//' <<<"$order")
	build_provider_index "${closure[*]}"
	local -A owners=()
	while IFS= read -r provider; do
		provider="${provider%.lslib}"
		if [[ -z "${provider_symbols_indexed[$provider]:-}" ]]; then
			echo "build-shared: dynamic graph provider $provider has no export index" >&2
			return 1
		fi
		while IFS= read -r symbol; do
			[[ -n "$symbol" ]] || continue
			owner="${owners[$symbol]:-}"
			if [[ -n "$owner" && "$owner" != "$provider" ]]; then
				echo "build-shared: dynamic graph providers $owner and $provider both export $symbol" >&2
				return 1
			fi
			owners[$symbol]="$provider"
		done <<<"${provider_exports[$provider]:-}"
	done <<<"$order"
	provider_export_audit_cache[$cache_key]=1
}

timing_event providers start
provider_started=$SECONDS
artifacts=()
declare -A artifact_available=()
declare -A provider_compile_digests=()
for spec in "$@"; do
	if [[ "$spec" == *=* ]]; then
		artifact="${spec%%=*}"
		crate="${spec#*=}"
	else
		artifact="$spec"
		crate="$spec"
	fi
	crate_rust="${crate//-/_}"
	if [[ ! "$artifact" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ || "$artifact" == lib* ]]; then
		echo "build-shared: invalid LiberSystem library name '$artifact'" >&2
		exit 2
	fi
	row="$(manifest_library_row "$artifact")" || {
		echo "build-shared: $artifact has no unique library manifest row" >&2
		exit 1
	}
	read -r row_kind row_artifact row_crate row_stage row_destination row_features row_providers <<<"$row"
	if [[ "$row_kind" != library || "$row_artifact" != "$artifact" || "$row_crate" != "$crate" || "$row_stage" != volume || ! "$row_destination" =~ ^lib/[a-z0-9][a-z0-9_-]*/$artifact\.lslib$ || -z "$row_features" ]]; then
		echo "build-shared: $artifact invocation differs from its library manifest row" >&2
		exit 1
	fi
	crate_dir="$(source_path "$crate")"
	manifest="$crate_dir/Cargo.toml"
	if [[ ! -f "$manifest" ]]; then
		echo "build-shared: missing $manifest" >&2
		exit 1
	fi
	out="$(library_file "$artifact")"
	out_dir="$(dirname "$out")"
	mkdir -p "$out_dir"
	features=()
	if [[ "$row_features" != - ]]; then
		if [[ ! "$row_features" =~ ^[A-Za-z0-9_-]+(,[A-Za-z0-9_-]+)*$ ]]; then
			echo "build-shared: $artifact has invalid feature set '$row_features'" >&2
			exit 1
		fi
		if [[ "$(tr ',' '\n' <<<"$row_features" | sort | uniq -d | head -n1)" != "" ]]; then
			echo "build-shared: $artifact repeats a build feature" >&2
			exit 1
		fi
		features=(--no-default-features --features "$row_features")
	fi
	provider_source_sha="${crate_source_digests[$crate_dir]:-$(source_digest "$crate_dir")}"
	if [[ "$row_crate" == *-client-provider ]]; then
		provider_api_dir="$(source_path "${row_crate%-provider}")"
		provider_compile_source="${crate_source_digests[$provider_api_dir]:-$(source_digest "$provider_api_dir")}"
	else
		provider_compile_source="$provider_source_sha"
	fi
	provider_state_key="library-$artifact"
	provider_state_header="plan=$artifact_state_plan"$'\n'"row=$row"$'\n'"source=$provider_source_sha"$'\n'"compile-source=$provider_compile_source"$'\n'
	for provider in $row_providers; do
		provider_state_header+="provider=$provider:${provider_identity_digests[$provider]:-}:${provider_compile_digests[$provider]:-}"$'\n'
	done
	provider_state_identity=""
	provider_state_compile=""
	IFS=$'\t' read -r provider_state_identity provider_state_compile <<<"${artifact_state_result[$provider_state_key]:-}"
	if [[ -n "$provider_state_identity" && -n "$provider_state_compile" ]] && artifact_state_valid "$provider_state_key" "$provider_state_header" "$row_providers"; then
		provider_identity_digests[$artifact]="$provider_state_identity"
		provider_compile_digests[$artifact]="$provider_state_compile"
		artifact_state_hit[$artifact]=1
		((provider_cache_hits += 1))
		timing_event unit "provider:hit:$artifact"
		verbose_log "build-shared: provider cache hit $artifact"
		artifacts+=("$artifact")
		artifact_available[$artifact]=1
		continue
	fi
	((artifact_state_misses += 1))
	if [[ -n "$image_graph" ]]; then
		deps="$image_target/$target/release/deps"
		rlib="$(graph_archive "$crate_dir")"
	else
		(cd "$crate_dir" && CARGO_TARGET_DIR="$provider_cargo_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo "${cargo_target_flags[@]}" -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$cargo_target" --lib "${features[@]}")
		deps="$provider_cargo_target/$target/release/deps"
		rlib="$(find "$deps" -maxdepth 1 -name "lib${crate_rust}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	fi
	if [[ -z "$rlib" ]]; then
		echo "build-shared: no rlib produced for $crate" >&2
		exit 1
	fi
	provider_compile_digests[$artifact]="$({
		printf 'format=liber-provider-compile-identity-v1\n'
		printf 'source=%s\n' "$provider_compile_source"
		printf 'features=%s\n' "$row_features"
		for provider in $row_providers; do
			if [[ -z "${provider_compile_digests[$provider]:-}" ]]; then
				echo "build-shared: $artifact has no compile identity for provider $provider" >&2
				exit 1
			fi
			printf 'provider=%s:%s\n' "$provider" "${provider_compile_digests[$provider]}"
		done
	} | sha256sum | awk '{print $1}')"
	provider_expected_identity="$(mktemp "$build_root/identity-record.XXXXXX")"
	write_identity_record library "$artifact" "$crate" "$provider_source_sha" "$row_features" "$row_providers" "$provider_expected_identity"
	provider_expected_needed="$(for provider in $row_providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
	provider_cache_prefix="$artifact_cache_dir/library-$artifact"
	provider_cache_inputs="$provider_cache_prefix.inputs.$$.expected"
	artifact_cache_record library "$row" "$provider_expected_identity" "cargo=${image_target_config_value:-standalone} rlib=$(sha256sum "$rlib" | awk '{print $1}')" >"$provider_cache_inputs"
	provider_cache_key="$(sha256sum "$provider_cache_inputs" | awk '{print $1}')"
	if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$out" "$provider_cache_prefix" "$provider_cache_key" "$provider_expected_identity" "$provider_expected_needed"; then
		verbose_log "build-shared: provider cache hit $artifact"
		((provider_cache_hits += 1))
		timing_event unit "provider:hit:$artifact"
		provider_identity_digests[$artifact]="$(build_file_digest "$provider_expected_identity")"
		rm -f "$provider_expected_identity" "$provider_cache_inputs"
		write_artifact_state "$provider_state_key" "$provider_state_header" \
			"${provider_identity_digests[$artifact]}"$'\t'"${provider_compile_digests[$artifact]}" \
			"$out" "$provider_cache_prefix.build-key" "$provider_cache_prefix.sha256" \
			"$provider_cache_prefix.audit" "$rlib" "$image_graph"
		artifacts+=("$artifact")
		artifact_available[$artifact]=1
		continue
	fi
	verbose_log "build-shared: provider cache miss $artifact"
	((provider_cache_misses += 1))
	timing_event unit "provider:miss:$artifact"
	link_deps=()
	export_flags=()
	symbolic_flags=(-Bsymbolic)
	archives=("$rlib")
	link_inputs=()
	if [[ "$artifact" == "lsrt" ]]; then
		symbolic_flags=()
		archives=()
		for dependency in core alloc compiler_builtins abi rt; do
			archive="$(find "$deps" -maxdepth 1 -name "lib${dependency}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
			if [[ -z "$archive" ]]; then
				echo "build-shared: missing PIC archive $dependency for lsrt.lslib" >&2
				exit 1
			fi
			archives+=("$archive")
		done
		object_root="$out_dir/.objects-lsrt"
		rm -rf "$object_root"
		mkdir -p "$object_root"
		for archive in "${archives[@]}"; do
			archive_path="$(realpath "$archive")"
			archive_name="$(basename "$archive" .rlib)"
			mkdir -p "$object_root/$archive_name"
			(cd "$object_root/$archive_name" && llvm-ar x "$archive_path")
		done
		while IFS= read -r -d '' object; do
			llvm-objcopy --set-symbol-visibility=memcpy=default --set-symbol-visibility=memmove=default --set-symbol-visibility=memset=default --set-symbol-visibility=memcmp=default --set-symbol-visibility=__udivti3=default --set-symbol-visibility=__umodti3=default "$object"
			link_inputs+=("$object")
		done < <(find "$object_root" -name '*.o' -print0)
	else
		link_inputs=(--whole-archive "${archives[@]}" --no-whole-archive)
	fi
	case "$artifact" in
	deflate)
		miniz_archive="$(find "$deps" -maxdepth 1 -name 'libminiz_oxide-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		adler_archive="$(find "$deps" -maxdepth 1 -name 'libadler2-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$miniz_archive" || -z "$adler_archive" ]]; then
			echo "build-shared: missing miniz_oxide/adler2 archive for deflate.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$miniz_archive" "$adler_archive" --no-whole-archive)
		;;
	qoi)
		if [[ -n "$image_graph" ]]; then
			qoi_codec_archive="$(jq -r 'select(.reason == "compiler-artifact" and (.package_id | startswith("registry+") and endswith("#qoi@0.4.1"))) | .filenames[] | select(endswith(".rlib"))' "$image_graph" | sort -u)"
		else
			qoi_codec_archive="$(find "$deps" -maxdepth 1 -name 'libqoi-*.rlib' ! -samefile "$rlib" -print | while read -r candidate; do if ! matches_output 'pix.*RgbaImage' llvm-readelf --wide --symbols "$candidate"; then printf '%s\n' "$candidate"; fi; done | sort -u)"
		fi
		if [[ "$(wc -l <<<"$qoi_codec_archive")" != 1 ]]; then qoi_codec_archive=""; fi
		bytemuck_archive="$(find "$deps" -maxdepth 1 -name 'libbytemuck-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$qoi_codec_archive" || -z "$bytemuck_archive" ]]; then
			echo "build-shared: missing qoi/bytemuck archive for qoi.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$qoi_codec_archive" "$bytemuck_archive" --no-whole-archive)
		;;
	gif)
		weezl_archive="$(find "$deps" -maxdepth 1 -name 'libweezl-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$weezl_archive" ]]; then
			echo "build-shared: missing weezl archive for gif.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$weezl_archive" --no-whole-archive)
		;;
	jpeg)
		jpeg_encoder_archive="$(find "$deps" -maxdepth 1 -name 'libjpeg_encoder-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		zune_core_archive="$(find "$deps" -maxdepth 1 -name 'libzune_core-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		zune_jpeg_archive="$(find "$deps" -maxdepth 1 -name 'libzune_jpeg-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$jpeg_encoder_archive" || -z "$zune_core_archive" || -z "$zune_jpeg_archive" ]]; then
			echo "build-shared: missing JPEG engine archives for jpeg.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$jpeg_encoder_archive" "$zune_core_archive" "$zune_jpeg_archive" --no-whole-archive)
		;;
	webp)
		webp_archives=()
		for dependency in ai_byteorder_lite ai_image_webp ai_quick_error allocator_api2 equivalent foldhash hashbrown memchr no_std_io; do
			archive="$(find "$deps" -maxdepth 1 -name "lib${dependency}-*.rlib" -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
			if [[ -z "$archive" ]]; then
				echo "build-shared: missing $dependency archive for webp.lslib" >&2
				exit 1
			fi
			webp_archives+=("$archive")
		done
		link_inputs=(--whole-archive "$rlib" "${webp_archives[@]}" --no-whole-archive)
		;;
	mp3)
		nanomp3_archive="$(find "$deps" -maxdepth 1 -name 'libnanomp3-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$nanomp3_archive" ]]; then
			echo "build-shared: missing nanomp3 archive for mp3.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$nanomp3_archive" --no-whole-archive)
		;;
	vorbis)
		libm_archive="$(find "$deps" -maxdepth 1 -name 'liblibm-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
		if [[ -z "$libm_archive" ]]; then
			echo "build-shared: missing libm archive for vorbis.lslib" >&2
			exit 1
		fi
		link_inputs=(--whole-archive "$rlib" "$libm_archive" --no-whole-archive)
		;;
	esac
	expected_needed=""
	provider_count=0
	for provider in $row_providers; do
		if [[ "$provider" == "$artifact" || ! "$provider" =~ ^[A-Za-z0-9][A-Za-z0-9_-]*$ || "$provider" == lib* ]] || ! matches_line "$provider" printf '%s\n' "${artifacts[@]}"; then
			echo "build-shared: library $artifact names invalid or unavailable provider $provider" >&2
			exit 1
		fi
		if grep -qx "$provider.lslib" <<<"$expected_needed"; then
			echo "build-shared: library $artifact repeats provider $provider" >&2
			exit 1
		fi
		link_deps+=("$(library_file "$provider")")
		expected_needed+="$provider.lslib"$'\n'
		provider_count=$((provider_count + 1))
	done
	if [[ "$artifact" != lsrt && "$provider_count" == 0 ]]; then
		echo "build-shared: library $artifact has no direct providers" >&2
		exit 1
	fi
	link_deps+=(--no-allow-shlib-undefined)
	published_out="$out"
	out="$published_out.$$.candidate"
	rm -f "$out"
	"$lld" -flavor gnu -m "$emulation" -shared --hash-style=sysv "${symbolic_flags[@]}" --gc-sections "${export_flags[@]}" "${link_inputs[@]}" "${link_deps[@]}" -soname "$artifact.lslib" -o "$out"
	llvm-strip --strip-debug "$out"
	actual_needed="$(llvm-readelf -d "$out" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
	expected_needed="$(sort -u <<<"$expected_needed" | sed '/^$/d')"
	if [[ "$actual_needed" != "$expected_needed" ]]; then
		echo "build-shared: $out providers differ from its manifest: $actual_needed" >&2
		exit 1
	fi
	imports="$(llvm-readelf --wide --dyn-syms "$out" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u)"
	declare -A used_providers=()
	declare -A provider_closures=()
	closure_providers=""
	for provider in $row_providers; do
		provider_closures[$provider]="$(canonical_provider_order "$provider" | sed 's/\.lslib$//')"
		closure_providers+="${provider_closures[$provider]}"$'\n'
	done
	closure_providers="$(sort -u <<<"$closure_providers" | sed '/^$/d')"
	for symbol in $imports; do
		owner=""
		for provider in $closure_providers; do
			if llvm-readelf --wide --dyn-syms "$(library_file "$provider")" | awk -v symbol="$symbol" '$7 != "UND" && $8 == symbol {found=1} END {exit !found}'; then
				if [[ -n "$owner" ]]; then
					echo "build-shared: library $artifact import $symbol has duplicate providers $owner and $provider" >&2
					exit 1
				fi
				owner="$provider"
			fi
		done
		if [[ -z "$owner" ]]; then
			echo "build-shared: library $artifact import $symbol has no direct provider" >&2
			exit 1
		fi
		if grep -qw "$owner" <<<"$row_providers"; then
			used_providers[$owner]=1
		else
			owner_root=""
			for provider in $row_providers; do
				if grep -qx "$owner" <<<"${provider_closures[$provider]}"; then
					if [[ -n "$owner_root" ]]; then
						owner_root="ambiguous"
						break
					fi
					owner_root="$provider"
				fi
			done
			if [[ -n "$owner_root" && "$owner_root" != ambiguous ]]; then
				used_providers[$owner_root]=1
			fi
		fi
	done
	for provider in $row_providers; do
		if [[ -z "${used_providers[$provider]:-}" ]]; then
			echo "build-shared: library $artifact provider $provider satisfies no direct import" >&2
			exit 1
		fi
	done
	if [[ "$artifact" == "ipc-client" ]]; then
		actual_imports="$(llvm-readelf --wide --dyn-syms "$out" | awk '$7 == "UND" && $8 != "" {print $8}' | sort -u)"
		expected_imports="$(printf '%s\n' recv_vec_blocking resolve | sort)"
		if [[ "$actual_imports" != "$expected_imports" ]]; then
			echo "build-shared: $out has unexpected runtime imports: $actual_imports" >&2
			exit 1
		fi
		for symbol in $actual_imports; do
			count="$(llvm-readelf --wide --dyn-syms "$(library_file lsrt)" | awk -v symbol="$symbol" '$7 != "UND" && $8 == symbol { count++ } END { print count + 0 }')"
			if [[ "$count" != 1 ]]; then
				echo "build-shared: $symbol has $count providers in lsrt.lslib (expected 1)" >&2
				exit 1
			fi
		done
	fi
	if [[ "$artifact" == "lsrt" ]]; then
		alloc_shim="$(llvm-readelf --wide --dyn-syms "$out" | awk '$8 == "_RNvCshfEkAwg4zv6_7___rustc35___rust_no_alloc_shim_is_unstable_v2" {print $4}')"
		if [[ "$alloc_shim" != "FUNC" ]]; then
			echo "build-shared: lsrt allocator shim alias is not one function" >&2
			exit 1
		fi
		for intrinsic in __udivti3 __umodti3; do
			if [[ "$(llvm-readelf --wide --dyn-syms "$out" | awk -v symbol="$intrinsic" '$7 != "UND" && $8 == symbol {count++} END {print count+0}')" != 1 ]]; then
				echo "build-shared: lsrt does not export exactly one $intrinsic compiler intrinsic" >&2
				exit 1
			fi
		done
	fi
	if ! matches_output 'Type:.*DYN' llvm-readelf -h "$out"; then
		echo "build-shared: $out is not ET_DYN" >&2
		exit 1
	fi
	if llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ { bad = 1 } END { exit bad }'; then
		:
	else
		echo "build-shared: $out contains a writable executable segment" >&2
		exit 1
	fi
	emit_identity "$out" "$provider_expected_identity"
	mv "$out" "$published_out"
	out="$published_out"
	provider_identity_digests[$artifact]="$(build_file_digest "$provider_expected_identity")"
	record_artifact_cache "$out" "$provider_cache_prefix" "$provider_cache_key" "$provider_cache_inputs"
	rm -f "$provider_expected_identity"
	write_artifact_state "$provider_state_key" "$provider_state_header" \
		"${provider_identity_digests[$artifact]}"$'\t'"${provider_compile_digests[$artifact]}" \
		"$out" "$provider_cache_prefix.build-key" "$provider_cache_prefix.sha256" \
		"$provider_cache_prefix.audit" "$rlib" "$image_graph"
	echo "build-shared: $out ($(stat -c %s "$out") bytes)"
	artifacts+=("$artifact")
	artifact_available[$artifact]=1
done
# The export index and the ownership audit read only the staged provider ELFs. When every
# provider proved its bytes unchanged, the previous run already audited these exact files.
if ((artifact_state_misses > 0)); then
	build_provider_index "${artifacts[*]}"
	for artifact in "${artifacts[@]}"; do
		audit_provider_export_ownership "$artifact"
	done
fi
provider_seconds=$((SECONDS - provider_started))
timing_event providers end

if [[ -n "$image_graph" ]]; then
	timing_event consumers start
	consumer_started=$SECONDS
	start_obj="$build_root/exe-start-$target.o"
	"$root/tools/build-exe-start.sh" "$target" "$start_obj"
	dynamic_rows="$(dynamic_rows)"
	declare -A package_source_digests=()
	while read -r package; do
		[[ -n "$package" ]] || continue
		if [[ "$package" != tools ]]; then
			package_source_digests[$package]="$(local_dependency_source_digest "$(source_path "$package")" "$package")"
		fi
	done < <(awk '{print $3}' <<<"$dynamic_rows" | sort -u)
	manifest_tools="$(awk '$3 == "tools" {print $2}' <<<"$dynamic_rows")"
	cargo_tools="$(cd "$root/$(source_path tools)" && cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "tools") | .targets[] | select(.kind == ["bin"]) | .name' | sort)"
	if [[ -z "$selected_artifact" && "$manifest_tools" != "$cargo_tools" ]]; then
		echo "build-shared: tools-package bins differ from dynamic volume manifest rows" >&2
		diff -u <(printf '%s\n' "$cargo_tools") <(printf '%s\n' "$manifest_tools") >&2 || true
		exit 1
	fi
	duplicate_consumer="$(awk '{print $2}' <<<"$dynamic_rows" | uniq -d | head -n1)"
	if [[ -n "$duplicate_consumer" ]]; then
		echo "build-shared: duplicate dynamic executable $duplicate_consumer" >&2
		exit 1
	fi
	while read -r kind consumer crate stage destination providers; do
		if [[ "$kind" != dynamic || "$stage" != volume ]]; then
			continue
		fi
		if [[ -z "$providers" ]]; then
			echo "build-shared: dynamic $consumer has no direct providers" >&2
			exit 1
		fi
		provider_count="$(wc -w <<<"$providers")"
		providers="$(tr ' ' '\n' <<<"$providers" | sort -u | xargs)"
		if [[ "$(wc -w <<<"$providers")" != "$provider_count" ]]; then
			echo "build-shared: dynamic $consumer repeats a direct provider" >&2
			exit 1
		fi
		consumer_dir="$root/$(source_path "$crate")"
		out_dir="$(dirname "$artifact_output_root/${destination%.lsexe}")"
		consumer_errors="$artifact_log_dir/$consumer.stderr"
		out="$artifact_output_root/${destination%.lsexe}"
		mkdir -p "$out_dir"
		consumer_source_sha="$(executable_source_digest "$consumer_dir" "$crate" "$consumer")"
		consumer_state_key="executable-$consumer"
		consumer_state_header="plan=$artifact_state_plan"$'\n'"row=$kind $consumer $crate $stage $destination $providers"$'\n'"source=$consumer_source_sha"$'\n'
		for provider in $providers; do
			consumer_state_header+="provider=$provider:${provider_identity_digests[$provider]:-}:${provider_compile_digests[$provider]:-}"$'\n'
		done
		if artifact_state_valid "$consumer_state_key" "$consumer_state_header" "$providers"; then
			artifact_state_hit[$consumer]=1
			((executable_cache_hits += 1))
			timing_event unit "executable:hit:$consumer"
			verbose_log "build-shared: executable cache hit $consumer"
			continue
		fi
		((artifact_state_misses += 1))
		build_provider_index "$providers"
		audit_provider_export_ownership "$providers"
		consumer_expected_identity="$(mktemp "$build_root/identity-record.XXXXXX")"
		write_identity_record executable "$consumer" "$crate" "$consumer_source_sha" shared-image "$providers" "$consumer_expected_identity"
		consumer_expected_needed="$(for provider in $providers; do printf '%s.lslib\n' "$provider"; done | sort -u)"
		consumer_cache_prefix="$artifact_cache_dir/executable-$consumer"
		consumer_cache_inputs="$consumer_cache_prefix.inputs.$$.expected"
		artifact_cache_record executable "$kind $consumer $crate $stage $destination $providers" "$consumer_expected_identity" "cargo=$image_target_config_value start=$(sha256sum "$start_obj" | awk '{print $1}')" >"$consumer_cache_inputs"
		consumer_cache_key="$(sha256sum "$consumer_cache_inputs" | awk '{print $1}')"
		object_inputs="$(mktemp "$build_root/object-inputs.XXXXXX")"
		object_cache_record "$consumer" "$crate" "$consumer_source_sha" "$providers" >"$object_inputs"
		object_key="$(sha256sum "$object_inputs" | awk '{print $1}')"
		object_cache_prefix="$artifact_cache_dir/object-$consumer-$object_key"
		consumer_obj="$object_cache_prefix.o"
		object_reference="$consumer_cache_prefix.object"
		if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$out" "$consumer_cache_prefix" "$consumer_cache_key" "$consumer_expected_identity" "$consumer_expected_needed"; then
			canonical_provider_order "$providers" >/dev/null
			if ! object_reference_valid "$object_reference" "$consumer_obj" "$object_key" "$object_cache_prefix"; then
				object_cache_valid "$consumer_obj" "$object_cache_prefix" "$object_key" || {
					echo "build-shared: dynamic $consumer has no valid current ET_REL object" >&2
					exit 1
				}
				record_object_reference "$object_reference" "$consumer_obj" "$object_key" "$object_cache_prefix"
			fi
			verbose_log "build-shared: executable cache hit $consumer"
			((executable_cache_hits += 1))
			timing_event unit "executable:hit:$consumer"
			rm -f "$consumer_expected_identity" "$consumer_cache_inputs" "$object_inputs"
			write_artifact_state "$consumer_state_key" "$consumer_state_header" "" \
				"$out" "$consumer_cache_prefix.build-key" "$consumer_cache_prefix.sha256" \
				"$consumer_cache_prefix.audit" "$object_reference" "$consumer_obj" \
				"$object_cache_prefix.build-key" "$object_cache_prefix.sha256" "$start_obj"
			continue
		fi
		verbose_log "build-shared: executable cache miss $consumer"
		((executable_cache_misses += 1))
		timing_event unit "executable:miss:$consumer"
		if [[ "$force_rebuild" == 0 ]] && object_cache_valid "$consumer_obj" "$object_cache_prefix" "$object_key"; then
			verbose_log "build-shared: object cache hit $consumer"
			((object_cache_hits += 1))
			timing_event unit "object:hit:$consumer"
		else
			verbose_log "build-shared: object cache miss $consumer"
			((object_cache_misses += 1))
			timing_event unit "object:miss:$consumer"
			consumer_obj_tmp="$consumer_obj.tmp.$$"
			rm -f "$consumer_obj_tmp"
			"$root/tools/build-consumer-object.sh" "$consumer_dir" "$image_target" "$rust_min_stack" "$rustflags" "$cargo_target" "$consumer" "$consumer_obj_tmp" "$consumer_errors" "${cargo_target_flags[@]}"
			mv "$consumer_obj_tmp" "$consumer_obj"
			record_object_cache "$consumer_obj" "$object_cache_prefix" "$object_key" "$object_inputs"
			object_inputs=""
		fi
		record_object_reference "$object_reference" "$consumer_obj" "$object_key" "$object_cache_prefix"
		provider_inputs=()
		expected_needed=""
		for provider in $providers; do
			if ! matches_line "$provider" printf '%s\n' "${artifacts[@]}"; then
				echo "build-shared: dynamic $consumer names unavailable provider $provider" >&2
				exit 1
			fi
			provider_inputs+=("$(library_file "$provider")")
			expected_needed+="$provider.lslib"$'\n'
		done
		expected_needed="$(sort -u <<<"$expected_needed" | sed '/^$/d')"
		consumer_imports="$(llvm-readelf --wide --symbols "$consumer_obj" | awk '$5 == "GLOBAL" && $7 == "UND" && $8 != "" {print $8}' | sort -u)"
		build_provider_index "$providers"
		case "$consumer" in
		arp | httpd | ip | nc | nslookup | ping | ss | tcp)
			if ! grep -q '^liber_channel_liber_network_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete network client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_network_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete network client provider" >&2
				exit 1
			fi
			;;
		config | set)
			if ! grep -q '^liber_channel_liber_config_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete config client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_config_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete config client provider" >&2
				exit 1
			fi
			;;
		lsdev | lsusb)
			if ! grep -q '^liber_channel_liber_device_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete device client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_device_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete device client provider" >&2
				exit 1
			fi
			;;
		log)
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter' <<<"$consumer_imports"; then
				echo "build-shared: log contains a generic channel client implementation" >&2
				exit 1
			fi
			for domain in log time; do
				if ! grep -q "^liber_channel_liber_${domain}_" <<<"$consumer_imports"; then
					echo "build-shared: log does not import the concrete $domain client provider" >&2
					exit 1
				fi
				if grep -Eq "^liber_channel_impl_liber_${domain}_" <<<"$consumer_imports"; then
					echo "build-shared: log bypasses the concrete $domain client provider" >&2
					exit 1
				fi
			done
			;;
		date)
			if ! grep -q '^liber_channel_liber_time_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_time_' <<<"$consumer_imports"; then
				echo "build-shared: date bypasses the concrete time client provider" >&2
				exit 1
			fi
			;;
		lssvc)
			if ! grep -q '^liber_channel_liber_observability_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_observability_' <<<"$consumer_imports"; then
				echo "build-shared: lssvc bypasses the concrete observability client provider" >&2
				exit 1
			fi
			;;
		ps | run)
			if ! grep -q '^liber_channel_liber_process_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete process client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_process_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete process client provider" >&2
				exit 1
			fi
			if [[ "$consumer" == ps ]] && ! grep -q '^liber_channel_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: ps does not import the concrete resources client provider" >&2
				exit 1
			fi
			if [[ "$consumer" == ps ]] && grep -Eq '^liber_channel_impl_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: ps bypasses the concrete resources client provider" >&2
				exit 1
			fi
			;;
		usage)
			if ! grep -q '^liber_channel_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer does not import the concrete resources client provider" >&2
				exit 1
			fi
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_resources_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete resources client provider" >&2
				exit 1
			fi
			;;
		beep)
			if ! grep -q '^liber_channel_liber_audio_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_audio_' <<<"$consumer_imports"; then
				echo "build-shared: beep bypasses the concrete audio client provider" >&2
				exit 1
			fi
			;;
		play)
			for symbol in audio_open_stream pcm_stream_write pcm_stream_close; do
				if ! grep -q "^liber_channel_liber_audio_${symbol}$" <<<"$consumer_imports"; then
					echo "build-shared: play does not import concrete audio symbol $symbol" >&2
					exit 1
				fi
			done
			if grep -Eq '^liber_channel_impl_liber_audio_' <<<"$consumer_imports"; then
				echo "build-shared: play bypasses the concrete audio client provider" >&2
				exit 1
			fi
			if ! grep -q '^liber_channel_liber_storage_volume_open$' <<<"$consumer_imports" || grep -Eq '^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: play bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		cat)
			if ! grep -q '^liber_channel_liber_storage_volume_open$' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: cat bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		rm | mkdir | rmdir)
			method="$consumer"
			if [[ "$consumer" == rm ]]; then method=remove; fi
			if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		write)
			for phase in begin finish; do
				if ! grep -q "^liber_channel_liber_storage_volume_write_stream_${phase}$" <<<"$consumer_imports"; then
					echo "build-shared: write does not import concrete write-stream $phase" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: write bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		du | ls | lsvol)
			if ! grep -q '^liber_channel_liber_storage_volume_list$' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: $consumer bypasses the concrete volume list client" >&2
				exit 1
			fi
			;;
		lsblk)
			for method in status capacity; do
				if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports"; then
					echo "build-shared: lsblk does not import concrete volume symbol $method" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: lsblk bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		snap)
			for method in snap_create snap_list snap_delete snap_open; do
				if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports"; then
					echo "build-shared: snap does not import concrete volume symbol $method" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: snap bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		volume)
			for method in status set_compression fsck restore; do
				if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports"; then
					echo "build-shared: volume does not import concrete volume symbol $method" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: volume bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		imgconv)
			for method in open write; do
				if ! grep -q "^liber_channel_liber_storage_volume_${method}$" <<<"$consumer_imports"; then
					echo "build-shared: imgconv does not import concrete volume symbol $method" >&2
					exit 1
				fi
			done
			if grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: imgconv bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		imgview)
			if ! grep -q '^liber_channel_liber_storage_volume_open$' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_storage_' <<<"$consumer_imports"; then
				echo "build-shared: imgview bypasses the concrete volume client provider" >&2
				exit 1
			fi
			;;
		perm)
			if ! grep -q '^liber_channel_liber_security_' <<<"$consumer_imports" || grep -Eq 'ChannelClient|ChannelTransport|VecWriter|^liber_channel_impl_liber_security_' <<<"$consumer_imports"; then
				echo "build-shared: perm bypasses the concrete security client provider" >&2
				exit 1
			fi
			;;
		esac
		declare -A used_consumer_providers=()
		for symbol in $consumer_imports; do
			count=0
			owner=""
			for provider in $providers; do
				if [[ -n "${provider_symbols["$provider|$symbol"]:-}" ]]; then
					((count += 1))
					owner="$provider"
				fi
			done
			if [[ "$count" != 1 ]]; then
				echo "build-shared: $consumer import $symbol has $count declared providers (expected 1)" >&2
				exit 1
			fi
			used_consumer_providers[$owner]=1
		done
		for provider in $providers; do
			if [[ -z "${used_consumer_providers[$provider]:-}" ]]; then
				echo "build-shared: dynamic $consumer provider $provider satisfies no direct import" >&2
				exit 1
			fi
		done
		published_out="$out"
		out="$published_out.$$.candidate"
		rm -f "$out"
		"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv --gc-sections --build-id=none -e _start "$start_obj" "$consumer_obj" "${provider_inputs[@]}" --no-allow-shlib-undefined -o "$out"
		if ! matches_output 'Type:.*DYN' llvm-readelf -h "$out"; then
			echo "build-shared: $out is not ET_DYN" >&2
			exit 1
		fi
		actual_needed="$(llvm-readelf -d "$out" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)"
		if [[ "$actual_needed" != "$expected_needed" ]]; then
			echo "build-shared: $out providers differ from its manifest: $actual_needed" >&2
			exit 1
		fi
		canonical_provider_order "$providers" >/dev/null
		consumer_program_headers="$(llvm-readelf -l "$out")"
		consumer_dynamic_section="$(llvm-readelf -d "$out")"
		if grep -q 'INTERP' <<<"$consumer_program_headers" || grep -Eq '\((RPATH|RUNPATH|TEXTREL)\)' <<<"$consumer_dynamic_section"; then
			echo "build-shared: $out has a forbidden dynamic-loader contract" >&2
			exit 1
		fi
		if ! llvm-readelf -l "$out" | awk '$1 == "LOAD" && $0 ~ /W/ && $0 ~ /E/ { bad = 1 } END { exit bad }'; then
			echo "build-shared: $out contains a writable executable segment" >&2
			exit 1
		fi
		forbidden_definitions="$(llvm-readelf --wide --symbols "$out" | awk '$7 != "UND" && $8 ~ /^(__rust_alloc|__rust_dealloc|rust_begin_unwind|memcpy|memmove|memset|memcmp|liber_rt_start|print|inherit_stdout)$/ {print $8}')"
		if [[ -n "$forbidden_definitions" ]]; then
			echo "build-shared: $out contains runtime/provider definitions: $forbidden_definitions" >&2
			exit 1
		fi
		llvm-strip --strip-debug "$out"
		emit_identity "$out" "$consumer_expected_identity"
		mv "$out" "$published_out"
		out="$published_out"
		record_artifact_cache "$out" "$consumer_cache_prefix" "$consumer_cache_key" "$consumer_cache_inputs"
		if [[ -n "$object_inputs" ]]; then rm -f "$object_inputs"; fi
		rm -f "$consumer_expected_identity"
		write_artifact_state "$consumer_state_key" "$consumer_state_header" "" \
			"$out" "$consumer_cache_prefix.build-key" "$consumer_cache_prefix.sha256" \
			"$consumer_cache_prefix.audit" "$object_reference" "$consumer_obj" \
			"$object_cache_prefix.build-key" "$object_cache_prefix.sha256" "$start_obj"
		echo "build-shared: $out ($(stat -c %s "$out") bytes, PIE)"
	done <<<"$dynamic_rows"
	consumer_seconds=$((SECONDS - consumer_started))
	timing_event consumers end
fi

if matches_line pix printf '%s\n' "${artifacts[@]}"; then
	probe="$(source_path dyn_probe)"
	probe_out="$(program_file dyn_probe)"
	probe_dir="$(dirname "$probe_out")"
	mkdir -p "$probe_dir"
	probe_source_sha="$(source_digest "$probe")"
	probe_expected_identity="$(mktemp "$build_root/identity-record.XXXXXX")"
	write_identity_record executable dyn_probe dyn_probe "$probe_source_sha" - "pix lsrt" "$probe_expected_identity"
	probe_expected_needed="$(printf '%s\n' pix.lslib lsrt.lslib | sort -u)"
	audit_provider_export_ownership "pix lsrt"
	probe_cache_prefix="$artifact_cache_dir/executable-dyn_probe"
	probe_cache_inputs="$probe_cache_prefix.inputs.$$.expected"
	artifact_cache_record executable "dynamic dyn_probe dyn_probe volume libexec/dyn_probe.lsexe pix lsrt" "$probe_expected_identity" "cargo=$image_target_config_value" >"$probe_cache_inputs"
	probe_cache_key="$(sha256sum "$probe_cache_inputs" | awk '{print $1}')"
	if [[ "$force_rebuild" == 0 ]] && artifact_cache_valid "$probe_out" "$probe_cache_prefix" "$probe_cache_key" "$probe_expected_identity" "$probe_expected_needed"; then
		canonical_provider_order "pix lsrt" >/dev/null
		verbose_log "build-shared: executable cache hit dyn_probe"
		rm -f "$probe_expected_identity" "$probe_cache_inputs"
		if [[ -z "$selected_artifact" ]]; then
			audit_library_destinations
			audit_program_destinations
		fi
		exit 0
	fi
	verbose_log "build-shared: executable cache miss dyn_probe"
	(cd "$probe" && CARGO_TARGET_DIR="$provider_cargo_target" RUST_MIN_STACK="$rust_min_stack" RUSTFLAGS="$rustflags" cargo -Z build-std=core,alloc,compiler_builtins -Z build-std-features=compiler-builtins-mem build --quiet --release --target "$target" --lib)
	probe_rlib="$(find "$provider_cargo_target/$target/release/deps" -maxdepth 1 -name 'libdyn_probe-*.rlib' -printf '%T@ %p\n' | sort -nr | head -n1 | cut -d' ' -f2-)"
	probe_candidate="$probe_out.$$.candidate"
	rm -f "$probe_candidate"
	"$lld" -flavor gnu -m "$emulation" -pie --no-dynamic-linker --hash-style=sysv -e _start --whole-archive "$probe_rlib" --no-whole-archive "$(library_file pix)" "$(library_file lsrt)" --no-allow-shlib-undefined -o "$probe_candidate"
	llvm-strip --strip-debug "$probe_candidate"
	if ! matches_output 'Type:.*DYN' llvm-readelf -h "$probe_candidate" || ! matches_output 'NEEDED.*pix.lslib' llvm-readelf -d "$probe_candidate"; then
		echo "build-shared: $probe_candidate is not a pix.lslib-linked ET_DYN" >&2
		exit 1
	fi
	emit_identity "$probe_candidate" "$probe_expected_identity"
	mv "$probe_candidate" "$probe_out"
	record_artifact_cache "$probe_out" "$probe_cache_prefix" "$probe_cache_key" "$probe_cache_inputs"
	rm -f "$probe_expected_identity"
	echo "build-shared: $probe_out ($(stat -c %s "$probe_out") bytes)"
fi

if [[ -z "$selected_artifact" ]]; then
	audit_library_destinations
	audit_program_destinations
fi
