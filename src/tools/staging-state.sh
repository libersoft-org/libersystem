#!/usr/bin/env bash
# What state an architecture's staging is in - which profile each part was built in - and how a step
# that needs another state refuses.
#
# THE PROFILE WAS SWITCHED UNDER STEPS THAT NEVER ASKED FOR IT. A gate that comes up through the
# development profile (`dev.sh up`, `lab.sh boot`, a cold run, a development image) rebuilds the shared
# staging in it, and the next step that reads the staging in the shipping profile failed for no fault
# of the code: the x86_64 suite on "system volume contains an undeclared entry
# drivers/acpi_fixture.lsexe" an hour into its run, `development-gate` on "not the shipping
# configuration". So `build.sh` records the profile of every part it builds, and a step that boots the
# staging refuses a part built in another profile AT ONCE, naming the part, the profile it needs and
# the command that fixes it.
#
# A REFUSAL IS NOT A FAILURE. It exits with `STATE_REFUSED`, a status of its own, before the step has
# tested anything: no envelope is published for it and no history is recorded, so the step's outcome
# is that of its next run, in the state it needs. The DMA mode of a bootable volume refuses the same
# way where a volume is paired with a medium or a guest (`mkimage.sh`, `qemu-run.sh`).
#
# AND IT SAYS HOW IT IS FIXED, to a runner that asks: under `verify.sh` every step is given a file in
# `LIBER_STATE_REFUSAL`, each refusal appends the command that brings the staging to the state it
# needs, and the runner runs those commands, alone, before it runs the step once more. Run by hand,
# the command is in the refusal's own line.

STATE_REFUSED=75

# The profile a run builds and boots in: `development` when `LIBER_DEVELOPMENT=1`, `shipping` otherwise.
run_profile() {
	if [[ "${LIBER_DEVELOPMENT:-0}" == 1 ]]; then echo development; else echo shipping; fi
}

# How a build is run in a profile, whatever profile its caller is in.
profile_env() {
	if [[ "$1" == development ]]; then echo "LIBER_DEVELOPMENT=1"; else echo "env -u LIBER_DEVELOPMENT"; fi
}

# The command that builds an architecture's staging in a profile.
staging_build_command() {
	echo "$(profile_env "$2") ./build.sh --arch $1"
}

# The command that builds the bootable volume of an architecture, signed for a DMA mode, in this run's profile.
volume_build_command() {
	echo "$(profile_env "$(run_profile)") ./build.sh --arch $1 --kernel-on-volume --dma-mode $2 --part volume"
}

# Hand the command that fixes a refusal to the runner that asked for it (`LIBER_STATE_REFUSAL`).
state_refusal_fix() {
	[[ -n "${LIBER_STATE_REFUSAL:-}" ]] || return 0
	printf '%s\n' "$1" >>"$LIBER_STATE_REFUSAL"
}

# Record that PARTs of ARCH's staging were just built in this run's profile, beside `build.sh`'s
# per-part source stamps: `.build/state/built-ARCH-PART.profile`.
#
# A CHANGE OF PROFILE IS SAID, because a switch is a rebuild of the staging and the steps around it
# pay for it: "the profile is switched back and forth" was found by reading build logs, not by being
# told.
record_staging_profile() {
	local build_dir="$1" arch="$2" profile part previous switched=()
	shift 2
	profile="$(run_profile)"
	mkdir -p "$build_dir/state"
	for part in "$@"; do
		previous="$(cat "$build_dir/state/built-$arch-$part.profile" 2>/dev/null || true)"
		[[ -z "$previous" || "$previous" == "$profile" ]] || switched+=("$part")
		printf '%s\n' "$profile" >"$build_dir/state/built-$arch-$part.profile.$$"
		mv "$build_dir/state/built-$arch-$part.profile.$$" "$build_dir/state/built-$arch-$part.profile"
	done
	((${#switched[@]} == 0)) || echo "staging: the $arch staging switched to the $profile profile (${switched[*]})" >&2
}

# The profile every part of ARCH's staging was built in - nothing when the parts disagree or one carries no record.
# What the runner compares with the profile a step declares (`verify.sh`).
staging_profile_of() {
	local build_dir="$1" arch="$2" part got="" one
	for part in libs user packages volume-test; do
		one="$(cat "$build_dir/state/built-$arch-$part.profile" 2>/dev/null || true)"
		[[ -n "$one" && (-z "$got" || "$got" == "$one") ]] || return 0
		got="$one"
	done
	echo "$got"
}

# The state of ARCH's bootable volume, `PROFILE/MODE` - nothing when either record is missing.
volume_state_of() {
	local build_dir="$1" arch="$2" profile mode
	profile="$(cat "$build_dir/state/built-$arch-volume.profile" 2>/dev/null || true)"
	mode="$(cat "$build_dir/boot/system-volume-bootable-$arch.dma-mode" 2>/dev/null || true)"
	[[ -z "$profile" || -z "$mode" ]] || echo "$profile/$mode"
}

# Print why ARCH's staging cannot serve a step that needs PROFILE, and succeed, when one of PARTs was
# built in another profile or carries no record; print nothing and fail when every part is in it.
staging_profile_refusal() {
	local build_dir="$1" arch="$2" want="$3" part got
	shift 3
	for part in "$@"; do
		got="$(cat "$build_dir/state/built-$arch-$part.profile" 2>/dev/null || true)"
		[[ "$got" == "$want" ]] && continue
		if [[ -n "$got" ]]; then
			echo "REFUSED: the $arch staging's '$part' was built in the $got profile, and this step needs the $want one - build it:  $(staging_build_command "$arch" "$want")"
		else
			echo "REFUSED: the $arch staging's '$part' carries no profile record, and this step needs the $want one - build it:  $(staging_build_command "$arch" "$want")"
		fi
		state_refusal_fix "$(staging_build_command "$arch" "$want")"
		return 0
	done
	return 1
}

# Print why ARCH's staging is not current for a boot in PROFILE - a part not built from these sources, the kernel
# and the loader by their own stamps, or a part built in another profile - and, with MODE, why its bootable volume
# is not current signed for MODE; hand a runner that asks the commands that make it current (`state_refusal_fix`),
# staging first; and succeed. Print nothing and fail when all of it is current.
#
# WHAT A COLD RUN ASKS BEFORE IT BUILDS ANYTHING: it built the staging and the bootable volume every time, so two of
# one target started together each rebuilt what the other was booting. A current staging is booted as it stands.
staging_current_refusal() {
	local build_dir="$1" arch="$2" want="$3" mode="${4:-}" part digest got stale="" volume_digest kernel_digest
	volume_digest="$(source_digest "${VOLUME_SOURCES[@]}")"
	kernel_digest="$(source_digest "${KERNEL_SOURCES[@]}")"
	for part in libs user packages volume-test kernel loader; do
		case "$part" in
		kernel) digest="$kernel_digest" ;;
		loader) digest="$(source_digest "${LOADER_SOURCES[@]}")" ;;
		*) digest="$volume_digest" ;;
		esac
		if [[ "$(cat "$build_dir/state/built-$arch-$part" 2>/dev/null || true)" != "$digest" ]]; then
			stale="the $arch staging's '$part' is not built from these sources"
			break
		fi
		[[ "$part" == kernel || "$part" == loader ]] && continue
		got="$(cat "$build_dir/state/built-$arch-$part.profile" 2>/dev/null || true)"
		if [[ "$got" != "$want" ]]; then
			stale="the $arch staging's '$part' was built in the ${got:-unrecorded} profile, and this needs the $want one"
			break
		fi
	done
	if [[ -n "$stale" ]]; then
		echo "$stale - build it:  $(staging_build_command "$arch" "$want")"
		state_refusal_fix "$(staging_build_command "$arch" "$want")"
	fi
	if [[ -n "$mode" ]]; then
		local volume="$build_dir/boot/system-volume-bootable-$arch.img" why=""
		if [[ -n "$stale" || "$(cat "$build_dir/state/built-$arch-volume" 2>/dev/null || true)" != "$(source_digest "${VOLUME_SOURCES[@]}" "${KERNEL_SOURCES[@]}")" ]]; then
			why="the $arch bootable volume is not built from these sources"
		elif [[ "$(cat "$build_dir/state/built-$arch-volume.profile" 2>/dev/null || true)" != "$want" ]]; then
			why="the $arch bootable volume was not built in the $want profile"
		elif [[ "$(cat "${volume%.img}.dma-mode" 2>/dev/null || true)" != "$mode" ]]; then
			why="the $arch bootable volume is not signed for DMA mode '$mode'"
		fi
		if [[ -n "$why" ]]; then
			echo "$why - build it:  $(profile_env "$want") ./build.sh --arch $arch --kernel-on-volume --dma-mode $mode --part volume"
			state_refusal_fix "$(profile_env "$want") ./build.sh --arch $arch --kernel-on-volume --dma-mode $mode --part volume"
			stale="$why"
		fi
	fi
	[[ -n "$stale" ]]
}

# Print why the bootable volume VOLUME cannot be paired with something that needs DMA mode MODE, and
# succeed, when its record names another mode or there is none; print nothing and fail when it agrees.
volume_mode_refusal() {
	local volume="$1" want="$2" arch="$3" got
	got="$(cat "${volume%.img}.dma-mode" 2>/dev/null || true)"
	[[ "$got" == "$want" ]] && return 1
	if [[ -z "$got" ]]; then
		echo "REFUSED: the bootable volume $volume carries no DMA-mode record, and this needs one signed for '$want' - build it:  $(volume_build_command "$arch" "$want")"
	else
		echo "REFUSED: the bootable volume was signed for DMA mode '$got', and this needs '$want' - build it:  $(volume_build_command "$arch" "$want")"
	fi
	state_refusal_fix "$(volume_build_command "$arch" "$want")"
	return 0
}
