# Shared plumbing for the gates that boot a guest and read what a probe printed.
#
# WHY IT IS SHARED. Four gates do the same five things - pick an architecture, check the artifact is
# staged, drive the console, wait for the boot, and reduce the log to the probe's own lines - and
# four copies of that is four places for the console handling to drift. What stays in each gate is
# what it ASSERTS, which is the only part that differs.
#
# THE ARCHITECTURE IS A PARAMETER because the completion gate says "on all three architectures". The
# two ports boot under emulation and are slow; that is a reason to run them deliberately, not a
# reason for a gate to be unable to.

GUEST_ARCH="x86_64"
GUEST_LINES=""
GUEST_LOG=""
guest_gate_work=""
guest_gate_driver=""

guest_gate_fail() {
	echo "${GUEST_GATE_NAME:-guest-gate}: $*" >&2
	exit 1
}

guest_gate_cleanup() {
	[[ -n "$guest_gate_driver" ]] && kill "$guest_gate_driver" 2>/dev/null
	[[ -n "$guest_gate_work" ]] && rm -rf "$guest_gate_work"
	return 0
}
trap guest_gate_cleanup EXIT

# `--arch ARCH`, defaulting to x86_64.
guest_gate_arch() {
	while [[ $# -gt 0 ]]; do
		case "$1" in
		--arch)
			[[ $# -ge 2 ]] || guest_gate_fail "--arch takes an architecture"
			GUEST_ARCH="$2"
			shift 2
			;;
		*) guest_gate_fail "unexpected argument '$1'" ;;
		esac
	done
	case "$GUEST_ARCH" in
	x86_64 | aarch64 | riscv64) ;;
	*) guest_gate_fail "unsupported architecture '$GUEST_ARCH'" ;;
	esac
	guest_gate_work="$(mktemp -d)"
}

guest_gate_triple() {
	case "$GUEST_ARCH" in
	x86_64) printf 'x86_64-unknown-none' ;;
	aarch64) printf 'aarch64-unknown-none' ;;
	riscv64) printf 'riscv64gc-unknown-none-elf' ;;
	esac
}

# A quarantine artifact is absent whenever its upstream is, and that is reported rather than failed.
guest_gate_require_quarantine() {
	local name="$1"
	local built="$root/../.build/foreign/pass2/$GUEST_ARCH/$name.lsexe"
	local staged="$root/../.build/image/$(guest_gate_triple)/libexec/$name"
	if [[ ! -f "$built" ]]; then
		echo "${GUEST_GATE_NAME:-guest-gate}: NOT PERFORMED: there is no $name at $built, so it was not run"
		exit 0
	fi
	[[ -f "$staged" ]] || guest_gate_fail "$name is not staged for $GUEST_ARCH - build the development image:  LIBER_DEVELOPMENT=1 ./build.sh --arch $GUEST_ARCH"
}

# Boot the guest, type `command`, and reduce the log to the lines `prefix:` begins.
guest_gate_run() {
	local command="$1" prefix="$2"
	local script="$guest_gate_work/script"
	# ONE LINE PER COMMAND. A caller with three of them passes them as three lines, which is what the
	# console driver types and what a gate observing a SEQUENCE needs.
	printf '%s\n' "$command" >"$script"
	GUEST_LOG="$guest_gate_work/guest"
	local socket="$guest_gate_work/console"
	rm -f "$socket"
	# THE PORTS ARE EMULATED AND THE BUDGETS SAY SO. x86_64 runs under KVM; aarch64 and riscv64 run
	# under TCG, where the same boot takes many times longer - measured at about twenty-four times on
	# this tree's own image gate. One budget for all three would either fail the ports or leave
	# x86_64 waiting for a guest that died minutes ago.
	local console="${GUEST_GATE_SECONDS:-70}" deadline="${GUEST_GATE_TIMEOUT:-200}"
	if [[ "$GUEST_ARCH" != x86_64 ]]; then
		console="${GUEST_GATE_SECONDS:-600}"
		deadline="${GUEST_GATE_TIMEOUT:-900}"
	fi
	python3 src/harness/guest-console.py --socket "$socket" --log "$GUEST_LOG" --script "$script" --seconds "$console" >"$guest_gate_work/driver" 2>&1 &
	guest_gate_driver=$!
	SERIAL="unix:$socket,server=on,wait=off" timeout "$deadline" ./run.sh --arch "$GUEST_ARCH" --smp 2 >"$guest_gate_work/run" 2>&1 || true
	wait "$guest_gate_driver" 2>/dev/null || true
	guest_gate_driver=""
	[[ -s "$GUEST_LOG" ]] || guest_gate_fail "the guest produced no console output"
	# AN EMPTY PREFIX KEEPS THE WHOLE LOG. A probe whose lines do not all begin with one word - the
	# ICD one prints `icd lowest ...` and `icd lookup ...` - is read from the log itself, and the
	# filtering exists only for the probes that have a prefix worth reducing to.
	GUEST_LINES="$guest_gate_work/lines"
	if [[ -z "$prefix" ]]; then
		cp "$GUEST_LOG" "$GUEST_LINES"
		return 0
	fi
	grep -a -o "$prefix: [a-zA-Z0-9 =_.,-]*" "$GUEST_LOG" >"$GUEST_LINES" || guest_gate_fail "$prefix printed nothing at all"
}
