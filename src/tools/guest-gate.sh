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
guest_gate_guest=""

guest_gate_fail() {
	echo "${GUEST_GATE_NAME:-guest-gate}: $*" >&2
	exit 1
}

guest_gate_cleanup() {
	[[ -n "$guest_gate_driver" ]] && kill "$guest_gate_driver" 2>/dev/null
	[[ -n "$guest_gate_guest" ]] && kill "$guest_gate_guest" 2>/dev/null
	[[ -n "$guest_gate_work" ]] && rm -rf "$guest_gate_work"
	guest_gate_restore_shipping
	return 0
}
trap guest_gate_cleanup EXIT

# A GATE THAT NEEDS THE DEVELOPMENT IMAGE builds it, and puts the shared staging back the way it found it. image.sh
# rebuilds `.build/image` and the system volume in the profile it runs under, so every later step reading them - a
# kernel suite above all - would find development fixtures in a shipping volume: on 2026-10-13 the x86_64 suite failed
# "system volume contains an undeclared entry drivers/acpi_fixture.lsexe" right after such a gate. A run that began in
# the shipping profile rebuilds the gate's architecture's staging on the way out.
guest_gate_shipping_to_restore=""
guest_gate_development() {
	if [[ "${LIBER_DEVELOPMENT:-}" != 1 ]]; then
		guest_gate_shipping_to_restore=1
	fi
	export LIBER_DEVELOPMENT=1
}
guest_gate_restore_shipping() {
	[[ -n "$guest_gate_shipping_to_restore" ]] || return 0
	guest_gate_shipping_to_restore=""
	echo "${GUEST_GATE_NAME:-guest-gate}: rebuilding the shipping $GUEST_ARCH staging this gate's development image replaced" >&2
	env -u LIBER_DEVELOPMENT "$root/../build.sh" --arch "$GUEST_ARCH" >/dev/null 2>&1 || echo "${GUEST_GATE_NAME:-guest-gate}: the shipping staging did not rebuild - run ./build.sh --arch $GUEST_ARCH" >&2
}

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

# The programs a gate needs, BUILT: a dynamic one where the image stages it, a static one - a fixture driver,
# a scenario probe, a service that links what no staged library publishes - where cargo built it, which is
# where the volume packager copies it from. The staged image directory holds only the dynamic half.
guest_gate_require_programs() {
	local image="$root/../.build/image/$(guest_gate_triple)" built="$root/../.build/cargo/user/$(guest_gate_triple)/debug" name
	# A PORT BOOTS THE SHARED STAGING, which a run in the shipping profile built without the development probes: the
	# ports' camera and firmware-tree gates answered "no artifact" under `verify.sh --release`. So the development
	# staging is built here, as an x86_64 gate builds its own medium.
	if [[ "$GUEST_ARCH" != x86_64 && "${LIBER_DEVELOPMENT:-}" != 1 ]]; then
		guest_gate_development
		echo "${GUEST_GATE_NAME:-guest-gate}: building the development $GUEST_ARCH staging this gate's probes are in" >&2
		"$root/../build.sh" --arch "$GUEST_ARCH" >"$guest_gate_work/build.log" 2>&1 || {
			tail -n 40 "$guest_gate_work/build.log" >&2
			guest_gate_fail "the development $GUEST_ARCH staging did not build"
		}
	fi
	for name in "$@"; do
		[[ -f "$image/libexec/$name" || -f "$image/bin/$name" || -f "$built/$name" ]] || guest_gate_fail "$name is not built - build the development image:  LIBER_DEVELOPMENT=1 ./build.sh --arch $GUEST_ARCH"
	done
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

# One current ISO per owned x86 gate. A caller's explicit image is authoritative;
# otherwise image.sh builds the required system and writes a private medium. The
# public run.sh deliberately builds nothing and must receive its --image flag.
# Harness signing follows the lab's development image; the existing gate carrier
# still selects the actual enforced/no-IOMMU machine. Build/profile flags are inherited.
guest_gate_image=""
guest_gate_image_args=()
guest_gate_prepare_image() {
	guest_gate_image_args=()
	[[ "$GUEST_ARCH" == x86_64 ]] || return 0
	local image="${BOOT_IMAGE:-}"
	if [[ -z "$image" ]]; then
		if [[ -z "$guest_gate_image" ]]; then
			# THE MEDIUM THESE GATES BOOT IS THE DEVELOPMENT ONE: their probes and fixtures are development-only,
			# and an image built in the shipping profile carries none of them - every probe answered "no artifact"
			# unless whoever ran the gate had exported LIBER_DEVELOPMENT=1 first, which `./check.sh --gate` and
			# `verify.sh` never do. The shared staging is rebuilt in the caller's profile on the way out.
			guest_gate_development
			guest_gate_image="$guest_gate_work/guest.iso"
			LIBER_IMAGE_OUTPUT="$guest_gate_image" "$root/../image.sh" --format iso --dma-mode harness >"$guest_gate_work/image.log" 2>&1 || {
				cat "$guest_gate_work/image.log" >&2
				guest_gate_fail "the current x86 gate image did not build"
			}
		fi
		image="$guest_gate_image"
	fi
	[[ -f "$image" ]] || guest_gate_fail "the selected x86 gate image does not exist: $image"
	guest_gate_image_args=(--image "$image")
	# A caller's persistent disk survives its later cold boots unchanged. Seed only a
	# new one, from THIS ISO rather than a global volume another image build can replace.
	if [[ -n "${RUN_DISK:-}" && ! -e "$RUN_DISK" ]]; then
		local paired
		paired="$(mktemp -d "$guest_gate_work/paired.XXXXXX")"
		if ! xorriso -osirrox on -indev "$image" -extract /boot/efiboot.img "$paired/esp.img" >"$paired/extract.log" 2>&1 ||
			! mcopy -i "$paired/esp.img" ::/system-volume.img "$paired/system-volume.img" >>"$paired/extract.log" 2>&1 ||
			! cp --reflink=auto "$paired/system-volume.img" "$RUN_DISK"; then
			cat "$paired/extract.log" >&2
			guest_gate_fail "the selected image could not seed its paired persistent disk"
		fi
		rm -rf "$paired"
	fi
}

# Boot the guest, type `command`, and reduce the log to the lines `prefix:` begins.
guest_gate_run() {
	local command="$1" prefix="$2"
	# Preparation precedes the console/guest deadline, so builds do not consume it.
	guest_gate_prepare_image
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
	SERIAL="unix:$socket,server=on,wait=off" timeout "$deadline" ./run.sh --arch "$GUEST_ARCH" --smp 2 "${guest_gate_image_args[@]}" >"$guest_gate_work/run" 2>&1 &
	guest_gate_guest=$!
	wait "$guest_gate_driver" 2>/dev/null || true
	guest_gate_driver=""
	# THE GUEST STOPS WHEN ITS CONSOLE DRIVER DOES. Nothing records the console after the driver has
	# exited, so nothing the console log holds can change after it: the guest ran on to the deadline
	# for nothing - 120 to 300 s past every recording of the 23 gates that type no `poweroff`. `timeout`
	# signals its whole process group, so the runner and its QEMU end together. QEMU's own output
	# (`run`) is only ever copied for diagnosis. A gate whose verdict also reads a simulator's log on
	# the host, which keeps growing until QEMU stops, keeps its window with `GUEST_GATE_WINDOW=keep`.
	if [[ "${GUEST_GATE_WINDOW:-}" != keep ]]; then
		kill -TERM "$guest_gate_guest" 2>/dev/null || true
	fi
	wait "$guest_gate_guest" 2>/dev/null || true
	guest_gate_guest=""
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
