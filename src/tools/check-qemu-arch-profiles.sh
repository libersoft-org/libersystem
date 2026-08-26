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

# One profile: boot it, then ask the boot what machine it was on.
run_profile() {
	local arch="$1" label="$2" cores="$3" want="$4"
	shift 4
	echo "arch-profiles: $arch $label, $cores core(s)"
	local out="$work/$arch-$label-$cores.log"
	if ! env "$@" ./test.sh --arch "$arch" --tags smoke --smp "$cores" --timeout 1800 >"$out" 2>&1; then
		echo "arch-profiles: the smoke suite failed on $arch $label at $cores core(s)" >&2
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
run_profile aarch64 gicv2 1 "GICv2 from the device tree" GIC=2
run_profile aarch64 gicv2 4 "GICv2 from the device tree" GIC=2
run_profile aarch64 gicv3 1 "GICv3 from the device tree" GIC=3
run_profile aarch64 gicv3 4 "GICv3 from the device tree" GIC=3
run_profile aarch64 gicv3-its 1 "GICv3 from the device tree" GIC=3its
run_profile aarch64 gicv3-its 4 "GICv3 from the device tree" GIC=3its

# AND THE ITS PROFILE MUST HAVE USED ITS ITS. `GICv3 from the device tree` is the same line the
# ITS-less profile prints, so on its own it would make the two profiles indistinguishable - which is
# exactly the shape of check this milestone exists to remove.
its_log="$(newest_guest_log aarch64)" || fail "the ITS profile produced no guest log"
if grep -aqE "interrupts: (an ITS with no redistributor|the machine describes an ITS but no msi-map)" "$its_log"; then
	echo "arch-profiles: the ITS profile came up without MSI" >&2
	grep -a "interrupts:\|its:" "$its_log" >&2
	exit 1
fi
echo "arch-profiles:   the ITS profile reported no reason it could not hand out an MSI"

# The RISC-V AIA. Nothing to select: this runner's only riscv64 machine is `virt,aia=aplic-imsic`, so
# the profile is what it boots - and passing a second `-machine` to add the AIA, which the first
# version of this gate did, is a QEMU command line with two of them.
run_profile riscv64 aia 1 "IMSIC S-mode files from the device tree"
run_profile riscv64 aia 4 "IMSIC S-mode files from the device tree"

echo "arch-profiles: every named profile booted, named the controller it has, delivered timer interrupts and brought up every declared core"
