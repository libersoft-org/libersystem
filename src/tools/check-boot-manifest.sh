#!/usr/bin/env bash
# Every source the loader may boot from carries a manifest that names what the loader will read
# from it, with the digest of the bytes that are actually there.
#
# THE VERIFIER IS HOST-TESTED AND THE STAGING IS NOT, which is where both of this mechanism's real
# defects were: `bootproto::boot_manifest::verify` has covered the four cases since the day it was
# written, while the x86_64 boot medium carried no manifest at all (the loader refused it, at the
# loader, on every machine) and the kernel read from a boot medium was checked against nothing. A
# manifest that is correct about files nobody reads is not integrity - so this looks at the media
# and the staging sets this tree produces and asks the question the loader will ask.
#
# It is a gate rather than a boot test because it needs no guest: a FAT image, `mcopy` and
# `sha256sum` answer it in seconds, and the failure it catches is a staging step that was never
# written rather than a kernel that misbehaves.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BOOT="${BOOT_DIR:-$root/../.build/boot}"
MAGIC='liberboot-manifest 1'
failures=0
sources=0

fail() {
	echo "boot-manifest: $*" >&2
	failures=$((failures + 1))
}

die() {
	echo "boot-manifest: $*" >&2
	exit 1
}

# The digest a manifest records for `path`, or nothing when it has no row for it. The row is
# `<64 hex><two spaces><path>`, and the path is matched whole so `libexec/sh` cannot answer for
# `libexec/shell`.
manifest_digest() {
	local manifest="$1" path="$2"
	awk -v want="$path" '
		NR > 1 && substr($0, 65, 2) == "  " && substr($0, 67) == want { print substr($0, 1, 64); found = 1; exit }
		END { exit !found }
	' "$manifest"
}

# One file that a loader reading this source would read: it must have a row, and the row must be
# the digest of the bytes that are there.
check_file() {
	local what="$1" manifest="$2" path="$3" blob="$4" recorded actual
	if ! recorded="$(manifest_digest "$manifest" "$path")"; then
		fail "$what: etc/boot.manifest has no row for '$path', so the loader would refuse this source"
		return
	fi
	actual="$(sha256sum "$blob" | cut -d" " -f1)"
	if [[ "$recorded" != "$actual" ]]; then
		fail "$what: '$path' does not match its row in etc/boot.manifest ($actual, recorded $recorded)"
	fi
}

# The paths `etc/bootstrap.list` names. The format is `<name> <path>` per line, blank lines and
# `#` comments skipped - the same reading `abi::bootstrap::parse_list` does.
list_paths() {
	awk 'NF && substr($1, 1, 1) != "#" { print $2 }' "$1"
}

# A staging directory - the set the system volume carries. Its manifest is written by `mkpackages`
# and covers the list and every program the list names; the kernel is not here, so it is not asked
# about.
check_staging_dir() {
	local dir="$1"
	local what="staging set $(basename "$dir")"
	local manifest="$dir/etc/boot.manifest" list="$dir/etc/bootstrap.list"
	[[ -f "$list" ]] || return 0
	sources=$((sources + 1))
	if [[ ! -f "$manifest" ]]; then
		fail "$what: has etc/bootstrap.list and no etc/boot.manifest"
		return
	fi
	[[ "$(head -n1 "$manifest")" == "$MAGIC" ]] || fail "$what: etc/boot.manifest does not start with '$MAGIC'"
	check_file "$what" "$manifest" "etc/bootstrap.list" "$list"
	local path
	while read -r path; do
		[[ -n "$path" ]] || continue
		if [[ ! -f "$dir/$path" ]]; then
			fail "$what: etc/bootstrap.list names '$path', which is not staged"
			continue
		fi
		check_file "$what" "$manifest" "$path" "$dir/$path"
	done < <(list_paths "$list")
}

# A FAT boot medium. Its manifest must cover the kernel as well: this is an independently staged
# copy, and the loader reads it from here rather than through the volume's manifest.
check_fat_medium() {
	local image="$1"
	local what="boot medium $(basename "$image")"
	mdir -i "$image" ::/kernel >/dev/null 2>&1 || return 0
	sources=$((sources + 1))
	local tmp
	tmp="$(mktemp -d)"
	trap 'rm -rf "$tmp"' RETURN
	if ! mcopy -i "$image" ::/etc/boot.manifest "$tmp/boot.manifest" 2>/dev/null; then
		fail "$what: carries a kernel and no etc/boot.manifest - the loader refuses to boot from it"
		return
	fi
	[[ "$(head -n1 "$tmp/boot.manifest")" == "$MAGIC" ]] || fail "$what: etc/boot.manifest does not start with '$MAGIC'"
	mcopy -i "$image" ::/kernel "$tmp/kernel" 2>/dev/null || die "cannot read ::/kernel out of $image"
	check_file "$what" "$tmp/boot.manifest" "kernel" "$tmp/kernel"
	# The bootstrap set, when this medium carries one. A medium with a list is a medium the loader
	# will assemble from, so every program on it is read and every program must be recorded.
	if mcopy -i "$image" ::/etc/bootstrap.list "$tmp/bootstrap.list" 2>/dev/null; then
		check_file "$what" "$tmp/boot.manifest" "etc/bootstrap.list" "$tmp/bootstrap.list"
		local path
		while read -r path; do
			[[ -n "$path" ]] || continue
			if ! mcopy -i "$image" "::/$path" "$tmp/blob" 2>/dev/null; then
				fail "$what: etc/bootstrap.list names '$path', which is not on the medium"
				continue
			fi
			check_file "$what" "$tmp/boot.manifest" "$path" "$tmp/blob"
			rm -f "$tmp/blob"
		done < <(list_paths "$tmp/bootstrap.list")
	fi
}

# The gate against a staging set built here: one that agrees, one whose program was changed under
# the record, and one with no manifest at all. A checker that reports success without measuring its
# subject is the failure this project has already found four times in one harness.
self_test() {
	local scratch
	scratch="$(mktemp -d)"
	mkdir -p "$scratch/etc" "$scratch/libexec"
	printf 'shell libexec/shell\n' >"$scratch/etc/bootstrap.list"
	printf 'ELF...' >"$scratch/libexec/shell"
	{
		printf '%s\n' "$MAGIC"
		printf '%s  etc/bootstrap.list\n' "$(sha256sum "$scratch/etc/bootstrap.list" | cut -d" " -f1)"
		printf '%s  libexec/shell\n' "$(sha256sum "$scratch/libexec/shell" | cut -d" " -f1)"
	} >"$scratch/etc/boot.manifest"

	failures=0
	sources=0
	check_staging_dir "$scratch"
	if ((failures != 0 || sources != 1)); then
		echo "boot-manifest: SELF-TEST FAILED - a staging set that agrees was reported as broken" >&2
		exit 1
	fi

	printf 'ELF..!' >"$scratch/libexec/shell"
	failures=0
	check_staging_dir "$scratch" 2>/dev/null
	if ((failures == 0)); then
		echo "boot-manifest: SELF-TEST FAILED - a program changed under its digest was not caught" >&2
		exit 1
	fi

	rm -f "$scratch/etc/boot.manifest"
	failures=0
	check_staging_dir "$scratch" 2>/dev/null
	if ((failures == 0)); then
		echo "boot-manifest: SELF-TEST FAILED - a set with no manifest was not caught" >&2
		exit 1
	fi
	rm -rf "$scratch"
	failures=0
	sources=0
}

self_test

for dir in "$BOOT"/bootstrap-*; do
	[[ -d "$dir" ]] && check_staging_dir "$dir"
done
for image in "$BOOT"/efiboot.img "$BOOT"/esp.img; do
	[[ -f "$image" ]] && check_fat_medium "$image"
done

# A gate that examined nothing must not report success. The media are build outputs, so the honest
# failure is "run the build first" rather than a silent pass.
((sources > 0)) || die "no boot source to check - build an image first (\`./build.sh\` and \`./test.sh\` produce them under .build/boot)"

((failures == 0)) || die "$failures check(s) failed - a source the loader would refuse"
echo "boot-manifest: $sources boot source(s) match their etc/boot.manifest"
