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

help() {
	usage_and_exit <<EOF
usage: verify.sh [--for-change | --for PATH[,PATH...] | --for-range A..B | --release | --sweep]
                 [--plan] [--explain] [--json] [--shadow] [--catalog] [--model-hash] [--age] [--trust]

Works out what a change needs verified and runs exactly that. With no arguments: --for-change.

  --for-change     everything the working tree says was changed (the default)
  --for PATH       plan for these paths instead of asking git
  --for-range A..B plan for the paths a commit range touched
  --release        the release gate: build all, check all, boot all three, no optimisation applied
  --sweep          the whole suite on every target at one immutable revision, in a git worktree
  --shadow         run the FULL suite and compare it against what this change would have scoped
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
	--shadow)
		action=shadow
		shift
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
	./check.sh
	./test.sh --arch all
}

# Refuse to guess. A planner that cannot answer leaves two honest options - run everything, or stop
# and say so - and the caller picks by whether the machine has the hours.
planner_failed() {
	local reason="$1"
	echo >&2
	echo "verify.sh: FULL VERIFICATION REQUIRED" >&2
	echo "verify.sh: the planner could not answer ($reason), and a plan that is missing is not a plan that is empty." >&2
	echo "verify.sh: run the whole thing yourself:  ./build.sh --arch all && ./check.sh && ./test.sh --arch all" >&2
	exit 3
}

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
	(cd "$worktree" && ./build.sh --arch all && ./check.sh && ./test.sh --arch all) || status=$?
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
	note "shadow: full sweep on ${targets//$'\n'/ }, compared against a selection that is not run"
	note "        pinned to source $source_before"
	for target in $targets; do
		./test.sh --arch "$target" || note "the $target sweep did not pass - the comparison below reads its log anyway, which is the point"
		log="$(find "$BUILD_DIR/logs/test" -name "$target-*-guest.log" -printf '%T@ %p\n' | sort -rn | head -1 | cut -d' ' -f2)"
		[[ -n "$log" ]] || die "no $target guest log to compare against"
		printf '%s\n' "$changed" | (cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- shadow --stdin --guest-log "../$log" --arch "$target") || shadow_failed=1
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

failed=()
step=0
# The plan is read on fd 3, not on stdin.
#
# On stdin the first step that reads anything - and `./test.sh` boots QEMU, which does - swallows
# the rest of the plan, and the run ends early reporting success over the steps it never took. That
# is a false green produced by the runner itself, which is the one place this milestone cannot
# afford one.
while IFS=$'\t' read -r -u 3 marker index keys label command note_text; do
	[[ "$marker" == STEP ]] || continue
	step=$((step + 1))
	echo
	note "[$step/$count] $label - $keys key(s)"
	[[ -n "$note_text" ]] && note "        $note_text"
	note "        $command"
	started=$SECONDS
	outcome=--passed
	if ! eval "$command"; then
		# Every step is run and every failure is reported, rather than stopping at the first.
		# A run that stops early tells you one thing is broken; a run that finishes tells you
		# how much is.
		note "FAILED: $label"
		failed+=("$label")
		outcome=--failed
	fi
	# Record the outcome against every PlanItemKey this one command discharged.
	#
	# Per key rather than per step, because that is what the age bound, the cost model and the
	# shadow record all range over - a step that only knew how many keys it covered could update
	# none of them. Recording is best-effort: losing a history entry costs a wider next sweep, and
	# refusing to report a result the run actually produced would cost more.
	keys_file="$(mktemp)"
	awk -v want="$index" -F'\t' '$1 == "KEY" && $2 == want { print $3 }' "$steps_file" >"$keys_file"
	if [[ -s "$keys_file" ]]; then
		(cd "$SRC_DIR" && cargo run --quiet --manifest-path tools/verify-model/Cargo.toml -- record --keys-file "$keys_file" "$outcome" --seconds "$((SECONDS - started))") || note "        (the run happened; recording it did not)"
	fi
	rm -f "$keys_file"
done 3<"$steps_file"

# The runner's own arithmetic, checked. A loop that silently took fewer steps than the plan listed
# would report a pass over work it never did.
if ((step != count)); then
	die "the plan listed $count step(s) and the runner took $step - refusing to report a result over a plan it did not finish"
fi

echo
if ((${#failed[@]} > 0)); then
	die "${#failed[@]} of $count step(s) failed: ${failed[*]}"
fi
note "all $count step(s) passed"

# What that green is WORTH, said before anything else.
#
# The design's rule is that a scoped answer is not believed because it is plausible: until a
# component has shadow evidence under the current model, a scoped run must not be the only thing
# standing behind a green. That rule existed in prose while the runner executed its plan regardless
# of any component's level, which is the gap between "we have a trust model" and "we use one".
#
# It reports rather than refuses, because refusing would make the tool unusable - nothing is TRUSTED
# yet and nothing can be until shadow comparisons accumulate. Set VERIFY_REQUIRE_TRUST=1 to make an
# unproven scoped run a failure instead, which is the strict contract for anyone who wants it.
level_line="$(printf '%s\n' "$changed" | cargo run --quiet --manifest-path "$PLANNER_MANIFEST" -- level --stdin 2>/dev/null || true)"
level="${level_line%%$'\t'*}"
case "$level" in
FULL)
	note "verification level: FULL - everything ran, this stands on its own"
	;;
TRUSTED)
	note "verification level: TRUSTED - every changed component has shadow evidence under this model"
	;;
SHADOW)
	note "verification level: SHADOW - ${level_line#*$'\t'} has no shadow evidence under this model yet."
	note "     A scoped green is good evidence and NOT equivalent to a full verification."
	note "     ./verify.sh --shadow  proves the selection for this change; ./verify.sh --sweep  sidesteps it."
	if [[ "${VERIFY_REQUIRE_TRUST:-0}" == "1" ]]; then
		die "VERIFY_REQUIRE_TRUST is set and this change touches components with no shadow evidence"
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
