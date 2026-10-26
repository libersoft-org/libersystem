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

STATE_REFUSED=75

# The profile a run builds and boots in: `development` when `LIBER_DEVELOPMENT=1`, `shipping` otherwise.
run_profile() {
	if [[ "${LIBER_DEVELOPMENT:-0}" == 1 ]]; then echo development; else echo shipping; fi
}

# The command that builds an architecture's staging in a profile.
staging_build_command() {
	local arch="$1" profile="$2"
	if [[ "$profile" == development ]]; then
		echo "LIBER_DEVELOPMENT=1 ./build.sh --arch $arch"
	else
		echo "env -u LIBER_DEVELOPMENT ./build.sh --arch $arch"
	fi
}

# Record that PARTs of ARCH's staging were just built in this run's profile, beside `build.sh`'s
# per-part source stamps: `.build/state/built-ARCH-PART.profile`.
record_staging_profile() {
	local build_dir="$1" arch="$2" profile part
	shift 2
	profile="$(run_profile)"
	mkdir -p "$build_dir/state"
	for part in "$@"; do
		printf '%s\n' "$profile" >"$build_dir/state/built-$arch-$part.profile.$$"
		mv "$build_dir/state/built-$arch-$part.profile.$$" "$build_dir/state/built-$arch-$part.profile"
	done
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
		return 0
	done
	return 1
}

# Print why the bootable volume VOLUME cannot be paired with something that needs DMA mode MODE, and
# succeed, when its record names another mode or there is none; print nothing and fail when it agrees.
volume_mode_refusal() {
	local volume="$1" want="$2" arch="$3" got
	got="$(cat "${volume%.img}.dma-mode" 2>/dev/null || true)"
	[[ "$got" == "$want" ]] && return 1
	if [[ -z "$got" ]]; then
		echo "REFUSED: the bootable volume $volume carries no DMA-mode record, and this needs one signed for '$want' - build it:  ./build.sh --arch $arch --kernel-on-volume --dma-mode $want --part volume"
	else
		echo "REFUSED: the bootable volume was signed for DMA mode '$got', and this needs '$want' - build it:  ./build.sh --arch $arch --kernel-on-volume --dma-mode $want --part volume"
	fi
	return 0
}
