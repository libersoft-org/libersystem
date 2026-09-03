#!/usr/bin/env bash
# Every named interrupt and SMP profile, booted, with an oracle per claim.
#
# A KNOB IS NOT A CHECK. The runner can put a GICv2, a GICv3 without its ITS, a GICv3 with one, or a
# RISC-V AIA in the machine, and each of those was turned by hand once and its output pasted into a
# milestone document. A result that exists as a log excerpt in prose is a result nobody re-runs: the
# next change to interrupt discovery is free to break any of them and the tree stays green.
#
# WHAT EACH PROFILE MUST SHOW, and why these lines and not "it booted":
#   - the controller this profile HAS, named by the kernel from what it discovered. A boot that fell
#     back to a compiled descriptor prints a different line, and a boot that found nothing prints
#     none - both pass an "it booted" check.
#   - a timer that TICKED. Not a count: a floor. The count alone used to be printed whatever it was,
#     including zero.
#   - on the four-core profiles, every declared core online. A machine that started one of four and
#     carried on is a working boot and a failed claim.
#
# aarch64 and riscv64 are emulated on an x86_64 host, so this is minutes rather than seconds. It is
# separate from `arch-surface` for that reason: that one is a static scan and belongs in every run.
set -euo pipefail

# shellcheck source=/dev/null
source "$(dirname "$0")/result-logs.sh"
cd "$(dirname "$0")/../.."
# ONE PROFILE PER INVOCATION WHEN ASKED, because eight profiles inside one step is one cost divided
# eight ways and one thing to schedule where there are eight.
#
# `--only <arch>:<label>:<cores>` runs exactly that profile. With no argument every profile runs, in
# order, which is what a person typing the gate's name expects and what the umbrella entry keeps.
# The catalog registers the eight individually: a merged step's per-profile figures are an artefact
# of how they happened to be batched, and an emulated aarch64 profile and a one-core riscv64 one
# differ by more than that arithmetic can express.
ONLY=""
if [[ "${1:-}" == "--only" ]]; then
	ONLY="${2:?--only needs <arch>:<label>:<cores>}"
fi
# A SELECTOR THAT NAMES NOTHING IS NOT A RUN OF NOTHING, IT IS A REFUSAL.
#
# `--only` took any string, `run_profile` returned success for every profile it did not name, and the
# success line at the bottom then printed that "$ONLY booted, named the controller it has, delivered
# timer interrupts and brought up every declared core" - about a profile that had never started. A
# typo in `check.sh` was a green gate that ran no QEMU. `RAN` counts the profiles this invocation
# actually booted and the bottom refuses a selector that booted none, so the list lives in one place
# (the `run_profile` calls) rather than being duplicated here to be validated against.
RAN=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
	echo "arch-profiles: $*" >&2
	exit 1
}

# THE LOGS A RUN SAID IT WROTE, joined into one file this gate reads as one thing.
#
# This was "the newest guest log of this architecture", which is another run's answer the moment two
# are in flight - and only the GUEST log, which on riscv64 holds U-Boot and the loader and none of
# the suite's output. `test-kernel.sh` names both files it wrote; joining them is what lets every
# assertion below stay one `grep` over one path.
run_result_log() {
	local captured="$1" merged="${1%.log}.result" logs
	mapfile -t logs < <(result_logs "$captured") || return 1
	((${#logs[@]})) || return 1
	cat "${logs[@]}" >"$merged" || return 1
	printf '%s\n' "$merged"
}

# The timer floor, read out of the line rather than matched as text: what matters is the NUMBER.
timer_ticked() {
	local log="$1" line ticks
	if grep -aq "NO TIMER IRQ WAS DELIVERED" "$log"; then
		echo "arch-profiles: no timer interrupt was delivered on this profile" >&2
		grep -a "NO TIMER IRQ" "$log" >&2
		return 1
	fi
	line="$(grep -a -m 1 -o 'timer IRQs delivered - [0-9]* ticks' "$log" || true)"
	[[ -n "$line" ]] || {
		echo "arch-profiles: the boot never reported a timer tick count at all" >&2
		return 1
	}
	ticks="$(sed 's/[^0-9]*\([0-9]*\).*/\1/' <<<"$line")"
	[[ "$ticks" -ge 5 ]] || {
		echo "arch-profiles: the timer delivered $ticks tick(s), fewer than the 5 this profile requires" >&2
		return 1
	}
	echo "arch-profiles:     timer delivered $ticks ticks"
}

# WHAT EACH PROFILE MUST BE SEEN TO DO, over and above coming up.
#
# The milestone names five: MSI delivery and MSI teardown at one core, and a remote wake IPI, a real
# TLB shootdown acknowledgement and secondary-core scheduling at four. Each is a test that already
# exists and drives the path end to end; what was missing was running them HERE, on the profile, and
# requiring them by name. The tag set is the one the milestone permits in place of the full suite.
# `memory` IS NOT HERE, AND ITS ABSENCE IS THE POINT.
#
# It pulled in `kernel.applications.imgconv_governed_working_set_is_measured`, which is tagged
# `[Image, Memory, Process, Service, Storage]` and takes 1149 SECONDS on an emulated aarch64 - 83% of
# a 23-minute profile, run eight times, for two and a half hours per gate. It is an image-conversion
# working-set measurement. This gate is about which interrupt controller the machine has.
#
# NOTHING THIS GATE ASSERTS IS LOST. Its oracles are the MSI pair and three multi-core tests, and
# `a_shootdown_is_answered_by_every_other_core` - the only one carrying `Memory` - also carries
# `Scheduler` and `Smp`, so it still runs. What goes is coverage the DEFAULT machine already provides
# and this profile has no claim on.
#
# Measured 2026-08-27, which is why the number above is a number and not "slow".
#
# AND THEN THE TAG SET WENT ENTIRELY. The reasoning above is kept because it is the measurement that
# justified trimming `memory`, and because it explains what this gate is and is not about - but the
# answer to "which tags does this profile need" turned out to be "none of them". A gate asserting N
# named things asks for those N things, and `run_profile` now names the ids it will assert on. The
# tag set was the last place this gate could quietly buy a test family to read four lines out of.
# MSI acquire, program, bind, dispatch and release - the delivery and the teardown - on whichever MSI
# controller this machine has. Set per profile, because a GICv3 with its ITS turned OFF has no MSI
# backend at all: asking it an MSI question proves nothing, and that profile exists for the timer and
# IPI paths. `MSI_ORACLE` empty means this profile makes no MSI claim, and it says so.
MSI_ORACLE=""
# THE ORACLE THAT NEEDS A DEVICE, empty on every profile that cannot supply one. See `run_profile`.
DEVICE_MSI_ORACLE=""
# WHETHER THIS ROW BOOTS THROUGH FIRMWARE. Zero for every DISCOVERY profile, which M6 asks to be
# direct boots, and one for the single CHECKPOINT row that needs what only a firmware boot carries.
# See the device-MSI row below for the measurement that separates the two.
PROFILE_UEFI=0
# A PROFILE THAT BOOTS ITS OWN LOADER, empty for one that boots the ordinary build.
#
# The no-device-tree rows need a loader that declines to pass the firmware's tree on, and that is a
# COMPILE-TIME profile - see the loader's `withholds_device_tree`. Building it over the ordinary
# loader would leave a tree that no other boot can use if this gate died in the middle, so the row
# builds its own into a scratch target directory and hands `qemu-run.sh` the path through
# `LOADER_EFI`, which every architecture already honours. Nothing shared is touched, and the image
# key covers the loader, so no cached medium is reused across the two shapes.
PROFILE_LOADER=""

# A LINE THIS PROFILE MUST NOT PRINT, empty for a profile that forbids nothing.
#
# The UEFI regression rows below are the only users and the reason it exists: what they have to show
# is not only that the loader path boots, but that booting through firmware did NOT reach the static
# no-DT descriptor. Both ports print a named line when they take it, so the absence of that line is
# the assertion - a negative one, which no `want` string can express.
PROFILE_FORBIDS=""
MULTI_CORE_ORACLES="kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick kernel.kernel.a_shootdown_is_answered_by_every_other_core kernel.sched.scheduler_runs_across_cores"

# One profile: boot it, then ask the boot what machine it was on.
run_profile() {
	if [[ -n "$ONLY" && "$ONLY" != "$1:$2:$3" ]]; then
		return 0
	fi
	RAN=$((RAN + 1))
	local arch="$1" label="$2" cores="$3" want="$4"
	shift 4
	echo "arch-profiles: $arch $label, $cores core(s)"
	local out="$work/$arch-$label-$cores.log"
	# THE DECLARED INTEGRATION SET, NOT THE SMOKE ONE.
	#
	# This ran `--tags smoke` and then greped for a controller name, five timer ticks and a full CPU
	# tally - and the milestone this gate belongs to names five more things each profile must be seen
	# to do: MSI delivery, MSI teardown, a remote wake IPI, a real TLB shootdown acknowledgement and
	# a thread actually scheduled on a secondary core. None of them was asserted, and none of the
	# tests that assert them ran here. The milestone permits exactly this set in place of the full
	# suite, and running it is what puts those tests on the profile rather than only on the default
	# machine.
	# AND THE HARNESS'S OWN BOUND, NOT ONE OF THIS GATE'S.
	#
	# This passed `--timeout 1800`, which is TIGHTER than what `test.sh` picks for a tag subset on an
	# emulated target (45m) and tighter than the stall detector that decides whether a run is wedged
	# (2400s of silence). A timeout below the stall bound means SLOW is always reported before WEDGED
	# can be distinguished - which is the defect P02M0156 is named after, arrived at from the other
	# side: not a gate that skips itself green, but one that cannot tell the two failures apart.
	#
	# Measured 2026-08-27: `aarch64 gicv2 at 4 core(s)` hit the 1800s bound with 82 tests passed, none
	# failed, and the boot still making progress - `DeviceManager: 10 of 10 device(s) online` four
	# lines from the end. That is a slow machine reported as a broken one.
	#
	# Two authorities over one window is the thing this tree keeps removing. The harness owns the
	# emulated calibration; this gate asks it to run and lets it decide.
	# A PROFILE WITH NO MSI BACKEND CANNOT BRING UP THE SERVICES THAT NEED ONE, and asking it to is
	# this gate contradicting its own configuration.
	#
	# `boot` pulls in `kernel.boot.init_package_starts_system_manager`, which requires EVERY manifest
	# service to report online. On a GICv3 with its ITS turned off there is no MSI controller at all -
	# `MSI_ORACLE` is empty for exactly that reason, and the gicv2m test declines the machine in as
	# many words - so virtio_net, virtio_gpu, virtio_snd and xhci cannot be given an interrupt, and
	# NetworkService, DisplayService and everything waiting behind them never start.
	#
	# Measured 2026-08-27 on `aarch64 gicv3 at 1 core(s)`: `6 of 10 device(s) online`, four drivers
	# refused with `resource-exhausted`, and the boot test failed on seven missing services. Nothing
	# was wrong with the kernel; the profile was asked for something it is defined not to have.
	#
	# WHAT THE PROFILE STILL PROVES IS UNCHANGED: the controller it discovered and its timer ticks are
	# asserted by this gate directly off the log, and the multi-core oracles arrive through
	# `scheduler` and `smp`. Only the whole-system boot assertion goes, and only where it is
	# structurally unsatisfiable.
	# A GATE ASSERTING N NAMED THINGS ASKS FOR THOSE N THINGS.
	#
	# This asked for six subject TAGS and then greped the result for the one MSI test and, on a
	# multi-core profile, three named SMP tests. So it ran every test carrying any of those six tags -
	# and every test any of them will ever acquire - to read four lines out of the log. `TEST_SELECTION`
	# takes ids, hard-fails on an id the kernel does not declare, and is exactly the instrument for
	# this; asking for a subject tag was buying a family to look at four of its members.
	#
	# The ids are the ones the assertions below use, built from the same two variables, so the request
	# and the assertion cannot drift apart.
	#
	# A PROFILE WITH NO ORACLES STILL BOOTS. `aarch64:gicv3:1` has no MSI backend and one core, so it
	# names no test at all - what it proves is the controller it discovered and its timer ticks, both
	# read off the boot. An empty selection would fall through to the tag path, so that case asks for
	# `smoke`: the smallest bounded run that gets the boot to a clean suite exit.
	local selection=""
	if [[ -n "$MSI_ORACLE" ]]; then
		selection="$MSI_ORACLE"
	else
		echo "arch-profiles:     no MSI backend on this profile - not requiring a full service bring-up"
	fi
	# AND, WHERE A PROFILE HAS ONE, THE ORACLE THAT NEEDS A DEVICE.
	#
	# The MSI oracle above allocates an ordinary RAM frame as a stand-in MSI-X table and calls the
	# backend's dispatch by hand: it proves the CONTROLLER can allocate, deliver and release, and it
	# cannot prove that a device raised anything. This one drives a real virtio-sound function -
	# programs its MSI-X table, enables MSI-X on the function, and waits for the interrupt the device
	# raises when a capture period is ready - so the message comes off the wire and is acknowledged
	# through the controller's own IAR.
	if [[ -n "$DEVICE_MSI_ORACLE" ]]; then
		selection="${selection:+$selection,}$DEVICE_MSI_ORACLE"
	fi
	if [[ "$cores" -gt 1 ]]; then
		local want_id
		for want_id in $MULTI_CORE_ORACLES; do
			selection="${selection:+$selection,}$want_id"
		done
	fi
	# DIRECT BOOT, WHICH IS WHAT M6 ASKS FOR AND WHAT THIS GATE COULD NOT DO.
	#
	# It forced `UEFI=1` because the direct path came up on ONE core: QEMU enters with `x0 = 0` and
	# this runner loads the tree at a fixed address, so `psci::conduit` was asked about zero, answered
	# `PSCI_NONE`, and no secondary ever started. That is fixed - the conduit is read from where the
	# tree IS - and a four-core direct profile now brings up four cores, so the profiles boot the way
	# the milestone names: the controller AND the bring-up read from the tree in front of them.
	# THE PROFILE'S OWN LOADER AND THE MATCHING KERNEL AUTHORISATION, both or neither.
	#
	# `LIBER_NO_DT_PROFILE` is one name on two sides: the loader withholds the tree and the kernel
	# authorises the static descriptor its absence selects. A loader that withholds it against a
	# kernel that has not authorised one is a machine that panics by design, so the row passes the
	# same variable to both builds.
	local -a profile_env=()
	if [[ -n "$PROFILE_LOADER" ]]; then
		profile_env=("LOADER_EFI=$PROFILE_LOADER" "LIBER_NO_DT_PROFILE=1")
	fi
	local -a request=()
	if [[ -n "$selection" ]]; then
		request=(env "UEFI=$PROFILE_UEFI" "${profile_env[@]}" "TEST_SELECTION=$selection" "$@" ./test.sh --arch "$arch" --smp "$cores")
		echo "arch-profiles:     asking for $(tr ',' ' ' <<<"$selection" | wc -w) named test(s)$([[ "$PROFILE_UEFI" == 1 ]] && echo " (through firmware)")"
	else
		request=(env "UEFI=$PROFILE_UEFI" "${profile_env[@]}" "$@" ./test.sh --arch "$arch" --tags smoke --smp "$cores")
	fi
	if ! "${request[@]}" >"$out" 2>&1; then
		echo "arch-profiles: the integration suite failed on $arch $label at $cores core(s)" >&2
		tail -20 "$out" >&2
		exit 1
	fi
	local log
	log="$(run_result_log "$out")" || fail "$arch $label did not say which logs it wrote"
	grep -aq "$want" "$log" || {
		echo "arch-profiles: $arch $label did not report the controller this profile has" >&2
		echo "arch-profiles:   wanted: $want" >&2
		grep -a -m 10 -E "GICv|IMSIC|PLIC|interrupts:" "$log" >&2 || echo "    (it named no interrupt controller at all)" >&2
		exit 1
	}
	echo "arch-profiles:     discovered: $(grep -a -m 1 -o "$want.*" "$log")"
	if [[ -n "$PROFILE_FORBIDS" ]]; then
		if grep -aq "$PROFILE_FORBIDS" "$log"; then
			echo "arch-profiles: $arch $label printed a line this profile forbids: $PROFILE_FORBIDS" >&2
			grep -a -m 5 "$PROFILE_FORBIDS" "$log" >&2
			exit 1
		fi
		echo "arch-profiles:     and the static no-DT descriptor was NOT selected - this boot read a tree"
	fi
	timer_ticked "$log" || exit 1
	# THE NAMED ORACLES, BY TEST ID. A profile that boots and counts its cores has shown that
	# discovery worked; it has not shown that anything discovered can be USED. Each id below is a
	# test that drives one of the five paths the milestone names, and requiring it by name is what
	# stops a profile from passing on a suite that quietly stopped running it.
	#
	# PRESENCE IS THE QUESTION, because failure is already answered. `test.sh` exits non-zero on any
	# failing test and the caller above stops there, so a test that appears in this log is one that
	# ran AND passed. What this catches is the other case: a test that stopped running here, which a
	# suite exiting zero cannot distinguish from one that never existed.
	passed() {
		grep -aq "^$1\.\.\." "$log"
	}
	local id
	if [[ -n "$MSI_ORACLE" ]]; then
		passed "$MSI_ORACLE" || fail "$arch $label at $cores core(s): $MSI_ORACLE did not run and pass, so this profile's MSI controller was never used"
		# AND IT WAS NOT SKIPPED. The test declines itself on a machine with no MSI backend and says
		# so, which is the honest answer there - and on a profile that HAS one, a skip is the test
		# reporting that discovery did not find it.
		if grep -aqi "skipped - this machine has no MSI" "$log"; then
			fail "$arch $label: the MSI test declined this machine, so the controller this profile is named for was not discovered"
		fi
		echo "arch-profiles:     MSI acquired, delivered, bound and released on this controller"
	else
		echo "arch-profiles:     this profile has no MSI backend and makes no MSI claim"
	fi
	if [[ "$cores" -gt 1 ]]; then
		for id in $MULTI_CORE_ORACLES; do
			passed "$id" || fail "$arch $label at $cores core(s): $id did not run and pass, so this profile proves nothing about cross-core work on it"
		done
		# WHAT THE WAKE TEST ACTUALLY MEASURED, RATHER THAN WHAT ITS NAME SUGGESTS.
		#
		# `a_remote_spawn_wakes_a_halted_core...` compares a woken run against a deliberately
		# unwoken one and PASSES on three outcomes: the wake saved time (the measurement), the gap
		# sat inside this machine's own noise floor (nothing to measure), or - failing - the wake
		# made it worse. Under emulation the middle one is common, and the line here claimed a
		# measured remote wake IPI for it. The positive acknowledgement this profile rests on is the
		# shootdown: every OTHER core must answer it, which is an IPI delivered and acknowledged and
		# is not a timing comparison. So the shootdown and the secondary-core thread are stated
		# unconditionally, and the wake is stated as what it was on this run.
		if grep -aq "there is nothing here to measure" "$log"; then
			echo "arch-profiles:     TLB shootdown acknowledged by every other core, and a thread on a secondary core"
			echo "arch-profiles:     (the remote wake could not be measured here - this machine's idle cores do not stay halted long enough)"
		else
			echo "arch-profiles:     remote wake IPI, TLB shootdown acknowledgement and a thread on a secondary core"
		fi
	fi
	if [[ "$cores" -gt 1 ]]; then
		# EVERY DECLARED CORE, not "more than one". A machine that started three of four is a boot
		# that works and a claim that does not.
		local smp
		smp="$(grep -a -m 1 -o 'SMP - [0-9]* of [0-9]* declared \(cores\|harts\) online' "$log" || true)"
		[[ -n "$smp" ]] || fail "$arch $label at $cores cores never reported its SMP outcome"
		local up total
		up="$(sed 's/SMP - \([0-9]*\) of.*/\1/' <<<"$smp")"
		total="$(sed 's/.* of \([0-9]*\) declared.*/\1/' <<<"$smp")"
		[[ "$up" == "$total" && "$up" == "$cores" ]] || fail "$arch $label: $smp, and this profile declares $cores"
		echo "arch-profiles:     $smp"
	fi
}

# The aarch64 controllers, booted the way the suite boots them.
#
# NOT `UEFI=0`, which is what the first version of this gate forced. `test.sh` boots the device-tree
# ports through their own loader - "they have no other way in since the packaged bootstrap archive
# and the magic scan that found it were retired" - and the loader hands the kernel a `BootInfo`
# carrying the device tree AND the PSCI conduit it read out of it. Forcing the raw-DTB entry instead
# produced a machine with no conduit, so no secondary ever started and a four-core profile came up on
# one core. The controller is still discovered FROM THE TREE either way, which is what these profiles
# are about; what the loader adds is the way in.
AARCH64_MSI=kernel.arch.aarch64.interrupts.gicv2m_msi_binds_and_dispatch_signals_the_driver
# A GICv2 machine has a v2m frame; a GICv3 with `its=off` has neither frame nor ITS.
MSI_ORACLE="$AARCH64_MSI"
run_profile aarch64 gicv2 1 "GICv2 from the device tree" GIC=2
run_profile aarch64 gicv2 4 "GICv2 from the device tree" GIC=2
MSI_ORACLE=""
run_profile aarch64 gicv3 1 "GICv3 from the device tree" GIC=3
run_profile aarch64 gicv3 4 "GICv3 from the device tree" GIC=3
MSI_ORACLE="$AARCH64_MSI"
run_profile aarch64 gicv3-its 1 "GICv3 from the device tree" GIC=3its
# THE REAL-DEVICE ITS/MSI CHECKPOINT, AND WHY IT IS ONE ROW BELOW RATHER THAN A PROPERTY OF THESE
# (2026-09-01).
#
# The previous note here concluded the checkpoint was unreachable. Two of its three steps hold: every
# MSI ORACLE programs a RAM-backed stand-in table and calls the shared dispatch by hand, so no report
# on that path can tell a device-raised message from the kernel calling itself; and these rows are
# DIRECT boots, which is what M6 asks them to be. What it got wrong is the conclusion, and what makes
# the difference is that the kernel's own hardware suite already programs a REAL `virtio-sound-pci`
# function's MSI-X table and waits for the interrupt that device raises - so the message exists on
# any machine carrying that function, and on an ITS machine it is a device-originated LPI by
# construction.
#
# What the checkpoint needed was therefore two instruments and one machine, not a new capability:
#
#   - a report where the INTID comes out of the GIC's own acknowledge register, which no oracle can
#     reach because an oracle never passes through it - see `is_device_lpi` and `gic.rs`;
#   - a teardown at the end of that test, which used to end holding the claim and the vector;
#   - and a boot that CARRIES THE VOLUME PACKAGE. This is the part a direct boot cannot supply, and
#     it is what the old note was reaching for: the sound test reads its driver artifact off the
#     volume, and on a direct row it fails with `volume package module not found` - measured
#     2026-09-01, by putting the oracle on this row and watching it fail exactly there.
#
# So the checkpoint is one row of its own, below, booted through firmware. The eight rows above stay
# direct, which is what M6 asks of the DISCOVERY profiles; this one is not a discovery profile and
# does not claim to be.
run_profile aarch64 gicv3-its 4 "GICv3 from the device tree" GIC=3its

# THE DEVICE HALF OF M3's CHECKPOINT - one row, through firmware, on the ITS machine. See the note
# above for why it is separate. The machine carries a `virtio-sound-pci` function whose MSI-X table
# the sound test programs itself; the ITS is what translates that device's write; and the test now
# releases the vector with its claim, which is the teardown half M3 asks for beside the delivery.
DEVICE_MSI_ORACLE=kernel.hardware.virtio_snd_driver_captures_a_period_from_the_device
PROFILE_UEFI=1
run_profile aarch64 gicv3-its-device 4 "GICv3 from the device tree" GIC=3its
PROFILE_UEFI=0
DEVICE_MSI_ORACLE=""

# THE UEFI SINGLE-NODE REGRESSION ROW, which M6 asks for by name and which this gate had stopped
# running at all (restored 2026-09-02).
#
# The eight discovery rows above are DIRECT boots, and making them so is what deleted this: before
# them every aarch64 profile came in through firmware, so the loader path was covered by accident.
# M6 asks for it to be covered on purpose - "keep the existing aarch64/riscv64 UEFI boots as separate
# single-node regression profiles ... they prove the loader path still works, not that controller
# discovery occurred" - and nothing was left proving it.
#
# WHAT IT PROVES AND WHAT IT DOES NOT. It proves the firmware entry still reaches a booted kernel
# that discovers its controller, ticks, and passes this profile's oracles on one core. It does NOT
# prove the no-DT descriptor works, and the `PROFILE_FORBIDS` line is there so it cannot be mistaken
# for that: QEMU's `virt` hands the firmware a device tree and the loader passes it on, so this boot
# HAS a tree and must be seen not to have taken the static descriptor. That is the half of the
# Definition of Done a boot with a tree can carry - "their static QEMU descriptors cannot be selected
# by a boot which has a DT" - asserted rather than assumed.
#
# The other half, a positive boot of a machine that publishes NO tree, is still blocked on a harness
# capability. See the note below.
MSI_ORACLE="$AARCH64_MSI"
PROFILE_UEFI=1
PROFILE_FORBIDS="authorises"
run_profile aarch64 uefi 1 "GICv2 from the device tree" GIC=2
PROFILE_FORBIDS=""
PROFILE_UEFI=0

# BUILD THE NO-DEVICE-TREE LOADER FOR ONE ARCHITECTURE, into a directory of its own.
#
# `LIBER_NO_DT_PROFILE=1` is what makes the loader decline to pass the firmware's device tree on, and
# it is compile-time - so this is a second loader, not a reconfiguration of the one every other boot
# uses. It goes in the gate's own work directory with its own `CARGO_TARGET_DIR`, so a failure here
# leaves the tree exactly as it found it. Answers the path to the built EFI binary.
build_no_dt_loader() {
	local arch="$1"
	local triple
	case "$arch" in
	aarch64) triple=aarch64-unknown-uefi ;;
	riscv64) triple=riscv64gc-unknown-none-elf ;;
	*) fail "no no-DT loader shape for $arch" ;;
	esac
	local out="$work/no-dt-loader-$arch"
	mkdir -p "$out"
	# TO STDERR, because this function's STDOUT is its answer: the path to the built loader. A
	# progress line on the same stream becomes part of the path, and the runner then reports a loader
	# that is not there - which is what happened the first time this ran.
	echo "arch-profiles: building the no-device-tree loader for $arch" >&2
	case "$arch" in
	aarch64)
		(cd src/boot/loader && CARGO_TARGET_DIR="$out" LIBER_NO_DT_PROFILE=1 cargo build --target "$triple" >/dev/null 2>&1) || fail "the no-DT loader did not build for $arch"
		;;
	riscv64)
		(cd src && CARGO_TARGET_DIR="$out" LIBER_NO_DT_PROFILE=1 tools/build-loader-riscv64.sh >/dev/null 2>&1) || fail "the no-DT loader did not build for $arch"
		;;
	esac
	local efi="$out/$triple/debug/libersystem-loader.efi"
	[[ -f "$efi" ]] || fail "the no-DT loader built for $arch and left no EFI binary at $efi"
	echo "$efi"
}

# THE POSITIVE NO-DEVICE-TREE ROWS, one per device-tree port.
#
# M6 asks for the static descriptor a treeless machine falls back to to be SELECTED by a named
# profile and by nothing else, and until now no caller set `LIBER_NO_DT_PROFILE` - so the authorised
# path was unreachable and the descriptor it selects was unproved. The note that used to stand here
# said the blocker was a harness capability, and it named the two ways out: a QEMU machine that
# publishes no tree, or a LOADER OPTION THAT DECLINES TO PASS ONE ON. The second is ours, and it is
# built now.
#
# WHAT THE ROW PROVES: the kernel is handed a `BootInfo` whose `dtb` is zero, recognises it as the
# named profile, says so in its own words, and boots far enough to tick and pass this profile's
# oracles. That is the descriptor being selected BY A PROFILE, which is the half a boot with a tree
# cannot show - and the rows above prove the other half, that a boot WITH a tree does not reach it.
for no_dt_arch in aarch64 riscv64; do
	if [[ -n "$ONLY" && "$ONLY" != "$no_dt_arch:no-dt:1" ]]; then
		continue
	fi
	case "$no_dt_arch" in
	aarch64)
		MSI_ORACLE="$AARCH64_MSI"
		# THE CONTROLLER LINE, which on this profile NAMES THE STATIC DESCRIPTOR and says there was
		# no tree - so passing is the descriptor having been selected, not merely a boot that came up.
		no_dt_want="GICv2 from qemu-virt-gicv2 (no device tree)"
		;;
	riscv64)
		MSI_ORACLE=kernel.arch.riscv64.interrupts.imsic_msi_binds_and_dispatch_signals_the_driver
		no_dt_want="no device tree, and this build authorises the named no-DT profile"
		;;
	esac
	PROFILE_LOADER="$(build_no_dt_loader "$no_dt_arch")"
	PROFILE_UEFI=1
	run_profile "$no_dt_arch" no-dt 1 "$no_dt_want"
	PROFILE_UEFI=0
	PROFILE_LOADER=""
done

# WHAT IS STILL NOT REGISTERED HERE, AND WHY (2026-08-30, narrowed 2026-09-02).
#
# The UEFI regression rows themselves ARE registered now - one per device-tree port, above - and they
# carry the half of M6 a machine with a tree can carry: the loader path boots, discovers, ticks and
# passes its oracles, and the static descriptor is seen NOT to have been selected.
#
# What is still missing is the POSITIVE no-DT boot: `LIBER_NO_DT_PROFILE=1` is the compile-time
# authorisation for the static descriptor a treeless machine falls back to, and no caller in this
# tree sets it, so the authorised path is unreachable and the descriptor it selects is unproved.
# Registering a row for it was tried and does not work, and the reason is the useful part: it needs a
# machine that publishes NO device tree, and this harness cannot produce one. Booting through
# firmware does not do it - QEMU's `virt` gives the firmware a DTB and the loader hands it on, so a
# `UEFI=1` boot still prints `aarch64: GICv2 from the device tree`, measured. That is exactly what
# the rows above now assert rather than merely observe.
#
# What is missing is a way to withhold the tree: a QEMU machine that publishes none, or a loader
# option that does not pass one on. That is a harness capability rather than a gate row, and it is
# what remains of this item.

# AND THE ITS PROFILE MUST HAVE USED ITS ITS. `GICv3 from the device tree` is the same line the
# ITS-less profile prints, so on its own it would make the two profiles indistinguishable.
#
# THIS USED TO BE THE ABSENCE OF TWO ERROR STRINGS - "an ITS with no redistributor" and "the machine
# describes an ITS but no msi-map" - and passing meant neither had been printed. A boot that never
# attempted an MSI at all satisfied that, and the line it printed said as much: "reported no reason
# it could not hand out an MSI". The MSI oracle above is the positive form: on this profile a vector
# is acquired through the ITS, programmed into a device table, dispatched to a bound Interrupt and
# released, and the profile passes only if that test ran and passed here.
# The ITS profile that just ran, by the capture `run_profile` wrote for it - a name this gate chose,
# not a file it went looking for.
if [[ -z "$ONLY" || "$ONLY" == "aarch64:gicv3-its:4" ]]; then
	its_log="$(run_result_log "$work/aarch64-gicv3-its-4.log")" || fail "the ITS profile did not say which logs it wrote"
	if grep -aqE "interrupts: (an ITS with no redistributor|the machine describes an ITS but no msi-map)" "$its_log"; then
		echo "arch-profiles: the ITS profile came up without MSI" >&2
		grep -a "interrupts:\|its:" "$its_log" >&2
		exit 1
	fi
	# THE CONTROLLER HALF, which is the oracle's: a vector acquired through the ITS, programmed into
	# a device table, dispatched to a bound Interrupt and released. It says "by the kernel's own
	# oracle" because that is what it is - the oracle allocates a RAM frame as a stand-in MSI-X table
	# and calls `dispatch_msi` itself.
	echo "arch-profiles:   the ITS profile discovered its ITS, and a vector was acquired through it, programmed into a device table, dispatched to a bound Interrupt and released - by the kernel's own oracle"
fi

# THE DEVICE HALF, WHICH THE ORACLE CANNOT MAKE, read from the checkpoint row's own log.
if [[ -z "$ONLY" || "$ONLY" == "aarch64:gicv3-its-device:4" ]]; then
	device_log="$(run_result_log "$work/aarch64-gicv3-its-device-4.log")" || fail "the ITS device row did not say which logs it wrote"
	# This INTID came out of the GIC's own acknowledge register, so nothing but the interrupt
	# controller put it there, and an LPI means an ITS translated a device's write to produce it. The
	# oracles call `dispatch_msi` directly and never reach the line this greps for, which is the whole
	# reason it sits where it does.
	grep -aq "interrupts: a device raised INTID .* an LPI the ITS translated and delivered" "$device_log" || {
		echo "arch-profiles: the ITS device row saw no device-originated LPI" >&2
		grep -a "interrupts:\|virtio-snd:\|its:" "$device_log" >&2 || echo "    (it reported nothing about interrupts at all)" >&2
		exit 1
	}
	# AND THE TEARDOWN THAT FOLLOWS IT. M3 asks for delivery AND teardown, and a vector that is
	# delivered on and never given back is half a checkpoint.
	grep -aq "virtio-snd: the device's MSI vector was delivered on and then torn down with its claim" "$device_log" || {
		echo "arch-profiles: the ITS device row delivered a device LPI and did not prove its teardown" >&2
		grep -a "virtio-snd:" "$device_log" >&2 || echo "    (the device oracle did not run)" >&2
		exit 1
	}
	echo "arch-profiles:   $(grep -a -m 1 -o "interrupts: a device raised INTID.*" "$device_log"), and the vector was torn down with its claim"
fi

# The RISC-V AIA. Nothing to select: this runner's only riscv64 machine is `virt,aia=aplic-imsic`, so
# the profile is what it boots - and passing a second `-machine` to add the AIA, which the first
# version of this gate did, is a QEMU command line with two of them.
MSI_ORACLE=kernel.arch.riscv64.interrupts.imsic_msi_binds_and_dispatch_signals_the_driver
run_profile riscv64 aia 1 "IMSIC S-mode files from the device tree"
run_profile riscv64 aia 4 "IMSIC S-mode files from the device tree"

# AND THE riscv64 SINGLE-NODE UEFI REGRESSION ROW. The aarch64 note above is the whole of the
# reasoning; this is the same row on the other device-tree port, and the line it must not print is
# riscv64's own no-DT authorisation.
PROFILE_UEFI=1
PROFILE_FORBIDS="authorises the named no-DT profile"
run_profile riscv64 uefi 1 "IMSIC S-mode files from the device tree"
PROFILE_FORBIDS=""
PROFILE_UEFI=0

if [[ -n "$ONLY" ]]; then
	((RAN)) || fail "no profile is named '$ONLY' - this gate ran nothing, and saying it passed is the false green it exists against"
	echo "arch-profiles: $ONLY booted, named the controller it has, delivered timer interrupts and brought up every declared core"
else
	echo "arch-profiles: every named profile booted, named the controller it has, delivered timer interrupts and brought up every declared core"
fi
