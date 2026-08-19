#!/usr/bin/env bash
# The dynamic-report checker, tested against a disposable mini-root.
#
# WHY THIS EXISTS. `check-dynamic-report.sh` decides whether the tracked provider/consumer reports
# still describe the tree, and it used to prove itself by RE-INVOKING ITSELF with an environment
# override - which regenerated every report, swept the whole ELF graph a second time, accepted any
# nonzero status as proof, and mutated only one of the three reports. That is a self-test that is
# expensive, that can pass because the script failed to start, and that never showed the other two
# comparisons refusing anything.
#
# So the gate's own behaviour is tested here instead, from outside, against a fixture: a mini-root
# with its own `lib.sh`, manifest shim, `llvm-readelf` shim and three-target graph of two tools. Every
# case asserts an EXIT STATUS from the closed contract and, where it is the point, the number of
# `llvm-readelf` calls - because "inventory does no ELF work" is a claim about calls, not about time.
#
# It makes zero real `llvm-readelf` calls, never reads the production `.build/image`, and never
# writes a tracked report: `--write` is exercised against the fixture's own copies. The tracked TSVs
# and `lib.sh` are hashed before and after the matrix and required to be unchanged.
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
repo="$(cd "$root/.." && pwd)"
# The checker under test. Defaults to the tracked one, which is what the gate runs; an explicit path
# is for developing a replacement against this fixture before it is installed.
checker="${1:-$root/tools/check-dynamic-report.sh}"

failures=0
cases=0

note() { printf 'dynamic-report-regressions: %s\n' "$1"; }
fail() {
	printf 'dynamic-report-regressions: FAIL - %s\n' "$1" >&2
	failures=$((failures + 1))
}

# The tracked files this must not touch.
tracked=("$repo/docs/DYNAMIC_EXECUTABLES.tsv" "$repo/docs/DYNAMIC_WAVES.tsv" "$repo/docs/DYNAMIC_IMAGE.tsv" "$repo/lib.sh")
before_hashes="$(sha256sum "${tracked[@]}")"

fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

TARGETS=(x86_64-unknown-none aarch64-unknown-none riscv64gc-unknown-none-elf)
# ONE tool, in wave 1. Five of the six waves are therefore EMPTY, which is deliberate: the wave and
# image aggregations read their accumulators per wave, and an accumulator only exists once a tool has
# landed in that wave - so an empty wave used to kill the generator with `unbound variable` under
# `set -u`. In the real tree every wave has tools and it never fired. One tool also keeps this gate
# to a few seconds: the checker spawns `jq` and `llvm-readelf` per row per target, and the matrix
# runs eight full generations.
TOOLS=(echo)

# --- the mini-root -------------------------------------------------------------------------------

mkdir -p "$fixture/src/tools" "$fixture/docs" "$fixture/bin"
cp "$checker" "$fixture/src/tools/check-dynamic-report.sh"
chmod +x "$fixture/src/tools/check-dynamic-report.sh"

# `lib.sh`, with only what the checker reads out of it.
cat >"$fixture/lib.sh" <<'LIB'
declare -A TOOL_WAVES=()
TOOL_WAVES[echo]=1
declare -A WAVE_TAGS=()
WAVE_TAGS[1]='service'
WAVE_TAGS[2]='storage'
WAVE_TAGS[3]='service'
WAVE_TAGS[4]='service'
WAVE_TAGS[5]='service'
WAVE_TAGS[6]='service'
LIB

# The manifest, as a shim rather than a cargo subprocess: this gate is about the checker, and a
# manifest exporter that has to be built is a dependency it does not need.
manifest_json='{
 "sources": {"tools": {"path": "user/apps/tools"}},
 "libraries": {"liba": {"name": "liba", "destination": "lib/liba.lslib", "providers": []}},
 "programs": {
  "echo": {"name": "echo", "owner": "tools", "role": "tool", "linkage": "dynamic", "stage": "volume", "destination": "bin/echo.lsexe", "providers": ["liba"]}
 }
}'
printf '%s\n' "$manifest_json" >"$fixture/manifest.json"
cat >"$fixture/src/tools/system-manifest.sh" <<'MANIFEST'
#!/usr/bin/env bash
cat "$(dirname "$0")/../../manifest.json"
MANIFEST
chmod +x "$fixture/src/tools/system-manifest.sh"

# The artifacts and ET_REL references the generator reads. Their CONTENT does not matter - the shim
# below answers every question about them - but their presence and recorded sizes do.
for target in "${TARGETS[@]}"; do
	mkdir -p "$fixture/.build/image/$target/bin" "$fixture/.build/image/$target/lib" "$fixture/.build/cache/$target"
	printf 'provider\n' >"$fixture/.build/image/$target/lib/liba.lslib"
	for tool in "${TOOLS[@]}"; do
		printf 'executable\n' >"$fixture/.build/image/$target/bin/$tool"
		key="$(printf '%s' "$target$tool" | sha256sum | awk '{print $1}')"
		object="$fixture/.build/cache/$target/object-$tool-$key.o"
		printf 'object\n' >"$object"
		printf '%s' "$key" >"${object%.o}.build-key"
		hash="$(sha256sum "$object" | awk '{print $1}')"
		printf '%s' "$hash" >"${object%.o}.sha256"
		{
			printf 'format=liber-image-object-reference-v1\n'
			printf 'key=%s\n' "$key"
			printf 'file=object-%s-%s.o\n' "$tool" "$key"
			printf 'sha256=%s\n' "$hash"
			printf 'bytes=%s\n' "$(stat -c %s "$object")"
		} >"$fixture/.build/cache/$target/executable-$tool.object"
	done
done

# The `llvm-readelf` shim: plausible output for each question the generator asks, one line per call
# in a counter file, and an optional failure mode.
cat >"$fixture/bin/llvm-readelf" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SHIM_CALLS"
# WHICH QUESTION FAILS, not "the first one asked".
#
# `SHIM_FAIL=1` failed every invocation, so the run died on the very first `-h` and the check passed
# without any of the segment or export readers ever being reached - which is exactly where the
# swallowed-status defect lived. Every one of them fed a loop from `done < <(llvm-readelf ...)`,
# whose exit status no bash setting propagates, and a reader that printed one plausible LOAD line
# and then died produced a metric of 4096 with status 0. The test that was supposed to catch that
# was green for a different reason, which is worse than not having it.
#
# `SHIM_FAIL` now names the flag whose read must fail, so each reader is proved separately.
if [[ -n "${SHIM_FAIL:-}" ]]; then
	if [[ "${SHIM_FAIL}" == 1 || "$*" == *"${SHIM_FAIL}"* ]]; then
		# Plausible partial output, then a failure - the shape that used to be read as a metric.
		printf '  LOAD           0x000000 0x0000000000000000 0x0000000000000000 0x001000 0x001000 RW  0x1000\n'
		exit 3
	fi
fi
case "$*" in
*-h*) printf '  Type:                              REL (Relocatable file)\n' ;;
*--wide\ --symbols*) printf '     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT      1 __user_main\n' ;;
*--dyn-syms*)
	if [[ "$*" == *liba.lslib* ]]; then
		printf '     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT      1 liber_start\n'
	else
		printf '     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT    UND liber_start\n'
	fi
	;;
*-dW*) printf ' 0x0000000000000001 (NEEDED)             Shared library: [liba.lslib]\n' ;;
*-lW*)
	printf '  LOAD           0x000000 0x0000000000000000 0x0000000000000000 0x001000 0x001000 R E 0x1000\n'
	printf '  LOAD           0x002000 0x0000000000002000 0x0000000000002000 0x001000 0x001000 RW  0x1000\n'
	;;
esac
SHIM
chmod +x "$fixture/bin/llvm-readelf"

export SHIM_CALLS="$fixture/readelf-calls"
: >"$SHIM_CALLS"

run() {
	: >"$SHIM_CALLS"
	PATH="$fixture/bin:$PATH" "$fixture/src/tools/check-dynamic-report.sh" "$@" >"$fixture/out" 2>"$fixture/err"
}

calls() { wc -l <"$SHIM_CALLS"; }

expect_status() {
	local what="$1" wanted="$2" got="$3"
	cases=$((cases + 1))
	[[ "$got" == "$wanted" ]] || fail "$what: status $got, expected $wanted ($(head -c 200 "$fixture/err" | tr '\n' ' '))"
}

expect_calls() {
	local what="$1" wanted="$2" got
	got="$(calls)"
	cases=$((cases + 1))
	[[ "$got" == "$wanted" ]] || fail "$what: $got llvm-readelf calls, expected $wanted"
}

# --- the matrix ----------------------------------------------------------------------------------

# Usage errors do NO work: no manifest read, no temporary file, no ELF call. The old script ran a
# full recursive check before it looked at the mode at all.
run --bogus
expect_status 'an unknown mode' 2 "$?"
expect_calls 'an unknown mode' 0
run --check --write
expect_status 'surplus arguments' 2 "$?"
expect_calls 'surplus arguments' 0

# `--write` builds the fixture's tracked reports, and is the only mode that writes.
run --write
expect_status 'the first write' 0 "$?"
write_calls="$(calls)"
[[ -s "$fixture/docs/DYNAMIC_EXECUTABLES.tsv" ]] || fail 'the write produced no detailed report'
cases=$((cases + 1))

# One generation per `--check`, and it must equal what `--write` cost: the old `--check` performed
# two sweeps, one of them inside the recursive self-test.
run --check
expect_status 'a matching check' 0 "$?"
expect_calls 'a matching check' "$write_calls"

# Inventory is name-only: no ELF work at all.
run --check-inventory
expect_status 'a matching inventory' 0 "$?"
expect_calls 'a matching inventory' 0

# A tool the report does not know about is a refresh, not a failure.
cp "$fixture/manifest.json" "$fixture/manifest.json.good"
jq '.programs.grep = {"name":"grep","owner":"tools","role":"tool","linkage":"dynamic","stage":"volume","destination":"bin/grep.lsexe","providers":["liba"]}' "$fixture/manifest.json.good" >"$fixture/manifest.json"
printf 'TOOL_WAVES[grep]=2\n' >>"$fixture/lib.sh"
run --check-inventory
expect_status 'an added tool' 3 "$?"
expect_calls 'an added tool' 0
cp "$fixture/manifest.json.good" "$fixture/manifest.json"
sed -i '/TOOL_WAVES\[grep\]/d' "$fixture/lib.sh"

# A manifest the wave table disagrees with is the checker's own inconsistency: status 1.
printf 'TOOL_WAVES[nosuchtool]=1\n' >>"$fixture/lib.sh"
run --check-inventory
expect_status 'a wave table that disagrees with the manifest' 1 "$?"
sed -i '/TOOL_WAVES\[nosuchtool\]/d' "$fixture/lib.sh"

# No tracked report is a refresh.
mv "$fixture/docs/DYNAMIC_EXECUTABLES.tsv" "$fixture/docs/detailed.keep"
run --check-inventory
expect_status 'a missing tracked report' 3 "$?"
mv "$fixture/docs/detailed.keep" "$fixture/docs/DYNAMIC_EXECUTABLES.tsv"

# A tracked report that differs in a value is status 4 - the report is stale, the gate is not broken.
for file in DYNAMIC_EXECUTABLES DYNAMIC_WAVES DYNAMIC_IMAGE; do
	cp "$fixture/docs/$file.tsv" "$fixture/docs/$file.keep"
	awk -F '\t' -v OFS='\t' 'NR == FNR { last = FNR; next } { if (FNR == last) { $NF = $NF "x" } ; print }' "$fixture/docs/$file.tsv" "$fixture/docs/$file.tsv" >"$fixture/docs/$file.mut"
	mv "$fixture/docs/$file.mut" "$fixture/docs/$file.tsv"
	run --check
	expect_status "a stale $file" 4 "$?"
	mv "$fixture/docs/$file.keep" "$fixture/docs/$file.tsv"
done

# A malformed tracked report is status 1, even though it differs: "this is not a report" and "this
# report is out of date" are different answers and the caller acts on them differently.
cp "$fixture/docs/DYNAMIC_EXECUTABLES.tsv" "$fixture/docs/detailed.keep"
sed -i '1s/.*/format=something-else/' "$fixture/docs/DYNAMIC_EXECUTABLES.tsv"
run --check
expect_status 'a malformed tracked report' 1 "$?"
mv "$fixture/docs/detailed.keep" "$fixture/docs/DYNAMIC_EXECUTABLES.tsv"

# And a subprocess that prints plausible partial output and then fails must stop the run before any
# comparison or write. This is the shape that used to be read as a metric.
#
# ONE CASE PER READER. A blanket failure only ever proved that the FIRST call is checked; each of
# these fails exactly one question and requires the run to refuse, so the segment readers and the
# export reader - the three that fed loops from a process substitution and therefore could not see a
# failure at all - are each shown to fail closed.
for question in -h "--wide --symbols" --dyn-syms -dW -lW; do
	SHIM_FAIL="$question" run --check
	expect_status "a failing llvm-readelf for '$question'" 1 "$?"
done
SHIM_FAIL=1 run --check
expect_status 'a failing llvm-readelf' 1 "$?"
SHIM_FAIL=1 run --write
expect_status 'a failing llvm-readelf on the write path' 1 "$?"
SHIM_FAIL=-lW run --write
expect_status 'a failing segment read on the write path' 1 "$?"

# A row written twice is not a report. The wave and image validators kept a SET of keys and compared
# its size, so a verbatim duplicate collapsed into the entry already there and the count still
# matched - the one shape a per-row validator exists to catch and the one it could not see.
for file in DYNAMIC_WAVES DYNAMIC_IMAGE DYNAMIC_EXECUTABLES; do
	cp "$fixture/docs/$file.tsv" "$fixture/docs/$file.keep"
	tail -1 "$fixture/docs/$file.tsv" >>"$fixture/docs/$file.tsv"
	run --check
	expect_status "a duplicated row in $file" 1 "$?"
	mv "$fixture/docs/$file.keep" "$fixture/docs/$file.tsv"
done

# And the column header is checked by NAME, not by how many columns it happens to have: a renamed or
# reordered column with the same count is a different report claiming to be this one.
for file in DYNAMIC_WAVES DYNAMIC_IMAGE DYNAMIC_EXECUTABLES; do
	cp "$fixture/docs/$file.tsv" "$fixture/docs/$file.keep"
	sed -i '2s/target/subject/' "$fixture/docs/$file.tsv"
	run --check
	expect_status "a renamed column in $file" 1 "$?"
	mv "$fixture/docs/$file.keep" "$fixture/docs/$file.tsv"
done

# --- the build wrapper's half of the contract -----------------------------------------------------
#
# The checker's status only matters because a caller acts on it, and that caller used to turn EVERY
# nonzero result into the same warning and then print `status=0`. So `check_dynamic_report_inventory`
# is exercised here too - read out of the tracked `build-shared.sh` rather than copied, so this tests
# the shipped text and cannot drift from it.
wrapper="$root/tools/build-shared.sh"
inventory_function="$(awk '/^check_dynamic_report_inventory\(\) \{$/, /^\}$/' "$wrapper")"
cases=$((cases + 1))
if [[ -z "$inventory_function" ]]; then
	fail 'check_dynamic_report_inventory was not found in build-shared.sh'
else
	# One stub checker whose exit status is dictated by the case, standing in for the real one.
	mkdir -p "$fixture/wrapper/tools"
	cat >"$fixture/wrapper/tools/check-dynamic-report.sh" <<'STUB'
#!/usr/bin/env bash
echo "stub: mode $*"
exit "${STUB_STATUS:-0}"
STUB
	chmod +x "$fixture/wrapper/tools/check-dynamic-report.sh"

	# status -> expected state, expected return. Only 3 is a warning; every other nonzero value is
	# the check itself being broken, including 4, which `--check-inventory` cannot legitimately
	# produce and which therefore means the caller is talking to the wrong script.
	while read -r stub_status expected_state expected_return; do
		[[ -n "$stub_status" ]] || continue
		observed="$(
			set +u
			root="$fixture/wrapper"
			verbose=0
			verbose_log() { :; }
			eval "$inventory_function"
			STUB_STATUS="$stub_status" check_dynamic_report_inventory >/dev/null 2>&1
			printf '%s %s %s\n' "$?" "$inventory_state" "$((inventory_stage_ms >= 0))"
		)"
		read -r got_return got_state got_timed <<<"$observed"
		cases=$((cases + 1))
		if [[ "$got_return" != "$expected_return" || "$got_state" != "$expected_state" ]]; then
			fail "an inventory status of $stub_status gave return $got_return/state $got_state, expected $expected_return/$expected_state"
		elif [[ "$got_timed" != 1 ]]; then
			fail "an inventory status of $stub_status produced no stage timing"
		fi
	done <<'MATRIX'
0 match 0
3 refresh-required 0
1 fatal 1
4 fatal 1
9 fatal 1
MATRIX

	# And the summary must exit with what it printed. This is a claim about the tracked script's
	# text: `report_build_summary` computes `status` and, before P02M0137, returned from the EXIT
	# trap without it - so a build that announced `status=1` exited 0.
	cases=$((cases + 1))
	summary_function="$(awk '/^report_build_summary\(\) \{$/, /^\}$/' "$wrapper")"
	if [[ "$summary_function" != *'exit "$status"'* ]]; then
		fail 'report_build_summary does not exit with the status it prints'
	fi
	cases=$((cases + 1))
	if [[ "$summary_function" != *'inventory_status=$inventory_state'* || "$summary_function" != *'inventory_stage_ms=$inventory_stage_ms'* ]]; then
		fail 'the build summary does not report the inventory state and stage time'
	fi
fi

# Nothing above touched the tracked files.
cases=$((cases + 1))
if [[ "$before_hashes" != "$(sha256sum "${tracked[@]}")" ]]; then
	fail 'the tracked reports or lib.sh changed during the matrix'
fi

if ((failures)); then
	note "$failures of $cases checks failed"
	exit 1
fi
note "$cases checks passed against a disposable fixture, zero production reads"
