#!/usr/bin/env bash
# One plan over every kind of check, and the boundary that makes a broken planner safe.
#
# `./test.sh --for-change` answered only the kernel-suite question, and when nothing there applied
# it printed a note and exited 0 - which a person reads as "nothing to do" and automation reads as
# "everything passed". This answers the whole question instead: builds, host suites, gates,
# conformance runs and per-architecture, per-environment guest runs, in one plan, from one selector.
#
# THIS SCRIPT IS DELIBERATELY STUPID. Every decision that needs the model was already made by
# `verify-model`, which runs as a SEPARATE PROCESS - because "the selector crashes, therefore select
# everything" cannot be implemented inside the selector. A planner that panics is in no position to
# choose its own fallback. So: non-zero exit, empty output or unparseable output from the planner all
# land in the canonical FULL path below, and none of them can produce a green exit status.

SCRIPT_NAME=verify.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

PLANNER_MANIFEST="$SRC_DIR/tools/verify-model/Cargo.toml"
# The candidate overlay to plan against, if one was named. Empty for an ordinary run.
candidate_arg=()
# WHICH LOGS A RUN WROTE, SAID BY THE RUN. The shadow comparison below used to find its evidence with
# `find .build/logs/test -name "<arch>-*-guest.log" | sort -rn | head -1` - the newest-file glob that
# `result-logs.sh` exists to replace. Two runs of one architecture in flight and it reads the OTHER
# one's log; and it is wrong on riscv64 even alone, where the suite output lands in the RUN log while
# the guest log holds only U-Boot and the loader. Every gate was moved off that glob and this producer
# was missed, because `check-gate-result-logs.sh` scans `check-*.sh` and not this file.
# shellcheck source=/dev/null
source "$SRC_DIR/tools/result-logs.sh"

# The one of a run's published logs that carries the suite's own output. The oracle is in the run log
# on riscv64 and in the guest log on the other two, so this asks the files rather than choosing by
# target - a rule keyed on the architecture is a rule that is wrong the day a port moves its output.
suite_result_log() {
	local captured="$1" path
	while IFS= read -r path; do
		if grep -qaE '^(test suite complete|running [0-9]+ (tests|selected tests))' "$path"; then
			printf '%s\n' "$path"
			return 0
		fi
	done < <(result_logs "$captured")
	echo "verify.sh: none of the logs that run published carries its suite output" >&2
	return 1
}

help() {
	usage_and_exit <<EOF
usage: verify.sh [--for-change | --for PATH[,PATH...] | --for-range A..B | --release | --sweep]
                 [--jobs N] [--plan] [--explain] [--json] [--shadow] [--allow-shadow] [--candidate FILE]
                 [--catalog] [--model-hash] [--age] [--trust]

Works out what a change needs verified and runs exactly that. With no arguments: --for-change.

  --for-change     everything the working tree says was changed (the default)
  --for PATH       plan for these paths instead of asking git
  --for-range A..B plan for the paths a commit range touched
  --release        the release gate: build all, check all, boot all three, no optimisation applied
  --sweep          the whole suite on every target at one immutable revision, in a git worktree
  --shadow         run the FULL suite and compare it against what this change would have scoped
  --shadow-exec    the same, but RUN the selection first and compare the two runs (one target)
  --allow-shadow   accept a scoped run with no shadow evidence (exit 0 instead of 4); --dev is an alias
  --candidate FILE plan the shadow comparison against a FROZEN narrowing and record it under that
                   candidate's model hash. The run that happens is unchanged and still full; what
                   the candidate changes is which selection the comparison is graded against, which
                   is how a narrowing earns the evidence its activation demands.
  --jobs N         how many guests may boot at once (default 1; parallelism is opted into)
  --plan           print the plan and run nothing
  --explain        print why every item is in the plan
  --json           the plan as JSON, for anything that is not a person
  --catalog        every check the planner can emit and the variants it has
  --model-hash     the hash a component's TRUSTED evidence is bound to
  --age            which keys have not run inside the window, over the catalog's universe
  --trust          which components are TRUSTED under the current model, and what is short
  -h, --help       this text

examples:
  ./verify.sh --plan                       # what would run
  ./verify.sh --for src/user/libs/audio/flac --explain
  ./verify.sh --for-range HEAD~3..HEAD --plan
  ./verify.sh --release

The plan is per PlanItemKey - (check, architecture, environment, configuration) - and the run
collapses those into commands: two hundred selected kernel tests are one boot, not two hundred.

A failure to PRODUCE a plan is never a pass. If the planner cannot answer, this script says
FULL VERIFICATION REQUIRED and exits non-zero.
EOF
}

mode=for-change
# HOW MANY GUESTS MAY RUN AT ONCE, and the default is ONE.
#
# Parallelism is opted into, because the failure mode of getting it wrong is a green that means
# nothing and the failure mode of not using it is a slow run. The machine has a hundred cores and
# used one; what changed is that asking for more stopped being dangerous - every writable image a
# guest touches is now this run's own copy, the fixtures it reads are attached read-only, and each
# consumer reads the logs its own run named.
#
# THIS IS THE ONLY SCHEDULER. `--release` and `--sweep` are flat by design - they consult no model
# and read no plan, because they are what runs when the thing that makes choices is broken - so they
# take no limiter and run one architecture at a time instead.
JOBS=1
# A CEILING ON WHAT THIS RUN MAY START, in ESTIMATED seconds. Zero means no ceiling.
#
# Not a timeout: it decides what to start and never kills a step that overran its estimate. What it
# prices is a whole prerequisite-closed branch, so a budget cannot spend its time building and then
# decline to test. `--sweep`, `--release` and `--shadow` refuse it outright - each of those means
# "all of it", and a bounded version of "all of it" is a contradiction with a number attached.
BUDGET=0
paths=""
range=""
action=run
planner_flags=()

while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help) help ;;
	--for-change)
		mode=for-change
		shift
		;;
	--for)
		[[ $# -ge 2 ]] || die "--for needs a path"
		mode=for
		paths="$2"
		shift 2
		;;
	--for-range)
		[[ $# -ge 2 ]] || die "--for-range needs a range like A..B"
		mode=for-range
		range="$2"
		shift 2
		;;
	--release)
		mode=release
		shift
		;;
	--sweep)
		mode=sweep
		shift
		;;
	# Accept a scoped run that has no shadow evidence behind it. The default refuses, because a
	# machine reading an exit status cannot read the note that says what the green is worth.
	# A FROZEN NARROWING TO PLAN AGAINST, so evidence can be gathered UNDER it while the run that
	# actually happens stays full. That is M5's shape - evidence first, split second - and without
	# this the only way to plan against a candidate was to activate it, which is the thing the
	# evidence is for.
	--candidate)
		[[ $# -ge 2 ]] || die "--candidate needs the path of a candidate file"
		candidate_arg=(--candidate "$2")
		shift 2
		continue
		;;
	--allow-shadow | --dev)
		allow_shadow=1
		shift
		;;
	--shadow)
		action=shadow
		shift
		;;
	# SHADOW-EXEC: run the selection, then the sweep, and compare. See the action below for why a
	# dry comparison cannot answer the question this one asks.
	--shadow-exec)
		action=shadow
		shadow_exec=1
		shift
		;;
	--budget)
		BUDGET="${2:?--budget needs a number of seconds}"
		[[ "$BUDGET" =~ ^[0-9]+$ ]] || die "--budget takes a whole number of seconds, not '$BUDGET'"
		shift 2
		;;
	--jobs)
		JOBS="${2:?--jobs needs a number}"
		[[ "$JOBS" =~ ^[1-9][0-9]*$ ]] || die "--jobs takes a positive whole number, not '$JOBS'"
		shift 2
		;;
	--plan)
		action=plan
		shift
		;;
	--explain)
		action=plan
		planner_flags+=(--explain)
		shift
		;;
	--json)
		action=json
		shift
		;;
	--catalog)
		action=catalog
		shift
		;;
	--model-hash)
		action=model-hash
		shift
		;;
	--age)
		action=age
		shift
		;;
	--trust)
		action=trust
		shift
		;;
	*) die "unexpected argument '$1' (try --help)" ;;
	esac
done

# The canonical FULL path: build everything, check everything, boot every target.
#
# Written flat on purpose. It consults no model, reads no plan and makes no choice, because it is
# what runs when the thing that makes choices is broken. If this needs the planner to work, it is
# not a fallback.
run_full() {
	note "FULL verification"
	./build.sh --arch all
	# ONE ARCHITECTURE AT A TIME, and this used to be `--arch all`.
	#
	# `test.sh --arch all` runs the three targets CONCURRENTLY and says so in its own comment, so
	# this flat path was a second scheduler - one that predates `--jobs` and answers a different
	# number. "How many QEMUs may run" has to have exactly one answer on this machine, and the flat
	# path cannot be the one that gives it: it consults no model and reads no plan by design, which
	# is the whole reason it exists, so giving it a limiter to consult would remove that.
	#
	# Serial is the right half to choose here. It costs wall-clock on the path that is deliberately
	# slow and thorough, and it leaves `verify.sh --jobs` as the only thing that decides.
	local architecture
	for architecture in x86_64 aarch64 riscv64; do
		./test.sh --arch "$architecture"
	done
	# TESTS BEFORE CHECKS, and this used to be the other way round.
	#
	# `capability-trace` is a CHECK that reads a log only a TEST produces, and it compares that log
	# against the kernel binary's timestamp - which `build.sh` above has just refreshed. So on this
	# path the gate could not pass however clean the tree was, and no amount of preparing a fresh
	# trace beforehand survives the build that runs before it. Measured on 2026-08-28, twice, the
	# second time with a trace confirmed fresh minutes earlier.
	./check.sh
}

# Refuse to guess. A planner that cannot answer leaves two honest options - run everything, or stop
# and say so - and the caller picks by whether the machine has the hours.
planner_failed() {
	local reason="$1"
	echo >&2
	echo "verify.sh: FULL VERIFICATION REQUIRED" >&2
	echo "verify.sh: the planner could not answer ($reason), and a plan that is missing is not a plan that is empty." >&2
	# THE ORDER MATTERS AND THIS PRINTED THE WRONG ONE. `capability-trace` is a check that reads a
	# log only a test produces, so `check` before `test` fails on a clean tree every time - and this
	# is the line a person reaches for exactly when the planner cannot be trusted to have got it
	# right, which makes it the worst of the four places to leave stale.
	echo "verify.sh: run the whole thing yourself:  ./build.sh --arch all && ./test.sh --arch x86_64 && ./test.sh --arch aarch64 && ./test.sh --arch riscv64 && ./check.sh" >&2
	exit 3
}

# THE THREE THAT MEAN "ALL OF IT" REFUSE A BUDGET. `--sweep`, `--release` and `--shadow` each assert
# that everything ran; a bounded version of that is a contradiction with a number attached, and the
# refusal is louder than a note nobody reads.
if ((BUDGET > 0)); then
	case "$mode" in
	sweep | release) die "--budget cannot be combined with --$mode: that mode's whole claim is that everything ran" ;;
	esac
	[[ "$action" == shadow ]] && die "--budget cannot be combined with --shadow: a shadow compares a scoped run against the FULL suite, and a bounded full suite is not one"
fi

if [[ "$mode" == release ]]; then
	# Deliberately outside everything else in this file. TRUSTED evidence, the cost estimator, the
	# age bound and the selector are all mechanisms for spending less on an ORDINARY change, and
	# every one of them is ignored here. Releases are rare and the insurance is nearly free; a wrong
	# selector decision shipped in an image is not.
	note "release gate: every optimisation in this script is ignored"
	note "revision: $(git rev-parse HEAD 2>/dev/null || echo 'not a git repository')"
	if [[ -n "$(git status --porcelain 2>/dev/null)" ]]; then
		die "the working tree is dirty; a release is cut from a revision, not from a desk"
	fi
	run_full
	note "release gate passed"
	exit 0
fi

if [[ "$mode" == sweep ]]; then
	# One revision, every target, the whole suite - and pinned, which is the part that needs a
	# worktree rather than discipline. An emulated sweep here takes tens of minutes to hours and the
	# tree changes while it does, so a sweep run in place judges a system that no longer exists by
	# the time it answers.
	#
	# This is NOT what the age bound does, and the difference is the reason both exist. Stale keys
	# joining the next manual run spreads coverage over many different trees: some tests on Monday's
	# commit, others on Wednesday's. That satisfies a bound while never once establishing that ONE
	# revision passes everything.
	[[ -z "$(git status --porcelain 2>/dev/null)" ]] || die "--sweep needs a clean tree: it pins a REVISION, and a dirty one is not a revision"
	revision="$(git rev-parse HEAD)"
	worktree="$BUILD_DIR/sweep/$revision"
	note "snapshot sweep at $revision"
	rm -rf "$worktree"
	git worktree add --detach "$worktree" "$revision" >/dev/null 2>&1 || die "could not create a worktree at $worktree"
	# shellcheck disable=SC2064
	trap "git worktree remove --force '$worktree' >/dev/null 2>&1 || true" EXIT
	status=0
	# Serial for the same reason `run_full` is: this is the other flat path, and a second scheduler
	# living in it would answer a different number from `--jobs`.
	# Serial, and the checks after the tests, for the two reasons `run_full` gives: a second
	# scheduler here would answer a different number from `--jobs`, and `capability-trace` is a check
	# that reads a log only a test produces.
	(cd "$worktree" && ./build.sh --arch all && ./test.sh --arch x86_64 && ./test.sh --arch aarch64 && ./test.sh --arch riscv64 && ./check.sh) || status=$?
	if [[ "$status" -ne 0 ]]; then
		die "the snapshot sweep of $revision failed (exit $status); the worktree is at $worktree until this shell exits"
	fi
	note "snapshot sweep passed: $revision, every target, whole suite"
	exit 0
fi

case "$action" in
catalog)
	exec cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- catalog
	;;
model-hash)
	exec cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- model-hash
	;;
age)
	exec cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- age
	;;
trust)
	exec cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- trust
	;;
esac

# What changed, asked of the model rather than parsed here.
#
# It WAS parsed here, with two `sed` expressions, and it was wrong: porcelain reports a rename as
# `old -> new` and the shell kept only the new path, so moving a file out of a component did not
# select that component and a rename into `docs/` looked like nothing had changed. The regression
# corpus used a third spelling - `git diff --name-only` - so it could not catch that either.
#
# One parser now, in `verify-model`, over the machine-readable formats, returning BOTH sides of a
# rename. The corpus calls the same function.
case "$mode" in
for-change)
	changed="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- changes)" || planner_failed "the planner could not read what changed"
	[[ -n "$changed" ]] || die "--for-change: the working tree is clean (use --for PATH, or --release)"
	;;
for)
	changed="$(printf '%s\n' "$paths" | tr ',' '\n' | grep -v '^$')"
	;;
for-range)
	changed="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- changes --range "$range")" || planner_failed "the planner could not read the range '$range'"
	[[ -n "$changed" ]] || die "--for-range: '$range' touched no files (or is not a range git knows)"
	;;
esac
note "$(printf '%s\n' "$changed" | wc -l) changed path(s)"

# The display modes go through the same boundary as the run.
#
# They are only printing, so the temptation is to let them fail quietly - but a reader who asks what
# would run and is shown nothing concludes that nothing would, and that is the same wrong answer the
# execution path is guarded against. Silence has to be impossible everywhere or it is trusted
# somewhere.
if [[ "$action" == json || "$action" == plan ]]; then
	rendered="$(mktemp)"
	trap 'rm -f "$rendered"' EXIT
	if [[ "$action" == json ]]; then
		printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- plan --stdin --json >"$rendered" || planner_failed "the planner exited non-zero"
	else
		printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- plan --stdin "${planner_flags[@]}" >"$rendered" || planner_failed "the planner exited non-zero"
	fi
	[[ -s "$rendered" ]] || planner_failed "the planner produced no output"
	cat "$rendered"
	exit 0
fi

if [[ "$action" == shadow ]]; then
	# DRY shadow: the scoped set is COMPUTED and never run. One boot serves both answers, which is
	# the whole economy of it - the expensive version runs the selection and then the full suite, and
	# against a 6104-second riscv64 sweep that second boot is not cosmetic.
	#
	# Pinned to the SOURCE, not to the model hash. Comparing model hashes was the first attempt and it
	# does not close the race: an ordinary edit to a `.rs` file changes what the sweep is testing and
	# leaves the model's identity untouched, so a tree edited during a multi-hour emulated run passed
	# the check. What has to hold still is the thing being tested.
	source_before="$(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- source-digest)" || planner_failed "could not digest the tree"
	model_before="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- model-hash)"
	targets="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- booted --stdin)" || planner_failed "the planner could not name the targets"
	[[ -n "$targets" ]] || planner_failed "the plan named no target to sweep"
	# THE BUILT SET, WHICH IS NOT THE BOOTED SET. A change that boots one target still has to compile
	# on the other two, and the build-evidence producer below asks a question about BUILDS - so it
	# loops over this and the guest shadow keeps `targets`. The two must not merge back: the plan
	# carries both fields because they answer different questions.
	build_targets="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- built --stdin)" || planner_failed "the planner could not name the build targets"
	[[ -n "$build_targets" ]] || planner_failed "the plan named no target to build"
	note "shadow: full sweep on ${targets//$'\n'/ }, compared against a selection that is not run"
	note "        pinned to source $source_before"
	for target in $targets; do
		# THE SELECTION FIRST, when a sample was asked for.
		#
		# A dry shadow computes the scoped set and never runs it, so it answers "did the selector
		# choose the right set S" and cannot answer "does running S work". Those are different
		# questions and this milestone has the proof: a planner emitting Rust function names against
		# a runner using stable IDs computed every selection correctly, could execute none of them,
		# and left every dry comparison clean. It was found by hand.
		#
		# One target's worth of second boot, which is why it is a flag rather than the default.
		scoped_arg=()
		if [[ "${shadow_exec:-0}" == "1" ]]; then
			selection="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- guest-selection --stdin --arch "$target")" || planner_failed "the planner could not name the guest selection"
			if [[ -n "$selection" ]]; then
				note "shadow-exec: running the selection on $target first (${selection//$'\n'/,})"
				scoped_capture="$(mktemp)"
				TEST_SELECTION="$(printf '%s' "$selection" | tr '\n' ',')" ./test.sh --arch "$target" 2>&1 | tee "$scoped_capture" || true
				scoped_log="$(suite_result_log "$scoped_capture")" || die "the $target scoped run did not say which logs it wrote"
				rm -f "$scoped_capture"
				[[ -n "$scoped_log" ]] || die "no $target scoped guest log to compare against"
				scoped_arg=(--scoped-log "../$scoped_log")
			else
				note "shadow-exec: the plan selects no guest test on $target, so there is nothing to execute"
			fi
		fi
		sweep_capture="$(mktemp)"
		./test.sh --arch "$target" 2>&1 | tee "$sweep_capture" || true
		log="$(suite_result_log "$sweep_capture")" || die "the $target sweep did not say which logs it wrote"
		rm -f "$sweep_capture"
		[[ -n "$log" ]] || die "no $target guest log to compare against"
		printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --stdin --guest-log "../$log" --arch "$target" "${scoped_arg[@]}") || shadow_failed=1
	done
	# The HOST universe, from the same sweep.
	#
	# `trusted_everywhere` asks for Host AND TestGuest and only TestGuest was ever written, so no
	# component could ever be fully trusted however much evidence it accumulated. The host suite is
	# the cheap half of a sweep - no boot, no image - so producing its evidence costs almost nothing
	# beyond running what a sweep runs anyway.
	#
	# Written by the runner rather than scraped: it knows each check's id and its exit status, and a
	# `total` line so a partial run cannot look like a clean one.
	host_log="$BUILD_DIR/logs/verify-host-shadow.txt"
	mkdir -p "$(dirname "$host_log")"
	: >"$host_log"
	host_ids="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- host-checks)" || planner_failed "the planner could not list the host checks"
	# THE WHOLE KEY per line, not the check's name. Two variants of one check used to write two
	# lines carrying the same id: the model's map kept the last and `total` counted both, so the
	# declared total and the number of outcomes disagreed and nothing said so.
	host_total=0
	while IFS=$'\t' read -r id arch env config command; do
		[[ -n "$id" ]] || continue
		host_total=$((host_total + 1))
		if (cd "$SRC_DIR/.." && eval "$command") >/dev/null 2>&1; then
			printf '%s\t%s\t%s\t%s\tPASS\n' "$id" "$arch" "$env" "$config" >>"$host_log"
		else
			printf '%s\t%s\t%s\t%s\tFAIL\n' "$id" "$arch" "$env" "$config" >>"$host_log"
		fi
	done <<<"$host_ids"
	printf 'total %s\n' "$host_total" >>"$host_log"
	# THE EXECUTION SAMPLE, when one was asked for: run the SCOPED host selection on its own first,
	# so the comparison can ask whether that selection is executable at all. A dry comparison answers
	# "did the selector choose the right set" and cannot answer "does running it work" - and the dev
	# producer next door is the proof that the second question is not theoretical: it emitted a
	# command bash could not parse and every dry comparison stayed clean.
	host_scoped_arg=()
	if [[ "${shadow_exec:-0}" == "1" ]]; then
		host_scoped_log="$BUILD_DIR/logs/verify-host-scoped.txt"
		: >"$host_scoped_log"
		host_scoped_total=0
		# The selection's own keys, lowered the same way the sweep lowers them - `commands` is what
		# both read, so the two sides cannot drift.
		host_scoped_ids="$(printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- host-checks --stdin --scoped))" || planner_failed "the planner could not list the scoped host checks"
		while IFS=$'\t' read -r id arch env config command; do
			[[ -n "$id" ]] || continue
			host_scoped_total=$((host_scoped_total + 1))
			if (cd "$SRC_DIR/.." && eval "$command") >/dev/null 2>&1; then
				printf '%s\t%s\t%s\t%s\tPASS\n' "$id" "$arch" "$env" "$config" >>"$host_scoped_log"
			else
				printf '%s\t%s\t%s\t%s\tFAIL\n' "$id" "$arch" "$env" "$config" >>"$host_scoped_log"
			fi
		done <<<"$host_scoped_ids"
		printf 'total %s\n' "$host_scoped_total" >>"$host_scoped_log"
		host_scoped_arg=(--host-scoped-log "../$host_scoped_log")
	fi
	printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --stdin --host-log "../$host_log" "${host_scoped_arg[@]}") || shadow_failed=1

	# The DEV GUEST universe, the third and last producer.
	#
	# It was declared and unfed: `Universe::of` routes `dev.*` checks to it and `trusted_everywhere`
	# asks the catalog which universes may judge a component, so `bin.dev_agent`, `bin.dev_channel`,
	# `harness.boot` and `proto` each required a certificate that no code path could grant. Two of
	# those are ordinary components, not development curiosities.
	dev_log="$BUILD_DIR/logs/verify-dev-shadow.txt"
	: >"$dev_log"
	dev_ids="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- dev-checks)" || planner_failed "the planner could not list the dev-guest checks"
	dev_total=0
	while IFS=$'\t' read -r id arch env config command; do
		[[ -n "$id" ]] || continue
		dev_total=$((dev_total + 1))
		if (cd "$SRC_DIR/.." && eval "$command") >/dev/null 2>&1; then
			printf '%s\t%s\t%s\t%s\tPASS\n' "$id" "$arch" "$env" "$config" >>"$dev_log"
		else
			printf '%s\t%s\t%s\t%s\tFAIL\n' "$id" "$arch" "$env" "$config" >>"$dev_log"
		fi
	done <<<"$dev_ids"
	printf 'total %s\n' "$dev_total" >>"$dev_log"
	# The same sample on the dev path, and this is the universe it would have caught first.
	dev_scoped_arg=()
	if [[ "${shadow_exec:-0}" == "1" ]]; then
		dev_scoped_log="$BUILD_DIR/logs/verify-dev-scoped.txt"
		: >"$dev_scoped_log"
		dev_scoped_total=0
		dev_scoped_ids="$(printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- dev-checks --stdin --scoped))" || planner_failed "the planner could not list the scoped dev checks"
		while IFS=$'\t' read -r id arch env config command; do
			[[ -n "$id" ]] || continue
			dev_scoped_total=$((dev_scoped_total + 1))
			if (cd "$SRC_DIR/.." && eval "$command") >/dev/null 2>&1; then
				printf '%s\t%s\t%s\t%s\tPASS\n' "$id" "$arch" "$env" "$config" >>"$dev_scoped_log"
			else
				printf '%s\t%s\t%s\t%s\tFAIL\n' "$id" "$arch" "$env" "$config" >>"$dev_scoped_log"
			fi
		done <<<"$dev_scoped_ids"
		printf 'total %s\n' "$dev_scoped_total" >>"$dev_scoped_log"
		dev_scoped_arg=(--dev-scoped-log "../$dev_scoped_log")
	fi
	printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --stdin --dev-log "../$dev_log" "${dev_scoped_arg[@]}") || shadow_failed=1

	# The BUILD universe, and it costs the sweep nothing it was not already paying.
	#
	# `HostBuild` was split out of `Host` because a certificate earned over gates, suites and
	# conformance was standing for builds nothing had compared - right, and it left a universe with
	# no producer. `trusted_everywhere` asks every judging universe, and `build.libs`, `build.user`,
	# `build.kernel` and the rest cover every crate and every program under their prefix: 189 of the
	# catalog's 192 components were judged by a universe that could never answer, so 98% of the tree
	# could accumulate any evidence and never reach TRUSTED.
	#
	# A full sweep has already built every part on every architecture by the time it gets here, so
	# each command below is a no-op re-run against a warm cache - it reports whether that part builds
	# rather than building it again. PER ARCHITECTURE, because a build of x86_64 says nothing about
	# aarch64, which is what `required_architectures` asks for.
	for build_arch in $build_targets; do
		build_log="$BUILD_DIR/logs/verify-build-shadow-$build_arch.txt"
		: >"$build_log"
		build_ids="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- build-checks)" || planner_failed "the planner could not list the build checks"
		build_total=0
		while IFS=$'\t' read -r id arch env config command; do
			[[ -n "$id" ]] || continue
			[[ "$arch" == "$build_arch" ]] || continue
			build_total=$((build_total + 1))
			if (cd "$SRC_DIR/.." && eval "$command") >/dev/null 2>&1; then
				printf '%s\t%s\t%s\t%s\tPASS\n' "$id" "$arch" "$env" "$config" >>"$build_log"
			else
				printf '%s\t%s\t%s\t%s\tFAIL\n' "$id" "$arch" "$env" "$config" >>"$build_log"
			fi
		done <<<"$build_ids"
		printf 'total %s\n' "$build_total" >>"$build_log"

		# SHADOW-EXEC FOR THE BUILD UNIVERSE, when one was asked for.
		#
		# Everything above runs the CATALOG's commands, one part at a time. The production runner
		# groups them - `./build.sh --arch X --part a,b,c` - and those are different code paths
		# through the same script, of which only the second one ships. A grouped part list whose
		# parser silently used only the first entry and exited zero would leave every check above
		# passing and the scoped runner building less than the selection said, with every record
		# clean and a certificate granted: the precise shape of the defect SHADOW-EXEC was built for.
		#
		# So the grouped command is RUN and what `build.sh` reports having built is compared against
		# the parts the selection named. The same three invariants the guest comparison uses: it is
		# executable, it is not wider than the selection, and the two agree.
		build_exec=no
		if [[ "${shadow_exec:-0}" == "1" ]]; then
			build_steps="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- build-steps --stdin)" || planner_failed "the planner could not lower the build steps"
			build_exec=ok
			while IFS=$'\t' read -r label command keys; do
				[[ -n "$command" ]] || continue
				[[ "$command" == *"--arch $build_arch "* ]] || continue
				built_line="$( (cd "$SRC_DIR/.." && eval "$command") 2>&1 | grep -E 'built: .* for ' | tail -1)" || true
				if [[ -z "$built_line" ]]; then
					note "shadow-exec (build, $build_arch): '$label' did not report what it built - the step is not executable as the runner ships it"
					build_exec=no
					break
				fi
				# `build.sh: built: sdk libs user kernel loader packages volume for x86_64`
				reported="$(sed -E 's/.*built: (.*) for .*/\1/' <<<"$built_line" | tr ' ' '\n' | sort -u | paste -sd, -)"
				wanted="$(tr '|' '\n' <<<"$keys" | sed -E 's#^build\.([^ ]+) .*#\1#' | sort -u | paste -sd, -)"
				if [[ "$reported" != "$wanted" ]]; then
					note "shadow-exec (build, $build_arch): the grouped step built [$reported] and the selection named [$wanted]"
					build_exec=no
					break
				fi
				note "shadow-exec (build, $build_arch): the grouped step built exactly what the selection named"
			done <<<"$build_steps"
		fi
		printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --stdin --build-log "../$build_log" --build-arch "$build_arch" --build-exec "$build_exec") || shadow_failed=1
	done

	source_after="$(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- source-digest)"
	model_after="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- model-hash)"
	if [[ "$source_before" != "$source_after" || "$model_before" != "$model_after" ]]; then
		die "the tree moved while the sweep ran, so the comparison judged two different systems and is void.
    source: $source_before -> $source_after
    model:  $model_before -> $model_after
    Use ./verify.sh --sweep for a comparison that cannot be overtaken, or leave the tree alone while this runs."
	fi
	[[ -z "${shadow_failed:-}" ]] || die "shadow did not come back Consistent - see the verdict above; a candidate miss must be confirmed before it is charged to the selector"
	note "shadow: no evidence against the selection, over a tree that did not move"
	exit 0
fi

# Ask for the commands, and treat every way of not getting them as the same answer.
steps_file="$(mktemp)"
trap 'rm -f "$steps_file"' EXIT
if ! printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- commands --stdin >"$steps_file"; then
	planner_failed "the planner exited non-zero"
fi
[[ -s "$steps_file" ]] || planner_failed "the planner produced no output"
grep -q $'^STATUS\t' "$steps_file" || planner_failed "the planner's output has no STATUS line"

status="$(grep -m1 $'^STATUS\t' "$steps_file" | cut -f2)"
status_detail="$(grep -m1 $'^STATUS\t' "$steps_file" | cut -f3)"

case "$status" in
nothing-to-do)
	# An empty plan is only ever legitimate for this one reason, and it is named rather than
	# implied. "Nothing selected" for any other reason is escalated by the planner itself.
	note "nothing to verify: $status_detail"
	exit 0
	;;
full)
	note "FULL: $status_detail"
	;;
scoped)
	note "scoped: $status_detail"
	;;
*)
	planner_failed "unknown status '$status'"
	;;
esac

count="$(grep -c $'^STEP\t' "$steps_file" || true)"
((count > 0)) || planner_failed "a $status plan with no steps"
note "$count step(s)"

# WHICH STEPS A BUDGET CAN AFFORD, decided before anything starts.
#
# `--budget` is in ESTIMATED seconds and is NOT a timeout: it decides what to START, never what to
# kill, and it never stops a step that overran its estimate. What it costs is a whole
# PREREQUISITE-CLOSED branch - the step and everything it needs - because putting the builds outside
# it makes `--budget 10` a run that spends forty minutes building and then declines to test.
#
# A prerequisite shared by two branches is counted ONCE: the second branch pays only its own
# incremental closure. Among affordable branches the cheapest is taken. If the cheapest COMPLETE
# branch does not fit, nothing is started and the minimum is named - a budget that half-builds has
# spent the time and bought no evidence.
budget_select() {
	local budget="$1" total=0 chosen best best_cost add i j
	local -A picked=() by_id=() closure=()
	for i in "${!step_ids[@]}"; do by_id["${step_ids[$i]}"]=$i; done
	# EACH CLOSURE COMPUTED ONCE. The first version resolved a step's prerequisites inside the
	# selection loop, which is that walk once per candidate per round - cubic in the number of steps,
	# and an eighty-seven step plan did not finish inside ten minutes. The graph does not change
	# while it is being spent, so it is walked once.
	for i in "${!step_ids[@]}"; do
		local seen=" $i " queue=("$i") node req
		while ((${#queue[@]})); do
			node="${queue[0]}"
			queue=("${queue[@]:1}")
			for req in ${step_reqs[$node]}; do
				j="${by_id[$req]:-}"
				[[ -z "$j" || " $seen " == *" $j "* ]] && continue
				seen+="$j "
				queue+=("$j")
			done
		done
		closure[$i]="$seen"
	done
	while :; do
		best=""
		best_cost=0
		for i in "${!step_ids[@]}"; do
			[[ -n "${picked[$i]:-}" ]] && continue
			add=0
			for j in ${closure[$i]}; do
				[[ -n "${picked[$j]:-}" ]] && continue
				add=$((add + ${step_costs[$j]}))
			done
			if [[ -z "$best" ]] || ((add < best_cost)); then
				best="$i"
				best_cost="$add"
			fi
		done
		[[ -z "$best" ]] && break
		if ((total + best_cost > budget)); then
			if ((${#picked[@]} == 0)); then
				note "the cheapest complete branch costs an estimated ${best_cost}s and the budget is ${budget}s, so nothing was started"
			fi
			break
		fi
		for j in ${closure[$best]}; do picked[$j]=1; done
		total=$((total + best_cost))
	done
	chosen=""
	for i in "${!step_ids[@]}"; do
		[[ -n "${picked[$i]:-}" ]] && chosen+=" $i"
	done
	# THE TOTAL COMES BACK ON THE SAME LINE. This set a global and the caller read it as empty:
	# the function runs inside `$(...)`, which is a subshell, so what it assigned died with it. A
	# budget that reports "starting an estimated 0s" while starting two steps is the kind of wrong
	# that reads as an accounting bug and hides a selection one.
	echo "$total$chosen"
}

# The plan's steps, read once into arrays so a budget can be decided over the whole of it before the
# first command runs - and so a backgrounded guest never shares the file descriptor the plan is on.
step_ids=()
step_reqs=()
step_costs=()
# HOW MANY GUESTS A STEP STARTS AT THE SAME TIME, for the few that start more than one.
#
# `--jobs` answers "how many QEMUs may run on this machine", and the answer has to be the same
# wherever it is asked - including inside a step. Every gate in this tree boots serially and needs
# one slot; `concurrent-selection` exists to prove that two same-architecture suites do not collide,
# so it starts two AT ONCE and cannot be made serial without deleting its subject. The model
# declares the count, this reserves it, and a `--jobs` that cannot hold it does not run the step.
step_guests=()
while IFS=$'\t' read -r marker index rest; do
	case "$marker" in
	STEPID) step_ids[$index]="$rest" ;;
	STEPREQ) step_reqs[$index]="${step_reqs[$index]:-} $rest" ;;
	STEPCOST) step_costs[$index]="$rest" ;;
	STEPGUESTS) step_guests[$index]="$rest" ;;
	esac
done <"$steps_file"
for i in "${!step_ids[@]}"; do
	step_reqs[$i]="${step_reqs[$i]:-}"
	step_costs[$i]="${step_costs[$i]:-0}"
	step_guests[$i]="${step_guests[$i]:-0}"
done

BUDGET_TOTAL=0
affordable=""
if ((BUDGET > 0)); then
	selection="$(budget_select "$BUDGET")"
	BUDGET_TOTAL="${selection%% *}"
	affordable="${selection#* }"
	[[ "$affordable" == "$selection" ]] && affordable=""
	note "budget ${BUDGET}s: starting an estimated ${BUDGET_TOTAL}s of work"
fi

failed=()
skipped=()
# STEPS WHOSE PREREQUISITE FAILED, and the ids of the steps that failed.
#
# `step_reqs` was read only while computing a budget closure; the execution loop never looked at it.
# So a failed build was followed by its guest, and a failed guest by `capability-trace` - which reads
# that guest's log - and the dependent's inevitable second failure was reported as a separate defect.
# One cause, several red lines, and the real one not necessarily first.
#
# ONLY PREREQUISITES THAT RAN IN THIS PLAN. A narrowed plan legitimately omits steps that other steps
# declare a requirement on: nothing changed under them, so they were not selected and their absence is
# not a failure. A requirement blocks only when it is in this plan AND it failed.
declare -A failed_ids=()
# AND THE STEPS THAT NEVER RAN BECAUSE SOMETHING THEY READ DID NOT, which is a different fact from
# failing and has to be tracked separately for blocking to be TRANSITIVE.
#
# `failed_ids` is written by `record_one_step`, so it only ever names steps that actually ran. A
# BLOCKED step records its label and no id, so its own dependents saw no failed prerequisite and ran:
# in the graph this model emits - build -> guest -> gate-after-guest - a failed build blocked the
# guest and then let the gate that reads the guest's log run anyway, against a log that was never
# written. One level of suppression is not suppression.
declare -A blocked_ids=()
blocked=()
step=0
guest_pids=()
guest_labels=()

# ONE COMMAND, RUN AND RECORDED. Split out of the loop so it can be called in the foreground or in a
# background job without the two drifting apart.
#
# It writes its outcome and duration to a file rather than recording them itself: the history is
# written by ONE writer, and parallel steps updating it concurrently is a lost-update race in the
# file the estimator reads.
run_one_step() {
	local index="$1" label="$2" command="$3" outfile="$4" started=$SECONDS status=0
	if ! eval "$command"; then
		status=1
	fi
	printf '%s\t%s\n' "$status" "$((SECONDS - started))" >"$outfile"
	return 0
}

# Record what a finished step did, against every PlanItemKey it discharged.
#
# Per key rather than per step, because that is what the age bound, the cost model and the shadow
# record all range over - a step that only knew how many keys it covered could update none of them.
# Recording is best-effort: losing a history entry costs a wider next sweep, and refusing to report a
# result the run actually produced would cost more.
#
# THE PARENT DOES THIS, ALWAYS. It is the single writer, and it is why `run_one_step` reports through
# a file instead of recording for itself.
record_one_step() {
	local index="$1" label="$2" outfile="$3" status seconds outcome keys_file
	IFS=$'\t' read -r status seconds <"$outfile"
	outcome=--passed
	if [[ "$status" != 0 ]]; then
		# Every step is run and every failure is reported, rather than stopping at the first.
		# A run that stops early tells you one thing is broken; a run that finishes tells you
		# how much is.
		note "FAILED: $label"
		failed+=("$label")
		failed_ids["${step_ids[$index]:-}"]=1
		outcome=--failed
	fi
	keys_file="$(mktemp)"
	awk -v want="$index" -F'\t' '$1 == "KEY" && $2 == want { print $3 }' "$steps_file" >"$keys_file"
	if [[ -s "$keys_file" ]]; then
		(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- record --step-id "${step_ids[$index]:-}" --keys-file "$keys_file" "$outcome" --seconds "$seconds") || note "        (the run happened; recording it did not)"
	fi
	rm -f "$keys_file" "$outfile"
}

# Wait for every guest in flight and record each one. A BARRIER, and everything that is not a guest
# step is behind one: `--jobs` answers "how many QEMUs may run", and the answer has to be the same
# number wherever it is asked.
drain_guests() {
	local i
	for i in "${!guest_pids[@]}"; do
		wait "${guest_pids[$i]}" || true
		record_one_step "${guest_indexes[$i]}" "${guest_labels[$i]}" "${guest_outfiles[$i]}"
	done
	guest_pids=()
	guest_labels=()
	guest_indexes=()
	guest_outfiles=()
}
guest_indexes=()
guest_outfiles=()

# The plan is read on fd 3, not on stdin.
#
# On stdin the first step that reads anything - and `./test.sh` boots QEMU, which does - swallows
# the rest of the plan, and the run ends early reporting success over the steps it never took. That
# is a false green produced by the runner itself, which is the one place this milestone cannot
# afford one. A backgrounded guest gets fd 3 CLOSED for the same reason, from the other side.
while IFS=$'\t' read -r -u 3 marker index keys label command note_text; do
	[[ "$marker" == STEP ]] || continue
	step=$((step + 1))
	# EVERY SKIPPED STEP IS PRINTED. A selector that quietly trims its own scope is the defect
	# P02M0156 is named after, and a budget is that defect with a flag on it unless it says what it
	# did not run.
	if ((BUDGET > 0)) && [[ " $affordable " != *" $index "* ]]; then
		note "[$step/$count] SKIPPED (budget): $label - $keys key(s)"
		skipped+=("$label")
		continue
	fi
	# A STEP WHOSE PREREQUISITE FAILED IS NOT RUN. Its result could only be the prerequisite's
	# failure a second time, in a form that names the wrong thing.
	blockers=""
	for req in ${step_reqs[$index]}; do
		if [[ -n "${failed_ids[$req]:-}" || -n "${blocked_ids[$req]:-}" ]]; then
			blockers+=" $req"
		fi
	done
	if [[ -n "$blockers" ]]; then
		note "[$step/$count] BLOCKED: $label - ${blockers# } did not produce what this step reads"
		blocked+=("$label")
		# THIS STEP IS NOW A BLOCKER ITSELF. Without this line the suppression stops one level down
		# and a grandchild runs against an output nothing produced.
		blocked_ids["${step_ids[$index]:-}"]=1
		continue
	fi
	# A GUEST STEP IS THE ONLY THING `--jobs` LETS OVERLAP, and only with another guest step.
	#
	# The expensive item in any plan is a boot, and two boots of different targets have nothing to
	# contend over now that every writable image is per-run. Everything else - a gate that boots one
	# of its own, a conformance suite, a build - runs alone, because "how many QEMUs may run" must
	# have exactly one answer on this machine and a gate's inner boot is not counted by this loop.
	is_guest=0
	[[ "$command" == *"./test.sh --arch "* ]] && is_guest=1
	if ((is_guest == 0)); then
		drain_guests
	fi
	# A STEP THAT STARTS SEVERAL GUESTS AT ONCE TAKES THAT MANY SLOTS, AND IS NOT RUN WITHOUT THEM.
	#
	# It is behind the barrier above, so nothing else is running when it starts - but a barrier is
	# not a budget. `concurrent-selection` starts two x86_64 suites simultaneously, and under
	# `--jobs 1` that put two QEMUs on a machine whose one answer was one: an inner scheduler with a
	# hardcoded width, which is exactly what this runner exists to be the only one of. Refused rather
	# than trimmed, because a gate about overlap that runs one guest proves nothing and would report
	# a pass for it. INCOMPLETE is the honest outcome and never reads as green.
	wants_guests="${step_guests[$index]:-0}"
	if ((wants_guests > JOBS)); then
		note "[$step/$count] SKIPPED (budget): $label - it starts $wants_guests guests at once and --jobs is $JOBS; run it with --jobs $wants_guests or more"
		skipped+=("$label")
		continue
	fi
	# AND THE STEP IS TOLD WHAT IT MAY START, so it refuses rather than exceeding a number it was
	# never given. The runner cannot see inside a gate; the gate can be told the answer.
	export LIBER_CONCURRENT_GUESTS="$JOBS"
	echo
	note "[$step/$count] $label - $keys key(s)"
	[[ -n "$note_text" ]] && note "        $note_text"
	# A selection of two hundred ids is a nine-kilobyte command line, and printing it whole buries
	# the run it belongs to. The command that RUNS is untouched; only the echo is shortened.
	if ((${#command} > 200)); then
		note "        ${command:0:150}... [${#command} chars] ${command##* }"
	else
		note "        $command"
	fi
	outfile="$(mktemp)"
	if ((is_guest == 1 && JOBS > 1)); then
		while ((${#guest_pids[@]} >= JOBS)); do
			wait "${guest_pids[0]}" || true
			record_one_step "${guest_indexes[0]}" "${guest_labels[0]}" "${guest_outfiles[0]}"
			guest_pids=("${guest_pids[@]:1}")
			guest_labels=("${guest_labels[@]:1}")
			guest_indexes=("${guest_indexes[@]:1}")
			guest_outfiles=("${guest_outfiles[@]:1}")
		done
		run_one_step "$index" "$label" "$command" "$outfile" 3<&- &
		guest_pids+=("$!")
		guest_labels+=("$label")
		guest_indexes+=("$index")
		guest_outfiles+=("$outfile")
	else
		run_one_step "$index" "$label" "$command" "$outfile"
		record_one_step "$index" "$label" "$outfile"
	fi
done 3<"$steps_file"
drain_guests

# The runner's own arithmetic, checked. A loop that silently took fewer steps than the plan listed
# would report a pass over work it never did.
if ((step != count)); then
	die "the plan listed $count step(s) and the runner took $step - refusing to report a result over a plan it did not finish"
fi

echo
if ((${#failed[@]} > 0)); then
	# FAIL OUTRANKS INCOMPLETE. A run that skipped work AND found a defect is a failure, and
	# reporting the skipping is the smaller half of what happened.
	if ((${#skipped[@]} > 0)); then
		note "and ${#skipped[@]} step(s) were skipped for the budget: ${skipped[*]}"
	fi
	if ((${#blocked[@]} > 0)); then
		note "and ${#blocked[@]} step(s) were not run because what they read failed: ${blocked[*]}"
	fi
	die "${#failed[@]} of $count step(s) failed: ${failed[*]}"
fi
if ((${#skipped[@]} > 0)); then
	note "${#skipped[@]} of $count step(s) were not started for the budget: ${skipped[*]}"
	# ITS OWN STATUS, NEVER A GREEN. A caller writing `if ./verify.sh; then publish; fi` must not
	# read a partial run as a verification, and nothing in exit 0 would say otherwise.
	note "INCOMPLETE: this run verified part of what the change needs"
	exit 6
fi
note "all $count step(s) passed"

# What that green is WORTH, said before anything else.
#
# The design's rule is that a scoped answer is not believed because it is plausible: until a
# component has shadow evidence under the current model, a scoped run must not be the only thing
# standing behind a green. That rule existed in prose while the runner executed its plan regardless
# of any component's level, which is the gap between "we have a trust model" and "we use one".
#
# SAFE IS THE DEFAULT AND PERMISSIVE IS THE OPT-OUT, which is the way round it was not.
#
# It used to report and exit 0, with `VERIFY_REQUIRE_TRUST=1` as the way in to the strict contract.
# That is transparent and it is still the wrong default: `if ./verify.sh; then publish; fi` reads a
# SHADOW run as done, and nothing in the exit status says otherwise. A green that means "good
# evidence, not a verification" has to be distinguishable by a machine, not only by a person reading
# the note above it.
#
# So SHADOW exits 4 and STALE exits 5 - their own statuses, distinct from 1, so a caller can tell a
# failed step from an unproven one. `--allow-shadow` is how somebody says they know, and
# `VERIFY_REQUIRE_TRUST=1` is still accepted and now redundant.
#
# This will fail runs that used to pass, and that is the point: nothing is TRUSTED yet, so the honest
# answer for most changes today IS "scoped, unproven". `./verify.sh --shadow` produces the evidence
# and `./verify.sh --allow-shadow` says it is not needed this time.
level_line="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- level --stdin 2>/dev/null || true)"
level="${level_line%%$'\t'*}"
case "$level" in
FULL)
	note "verification level: FULL - everything ran, this stands on its own"
	;;
TRUSTED)
	note "verification level: TRUSTED - every changed component has shadow evidence under this model"
	;;
STALE)
	note "verification level: STALE - ${level_line#*$'\t'} key(s) this run did not cover are past the age window."
	note "     The bound is what makes age a bound: a scoped run cannot be the only green while work"
	note "     that old is uncovered. ./verify.sh --age  lists them, ./verify.sh --sweep  clears them."
	if [[ "${allow_shadow:-0}" != "1" ]]; then
		note "     --allow-shadow accepts this; exiting 5 so a caller can tell it from a failure."
		exit 5
	fi
	;;
SHADOW)
	note "verification level: SHADOW - ${level_line#*$'\t'} has no shadow evidence under this model yet."
	note "     A scoped green is good evidence and NOT equivalent to a full verification."
	note "     ./verify.sh --shadow  proves the selection for this change; ./verify.sh --sweep  sidesteps it."
	if [[ "${allow_shadow:-0}" != "1" ]]; then
		note "     --allow-shadow accepts this; exiting 4 so a caller can tell it from a failure."
		exit 4
	fi
	;;
*)
	note "verification level: unknown - the planner could not judge it, so treat this as scoped-only"
	;;
esac

# What this run did NOT cover, said out loud rather than left to be noticed.
#
# There is no CI, so "on a timer" schedules nothing. The alternative the milestone settles on is that
# stale keys join the next manual run - but joining them silently would turn a two-minute scoped run
# into a two-hour one without asking, and the same measurement that justifies the architecture policy
# says an emulated boot is 2877 s. So the runner REPORTS, every time, and names the command; acting
# on it is a decision with a price tag and belongs to whoever is at the terminal.
stale="$(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- age 2>/dev/null | head -1 || true)"
if [[ -n "$stale" ]]; then
	note "age: $stale"
	note "     ./verify.sh --age  to see them, ./verify.sh --sweep  to clear them at one revision"
fi
