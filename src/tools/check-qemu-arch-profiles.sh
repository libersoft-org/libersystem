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

cd "$(dirname "$0")/../.."
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
	echo "arch-profiles: $*" >&2
	exit 1
}

# The newest guest log a run produced, read without a reader that stops early.
newest_guest_log() {
	local arch="$1" logs
	shopt -s nullglob
	logs=(.build/logs/test/"$arch"-*-guest.log)
	shopt -u nullglob
	((${#logs[@]})) || return 1
	readarray -t logs < <(printf '%s\n' "${logs[@]}" | sort)
	printf '%s\n' "${logs[-1]}"
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
TAGS="boot,smp,interrupt,paging,scheduler,drivers,memory"
# MSI acquire, program, bind, dispatch and release - the delivery and the teardown - on whichever MSI
# controller this machine has. Set per profile, because a GICv3 with its ITS turned OFF has no MSI
# backend at all: asking it an MSI question proves nothing, and that profile exists for the timer and
# IPI paths. `MSI_ORACLE` empty means this profile makes no MSI claim, and it says so.
MSI_ORACLE=""
MULTI_CORE_ORACLES="kernel.sched.a_remote_spawn_wakes_a_halted_core_without_waiting_for_the_tick kernel.kernel.a_shootdown_is_answered_by_every_other_core kernel.sched.scheduler_runs_across_cores"

# One profile: boot it, then ask the boot what machine it was on.
run_profile() {
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
	if ! env "$@" ./test.sh --arch "$arch" --tags "$TAGS" --smp "$cores" --timeout 1800 >"$out" 2>&1; then
		echo "arch-profiles: the integration suite failed on $arch $label at $cores core(s)" >&2
		tail -20 "$out" >&2
		exit 1
	fi
	local log
	log="$(newest_guest_log "$arch")" || fail "$arch $label produced no guest log"
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
		echo "arch-profiles:     remote wake IPI, TLB shootdown acknowledgement and a thread on a secondary core"
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

# AND THE ITS PROFILE MUST HAVE USED ITS ITS. `GICv3 from the device tree` is the same line the
# ITS-less profile prints, so on its own it would make the two profiles indistinguishable.
#
# THIS USED TO BE THE ABSENCE OF TWO ERROR STRINGS - "an ITS with no redistributor" and "the machine
# describes an ITS but no msi-map" - and passing meant neither had been printed. A boot that never
# attempted an MSI at all satisfied that, and the line it printed said as much: "reported no reason
# it could not hand out an MSI". The MSI oracle above is the positive form: on this profile a vector
# is acquired through the ITS, programmed into a device table, dispatched to a bound Interrupt and
# released, and the profile passes only if that test ran and passed here.
its_log="$(newest_guest_log aarch64)" || fail "the ITS profile produced no guest log"
if grep -aqE "interrupts: (an ITS with no redistributor|the machine describes an ITS but no msi-map)" "$its_log"; then
	echo "arch-profiles: the ITS profile came up without MSI" >&2
	grep -a "interrupts:\|its:" "$its_log" >&2
	exit 1
fi
echo "arch-profiles:   the ITS profile discovered its ITS and delivered a real MSI through it"

# The RISC-V AIA. Nothing to select: this runner's only riscv64 machine is `virt,aia=aplic-imsic`, so
# the profile is what it boots - and passing a second `-machine` to add the AIA, which the first
# version of this gate did, is a QEMU command line with two of them.
MSI_ORACLE=kernel.arch.riscv64.interrupts.imsic_msi_binds_and_dispatch_signals_the_driver
run_profile riscv64 aia 1 "IMSIC S-mode files from the device tree"
run_profile riscv64 aia 4 "IMSIC S-mode files from the device tree"

echo "arch-profiles: every named profile booted, named the controller it has, delivered timer interrupts and brought up every declared core"
