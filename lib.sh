#!/usr/bin/env bash
# Shared by every entry-point script in this directory.
#
# The scripts are the build INTERFACE; the work still lives where it lived - `src/harness/mkimage.sh`,
# `src/harness/qemu-run.sh`, `src/harness/test-kernel.sh`, `src/harness/lab.py` and cargo. What these add is
# flags instead of names: the Justfile spelled every combination of architecture, mode and target
# into its own recipe and reached 123 of them, which is a discovery surface nobody reads.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$REPO_ROOT/src"
BUILD_DIR="$REPO_ROOT/.build"

ARCHS_ALL=(x86_64 aarch64 riscv64)

# ENDING ONE OF THESE SCRIPTS ENDS THE GUESTS IT STARTED.
#
# `timeout 300 ./run.sh --arch aarch64` ends the SCRIPT and not the `qemu-system-aarch64` underneath
# it, so an interrupted run leaves a guest holding a write lock on the disk images under
# `.build/boot`. The next run then fails with a QEMU lock error naming an IMAGE rather than the run
# that holds it - a diagnosis `test.sh` already warns is unreadable when it happens for other reasons.
#
# `timeout` HANDLES ITS OWN EXPIRY CORRECTLY - measured, not assumed: it puts its child in a new
# process group and signals the GROUP, so a grandchild dies with it. The hole is only the other
# ending, where something kills the script and nothing ever signals that group.
#
# TWO HALVES, AND THE TRAP ALONE IS NOT ENOUGH. Three designs were tried before this one and the
# first two were refuted by measurement; P02M0156 records them. What governs the problem is that
# BASH DEFERS A TRAP WHILE A FOREGROUND CHILD IS RUNNING: a `kill` of `./check.sh` sat queued behind
# `eval "$cmd"` and the trap would not have run until the gate finished on its own, which is the
# moment there is nothing left to clean up. So the callers below run their long child in the
# BACKGROUND and `wait` for it, because `wait` is interruptible and a foreground command is not.
guest_cleanup() {
	# Deepest first, so a parent is never signalled before its children. The walk is the one
	# `test-kernel.sh` uses to decide a guest is ITS OWN rather than any QEMU on the machine:
	# ancestry is what makes the victim ours, and a process name is not - the name matches every
	# QEMU on a shared machine, including other people's.
	# `$BASHPID`, NOT `$$` - AND CAPTURED BEFORE THE SUBSTITUTION. `$$` keeps the original shell's pid
	# inside a subshell, so it names the wrong tree there; `$BASHPID` names the right one, but written
	# as `$(_guest_descendants "$BASHPID")` it is expanded INSIDE the command substitution, which is
	# itself a subshell with no descendants - so the sweep silently found nothing. Both were live
	# defects here within an hour, in opposite directions: the first killed the gate that was running
	# it, the second quietly stopped cleaning up at all.
	local me="$BASHPID" victim
	for victim in $(_guest_descendants "$me"); do
		kill -TERM "$victim" 2>/dev/null || true
	done
}

_guest_descendants() {
	local parent="$1" child
	for child in $(ps -o pid= --ppid "$parent" 2>/dev/null); do
		_guest_descendants "$child"
		printf '%s\n' "$child"
	done
}

# INSTALLED BY THE ENTRY POINTS, NOT HERE. This file is sourced by nested subshells that want one
# helper out of it - `check-implementation-mutations.sh` sources it inside `( cd "$WORK"; ... )` just
# to reuse `source_digest` - and installing a trap there gives a short-lived subshell the authority
# to sweep a tree it does not own. That killed the mutation gate with SIGTERM before it had copied
# the tree. A script that wants this calls `install_guest_cleanup` after sourcing.
install_guest_cleanup() {
	# REGISTERED WITH THE VERDICT DISPATCHER WHEN THERE IS ONE, and installed as its own trap when
	# there is not. The five entry points arm the verdict first, so this REGISTERS there - which is
	# what stopped `check.sh` from replacing the verdict with its guest cleanup, and what makes the
	# guest cleanup run even on a refused flag. `run.sh` and anything else outside those five still
	# gets the plain trap it always had.
	if [[ "${__run_owner:-}" == "$BASHPID" ]]; then
		register_run_cleanup guest_cleanup
		return 0
	fi
	trap guest_cleanup EXIT
	trap 'guest_cleanup; exit 130' INT
	trap 'guest_cleanup; exit 143' TERM
}

# CARGO ON THE PATH, FOUND RATHER THAN REQUIRED.
#
# `setup.sh` cannot put it there: `rustup` adds `~/.cargo/bin` through the shell PROFILE, and the
# shell that runs `setup.sh` read its profile before rustup existed. So the documented first
# session - `./setup.sh`, then `./build.sh` - died on `cargo: command not found`, and the fix was a
# `source ~/.cargo/env` nothing told the reader about. A script that needs cargo finds it.
#
# Exported, because the work is not done here: `src/harness/mkimage.sh`, `src/harness/qemu-run.sh` and
# `src/harness/lab.py` all call cargo as child processes of these scripts.
if ! command -v cargo >/dev/null 2>&1; then
	_cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
	[[ -x "$_cargo_bin/cargo" ]] && export PATH="$_cargo_bin:$PATH"
	unset _cargo_bin
fi

die() {
	echo "${SCRIPT_NAME:-$(basename "$0")}: $*" >&2
	exit 1
}

note() {
	echo "${SCRIPT_NAME:-$(basename "$0")}: $*" >&2
}

# ONE VERDICT, ONE SHAPE, FROM ONE OWNER - so a run that ENDS always says so.
#
# WHAT THIS FIXES, MEASURED. Every entry point announced success in its own words (`built: ...`,
# `all selected checks passed`, `PASS:`) and announced FAILURE in whatever words the tool underneath
# it happened to use - `error[E0603]`, `build-shared: ... status=1`, `... did not stop after emitting
# its ET_REL seed object`, `warning: build failed`. So a finished-and-failed run and a still-running
# one looked THE SAME to anything reading the log, and on 2026-09-13 a build that had failed in nine
# seconds was waited on for thirty minutes because the failure did not match the words being watched
# for. That is the whole defect: the absence of a verdict, not the variety of the messages.
#
# IT IS A TRAP AND NOT A LINE AT THE BOTTOM, because the runs that are hardest to notice are exactly
# the ones that never reach the bottom: `set -e` aborting mid-script, a signal, a killed parent
# taking the process group with it. A trap fires on all of those.
#
# AND IT IS ONE DISPATCHER RATHER THAN A CHAIN OF TRAP TEXT. The first version read the existing EXIT
# trap back out of `trap -p`, pasted it into a new one and re-armed; every entry point that installed
# a cleanup AFTER arming silently replaced the verdict, and every one that armed after a cleanup
# carried that cleanup's text into a string nobody could read. There is one trap now, installed once
# by the invocation that owns it, and cleanups REGISTER with it.
#
# WHAT AN OWNER IS. The shell whose `BASHPID` armed it. A subshell, a command substitution or a
# background observer inherits the shell's functions and variables and MUST NOT finalize the run it
# is part of - so every entry point below checks the owner before doing anything, and a separately
# executed script arms its own independent invocation.
#
# AND IT WRITES A FILE, because prose is not a protocol. The record exists when the run has ENDED and
# does not exist while it runs, so "has it finished" is a file test rather than a search for words.
# Set `RUN_STATUS_FILE` to a FRESH, UNUSED path before the run to choose where it lands; without it
# only the line is printed, which is what an interactive run wants. The rules that make a reader able
# to trust it are in `docs/TESTING.md` and in `__run_claim_status_file` below.

# The private state of one invocation. Never exported: a child gets its own.
__run_owner=""
__run_script=""
__run_started=0
__run_status_path=""
__run_status_usable=0
__run_finalized=0
__run_cleanups=()
__run_children=()

# Arm the verdict for this invocation. Called once, near the top of an entry point, AFTER
# `SCRIPT_NAME` is set and BEFORE argument handling - so a refused flag ends with a verdict too.
#
# IDEMPOTENT WITHIN ONE OWNER: a script that calls it twice keeps its first start time and its first
# status destination, because an invocation has one elapsed time and one terminal record.
arm_run_verdict() {
	if [[ "${__run_owner:-}" == "$BASHPID" ]]; then
		return 0
	fi
	__run_owner="$BASHPID"
	__run_script="${SCRIPT_NAME:-$(basename "$0")}"
	# THE START TIME IS THIS INVOCATION'S AND IS NOT INHERITED. It used to come from a `RUN_STARTED_AT`
	# the environment could supply, so a child reported its parent's elapsed time.
	__run_started="$(date +%s)"
	__run_finalized=0
	__run_cleanups=()
	__run_children=()
	__run_claim_status_file
	trap '__run_finalize "$?"' EXIT
	# A KILLED RUN REPORTS A FAILURE, WHICH IS THE WHOLE POINT OF THE EXERCISE. The EXIT trap alone is
	# not enough: a script killed by a signal runs it with `$?` still zero, so the verdict said
	# `ok exit=0` over a run that was shot down - which is WORSE than no verdict.
	trap '__run_interrupt 1' HUP
	trap '__run_interrupt 2' INT
	trap '__run_interrupt 3' QUIT
	trap '__run_interrupt 15' TERM
}

# Register a cleanup function to run once, in registration order, before the verdict.
#
# REGISTERED IMMEDIATELY AFTER THE RESOURCE IS ACQUIRED, which is the rule that makes a failure
# BETWEEN acquisition and the rest of setup clean up anyway. A callback must tolerate partially
# initialized state, must not call `exit`, and must not install another EXIT trap.
register_run_cleanup() {
	[[ "${__run_owner:-}" == "$BASHPID" ]] || return 0
	__run_cleanups+=("$1")
}

# Record a child this invocation started, so an interruption stops and reaps what it owns - and only
# what it owns. Never by executable name: this tree has killed its own gate that way.
register_run_child() {
	[[ "${__run_owner:-}" == "$BASHPID" ]] || return 0
	__run_children+=("$1")
}

forget_run_child() {
	local keep=() pid
	for pid in ${__run_children[@]+"${__run_children[@]}"}; do
		[[ "$pid" == "$1" ]] || keep+=("$pid")
	done
	__run_children=(${keep[@]+"${keep[@]}"})
}

# RUN A LEAF COMMAND SO A SIGNAL REACHES THIS SHELL WHILE IT IS RUNNING.
#
# Bash defers a trap until the FOREGROUND child returns, so a run interrupted during a twenty-minute
# compile answered its signal twenty minutes later - which is the same missing verdict by another
# route. Backgrounding the leaf and waiting interruptibly is what makes the trap prompt; the child's
# real status (and `pipefail` inside a wrapped pipeline) is preserved and returned.
run_owned() {
	local status=0 pid
	"$@" &
	pid=$!
	register_run_child "$pid"
	wait "$pid" || status=$?
	forget_run_child "$pid"
	return "$status"
}

# The same for a leaf that is a SHELL FRAGMENT rather than a command: a subshell that changes
# directory first, or a pipeline whose status matters.
#
# `eval` INTO A BACKGROUND SUBSHELL keeps the caller's functions, variables and `pipefail`, which
# `bash -c` would not - and the whole point is that the fragment behaves exactly as it did when it
# ran in the foreground, except that a signal now reaches the shell waiting on it.
run_owned_shell() {
	local status=0 pid
	# THE SUBSHELL EXITS EXPLICITLY WITH THE FRAGMENT'S STATUS, and that is not a formality: with
	# `errexit` on, a fragment that fails inside `eval` makes the shell exit 1 rather than with the
	# command's own status - so a compiler that refused with 37 was reported as a plain failure and a
	# subordinate tool's exit code never reached the verdict. Measured here, not inferred.
	(
		set +e
		eval "$1"
		exit $?
	) &
	pid=$!
	register_run_child "$pid"
	wait "$pid" || status=$?
	forget_run_child "$pid"
	return "$status"
}

# Take the requested terminal destination ONCE, resolve it, and decide whether it is usable.
#
# THE REQUEST IS UNEXPORTED IMMEDIATELY, which is what stops a descendant from publishing into its
# parent's record: a child keeps its own verdict LINE and has no destination unless its caller gives
# it a different fresh one.
__run_claim_status_file() {
	__run_status_path=""
	__run_status_usable=0
	local request="${RUN_STATUS_FILE:-}"
	unset RUN_STATUS_FILE
	[[ -n "$request" ]] || return 0
	# RESOLVED AGAINST THE INVOCATION'S ORIGINAL DIRECTORY, because a run that changes into a release
	# snapshot later must not publish somewhere else.
	local path="$request"
	[[ "$path" == /* ]] || path="$PWD/$path"
	local dir="${path%/*}"
	[[ "$dir" == "$path" ]] && dir="."
	if ! mkdir -p "$dir" 2>/dev/null; then
		echo "$__run_script: no terminal record: the directory for $path cannot be created" >&2
		return 0
	fi
	# A DESTINATION THAT ALREADY EXISTS IS NOT THIS RUN'S. Deleting or truncating it would destroy
	# somebody else's record and would make a stale success look like this run's answer, so the
	# request is refused and the work continues without one.
	if [[ -e "$path" || -L "$path" ]]; then
		echo "$__run_script: no terminal record: $path already exists; give each run a fresh path" >&2
		return 0
	fi
	__run_status_path="$path"
	__run_status_usable=1
}

# The one EXIT dispatcher: save the status, run the registered cleanups once, then publish.
__run_finalize() {
	local status="$1" fn
	[[ "${__run_owner:-}" == "$BASHPID" ]] || return 0
	[[ "${__run_finalized:-0}" == 0 ]] || return 0
	__run_finalized=1
	# No second delivery: a cleanup that exits, or a signal arriving during cleanup, must not start
	# this again or replace the status that is already saved.
	trap - EXIT HUP INT QUIT TERM
	for fn in ${__run_cleanups[@]+"${__run_cleanups[@]}"}; do
		"$fn" || echo "$__run_script: cleanup '$fn' failed; the run's own result is unchanged" >&2
	done
	__run_publish "$status"
	return 0
}

# Stop what this invocation started, then end with the shell's own 128+n convention - which is the
# status the EXIT dispatcher then reports.
__run_interrupt() {
	local signal="$1" pid
	if [[ "${__run_owner:-}" != "$BASHPID" ]]; then
		exit $((128 + signal))
	fi
	for pid in ${__run_children[@]+"${__run_children[@]}"}; do
		kill -TERM "$pid" 2>/dev/null || true
	done
	for pid in ${__run_children[@]+"${__run_children[@]}"}; do
		wait "$pid" 2>/dev/null || true
	done
	__run_children=()
	exit $((128 + signal))
}

# The line and the file, from the same four values, written once.
__run_publish() {
	local status="$1" outcome=ok now seconds tmp dir
	[[ "$status" == 0 ]] || outcome=failed
	now="$(date +%s)"
	seconds=$((now - __run_started))
	((seconds < 0)) && seconds=0
	if ((__run_status_usable)); then
		dir="${__run_status_path%/*}"
		[[ "$dir" == "$__run_status_path" ]] && dir="."
		tmp="$dir/.run-verdict.$$.$RANDOM.tmp"
		# WRITTEN BESIDE THE DESTINATION AND RENAMED, so a reader never sees half a record. A rename
		# within one directory is atomic; a writer that filled the terminal name in place would let a
		# reader see an outcome without its exit code.
		if printf 'script=%s\noutcome=%s\nexit=%s\nseconds=%s\n' "$__run_script" "$outcome" "$status" "$seconds" >"$tmp" 2>/dev/null &&
			mv -f "$tmp" "$__run_status_path" 2>/dev/null; then
			:
		else
			rm -f "$tmp" 2>/dev/null || true
			echo "$__run_script: the terminal record could not be published to $__run_status_path" >&2
		fi
	fi
	echo "$__run_script: RESULT $outcome exit=$status seconds=$seconds" >&2
}

# Expand `all` and validate. Accepts repeated flags and comma-separated lists, so
# `--arch aarch64 --arch riscv64` and `--arch aarch64,riscv64` mean the same thing.
parse_list() {
	local raw="$1" what="$2" valid="$3" out=() item
	IFS=', ' read -r -a items <<<"$raw"
	for item in "${items[@]}"; do
		[[ -z "$item" ]] && continue
		if [[ "$item" == all ]]; then
			# shellcheck disable=SC2206
			out=($valid)
			break
		fi
		[[ " $valid " == *" $item "* ]] || die "unknown $what '$item' (valid: $valid, or 'all')"
		out+=("$item")
	done
	printf '%s\n' "${out[@]}"
}

# The rust target triple for an architecture. One place, because it was written out per recipe and
# a hard-coded x86_64 triple was still being passed for other architectures as recently as today.
target_triple() {
	case "$1" in
	x86_64) echo x86_64-unknown-none ;;
	aarch64) echo aarch64-unknown-none ;;
	riscv64) echo riscv64gc-unknown-none-elf ;;
	*) die "no target triple for '$1'" ;;
	esac
}

# The LOADER's triple, which is NOT the kernel's: it is a UEFI application on x86_64 and aarch64,
# and a hand-rolled ELF on riscv64. Checking for it under the kernel's triple is why a build that
# had just produced it was reported as missing.
loader_triple() {
	case "$1" in
	x86_64) echo x86_64-unknown-uefi ;;
	aarch64) echo aarch64-unknown-uefi ;;
	riscv64) echo riscv64gc-unknown-none-elf ;;
	*) die "no loader triple for '$1'" ;;
	esac
}

# Run a build step at most once per invocation.
#
# This replaces the Justfile's dependency graph, and it is the part of the move that has to be
# deliberate rather than incidental: `test-riscv64: test-preflight-riscv64 loader-riscv64` was a
# statement that both run first and exactly once. Getting it wrong is not theoretical - a disk
# image shipped carrying the TEST kernel because a volume was assembled before the file it read
# was written.
declare -A _ensure_done=()
ensure() {
	local key="$*"
	[[ -n "${_ensure_done[$key]:-}" ]] && return 0
	_ensure_done[$key]=1
	"$@"
}

# The sources a built system volume reflects. Shared by build.sh, which records their digest, and
# test.sh, which refuses to run a suite against a build that does not match it. Defined HERE
# because both need it: keeping it in build.sh left test.sh computing a digest of nothing, which
# matched no stamp and refused every run.
# Which tool belongs to which wave, and what a wave is tested with.
#
# The waves came from the dynamic-linking migration and are the structure `check-dynamic-report.sh`
# organises docs/DYNAMIC_EXECUTABLES.tsv around: which tools were converted together, and what each
# group's shared-object footprint is.
#
# They are NOT a scoping mechanism, and used as one they were the measured problem:
# wave 5 selects `image,audio,service,process,storage`, and because `service` and `process` are on a
# third of the suite each, that is 109 of 205 tests for a one-tool change. `./verify.sh` answers that
# question from the dependency graph instead.
declare -A TOOL_WAVES=()
for tool in echo uname uptime dmesg free lscpu lsmem lsirq lspci ptyecho readln script; do TOOL_WAVES[$tool]=1; done
for tool in cat write rm ls du mkdir rmdir snap volume lsvol lsblk; do TOOL_WAVES[$tool]=2; done
for tool in date log config set lsdev lsfont lsusb lssvc usage ps run perm start stop beep; do TOOL_WAVES[$tool]=3; done
for tool in ping ip nslookup tcp nc arp httpd ss traceroute; do TOOL_WAVES[$tool]=4; done
for tool in imgview imgconv audioconv audiorec play frame_probe graphics_probe lico licoedit licoview textconf; do TOOL_WAVES[$tool]=5; done
# Wave 6: the text-processing command family. They are their own wave because they share a shape - the
# bounded window read, the shared parsers, the volume bundle - so a regression in that shape shows
# as a wave rather than as one tool, and because measuring them beside the image and audio tools
# would mix a kilobyte of argument parsing with a megabyte of codec.
for tool in pwd clear which wc head tail hexdump touch truncate tree find grep cp mv sort cut kill tee watch less; do TOOL_WAVES[$tool]=6; done
unset tool

declare -A WAVE_TAGS=()
WAVE_TAGS[1]='service,process,storage'
WAVE_TAGS[2]='service,process,storage'
WAVE_TAGS[3]='service,process,storage'
WAVE_TAGS[4]='service,process'
WAVE_TAGS[5]='image,audio,service,process,storage'
WAVE_TAGS[6]='service,process,storage,permission-service'

# Which part of the tree a change is verified with is NOT answered here any more.
#
# There was a hand-written table below this line - a path prefix mapped to a tag list - and it was
# wrong in both directions. It said `src/fs` was tested with `filesystem,storage,volume`, missing
# that the kernel and the loader both statically link LiberFS; and picking any tag from it that
# nearly every test carries (`process` is on 35% of the suite, `service` on 30%) collapsed a
# one-tool change to half the suite. A hand table cannot stay right as the tool count multiplies,
# and nothing tells you when it has stopped being right.
#
# `./verify.sh` answers the question now, from a model that is derived rather than written: crate
# directories and `[[bin]]` entries give ownership, the three Cargo dependency kinds and
# services/manifest.toml's providers give the edges, and what remains is declared once in
# src/tools/verify-model/model/registry.toml with a reason attached.

# The directories whose content can end up in the system volume.
#
# DERIVED, and gated: `verify-model volume-sources` computes it from the manifest's staged programs
# and libraries, closed over static links, dynamic providers and build-time generation, and
# `verify-model check` fails if this list and that answer disagree. The list stays literal here
# because `source_digest` is on the hot path of every build and test invocation.
#
# It used to be `(user fs wire abi proto idl tools/mkpackages)` and it was missing four entries, two
# of which mattered a great deal. CoreServices statically links `src/term`, so editing the terminal
# stack could compile a new CoreServices, skip packaging - `build.sh` deliberately does not chain
# `user` into `packages` - and leave the staleness check content that the volume was current. The
# guest then booted the PREVIOUS userspace and passed. `src/volume` is worse in its simplicity: it is
# the factory files the volume literally ships, and changing one of them changed nothing the check
# could see.
VOLUME_SOURCES=(abi boot fs idl proto sdk term tools user volume wasm wire)

# WHAT THE LOADER IS BUILT FROM, which is not `boot/loader`.
#
# Its receipt hashed the loader directory alone, and `source_digest` hashes exactly the paths it is
# given - so a change in the crates the loader LINKS left the receipt unchanged. Those crates are
# where its verifier actually lives: `boot/signature` is the Ed25519 verification, `boot/protocol`
# the manifest format, `boot/uefi` the firmware-facing algorithms, and the filesystem crates read the
# volume the manifest describes. A cross-port freshness check comparing that receipt would have
# accepted a stale aarch64 or riscv64 binary across every one of those edits, and the whole point of
# the check is that the port's loader is the one this tree describes.
#
# The local path dependencies of `src/boot/loader/Cargo.toml`, plus the loader itself. Kept beside
# `VOLUME_SOURCES` because it is the same kind of list and drifts the same way: a `path = "..."`
# added there needs a directory added here.
LOADER_SOURCES=(boot/loader boot/protocol boot/signature boot/uefi fdt fs abi)

# A digest of every source file a build reads, so "has it changed" is answered by CONTENT.
#
# Modification times cannot answer it. `mkpackages` skips a write whose bytes are unchanged, so an
# artifact can be older than a source it already reflects; and a `git checkout`, a commit hook or a
# formatter touches a source without changing a byte of it. Both happened in one day, and both made
# the suite refuse to run against a build that was already correct.
#
# Cheap enough to do on every check: a few thousand files hashed once, against test runs measured in
# tens of minutes.
source_digest() {
	local dir
	for dir in "$@"; do
		[[ -d "$SRC_DIR/$dir" ]] || continue
		find "$SRC_DIR/$dir" -name '*.rs' -o -name '*.toml' -o -name '*.lsidl' | sort
	done | xargs -r sha256sum 2>/dev/null | sha256sum | cut -d" " -f1
}

# `--help` for every script, built from a here-doc the script supplies.
usage_and_exit() {
	cat
	exit "${1:-0}"
}
