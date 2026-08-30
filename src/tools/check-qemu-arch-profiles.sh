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
	local -a request=()
	if [[ -n "$selection" ]]; then
		request=(env UEFI=0 "TEST_SELECTION=$selection" "$@" ./test.sh --arch "$arch" --smp "$cores")
		echo "arch-profiles:     asking for $(tr ',' ' ' <<<"$selection" | wc -w) named test(s)"
	else
		request=(env UEFI=0 "$@" ./test.sh --arch "$arch" --tags smoke --smp "$cores")
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
run_profile aarch64 gicv3-its 4 "GICv3 from the device tree" GIC=3its

# THE UEFI / NO-DEVICE-TREE REGRESSION PROFILES ARE NOT REGISTERED HERE, AND THIS SAYS WHY
# (2026-08-30). M6 asks for separate, labelled aarch64 and riscv64 UEFI/no-DT profiles and the
# Definition of Done asks for them green; `LIBER_NO_DT_PROFILE=1` is the compile-time authorisation
# for the static descriptor such a machine falls back to, and no caller in this tree sets it - so the
# authorised profile is unreachable and the named refusal it guards is untestable.
#
# Registering two rows here was tried and does not work, and the reason is the useful part: the
# profile needs a machine that publishes NO device tree, and this harness cannot produce one. Booting
# through firmware instead of directly does not do it - QEMU's `virt` gives the firmware a DTB and the
# loader hands it on, so a `UEFI=1` boot still prints `aarch64: GICv2 from the device tree`, measured.
#
# What is missing is a way to withhold the tree: a QEMU machine that publishes none, or a loader
# option that does not pass one on. That is a harness capability rather than a gate row, and it is
# what this item is actually blocked on.

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
	# AND THE CLAIM IS THE ONE THE ORACLE MAKES (corrected 2026-08-30). This said "delivered a REAL
	# MSI", and the oracle behind it allocates an ordinary RAM frame as a stand-in MSI-X table and
	# calls `dispatch_msi` by hand - the controller path is exercised end to end, the DEVICE path is
	# not. A device-to-ITS write is what the ITS profile would have to see to say "real", and no test
	# on this profile produces one: the virtio-snd hardware test stops at stream acknowledgement and
	# releases neither the claim nor the vector. Saying what was proved is the fix available here;
	# proving the device path needs a device on this profile, which is its own item.
	echo "arch-profiles:   the ITS profile discovered its ITS, and a vector was acquired through it, programmed into a device table, dispatched to a bound Interrupt and released - by the kernel's own oracle rather than by a device"
	echo "arch-profiles:   (a device-originated MSI through the ITS is NOT proved here - no device on this profile raises one)"
fi

# The RISC-V AIA. Nothing to select: this runner's only riscv64 machine is `virt,aia=aplic-imsic`, so
# the profile is what it boots - and passing a second `-machine` to add the AIA, which the first
# version of this gate did, is a QEMU command line with two of them.
MSI_ORACLE=kernel.arch.riscv64.interrupts.imsic_msi_binds_and_dispatch_signals_the_driver
run_profile riscv64 aia 1 "IMSIC S-mode files from the device tree"
run_profile riscv64 aia 4 "IMSIC S-mode files from the device tree"

if [[ -n "$ONLY" ]]; then
	((RAN)) || fail "no profile is named '$ONLY' - this gate ran nothing, and saying it passed is the false green it exists against"
	echo "arch-profiles: $ONLY booted, named the controller it has, delivered timer interrupts and brought up every declared core"
else
	echo "arch-profiles: every named profile booted, named the controller it has, delivered timer interrupts and brought up every declared core"
fi
