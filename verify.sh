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
# EVERY RUN ENDS WITH A VERDICT, from the one EXIT dispatcher in lib.sh - and cleanups REGISTER with
# it rather than installing traps of their own. A run that fails is otherwise indistinguishable from
# a run that is still going, which is exactly how a failed build came to be waited on for half an
# hour. `docs/TESTING.md` states what the line and the terminal record do and do not promise.
arm_run_verdict
source "$SRC_DIR/tools/evidence.sh"

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
usage: verify.sh [--for-change | --for PATH[,PATH...] | --for-range A..B | --merge FILE | --release | --sweep]
                 [--jobs N] [--plan] [--explain] [--json] [--shadow] [--allow-shadow] [--candidate FILE]
                 [--catalog] [--model-hash] [--age] [--trust]

With no arguments, run the inner share of the working-tree change and write a merge handoff.
Commit exactly the tested content, then complete verification with --merge FILE.

  --for-change     everything the working tree says was changed (the default)
  --for PATH       plan for these paths instead of asking git
  --for-range A..B plan for the paths a commit range touched
  --merge FILE     complete an inner handoff at HEAD; no budget, four guest slots by default
  --release        the release run: one sealed snapshot, every release-required key, one dossier
    --out-root DIR   where the run's evidence is published (default .build/runs, outside the snapshot)
    --required FILE  a narrowed required set - a REHEARSAL, and the dossier says so
    --in-place       run over this tree as it stands, unsealed - a REHEARSAL, and the dossier says so
  --sweep          the whole suite on every target at one immutable revision, in a git worktree
  --shadow         run the FULL suite and compare it against what this change would have scoped
  --shadow-exec    the same, but RUN the selection first and compare the two runs (one target)
  --allow-shadow   accept evidence without shadow trust; inner still exits 6 (--dev is an alias)
  --candidate FILE plan the shadow comparison against a FROZEN narrowing and record it under that
                   candidate's model hash. The run that happens is unchanged and still full; what
                   the candidate changes is which selection the comparison is graded against, which
                   is how a narrowing earns the evidence its activation demands.
  --jobs N         guest slots (inner default 1, merge and release default 4; explicit values override them)
  --budget N       estimated seconds the inner run may start, including prerequisites
                   work without a current measurement or conservative seed is skipped
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
  ./verify.sh --merge .build/state/verify-tiers/RUN/handoff.json
  ./verify.sh --release

The plan is per PlanItemKey - (check, architecture, environment, configuration) - and the run
collapses those into commands: two hundred selected kernel tests are one boot, not two hundred.

A failure to PRODUCE a plan is never a pass. If the planner cannot answer, this script says
FULL VERIFICATION REQUIRED and exits non-zero.
EOF
}

mode=for-change
# Inner starts with one guest slot; merge and release use four unless the caller supplies --jobs.
#
# FOUR, AND IT GROWS ONLY BY MEASUREMENT: load made three timing assertions fail on 2026-10-20/21, so the width is
# raised only after five L2 runs in which the flake ledger (`docs/verification/FLAKES.md`) attributes no failure to
# load, and the gates that measure time or the machine's processes run alone (`catalog::ALONE_GATES`).
# Writable media and producer acquisitions remain private to each overlapping run.
#
# THIS IS THE ONLY SCHEDULER, and `--release` runs its plan through it. `--sweep` is flat by design - it
# consults no model and reads no plan, because it is what runs when the thing that makes choices is
# broken - so it takes no limiter and runs one architecture at a time instead.
JOBS=1
JOBS_EXPLICIT=0
merge_file=""
# A CEILING ON WHAT THIS RUN MAY START, in ESTIMATED seconds. Zero means no ceiling.
#
# Not a timeout: it decides what to start and never kills a step that overran its estimate. What it
# prices is a whole prerequisite-closed branch, so a budget cannot spend its time building and then
# decline to test. `--sweep`, `--release` and `--shadow` refuse it outright - each of those means
# "all of it", and a bounded version of "all of it" is a contradiction with a number attached.
# ZERO IS A BUDGET, AND "NO BUDGET" IS THE ABSENCE OF ONE (2026-09-03). The parser accepts
# `--budget 0` and every use of it was guarded by `BUDGET > 0`, so the one number that means "start
# nothing" meant "start everything" - the opposite of what a budget is, and the case the definition
# of done names by asking what happens when nothing fits. The flag being GIVEN is its own fact.
BUDGET=0
BUDGET_SET=0
paths=""
range=""
action=run
planner_flags=()

release_out_root=""
release_required_file=""
release_in_place=0
release_snapshot=""
release_run_dir=""
release_rehearsal=""
release_main_root="$REPO_ROOT"
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
	--merge)
		[[ $# -ge 2 ]] || die "--merge needs an inner handoff file"
		merge_file="$2"
		mode=merge
		shift 2
		;;
	--release)
		mode=release
		shift
		;;
	--out-root)
		[[ $# -ge 2 ]] || die "--out-root needs a directory"
		release_out_root="$2"
		shift 2
		;;
	--required)
		[[ $# -ge 2 ]] || die "--required needs a file of keys"
		release_required_file="$2"
		shift 2
		;;
	--in-place)
		release_in_place=1
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
		# Shadow queries run from both the repository root and src; keep one input pathname.
		[[ "$2" == /* ]] || candidate_arg=(--candidate "$PWD/$2")
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
		BUDGET_SET=1
		shift 2
		;;
	--jobs)
		JOBS="${2:?--jobs needs a number}"
		[[ "$JOBS" =~ ^[1-9][0-9]*$ ]] || die "--jobs takes a positive whole number, not '$JOBS'"
		JOBS_EXPLICIT=1
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

if [[ -n "$merge_file" ]]; then
	[[ "$mode" == merge && -z "$paths" && -z "$range" ]] || die "--merge cannot be combined with another change or flat mode"
	[[ "$action" == run && ${#candidate_arg[@]} == 0 ]] || die "--merge completes its handoff; display and shadow modes are separate entry points"
	((JOBS_EXPLICIT == 1)) || JOBS=4
fi
# The release - L2 in place, L3 sealed - runs the release plan through the same scheduler, at the same width.
if [[ "$mode" == release ]]; then
	((JOBS_EXPLICIT == 1)) || JOBS=4
fi

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
if ((BUDGET_SET == 1)); then
	case "$mode" in
	sweep | release | merge) die "--budget cannot be combined with --$mode: that mode's whole claim is that everything ran" ;;
	esac
	[[ "$action" == shadow ]] && die "--budget cannot be combined with --shadow: a shadow compares a scoped run against the FULL suite, and a bounded full suite is not one"
fi

# THE RELEASE RUN: one immutable snapshot, one identity, every release-required key, one durable
# dossier.
#
# TRUSTED evidence, the cost estimator, the age bound and the selector are all mechanisms for
# spending less on an ORDINARY change, and every one of them is ignored here: the plan is the
# release plan - every release-required key at this revision, from the catalog - and the executor
# below runs it exactly as it runs any other plan. What the release adds is around the steps:
#
#   the SNAPSHOT     a detached worktree of HEAD, sealed read-only except `.build`, so a producer
#                    cannot consume an edit nobody sees (`--in-place` skips both and is a rehearsal)
#   the RUN          `verify-model run-start` in a durable root OUTSIDE the snapshot: the run id and
#                    the identity block - source, tools, firmware, configurations, permitted
#                    environment - collected before anything runs, and an undeclared override
#                    refuses the run before it starts
#   the EVIDENCE     `LIBER_VERIFY_RUN` tells every producer to publish: the gate runner, the guest
#                    runner, the image builder and the lifecycle gate publish their own envelopes and
#                    keep their logs; `record_one_step` publishes for a required key whose producer
#                    did not, from the step's captured output
#   the DOSSIER      collected at the end whatever happened: every required key with its outcome,
#                    inputs, outputs and logs by digest, refusals for anything missing, foreign,
#                    stale, altered or duplicated, and the tree compared against the identity it
#                    started with; the run directory is then made read-only
release_cleanup() {
	if [[ -n "$release_snapshot" && "$release_snapshot" != "$release_main_root" ]]; then
		(cd "$release_main_root" && src/tools/release-snapshot.sh remove "$release_snapshot")
	fi
}
release_prepare() {
	local revision out_root="${release_out_root:-$release_main_root/.build/runs}"
	[[ "$out_root" == /* ]] || out_root="$PWD/$out_root"
	if ((release_in_place == 1)); then
		release_snapshot="$release_main_root"
		release_rehearsal="in place: the tree was neither snapshotted nor sealed"
		note "release REHEARSAL, in place: the tree is used as it stands, writable; the dossier will say so"
	else
		[[ -z "$(git status --porcelain 2>/dev/null)" ]] || die "the working tree is dirty; a release is cut from a revision, not from a desk (--in-place rehearses the release path over this tree and never produces a release)"
		revision="$(git rev-parse HEAD)"
		release_snapshot="$BUILD_DIR/release/$revision"
		[[ "$out_root" != "$release_snapshot"* ]] || die "--out-root must be OUTSIDE the snapshot, which is removed when the run ends"
		note "release snapshot: $revision at $release_snapshot"
		src/tools/release-snapshot.sh create "$release_snapshot" "$revision" >/dev/null || die "could not create the snapshot"
		# REGISTERED THE MOMENT THE SNAPSHOT EXISTS, so a failure between here and the rest of the
		# release setup still removes it.
		register_run_cleanup release_cleanup
	fi
	if [[ -n "$release_required_file" ]]; then
		[[ "$release_required_file" == /* ]] || release_required_file="$PWD/$release_required_file"
		[[ -r "$release_required_file" ]] || die "--required names a file this cannot read: $release_required_file"
		release_rehearsal="${release_rehearsal:+$release_rehearsal; }narrowed required set: $release_required_file"
		note "release REHEARSAL: the required set is $release_required_file, not the frozen list"
	fi
	mkdir -p "$out_root"
	# THE RUN AND ITS IDENTITY, before anything runs. Collected by the snapshot's own model, so a
	# release's identity is computed by the revision it describes.
	release_run_dir="$(cd "$release_snapshot" && cargo run --quiet --manifest-path src/tools/verify-model/Cargo.toml -- run-start --out-root "$out_root")" || die "the run could not be started: an undeclared environment override, or a tree the identity cannot read"
	note "run: $release_run_dir"
	if ((release_in_place == 0)); then
		src/tools/release-snapshot.sh seal "$release_snapshot" || die "the snapshot could not be sealed"
	fi
	# AN L2 - IN PLACE, OVER THE WHOLE REQUIRED SET - IS MARKED, so a later identical one can continue what it left
	# (`continue-run`, after the plan). A sealed release (L3) and a narrowed rehearsal continue nothing.
	if ((release_in_place == 1)) && [[ -z "$release_required_file" ]]; then
		echo L2 >"$release_run_dir/level"
	fi
	export LIBER_VERIFY_RUN="$release_run_dir"
	export LIBER_RELEASE_REQUIRED="${release_required_file:-$release_snapshot/src/tools/verify-model/model/release-required.toml}"
	# THE HISTORY IS THE TREE'S, read and written there: a sealed release recorded its steps' prices into the snapshot
	# it deletes, so the next run started unpriced and sorted the cheap host suites last. Prices order and admit work;
	# they are no evidence, so a release reading the tree's changes nothing it proves.
	export LIBER_VERIFY_HISTORY="$release_main_root/.build/state/verify-history.json"
	# THE EXECUTOR'S HOME IS THE SNAPSHOT from here on: every step, every producer, every model query.
	cd "$release_snapshot"
	REPO_ROOT="$release_snapshot"
	SRC_DIR="$REPO_ROOT/src"
	BUILD_DIR="$REPO_ROOT/.build"
	PLANNER_MANIFEST="$SRC_DIR/tools/verify-model/Cargo.toml"
	# shellcheck source=src/tools/evidence.sh
	source "$SRC_DIR/tools/evidence.sh"
}
release_finish() {
	local failures="$1" incomplete="${2:-}" status=0 state
	local -a rehearsal=()
	[[ -z "$release_rehearsal" ]] || rehearsal=(--rehearsal "$release_rehearsal")
	echo
	note "release: collecting the dossier for run $release_run_dir"
	(cd "$SRC_DIR/tools/verify-model" && cargo run --quiet --manifest-path Cargo.toml -- dossier --run "$release_run_dir" --required "$LIBER_RELEASE_REQUIRED" "${rehearsal[@]}") || status=$?
	state="$(sed -n 's/^# Release dossier - run [^ ]* (\(.*\))$/\1/p' "$release_run_dir/dossier.md" 2>/dev/null | head -1)"
	# FINALIZED IMMUTABLY: the run directory and everything in it is read-only from here on. A failed
	# or incomplete run keeps its evidence with its state rather than losing it with the snapshot.
	chmod -R a-w "$release_run_dir" 2>/dev/null || true
	note "release: dossier $release_run_dir/dossier.md (state: ${state:-not rendered})"
	if ((failures > 0 || status != 0)); then
		die "the release run is NOT complete: $failures failed step(s); dossier state: ${state:-not rendered}"
	fi
	if [[ -n "$incomplete" ]]; then
		note "the release run is INCOMPLETE: $incomplete; dossier state: ${state:-not rendered}"
		exit 6
	fi
	note "release run finished: ${state}"
	exit 0
}
# The logs sweep and the logs lock, for a run that runs steps: before the release moves into its
# snapshot, so the lock is the tree's own.
case "$action" in
run | shadow) logs_run_begin ;;
esac

# A RELEASE THAT ONLY ANSWERS A QUESTION BUILDS NOTHING (corrected 2026-10-26). The snapshot, the run and
# its identity are what the release RUN needs, and `--plan`, `--catalog`, `--age`, `--trust` and
# `--model-hash` made and sealed a worktree of the revision to print an answer - and `--plan` then
# stopped on `changed: unbound variable`, because a release reads no change set. They are answered from
# the tree's own model.
release_answers=0
if [[ "$mode" == release ]]; then
	case "$action" in
	plan | json | catalog | model-hash | age | trust) release_answers=1 ;;
	esac
fi
if [[ "$mode" == release && "$release_answers" == 0 ]]; then
	note "release: every optimisation in this script is ignored"
	release_prepare
elif [[ "$mode" != release && (-n "$release_out_root" || -n "$release_required_file" || "$release_in_place" == 1) ]]; then
	die "--out-root, --required and --in-place belong to --release"
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
	sweep_worktree="$worktree"
	remove_sweep_worktree() {
		[[ -n "${sweep_worktree:-}" ]] || return 0
		git worktree remove --force "$sweep_worktree" >/dev/null 2>&1 || true
	}
	register_run_cleanup remove_sweep_worktree
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

# THE FOUR MODEL QUERIES RUN AS CHILDREN AND NOT AS `exec`.
#
# `exec` REPLACES THIS SHELL, so its EXIT trap never runs: every one of these four answered with the
# planner's status and NO verdict at all - the exact silence this milestone exists to remove. Run as
# an owned child, the planner's real status reaches the dispatcher, which reports it.
case "$action" in
catalog | model-hash | age | trust)
	model_status=0
	run_owned cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "$action" || model_status=$?
	exit "$model_status"
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
merge)
	# The tier model reads the pinned commit delta after validating the handoff.
	changed=""
	;;
release)
	# A RELEASE READS NO CHANGE SET: its plan is every release-required key at its revision.
	changed=""
	;;
for-change)
	# A PREPARED PLAN IS ABOUT THE EXECUTOR AND NOT ABOUT THE TREE (2026-09-03).
	#
	# `LIBER_VERIFY_STEPS` hands this script the steps to run, so there is no change set to read and
	# a clean tree is not a reason to refuse: `./check.sh --gate verify-scheduler` drives the
	# executor over synthetic plans and got "the working tree is clean" for every one of its cases,
	# which is why that gate could only ever pass over a DIRTY tree - and a checkout is clean.
	if [[ -n "${LIBER_VERIFY_STEPS:-}" ]]; then
		changed=""
	else
		changed="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- changes)" || planner_failed "the planner could not read what changed"
		[[ -n "$changed" ]] || die "--for-change: the working tree is clean (use --for PATH, or --release)"
	fi
	;;
for)
	changed="$(printf '%s\n' "$paths" | tr ',' '\n' | grep -v '^$')"
	;;
for-range)
	changed="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- changes --range "$range")" || planner_failed "the planner could not read the range '$range'"
	[[ -n "$changed" ]] || die "--for-range: '$range' touched no files (or is not a range git knows)"
	;;
esac
[[ "$mode" == release ]] || note "$(printf '%s\n' "$changed" | wc -l) changed path(s)"

# The display modes go through the same boundary as the run.
#
# They are only printing, so the temptation is to let them fail quietly - but a reader who asks what
# would run and is shown nothing concludes that nothing would, and that is the same wrong answer the
# execution path is guarded against. Silence has to be impossible everywhere or it is trusted
# somewhere.
if [[ "$action" == json || "$action" == plan ]]; then
	rendered="$(mktemp)"
	remove_rendered() {
		rm -f "$rendered"
	}
	register_run_cleanup remove_rendered
	if [[ "$mode" == release ]]; then
		# THE PLAN THE RELEASE RUN WOULD EXECUTE, in the executor's own format - narrowed, as the run
		# would be, by `--required`.
		[[ "$action" == plan ]] || die "the release plan has no JSON form: --release --plan prints it in the executor's own format"
		release_plan_args=()
		if [[ -n "$release_required_file" ]]; then
			[[ "$release_required_file" == /* ]] || release_required_file="$PWD/$release_required_file"
			release_plan_args=(--required "$release_required_file")
		fi
		cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- release-plan "${release_plan_args[@]}" >"$rendered" || planner_failed "the release plan could not be produced"
	elif [[ "$action" == json ]]; then
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
	# EVERY QUESTION IS ASKED OF ONE MODEL, AND WHEN A CANDIDATE IS GIVEN THAT MODEL IS THE
	# CANDIDATE (2026-09-03).
	#
	# `--candidate` reached only the final `shadow` comparisons. Everything that DECIDED what to run
	# - the booted and built targets, the guest selection, the scoped host and dev lists, the grouped
	# build steps - was asked of the ACTIVE model, so for a genuine narrowing the run executed the
	# active model's wider selection and then compared it against the candidate's narrower one.
	# `compare_exec` correctly reads the difference as execution the candidate did not select and
	# refuses to record a `shadow_exec` sample, which is precisely the evidence a narrowing needs and
	# the one thing this route existed to produce. The tree digest below is the exception and stays
	# candidate-free: it is a fact about the FILES, which no overlay changes.
	source_before="$(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- source-digest)" || planner_failed "could not digest the tree"
	model_before="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" model-hash)"
	# Evidence is private until every comparison has observed this same source/model.
	mkdir -p "$BUILD_DIR/logs"
	shadow_dir="$(mktemp -d "$BUILD_DIR/logs/verify-shadow.XXXXXXXX")"
	pending_records="$shadow_dir/records.json"
	remove_pending_records() {
		rm -f "$pending_records"
	}
	register_run_cleanup remove_pending_records
	targets="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" booted --stdin)" || planner_failed "the planner could not name the targets"
	[[ -n "$targets" ]] || planner_failed "the plan named no target to sweep"
	# THE BUILT SET, WHICH IS NOT THE BOOTED SET. A change that boots one target still has to compile
	# on the other two, and the build-evidence producer below asks a question about BUILDS - so it
	# loops over this and the guest shadow keeps `targets`. The two must not merge back: the plan
	# carries both fields because they answer different questions.
	build_targets="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" built --stdin)" || planner_failed "the planner could not name the build targets"
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
			selection="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" guest-selection --stdin --arch "$target")" || planner_failed "the planner could not name the guest selection"
			if [[ -n "$selection" ]]; then
				note "shadow-exec: running the selection on $target first (${selection//$'\n'/,})"
				scoped_capture="$(mktemp)"
				TEST_SELECTION="$(printf '%s' "$selection" | tr '\n' ',')" ./test.sh --arch "$target" 2>&1 | tee "$scoped_capture" || true
				scoped_log="$(suite_result_log "$scoped_capture")" || die "the $target scoped run did not say which logs it wrote"
				rm -f "$scoped_capture"
				[[ -n "$scoped_log" ]] || die "no $target scoped guest log to compare against"
				scoped_arg=(--scoped-log "$scoped_log")
			else
				note "shadow-exec: the plan selects no guest test on $target, so there is nothing to execute"
			fi
		fi
		sweep_capture="$(mktemp)"
		./test.sh --arch "$target" 2>&1 | tee "$sweep_capture" || true
		log="$(suite_result_log "$sweep_capture")" || die "the $target sweep did not say which logs it wrote"
		rm -f "$sweep_capture"
		[[ -n "$log" ]] || die "no $target guest log to compare against"
		printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --pending-records "$pending_records" --stdin --guest-log "$log" --arch "$target" "${scoped_arg[@]}") || shadow_failed=1
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
	host_log="$shadow_dir/host-full.txt"
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
		host_scoped_log="$shadow_dir/host-scoped.txt"
		: >"$host_scoped_log"
		host_scoped_total=0
		# The selection's own keys, lowered the same way the sweep lowers them - `commands` is what
		# both read, so the two sides cannot drift.
		host_scoped_ids="$(printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" host-checks --stdin --scoped))" || planner_failed "the planner could not list the scoped host checks"
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
		host_scoped_arg=(--host-scoped-log "$host_scoped_log")
	fi
	printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --pending-records "$pending_records" --stdin --host-log "$host_log" "${host_scoped_arg[@]}") || shadow_failed=1

	# The DEV GUEST universe, the third and last producer.
	#
	# It was declared and unfed: `Universe::of` routes `dev.*` checks to it and `trusted_everywhere`
	# asks the catalog which universes may judge a component, so `bin.dev_agent`, `bin.dev_channel`,
	# `harness.boot` and `proto` each required a certificate that no code path could grant. Two of
	# those are ordinary components, not development curiosities.
	dev_log="$shadow_dir/dev-full.txt"
	: >"$dev_log"
	dev_ids="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" dev-checks)" || planner_failed "the planner could not list the dev-guest checks"
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
		dev_scoped_log="$shadow_dir/dev-scoped.txt"
		: >"$dev_scoped_log"
		dev_scoped_total=0
		dev_scoped_ids="$(printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" dev-checks --stdin --scoped))" || planner_failed "the planner could not list the scoped dev checks"
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
		dev_scoped_arg=(--dev-scoped-log "$dev_scoped_log")
	fi
	printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --pending-records "$pending_records" --stdin --dev-log "$dev_log" "${dev_scoped_arg[@]}") || shadow_failed=1

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
		build_log="$shadow_dir/build-$build_arch.txt"
		: >"$build_log"
		build_ids="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" build-checks)" || planner_failed "the planner could not list the build checks"
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
			build_steps="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" build-steps --stdin)" || planner_failed "the planner could not lower the build steps"
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
		printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- "${candidate_arg[@]}" shadow --pending-records "$pending_records" --stdin --build-log "$build_log" --build-arch "$build_arch" --build-exec "$build_exec") || shadow_failed=1
	done

	source_after="$(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- source-digest)"
	model_after="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" model-hash)"
	if [[ "$source_before" != "$source_after" || "$model_before" != "$model_after" ]]; then
		die "the tree moved while the sweep ran, so the comparison judged two different systems and is void.
    source: $source_before -> $source_after
    model:  $model_before -> $model_after
    Use ./verify.sh --sweep for a comparison that cannot be overtaken, or leave the tree alone while this runs."
	fi
	cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- "${candidate_arg[@]}" shadow-publish --pending-records "$pending_records" --expected-source "$source_before" || die "the shadow evidence could not be published"
	[[ -z "${shadow_failed:-}" ]] || die "shadow did not come back Consistent - see the verdict above; a candidate miss must be confirmed before it is charged to the selector"
	note "shadow: no evidence against the selection, over a tree that did not move"
	exit 0
fi

# Ask for the commands, and treat every way of not getting them as the same answer.
steps_file="$(mktemp)"
remove_steps_file() {
	rm -f "$steps_file" "$steps_file.covered"
}
register_run_cleanup remove_steps_file
# AND THE RELEASE CLEANUP AGAIN, because this path is reached by a release run too - registration is
# idempotent in effect: `release_cleanup` is written to tolerate having nothing to do.
register_run_cleanup release_cleanup
tier_mode=""
tier_state=""
tier_outcomes=""
tier_handoff=""
# A PREPARED PLAN, WHICH IS HOW THE EXECUTOR ITSELF IS TESTED (added 2026-09-02).
#
# Everything below this line is the executor - the blocker suppression, the guest barrier, the
# `--jobs` reservation, the `FAIL`-over-`INCOMPLETE` arithmetic - and none of it was reachable by a
# test, because the only way to reach it was to run a real plan over a real tree. So its one defect
# class, an ordering one, could only be found by reading, and it was: three ordering corrections in
# four days, none of which a registered test would have caught.
#
# The file is the planner's own format and nothing else changes. A prepared plan carries no `KEY`
# lines, and `record_one_step` files nothing against the history for a step with no keys - so the
# executor can be driven over synthetic steps whose commands are `true` and `false` without touching
# the tree's record. See `tools/check-verify-scheduler.sh`, which is the only caller.
if [[ -n "${LIBER_VERIFY_STEPS:-}" ]]; then
	[[ -r "$LIBER_VERIFY_STEPS" ]] || die "LIBER_VERIFY_STEPS names a file this cannot read: $LIBER_VERIFY_STEPS"
	cat "$LIBER_VERIFY_STEPS" >"$steps_file"
elif [[ "$mode" == release ]]; then
	# THE RELEASE PLAN, from the snapshot's model: every release-required key, producers before the
	# gates that read what they build, the suites before the gate that reads a guest log.
	release_plan_args=()
	[[ -z "$release_required_file" ]] || release_plan_args=(--required "$LIBER_RELEASE_REQUIRED")
	cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- release-plan "${release_plan_args[@]}" >"$steps_file" || planner_failed "the release plan could not be produced"
	# A RUN CONTINUES WHAT AN IDENTICAL ONE LEFT: an L2 takes over the passed envelopes of the newest earlier L2 whose
	# tree, tools, firmware and configuration are this one's, and runs only what failed or never ran.
	if [[ -f "$release_run_dir/level" ]]; then
		cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- continue-run --run "$release_run_dir" --steps "$steps_file" >"$steps_file.covered" || planner_failed "the earlier run could not be continued"
	fi
else
	mkdir -p "$BUILD_DIR/state/verify-tiers"
	tier_run="$(mktemp -d "$BUILD_DIR/state/verify-tiers/run.XXXXXX")"
	tier_state="$tier_run/state.json"
	tier_outcomes="$tier_run/outcomes.tsv"
	tier_handoff="$tier_run/handoff.json"
	: >"$tier_outcomes"
	if [[ "$mode" == merge ]]; then
		tier_mode=merge
		cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-merge-prepare --handoff "$merge_file" --state "$tier_state" >"$tier_run/inventory.tsv" || planner_failed "merge refused its handoff"
		while IFS=$'\t' read -r marker architecture; do
			[[ "$marker" == TARGET ]] || planner_failed "unrecognized inventory preparation output"
			case "$architecture" in
			x86_64 | aarch64 | riscv64) ;;
			*) planner_failed "unknown inventory target '$architecture'" ;;
			esac
			note "merge inventory: $architecture"
			"$SRC_DIR/harness/test-kernel.sh" "$architecture" --build-only || die "merge inventory did not build for $architecture"
		done <"$tier_run/inventory.tsv"
		cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-merge-commands --state "$tier_state" >"$steps_file" || planner_failed "merge could not reconcile its refreshed inventory"
	else
		tier_mode=inner
		printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-inner --state "$tier_state" --stdin >"$steps_file" || planner_failed "the inner tier could not produce its partition"
	fi
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
	[[ -n "$tier_mode" ]] || exit 0
	;;
full)
	note "FULL: $status_detail"
	;;
scoped)
	note "scoped: $status_detail"
	;;
release)
	note "RELEASE: $status_detail"
	;;
*)
	planner_failed "unknown status '$status'"
	;;
esac

count="$(grep -c $'^STEP\t' "$steps_file" || true)"
((count > 0)) || [[ -n "$tier_mode" ]] || planner_failed "a $status plan with no steps"
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
	local budget="$1" total=0 chosen best best_cost add i j unknown
	local -A picked=() by_id=() closure=() unpriced=()
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
		for j in $seen; do
			[[ "${step_unpriced[$j]}" == 1 ]] && unpriced[$i]=1
		done
	done
	while :; do
		best=""
		best_cost=0
		unknown=0
		for i in "${!step_ids[@]}"; do
			[[ -n "${picked[$i]:-}" ]] && continue
			if [[ -n "${unpriced[$i]:-}" ]]; then
				unknown=1
				continue
			fi
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
		if [[ -z "$best" ]]; then
			if ((unknown == 1 && ${#picked[@]} == 0)); then
				note "no complete branch has a measured or conservatively seeded cost; the minimum budget is unknown, so nothing was started"
			fi
			break
		fi
		if ((total + best_cost > budget)); then
			if ((${#picked[@]} == 0)); then
				note "the cheapest priced complete branch costs an estimated ${best_cost}s and the budget is ${budget}s, so nothing was started"
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
step_unpriced=()
# HOW MANY GUESTS A STEP STARTS AT THE SAME TIME, for the few that start more than one.
#
# `--jobs` answers "how many QEMUs may run on this machine", and the answer has to be the same
# wherever it is asked - including inside a step. Every gate in this tree boots serially and needs
# one slot; `concurrent-selection` exists to prove that two same-architecture suites do not collide,
# so it starts two AT ONCE and cannot be made serial without deleting its subject. The model
# declares the count, this reserves it, and a `--jobs` that cannot hold it does not run the step.
step_guests=()
step_exclusive=()
# WHAT A STEP USES, as the plan declares it: `RESOURCE=ACCESS:STATE` tokens (`verify-model`'s `resources`), ACCESS `r`,
# `w` or `x` - one holder at a time - and STATE what a staging or a bootable volume has to be in for it. Two steps whose
# tokens conflict never run at once, and a step whose staging is in another state waits until nothing uses it and has
# it brought about. A step that declares nothing uses nothing this runner knows of.
step_uses=()
while IFS=$'\t' read -r marker index rest; do
	case "$marker" in
	STEPID) step_ids[$index]="$rest" ;;
	STEPREQ) step_reqs[$index]="${step_reqs[$index]:-} $rest" ;;
	STEPCOST) step_costs[$index]="$rest" ;;
	STEPUNPRICED) step_unpriced[$index]=1 ;;
	STEPGUESTS) step_guests[$index]="$rest" ;;
	STEPEXCLUSIVE) step_exclusive[$index]="$rest" ;;
	STEPUSES) step_uses[$index]="$rest" ;;
	esac
done <"$steps_file"
for i in "${!step_ids[@]}"; do
	step_reqs[$i]="${step_reqs[$i]:-}"
	step_costs[$i]="${step_costs[$i]:-0}"
	step_unpriced[$i]="${step_unpriced[$i]:-0}"
	step_guests[$i]="${step_guests[$i]:-0}"
	step_exclusive[$i]="${step_exclusive[$i]:-0}"
	[[ "${step_exclusive[$i]}" == 0 || "${step_exclusive[$i]}" == 1 ]] || die "step $i has an invalid exclusive-guest declaration: ${step_exclusive[$i]}"
	step_uses[$i]="${step_uses[$i]:-}"
done

BUDGET_TOTAL=0
affordable=""
if ((BUDGET_SET == 1)); then
	selection="$(budget_select "$BUDGET")"
	BUDGET_TOTAL="${selection%% *}"
	affordable="${selection#* }"
	[[ "$affordable" == "$selection" ]] && affordable=""
	note "budget ${BUDGET}s: starting an estimated ${BUDGET_TOTAL}s of work"
fi

failed=()
skipped=()
# THE RUNNER OWNS THE STAGING'S STATE WHILE IT RUNS (`staging-state.sh`), and says so to every step: a
# step that switches into the development profile leaves it there (`guest-gate.sh` puts it back only
# when it is run by hand), a step that needs another state refuses and is run again once that state is
# brought about, and the run ends with the staging it changed put back, once.
export LIBER_STAGING_RUNNER=1
declare -A state_retried=() step_commands=()
state_queue=()
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

# THE STAGING'S STATE IN ONE STRING: the profile of every part of every target, and the bootable
# volume's DMA mode - what a run compares at its end against its start.
staging_fingerprint() {
	local record
	for record in "$BUILD_DIR"/state/built-*.profile "$BUILD_DIR"/boot/system-volume-bootable-*.dma-mode; do
		[[ -f "$record" ]] && printf '%s=%s\n' "${record##*/}" "$(<"$record")"
	done
	return 0
}

# PUT BACK ONCE, WHEN THE RUN ENDS, what the run changed: every target a step left in another profile
# goes back to the shipping one, and the x86_64 bootable volume is signed `enforcing-required` again.
# A run that changed nothing leaves the staging as it found it, and a sealed snapshot's staging goes
# with the snapshot.
restore_staging() {
	[[ -z "$release_snapshot" || "$release_snapshot" == "$release_main_root" ]] || return 0
	[[ "$(staging_fingerprint)" != "$staging_at_start" ]] || return 0
	local arch command
	for arch in "${ARCHS_ALL[@]}"; do
		grep -qvx shipping "$BUILD_DIR"/state/built-"$arch"-*.profile 2>/dev/null || continue
		command="$(staging_build_command "$arch" shipping)"
		note "staging: the run left the $arch staging in another profile; putting it back:  $command"
		run_owned_shell "$command" || note "staging: the $arch staging could not be put back - run:  $command"
	done
	# THE VOLUME BY ITS PROFILE AND ITS MODE: a cold run leaves it `enforcing-required` in the development profile.
	if [[ -f "$BUILD_DIR/boot/system-volume-bootable-x86_64.img" && "$(volume_state_of "$BUILD_DIR" x86_64)" != shipping/enforcing-required ]]; then
		command="env -u LIBER_DEVELOPMENT ./build.sh --arch x86_64 --kernel-on-volume --dma-mode enforcing-required --part volume"
		note "staging: the run left the x86_64 bootable volume in another state; building it again:  $command"
		run_owned_shell "$command" || note "staging: the bootable volume could not be signed again - run:  $command"
	fi
}

# ONE COMMAND, RUN AND RECORDED. Split out of the loop so it can be called in the foreground or in a
# background job without the two drifting apart.
#
# It writes its outcome and duration to a file rather than recording them itself: the history is
# written by ONE writer, and parallel steps updating it concurrently is a lost-update race in the
# file the estimator reads.
run_one_step() {
	local index="$1" label="$2" command="$3" outfile="$4" started=$SECONDS status=0 elapsed
	if [[ -n "${LIBER_VERIFY_RUN:-}" ]]; then
		# INSIDE A RUN THE STEP'S OUTPUT IS ITS RESULT LOG: streamed as before, through `tee`, and
		# copied into the run by the envelope `record_one_step` publishes for a producer that did not
		# publish its own. `pipefail` makes the pipeline's status the step's.
		# Owned and waited for, so a signal to the executor stops the step it started rather than
		# being answered when that step finishes on its own.
		# THE STEP'S OWN STATUS, not a flattened 1: a refusal (`STATE_REFUSED`) is told apart from a
		# failure by it.
		LIBER_STATE_REFUSAL="$outfile.refusal" run_owned_shell 'eval "$command" 2>&1 | tee "$LIBER_VERIFY_RUN/logs/.step-$index.log"' || status=$?
	else
		LIBER_STATE_REFUSAL="$outfile.refusal" run_owned_shell 'eval "$command"' || status=$?
	fi
	elapsed=$((SECONDS - started))
	# The clock has whole-second resolution. Round an observed successful sub-second run up,
	# so unbudgeted execution can measure fast host work without inventing a cold-start seed.
	if ((status == 0 && elapsed == 0)); then elapsed=1; fi
	printf '%s\t%s\n' "$status" "$elapsed" >"$outfile"
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
	# A REFUSED STEP RAN NOTHING (`staging-state.sh`): what it boots was not in the state it needs, and
	# it said so before it tested anything. So it publishes no envelope and records no history - its
	# outcome is that of its next run, in the state it needs. The first refusal of a step that named
	# its fix is run again once that state is brought about - when nothing else runs, since other steps
	# may still be booting the staging the fix rebuilds; a second refusal, or one with no fix, is a failure.
	if [[ "$status" == "$STATE_REFUSED" ]]; then
		if [[ -z "${state_retried[$index]:-}" && -s "$outfile.refusal" ]]; then
			if [[ "${4:-}" == background ]]; then
				state_queue+=("$index"$'\t'"$label"$'\t'"$outfile")
			else
				retry_refused_step "$index" "$label" "$outfile"
			fi
			return 0
		fi
		note "REFUSED: $label - it needs another state of the staging, named above; nothing is recorded for it"
		failed+=("$label (refused)")
		failed_ids["${step_ids[$index]:-}"]=1
		[[ -z "${LIBER_VERIFY_RUN:-}" ]] || rm -f "$LIBER_VERIFY_RUN/logs/.step-$index.log"
		rm -f "$outfile" "$outfile.refusal"
		return 0
	fi
	rm -f "$outfile.refusal"
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
	if [[ -n "${LIBER_VERIFY_RUN:-}" ]]; then
		release_publish_fallback "$index" "${outcome#--}" "$seconds" "$keys_file"
	fi
	if [[ -s "$keys_file" ]]; then
		if [[ -n "$tier_outcomes" ]]; then
			# The handoff's evidence is recorded before the best-effort mutable history.
			awk -v outcome="${outcome#--}" '{ print outcome "\t" $0 }' "$keys_file" >>"$tier_outcomes"
		fi
		(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- record --step-id "${step_ids[$index]:-}" --command "${step_commands[$index]:-}" --keys-file "$keys_file" "$outcome" --seconds "$seconds") || note "        (the run happened; recording it did not)"
	fi
	rm -f "$keys_file" "$outfile"
}

# THE RUNNER'S OWN ENVELOPE FOR A REQUIRED KEY WHOSE PRODUCER PUBLISHED NONE.
#
# Builds, host suites and conformance suites publish nothing themselves: this step ran their command
# and is their producer, and it says so. A gate, a whole suite, an image producer and the lifecycle
# publish their own, with their inputs and their kept logs; `--if-absent` leaves those standing and
# writes one only for a producer that crashed before it could publish, whose outcome is `failed`.
# Only REQUIRED keys, so a whole-suite step's kernel tests are discharged by the guest runner's
# envelope rather than given hundreds of envelopes of their own.
release_publish_fallback() {
	local index="$1" outcome="$2" seconds="$3" keys_file="$4" key step_log="$LIBER_VERIFY_RUN/logs/.step-$index.log"
	while IFS= read -r key; do
		[[ -n "$key" ]] || continue
		grep -qF "\"$key\"" "${LIBER_RELEASE_REQUIRED:-/dev/null}" || continue
		evidence_publish "$key" "verify.sh step ${step_ids[$index]:-$index}" "$outcome" "$seconds" --if-absent --log "$step_log"
	done <"$keys_file"
	rm -f "$step_log"
}

# THE STATE A REFUSED STEP NEEDS, BROUGHT ABOUT, AND THE STEP RUN AGAIN - ONCE (`staging-state.sh`).
#
# The staging is switched when a step needs another state, not put back after every step that used
# it: a step that switches into the development profile leaves it there, and the next one that needs
# the shipping profile - or another DMA mode on the bootable volume - refuses, naming the commands that
# fix it. They are run here, with nothing else running, and the step's second run is its outcome.
# Until the catalog's steps declare what they read, the refusal is the declaration.
retry_refused_step() {
	local index="$1" label="$2" outfile="$3" fix fixed=1
	state_retried[$index]=1
	note "REFUSED: $label - bringing the staging to the state it needs, then running it once more"
	while IFS= read -r fix; do
		[[ -n "$fix" ]] || continue
		note "        $fix"
		if ! run_owned_shell "$fix"; then
			fixed=0
			break
		fi
	done < <(awk '!seen[$0]++' "$outfile.refusal")
	rm -f "$outfile.refusal"
	if ((fixed == 0)); then
		note "        the state it needs could not be brought about"
		printf '%s\t0\n' "$STATE_REFUSED" >"$outfile"
	else
		run_one_step "$index" "$label" "${step_commands[$index]}" "$outfile"
	fi
	record_one_step "$index" "$label" "$outfile"
}

# WHETHER TWO STEPS' DECLARATIONS CONFLICT: a resource both use, written or held by either, or needed by both in two
# states - a staging is in one profile at a time, and a bootable volume is signed for one mode.
uses_conflict() {
	local mine theirs
	for mine in $1; do
		for theirs in $2; do
			[[ "${mine%%=*}" == "${theirs%%=*}" ]] || continue
			[[ "${mine#*=}" == r:* && "${mine#*=}" == "${theirs#*=}" ]] || return 0
		done
	done
	return 1
}

# THE STATE A STAGING OR A BOOTABLE VOLUME IS IN NOW, as a token names it - a staging's profile, a volume's
# `PROFILE/MODE` - from the records the builds keep beside them (`staging-state.sh`); and the command that brings it
# to another.
resource_state() {
	case "$1" in
	stage:*) staging_profile_of "$BUILD_DIR" "${1#stage:}" ;;
	volume:*) volume_state_of "$BUILD_DIR" "${1#volume:}" ;;
	esac
}
resource_command() {
	case "$1" in
	stage:*) staging_build_command "${1#stage:}" "$2" ;;
	volume:*) echo "$(profile_env "${2%%/*}") ./build.sh --arch ${1#volume:} --kernel-on-volume --dma-mode ${2#*/} --part volume" ;;
	esac
}

# THE STEPS IN FLIGHT, and what they hold: the guest slots they take between them, the one host lane, and whether one
# that runs alone is among them.
running=()
declare -A run_pid=() run_out=() decided=()
guests_running=0
host_busy=0
alone_running=0

start_step() {
	local index="$1" command="${step_commands[$1]}" outfile
	step=$((step + 1))
	# AND THE STEP IS TOLD WHAT IT MAY START, so it refuses rather than exceeding a number it was
	# never given. The runner cannot see inside a gate; the gate can be told the answer.
	export LIBER_CONCURRENT_GUESTS="$JOBS"
	echo
	note "[$step/$count] ${step_label[$index]} - ${step_keys[$index]} key(s)"
	[[ -n "${step_note[$index]}" ]] && note "        ${step_note[$index]}"
	# A selection of two hundred ids is a nine-kilobyte command line, and printing it whole buries
	# the run it belongs to. The command that RUNS is untouched; only the echo is shortened.
	if ((${#command} > 200)); then
		note "        ${command:0:150}... [${#command} chars] ${command##* }"
	else
		note "        $command"
	fi
	outfile="$(mktemp)"
	run_one_step "$index" "${step_label[$index]}" "$command" "$outfile" &
	run_pid[$index]=$!
	run_out[$index]="$outfile"
	running+=("$index")
	if ((step_guests[index] > 0)); then
		guests_running=$((guests_running + step_guests[index]))
	else
		host_busy=1
	fi
	[[ "${step_exclusive[$index]}" == 1 ]] && alone_running=1
	return 0
}

# A FINISHED STEP, RECORDED BY THE PARENT - the one writer of the history. It is DECIDED, for the steps that require it,
# once its outcome is: a refusal queued for its fix is decided by its second run.
finish_step() {
	local index="$1" at queued="${#state_queue[@]}" kept=()
	record_one_step "$index" "${step_label[$index]}" "${run_out[$index]}" background
	((${#state_queue[@]} > queued)) || decided["${step_ids[$index]:-}"]=1
	for at in "${running[@]}"; do
		[[ "$at" == "$index" ]] || kept+=("$at")
	done
	running=(${kept[@]+"${kept[@]}"})
	if ((step_guests[index] > 0)); then
		guests_running=$((guests_running - step_guests[index]))
	else
		host_busy=0
	fi
	[[ "${step_exclusive[$index]}" == 1 ]] && alone_running=0
	unset 'run_pid[$index]' 'run_out[$index]'
	return 0
}

wait_one() {
	local done_pid="" index pids=()
	for index in "${running[@]}"; do pids+=("${run_pid[$index]}"); done
	wait -n -p done_pid "${pids[@]}" || true
	for index in "${running[@]}"; do
		if [[ "${run_pid[$index]}" == "$done_pid" ]]; then
			finish_step "$index"
			return 0
		fi
	done
}

# THE REFUSED STEPS, fixed and run again, with nothing else running: a fix rebuilds what other steps may be booting.
retry_queued() {
	local queued index label outfile
	for queued in "${state_queue[@]}"; do
		IFS=$'\t' read -r index label outfile <<<"$queued"
		retry_refused_step "$index" "$label" "$outfile"
		decided["${step_ids[$index]:-}"]=1
	done
	state_queue=()
}

# ONE PASS OVER THE STEPS NOT YET STARTED, in the plan's order, starting each that can start now.
#
# NOTHING IS DRAINED BUT WHAT CONFLICTS. This was a barrier before every step that boots no guest and before every
# exclusive one, which drained every guest in flight - so the run was serial in practice, 22.3 hours of steps on a
# host that stayed at a load of 1 to 3. A step starts once what it requires has run, a lane is free for it - a guest
# slot for a guest step, the one host lane for the rest, so the host work runs beside the guests - its declaration
# conflicts with nothing running, and its staging is in the state it needs; a step that runs alone starts when nothing
# else runs, and nothing starts beside it.
#
# THE STATE IS SWITCHED ONCE PER GROUP. The plan puts the steps that need one state of a staging together (the profile,
# then the bootable volume's mode); a step whose staging is in another state is held back while an earlier step still
# needs the one it is in, and when none does and nothing is using it the runner brings it about, then starts the step.
#
# A STEP READY BUT FOR ITS LANE HOLDS ITS PLACE: a guest step waiting for slots keeps later guest steps from taking them,
# and one that runs alone keeps everything later from starting, so neither waits for ever behind work that keeps coming.
dispatch() {
	local index req token resource state current waiting blockers conflict other reserve_guests=0 reserve_all=0 kept=() fix
	local -A first_state=()
	for index in ${pending[@]+"${pending[@]}"}; do
		if ((reserve_all == 1)); then
			kept+=("$index")
			continue
		fi
		# EVERY SKIPPED STEP IS PRINTED. A selector that quietly trims its own scope is the defect
		# P02M0156 is named after, and a budget is that defect with a flag on it unless it says what it
		# did not run.
		if ((BUDGET_SET == 1)) && [[ " $affordable " != *" $index "* ]]; then
			step=$((step + 1))
			note "[$step/$count] SKIPPED (budget): ${step_label[$index]} - ${step_keys[$index]} key(s)"
			[[ "${step_unpriced[$index]}" == 1 ]] && note "        unpriced: no current measurement or conservative seed; an unbudgeted run can measure it"
			skipped+=("${step_label[$index]}")
			decided["${step_ids[$index]:-}"]=1
			progress=1
			continue
		fi
		# A STEP WHOSE PREREQUISITE FAILED IS NOT RUN, and one whose prerequisite has not run yet waits for it - ONLY
		# PREREQUISITES IN THIS PLAN: a narrowed plan legitimately omits steps that other steps declare a requirement on.
		waiting=0
		blockers=""
		for req in ${step_reqs[$index]}; do
			[[ -n "${step_index_of[$req]:-}" ]] || continue
			if [[ -n "${failed_ids[$req]:-}" || -n "${blocked_ids[$req]:-}" ]]; then
				blockers+=" $req"
			elif [[ -z "${decided[$req]:-}" ]]; then
				waiting=1
			fi
		done
		if [[ -n "$blockers" ]]; then
			step=$((step + 1))
			note "[$step/$count] BLOCKED: ${step_label[$index]} - ${blockers# } did not produce what this step reads"
			blocked+=("${step_label[$index]}")
			# THIS STEP IS NOW A BLOCKER ITSELF. Without this line the suppression stops one level down
			# and a grandchild runs against an output nothing produced.
			blocked_ids["${step_ids[$index]:-}"]=1
			decided["${step_ids[$index]:-}"]=1
			progress=1
			continue
		fi
		# A STEP THAT STARTS SEVERAL GUESTS AT ONCE TAKES THAT MANY SLOTS, AND IS NOT RUN WITHOUT THEM.
		# Refused rather than trimmed, because a gate about overlap that runs one guest proves nothing and would
		# report a pass for it. INCOMPLETE is the honest outcome and never reads as green.
		if ((step_guests[index] > JOBS)); then
			step=$((step + 1))
			note "[$step/$count] SKIPPED (budget): ${step_label[$index]} - it starts ${step_guests[$index]} guests at once and --jobs is $JOBS; run it with --jobs ${step_guests[$index]} or more"
			skipped+=("${step_label[$index]}")
			decided["${step_ids[$index]:-}"]=1
			progress=1
			continue
		fi
		# HELD BACK, in its place: for a prerequisite, an earlier step's state, a conflict or a lane.
		conflict="$waiting"
		if ((conflict == 0)); then
			for token in ${step_uses[$index]}; do
				resource="${token%%=*}"
				[[ "$resource" == stage:* || "$resource" == volume:* ]] || continue
				state="${token#*=}"
				state="${state#*:}"
				if [[ -n "${first_state[$resource]:-}" && "${first_state[$resource]}" != "$state" ]]; then
					conflict=1
					break
				fi
			done
		fi
		if ((conflict == 0)); then
			for other in ${running[@]+"${running[@]}"}; do
				if uses_conflict "${step_uses[$index]}" "${step_uses[$other]}"; then
					conflict=1
					break
				fi
			done
		fi
		if ((conflict == 0)) && [[ "${step_exclusive[$index]}" == 1 ]] && ((${#running[@]} > 0)); then
			conflict=1
			reserve_all=1
		fi
		if ((conflict == 0)); then
			if ((step_guests[index] > 0)); then
				if ((reserve_guests == 1 || guests_running + step_guests[index] > JOBS)); then
					conflict=1
					reserve_guests=1
				fi
			elif ((host_busy == 1)); then
				conflict=1
			fi
		fi
		if ((conflict == 0)); then
			# ITS STAGING IN THE STATE IT NEEDS, brought about here when nothing running uses it.
			for token in ${step_uses[$index]}; do
				resource="${token%%=*}"
				[[ "$token" == *=r:* && ("$resource" == stage:* || "$resource" == volume:*) ]] || continue
				state="${token#*=r:}"
				current="$(resource_state "$resource")"
				[[ "$current" == "$state" ]] && continue
				for other in ${running[@]+"${running[@]}"}; do
					[[ " ${step_uses[$other]} " == *" $resource="* ]] && conflict=1
				done
				((conflict == 0)) || break
				fix="$(resource_command "$resource" "$state")"
				note "staging: ${step_label[$index]} needs $resource in '$state', and it is in '${current:-no recorded state}' - bringing it about:  $fix"
				run_owned_shell "$fix" || true
				progress=1
				current="$(resource_state "$resource")"
				if [[ "$current" != "$state" ]]; then
					step=$((step + 1))
					note "[$step/$count] REFUSED: ${step_label[$index]} - $resource could not be brought to '$state'; nothing is recorded for it"
					failed+=("${step_label[$index]} (refused)")
					failed_ids["${step_ids[$index]:-}"]=1
					decided["${step_ids[$index]:-}"]=1
					conflict=2
					break
				fi
			done
			((conflict == 2)) && continue
		fi
		if ((conflict != 0)); then
			kept+=("$index")
			for token in ${step_uses[$index]}; do
				resource="${token%%=*}"
				[[ "$resource" == stage:* || "$resource" == volume:* ]] || continue
				state="${token#*=}"
				[[ -n "${first_state[$resource]:-}" ]] || first_state[$resource]="${state#*:}"
			done
			continue
		fi
		start_step "$index"
		progress=1
		if [[ "${step_exclusive[$index]}" == 1 ]]; then
			reserve_all=1
		fi
	done
	pending=(${kept[@]+"${kept[@]}"})
}

# THE STEPS, in the plan's order - the order they are considered in - read whole before any runs. They were read on
# fd 3, one at a time between steps, because the first step that read stdin swallowed the rest of the plan; read
# whole, no step can.
order=()
declare -A step_label=() step_keys=() step_note=() step_index_of=()
while IFS=$'\t' read -r marker index keys label command note_text; do
	[[ "$marker" == STEP ]] || continue
	order+=("$index")
	step_label[$index]="$label"
	step_keys[$index]="$keys"
	step_note[$index]="$note_text"
	step_commands[$index]="$command"
done <"$steps_file"
for index in ${order[@]+"${order[@]}"}; do
	step_index_of["${step_ids[$index]:-}"]="$index"
done
pending=(${order[@]+"${order[@]}"})
# THE STEPS AN EARLIER IDENTICAL RUN ALREADY PASSED (`continue-run`): decided, and their envelopes are its.
if [[ -s "$steps_file.covered" ]]; then
	kept_order=()
	declare -A covered=()
	while IFS=$'\t' read -r marker index source; do
		[[ "$marker" == COVERED ]] && covered[$index]="$source"
	done <"$steps_file.covered"
	for index in ${pending[@]+"${pending[@]}"}; do
		if [[ -n "${covered[$index]:-}" ]]; then
			step=$((step + 1))
			note "[$step/$count] CONTINUED: ${step_label[$index]} - passed in run ${covered[$index]}, whose identity is this run's"
			decided["${step_ids[$index]:-}"]=1
		else
			kept_order+=("$index")
		fi
	done
	pending=(${kept_order[@]+"${kept_order[@]}"})
fi
staging_at_start="$(staging_fingerprint)"
while ((${#pending[@]} > 0 || ${#running[@]} > 0 || ${#state_queue[@]} > 0)); do
	progress=0
	if ((${#state_queue[@]} == 0 && alone_running == 0)); then
		dispatch
	fi
	((progress == 1)) && continue
	if ((${#running[@]} > 0)); then
		wait_one
	elif ((${#state_queue[@]} > 0)); then
		retry_queued
	else
		die "the runner could start none of the ${#pending[@]} step(s) left, and nothing is running: ${pending[*]}"
	fi
done
restore_staging

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
	# A RELEASE RUN COLLECTS ITS DOSSIER WHATEVER HAPPENED, and the dossier says what is missing.
	[[ -z "$release_run_dir" ]] || release_finish "${#failed[@]}"
	die "${#failed[@]} of $count step(s) failed: ${failed[*]}"
fi
if ((${#skipped[@]} > 0)); then
	note "${#skipped[@]} of $count step(s) were not started for the budget: ${skipped[*]}"
	# ITS OWN STATUS, NEVER A GREEN. A caller writing `if ./verify.sh; then publish; fi` must not
	# read a partial run as a verification, and nothing in exit 0 would say otherwise.
	note "INCOMPLETE: this run verified part of what the change needs"
	# AND A RELEASE COLLECTS ITS DOSSIER ALL THE SAME: a step skipped for the slots - the two-guest gate at
	# `--jobs 1` - left a run with no dossier at all, where the dossier is what says which keys it lacks.
	[[ -z "$release_run_dir" ]] || release_finish 0 "${#skipped[@]} step(s) were not started"
	exit 6
fi
note "all $count step(s) passed"
[[ -z "$release_run_dir" ]] || release_finish 0

if [[ "$tier_mode" == inner ]]; then
	cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-inner-finish --state "$tier_state" --outcomes "$tier_outcomes" --handoff "$tier_handoff" || planner_failed "the inner snapshot or its evidence changed; no usable handoff was produced"
	note "inner handoff: $tier_handoff"
	printf 'verify.sh: after committing exactly the tested content, run: ./verify.sh --merge %q\n' "$tier_handoff"
elif [[ "$tier_mode" == merge ]]; then
	cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-merge-finish --state "$tier_state" --outcomes "$tier_outcomes" || planner_failed "merge could not account for every obligation at the pinned revision"
fi

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
# AND A PREPARED PLAN JUDGES NOTHING, because it says nothing about the tree (2026-09-03).
#
# The steps came from a file rather than from the planner, over a change set that does not exist, so
# a trust level computed here is a verdict about a run that verified nothing - and it was reached:
# the scheduler gate's last case checks the executor's exit status, and this block exited 5 (STALE)
# over the caller's own working tree, on facts the gate has no relationship to. A run driven by
# `LIBER_VERIFY_STEPS` ends with the executor's own result, which is the only thing it measured.
if [[ -n "${LIBER_VERIFY_STEPS:-}" ]]; then
	note "prepared plan: the executor ran it; no trust level is claimed, because this run says nothing about the tree"
	exit 0
fi
level_line="$(cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- tier-level --state "$tier_state")" || planner_failed "the tier's evidence level could not be judged"
level="${level_line%%$'\t'*}"
case "$level" in
FULL)
	note "verification level: FULL - everything ran, this stands on its own"
	;;
INCOMPLETE)
	note "inner share passed; revision verification is still owed by the person proposing the change"
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
	planner_failed "unknown tier evidence level '$level'"
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

if [[ "$tier_mode" == inner ]]; then
	note "INCOMPLETE: merge is required even when the deferred set is empty"
	exit 6
fi
note "merge completed at the pinned revision"
