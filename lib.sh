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

# ONE VERDICT, ONE SHAPE, FROM A TRAP - so a run that ENDS always says so.
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
# AND IT WRITES A FILE, because prose is not a protocol. `<log>.status` exists when the run has
# ENDED and does not exist while it runs, so "has it finished" is a file test rather than a search
# for words. Set `RUN_STATUS_FILE` before the run to choose where it lands; without it only the line
# is printed, which is what an interactive run wants.
RUN_STARTED_AT="${RUN_STARTED_AT:-$(date +%s)}"
run_verdict() {
	# THE STATUS IS PASSED IN WHEN THERE IS A TRAP BEFORE THIS ONE, and taken from `$?` when there is
	# not. Chaining `trap "cleanup; run_verdict" EXIT` makes `$?` the status of CLEANUP rather than of
	# the script, so a run that failed reported `ok` as soon as its cleanup succeeded - which is the
	# same lie the signal case told, arriving by a different route. `arm_run_verdict` captures the
	# script's status first and hands it over.
	local status="${1:-$?}"
	local seconds=$(($(date +%s) - RUN_STARTED_AT))
	local outcome=ok
	[[ "$status" == 0 ]] || outcome=failed
	local name="${SCRIPT_NAME:-$(basename "$0")}"
	echo "$name: RESULT $outcome exit=$status seconds=$seconds" >&2
	if [[ -n "${RUN_STATUS_FILE:-}" ]]; then
		# The directory may not exist yet on a run that failed before it built anything.
		mkdir -p "$(dirname "$RUN_STATUS_FILE")" 2>/dev/null || true
		printf 'script=%s\noutcome=%s\nexit=%s\nseconds=%s\n' "$name" "$outcome" "$status" "$seconds" >"$RUN_STATUS_FILE" 2>/dev/null || true
	fi
	return "$status"
}

# Arm the verdict for this script. Called once, near the top, AFTER `SCRIPT_NAME` is set.
#
# It chains rather than replaces: `test.sh` and `check.sh` already install a guest cleanup on EXIT,
# and an entry point that armed this by overwriting the trap would leave a guest holding a disk image
# - which is the failure `install_guest_cleanup` exists to prevent.
arm_run_verdict() {
	local existing
	existing="$(trap -p EXIT | sed -E "s/^trap -- '(.*)' EXIT$/\1/")"
	if [[ -n "$existing" && "$existing" != "trap -- '' EXIT" ]]; then
		trap "__run_status=\$?; $existing; run_verdict \"\$__run_status\"" EXIT
	else
		trap run_verdict EXIT
	fi
	# AND A KILLED RUN REPORTS A FAILURE, WHICH IS THE WHOLE POINT OF THE EXERCISE.
	#
	# The EXIT trap alone is not enough: a script killed by a signal runs it with `$?` still zero, so
	# the verdict said `ok exit=0` over a run that was shot down. That is WORSE than no verdict - a
	# reader is told the run succeeded. Each signal exits with the shell's own 128+n convention, and
	# that status is what the EXIT trap then reports.
	local signal
	for signal in INT TERM HUP QUIT; do
		trap "exit \$((128 + \$(kill -l "$signal")))" "$signal"
	done
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
for tool in imgview imgconv audioconv audiorec play graphics_probe lico licoedit licoview; do TOOL_WAVES[$tool]=5; done
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
