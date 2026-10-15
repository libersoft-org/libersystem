#!/bin/bash
# The portable static target: compile the pinned loader configuration and archive it, for one target.
#
# WHY A SCRIPT AND NOT CMAKE'S OWN STATIC OPTION. Upstream's `BUILD_STATIC_LOADER` was renamed
# `APPLE_STATIC_LOADER` precisely because the name implied a portability it never had, and non-Apple
# use is made to fail. None of this system's three targets can obtain a static loader from it, so the
# portable static target is this milestone's own deliverable.
#
# WHAT "DONE" MEANS HERE, AND WHAT IT DOES NOT. An archive does not resolve its external symbols, so
# this proves COMPILATION and ARCHIVING and claims nothing about linking. The strict converging link
# needs the profile sysroot, the foreign-ABI substrate and the platform port - all of which come
# after pass 1 - and it belongs to pass 2, where the derived pin already records it.
#
# THE UNDEFINED SYMBOLS IN THE ARCHIVE ARE THE DELIVERABLE'S OTHER HALF. They are the candidate
# surface pass 1 measures: what this configuration asks of a system, stated by the objects rather
# than guessed from the sources.
#
# THE LOCKFILE IS WHAT THIS READS, AND WHAT IT COMPILES IS DERIVED FROM IT (2026-10-15). Until then the
# lockfile was checked against the tree and the build read none of it: the trees under
# `.build/foreign` had been unpacked and patched once, by hand, so an edit made directly in one moved
# the evidence with every pin still green, and an edit to a pinned patch moved nothing until somebody
# re-applied it. Now, on every run:
#
#   THE TREES ARE DERIVED     from the upstream archives, each checked against the digest the
#                               bootstrap part pins, plus the patch series the static-target and
#                               derived parts pin, in their order, each checked against its digest.
#                               A tree that differs from that derivation is replaced by it and the
#                               replacement is reported; nothing compiles a tree nobody can reproduce.
#   THE TOOLS ARE THE PINNED  the compiler and the archiver are the ones the bootstrap part names, by
#     ONES                      the version line each prints.
#   THE BUILD IS THE PINNED   optimisation, assertions and debug information come from the bootstrap
#     ONE                       part's [build], and the sources are upstream's own list for this
#                               platform, read out of the pinned CMakeLists rather than restated.
#   THE OPTIONS HOLD          every pinned option exists in the pinned revision or is recorded absent,
#                               and each one's compile-time effect is present exactly when it is ON;
#                               the vendored generated sources match their pinned digest; and the cross
#                               file for this target describes the ABI this compile is given.
#
# Any of those failing stops the build before it compiles anything.

set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
cd "$ROOT"

usage() {
	cat >&2 <<EOF
usage: build-foreign-static.sh --arch ARCH [--out DIR] [--sysroot WHICH] [--ported] [--build-type TYPE]
       build-foreign-static.sh --arch ARCH --check-lockfile [--ported]

  --arch ARCH         x86_64 | aarch64 | riscv64
  --out DIR           where the objects and the archive go (default: .build/foreign/ARCH)
  --sysroot WHICH     bootstrap (default) | profile
  --ported            compile the PLATFORM-PORTED sources instead of the pinned ones
  --build-type TYPE   pinned (default) | debug | release - the two CMake types the lockfile's
                      [build] is measured against; anything but the default is a measurement
  --check-lockfile    derive the trees and check this configuration against the lockfile, and stop

THE PORTED TREE IS A COPY AND THE PINNED ONE IS NEVER EDITED. Pass 2 compiles the loader with this
milestone's platform port applied; the static-target pin froze the archives WITHOUT it, and both have
to stay reproducible - so the port lives in a copy under .build/foreign/src-loader-port and this
flag says which of the two is being built.

THE TWO SYSROOTS ARE NOT ALTERNATIVES AND THE DEFAULT IS NOT A PREFERENCE. The BOOTSTRAP one is what
the archives in the static-target pin were built against, so reproducing those digests has to name
it. The PROFILE one is sized by the inventory instead of by the option set, and building against it
is how the trim is PROVED to have removed only surface nobody required: the objects are the same
objects, or a declaration that was removed was one the configuration actually needed.

Needs the two pinned upstream archives under .build/foreign, placed there once, by a person; every
tree compiled is derived from them. The build FETCHES NOTHING: see docs/DEPENDENCY_POLICY.md.
EOF
	exit 2
}

arch=""
out=""
sysroot="bootstrap"
ported=0
build_type="pinned"
check_only=0
while [[ $# -gt 0 ]]; do
	case "$1" in
	--arch)
		[[ $# -ge 2 ]] || usage
		arch="$2"
		shift 2
		;;
	--out)
		[[ $# -ge 2 ]] || usage
		out="$2"
		shift 2
		;;
	--sysroot)
		[[ $# -ge 2 ]] || usage
		sysroot="$2"
		shift 2
		;;
	--ported)
		ported=1
		shift
		;;
	--build-type)
		[[ $# -ge 2 ]] || usage
		build_type="$2"
		shift 2
		;;
	--check-lockfile)
		check_only=1
		shift
		;;
	-h | --help) usage ;;
	*) usage ;;
	esac
done
[[ -n "$arch" ]] || usage
out="${out:-$ROOT/.build/foreign/$arch}"

fail() {
	echo "build-foreign-static: $*" >&2
	exit 1
}

STORE="$ROOT/.build/foreign"
LOADER="$STORE/src-loader"
tree="pinned"
if [[ "$ported" == 1 ]]; then
	LOADER="$STORE/src-loader-port"
	tree="ported"
fi
HEADERS="$STORE/src-headers"

# 1. THE LOCKFILE, READ. Every pinned value below comes from the part that owns it: the archives, the
# tools and the build from the bootstrap part, the static target's patch from its own part, and the
# ordered series the ported tree is built from from the derived part.
pinned="$(
	python3 - "$ROOT" <<'PY'
import pathlib
import shlex
import sys
import tomllib

root = pathlib.Path(sys.argv[1])
foreign = root / "src/foreign"
bootstrap = tomllib.load(open(foreign / "PIN-bootstrap.toml", "rb"))
static = tomllib.load(open(foreign / "PIN-static-target.toml", "rb"))
derived = tomllib.load(open(foreign / "PIN-derived.toml", "rb"))


def emit(name, value):
	if isinstance(value, list):
		print(f"{name}=(" + " ".join(shlex.quote(str(item)) for item in value) + ")")
	else:
		print(f"{name}={shlex.quote(str(value))}")


for key, name in (("vulkan-loader", "loader"), ("vulkan-headers", "headers")):
	entry = bootstrap["upstream"][key]
	emit(f"{name}_archive", root / ".build/foreign" / f"{key}-{entry['revision']}.tar.gz")
	emit(f"{name}_archive_sha256", entry["archive_sha256"])
# THE PINNED TREE CARRIES THE STATIC-TARGET PART'S ONE PATCH; THE PORTED TREE THE DERIVED SERIES, IN
# ITS ORDER, which names that same patch first.
emit("pinned_series", [static["patch"]["path"]])
emit("pinned_series_sha256", [static["patch"]["sha256"]])
series = sorted(derived["patches"]["series"], key=lambda entry: entry["order"])
emit("ported_series", [entry["path"] for entry in series])
emit("ported_series_sha256", [entry["sha256"] for entry in series])
toolchain = bootstrap["toolchain"]
for key in ("compiler", "compiler_version", "archiver", "archiver_version"):
	emit(f"pinned_{key}", toolchain[key])
for key in ("optimisation", "assertions", "debug_info"):
	emit(f"pinned_{key}", bootstrap["build"][key])
PY
)" || fail "the lockfile could not be read"
eval "$pinned"

first_line() {
	local text="$1"
	printf '%s\n' "${text%%$'\n'*}"
}

# 2. THE TOOLS ARE THE PINNED ONES. A version line is what a person reads and what the identity
# record carries; a different one is a different compiler, whose code generation the evidence was
# not taken with.
compiler="$pinned_compiler"
archiver="$pinned_archiver"
[[ "$(first_line "$("$compiler" --version)")" == "$pinned_compiler_version" ]] || fail "$compiler is '$(first_line "$("$compiler" --version)")' and the lockfile pins '$pinned_compiler_version'"
[[ "$(first_line "$("$archiver" --version)")" == "$pinned_archiver_version" ]] || fail "$archiver is '$(first_line "$("$archiver" --version)")' and the lockfile pins '$pinned_archiver_version'"

# 3. THE TREES ARE DERIVED FROM THE PINNED INPUTS - all three, on every run.
verify_series() {
	local -n paths="$1" digests="$2"
	local index actual
	for index in "${!paths[@]}"; do
		[[ -f "$ROOT/${paths[$index]}" ]] || fail "${paths[$index]} is pinned and missing"
		actual="$(sha256sum "$ROOT/${paths[$index]}" | cut -d' ' -f1)"
		[[ "$actual" == "${digests[$index]}" ]] || fail "${paths[$index]} is not the patch the lockfile pins ($actual) - a changed patch is a pin change, and goes through the freeze order"
	done
}

apply_series() {
	local into="$1" patch
	shift
	for patch in "$@"; do
		# NO FUZZ, NO BACKUP, NOT TWICE. A patch that applies only approximately is a source tree
		# nobody reviewed, and a backup file is a file the tree digest would then have to explain.
		patch --quiet --forward --fuzz=0 --no-backup-if-mismatch -p1 -d "$into" -i "$ROOT/$patch" >&2 || fail "$patch does not apply exactly to the pinned revision"
	done
}

derive_trees() {
	local archive digest actual stage name
	for archive in loader headers; do
		local -n path="${archive}_archive" pin="${archive}_archive_sha256"
		[[ -f "$path" ]] || fail "$path is missing - the pinned archive is placed there once, by a person; the build fetches nothing (see docs/DEPENDENCY_POLICY.md)"
		actual="$(sha256sum "$path" | cut -d' ' -f1)"
		[[ "$actual" == "$pin" ]] || fail "$path is not the archive the lockfile pins ($actual)"
		unset -n path pin
	done
	verify_series pinned_series pinned_series_sha256
	verify_series ported_series ported_series_sha256

	# ONE DERIVATION AT A TIME, AND NO TREE REPLACED UNDER A BUILD COMPILING IT. Exclusive while the
	# trees are derived and compared, shared from then until this build exits.
	mkdir -p "$STORE"
	exec 9>"$STORE/.trees.lock"
	flock 9
	stage="$(mktemp -d "$STORE/.derive.XXXXXX")"
	mkdir "$stage/src-headers" "$stage/src-loader"
	tar -xzf "$headers_archive" -C "$stage/src-headers" --strip-components=1
	tar -xzf "$loader_archive" -C "$stage/src-loader" --strip-components=1
	cp -a "$stage/src-loader" "$stage/src-loader-port"
	apply_series "$stage/src-loader" "${pinned_series[@]}"
	apply_series "$stage/src-loader-port" "${ported_series[@]}"
	for name in src-headers src-loader src-loader-port; do
		if [[ -d "$STORE/$name" ]] && diff -rq "$stage/$name" "$STORE/$name" >/dev/null 2>&1; then
			continue
		fi
		# THE TREES ARE COMPILED WHERE THEY ALWAYS WERE, because they have to be: an assertion
		# records its source file's path in the object, so the archive digests the lockfile holds
		# are digests of a compile at exactly these paths.
		if [[ -d "$STORE/$name" ]]; then
			echo "build-foreign-static: .build/foreign/$name was not what the pinned archive and patch series produce - replaced by the derivation" >&2
			mv "$STORE/$name" "$stage/replaced-$name"
		fi
		mv "$stage/$name" "$STORE/$name"
	done
	rm -rf "$stage"
	flock -s 9
}
derive_trees

# 4. THE CONFIGURATION THIS RUN COMPILES.
#
# THE MECHANISMS THIS PROFILE FORBIDS, STATED AS FLAGS AND NOT ONLY AS A CHECK. The inventory names
# no exception table, no unwind symbol and no RTTI, so the answer for each is FORBIDDEN - and a
# forbidden mechanism gets the compiler flags that stop it being emitted as well as the artifact
# check that catches it if it ever is. The flags are what makes the absence a decision; the check is
# what makes it stay one.
#
# THEY APPLY TO THE PORTED TREE ONLY, and that is the freeze order rather than a hedge: the
# static-target part froze three archive digests for the PINNED tree before pass 1, and a flag added
# to that build now would move values a part that precedes this decision already holds. The ported
# tree is the derived part's, which is where a derived decision belongs.
#
# `-fno-rtti` IS NOT HERE because clang refuses it for a C compile. The pinned production
# configuration is C99; the RTTI answer is the artifact check, which is the half that would catch a
# C++ object appearing.
forbid=()
if [[ "$ported" == 1 ]]; then
	forbid=(-fno-exceptions -fno-unwind-tables -fno-asynchronous-unwind-tables)
fi

# THE BUILD THE LOCKFILE PINS, or one of the two CMake types it is measured against - with CMake's own
# flags for each, so the measurement is of what those names mean upstream.
case "$build_type" in
pinned)
	optimise=("$pinned_optimisation")
	case "$pinned_assertions" in
	live) ;;
	off) optimise+=(-DNDEBUG) ;;
	*) fail "the lockfile's [build] assertions is '$pinned_assertions' - live or off" ;;
	esac
	case "$pinned_debug_info" in
	none) ;;
	full) optimise+=(-g) ;;
	*) fail "the lockfile's [build] debug_info is '$pinned_debug_info' - none or full" ;;
	esac
	;;
debug) optimise=(-O0 -g) ;;
release) optimise=(-O3 -DNDEBUG) ;;
*) fail "unknown build type '$build_type' - pinned, debug or release" ;;
esac

case "$sysroot" in
bootstrap)
	include=("-isystem" "$ROOT/src/foreign/bootstrap-sysroot/include")
	;;
profile)
	# THE TARGET HEADER IS FORCE-INCLUDED AND NOT LEFT TO A SOURCE TO INCLUDE. Its static assertions
	# are the data model, and a model asserted in one translation unit describes one translation
	# unit; this way every object carries the check.
	include=("-isystem" "$ROOT/src/foreign/profile-sysroot/include" "-include" "$ROOT/src/foreign/profile-sysroot/arch/$arch.h")
	;;
*)
	fail "unknown sysroot '$sysroot' - bootstrap or profile"
	;;
esac

# THE ABI COMES FROM THE SAME PLACE THE RUST SIDE'S DOES, and from the same place the image build's
# does. It was written out here until the image build needed it too; a third copy of one ABI is how a
# C object and a Rust object come to disagree about a type, so it moved to one file both read.
# shellcheck source=../foreign/profile-abi.sh
source "$ROOT/src/foreign/profile-abi.sh"
foreign_profile_abi "$arch" || exit 2
target="$foreign_triple"
abi=("${foreign_abi_flags[@]}")

# Every flag that is neither a define nor a path, in the order the compiler is given them.
cflags=("--target=$target" -ffreestanding -nostdlibinc -fPIC "${optimise[@]}" "${forbid[@]}" "${abi[@]}")

# THE DEFINES THE PINNED OPTION SET AND THE PLATFORM COME TO. Every WSI backend off because
# LiberSystem is none of those window systems; assembly off because per-architecture dispatch stubs
# are exactly the relocation surface this milestone measures; the unsafe file search off because the
# ambient search is what the discovery item replaces - so none of their defines is here, and the
# lockfile check below holds that both ways.
defines=(
	-D__LiberSystem__
	-DHAVE_ALLOCA_H
	-DVK_ENABLE_BETA_EXTENSIONS
	# THE QUOTES HAVE TO SURVIVE INTO THE COMPILER, which bare shell quoting does not do: the shell
	# strips them and the source then sees a bare path where it expects a string literal.
	#
	# THESE ARE THE PINNED TREE'S VALUES AND THEY ARE WHAT UPSTREAM'S CMAKE WOULD SET. The ported
	# tree overrides them below: what a configuration directory MEANS on this system is the discovery
	# item's answer, and giving it is part of the port rather than of the option set pass 1 measured.
	-DFALLBACK_CONFIG_DIRS='"/etc/xdg"'
	-DFALLBACK_DATA_DIRS='"/usr/local/share:/usr/share"'
	-DSYSCONFDIR='"/etc"'
	# LOADER_ENABLE_LINUX_SORT IS NOT DEFINED AT ALL, and `=0` was wrong: the source guards it with
	# `#if defined(...)`, so defining it to zero switched it ON. The inventory is what caught it - it
	# named `linux_read_sorted_physical_devices` and `linux_sort_physical_device_groups`, which live
	# in `loader_linux.c`, a source this configuration does not compile. A candidate surface holding
	# two symbols no object can ever provide is the inventory describing a build nobody made.
)

# WHERE A DRIVER MANIFEST LIVES ON THIS SYSTEM, which is the discovery item's answer and is given
# here because the loader reads it as a compiled-in string. The pinned tree keeps upstream's own
# values; the ported tree gets this system's, so the paths the loader composes are the paths the
# substrate's bounded record answers for. Without this the loader searched `/usr/share/vulkan/icd.d`,
# the record held `vol://system/share/vulkan/icd.d`, and the two never met - the enumeration
# succeeded and reached no driver at all, which is the quietest way a discovery model can be wrong.
#
# AND THEY CARRY NO SCHEME, WHICH IS NOT COSMETIC. These values are SEARCH LISTS and the loader
# splits them on `:`, so `vol://system/share` becomes `vol` and `//system/share` - two directories
# that exist nowhere. The record's paths reach no file system either way; what they have to be is
# spellings the loader can compose and compare, and an absolute one with no colon is that.
if [[ "$ported" == 1 ]]; then
	defines+=(
		-U FALLBACK_CONFIG_DIRS -U FALLBACK_DATA_DIRS -U SYSCONFDIR
		-DFALLBACK_CONFIG_DIRS='"/system/etc/xdg"'
		-DFALLBACK_DATA_DIRS='"/system/share"'
		-DSYSCONFDIR='"/system/etc"'
	)
fi

# THE SOURCES ARE UPSTREAM'S OWN LIST, read out of the pinned CMakeLists rather than restated here. On
# this platform that list is all of it: the Linux and Windows sources are appended only on those
# systems, and the assembly ones only with an assembler, which the pinned option set turns off.
mapfile -t sources < <(sed -n '/^set(NORMAL_LOADER_SRCS$/,/^    )$/p' "$LOADER/loader/CMakeLists.txt" | sed -n 's/^[[:space:]]*\([A-Za-z0-9_]*\)\.c$/\1/p')
[[ "${#sources[@]}" -gt 0 ]] || fail "the pinned loader/CMakeLists.txt names no NORMAL_LOADER_SRCS - the source list cannot be read"

# 5. THE CONFIGURATION AGAINST THE LOCKFILE, before anything is compiled.
python3 - "$ROOT" "$LOADER" "$arch" "$build_type" "$(printf '%s\n' "${cflags[@]}")" "$(printf '%s\n' "${defines[@]}")" "$(printf '%s\n' "${sources[@]}")" <<'PY' || exit 1
import hashlib
import pathlib
import re
import sys
import tomllib

root, loader, arch, build_type = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3], sys.argv[4]
cflags, defines, sources = (argument.split("\n") for argument in sys.argv[5:8])
bootstrap = tomllib.load(open(root / "src/foreign/PIN-bootstrap.toml", "rb"))
static = tomllib.load(open(root / "src/foreign/PIN-static-target.toml", "rb"))
failures = []

# WHAT EACH PINNED OPTION DOES IN THE PINNED REVISION, when it is ON. Read from that revision's own
# CMake and written down here because this build has no CMake to ask. `None` is an option with no
# compile-time effect on this platform - Windows-only, or not a compile at all - named so that its
# absence from the build is a decision rather than an oversight. A pinned option missing from this
# table is one nobody has said the effect of, and that fails.
EFFECTS = {
	"BUILD_TESTS": ("define", "SHOULD_EXPORT_TEST_FUNCTIONS"),
	"BUILD_WSI_XCB_SUPPORT": ("define", "VK_USE_PLATFORM_XCB_KHR"),
	"BUILD_WSI_XLIB_SUPPORT": ("define", "VK_USE_PLATFORM_XLIB_KHR"),
	"BUILD_WSI_WAYLAND_SUPPORT": ("define", "VK_USE_PLATFORM_WAYLAND_KHR"),
	"BUILD_WSI_DIRECTFB_SUPPORT": ("define", "VK_USE_PLATFORM_DIRECTFB_EXT"),
	"LOADER_ENABLE_ADDRESS_SANITIZER": ("flag", "-fsanitize=address,undefined"),
	"LOADER_ENABLE_THREAD_SANITIZER": ("flag", "-fsanitize=thread"),
	"BUILD_WERROR": ("flag", "-Werror"),
	"USE_GAS": ("define", "UNKNOWN_FUNCTIONS_SUPPORTED"),
	"USE_MASM": ("define", "UNKNOWN_FUNCTIONS_SUPPORTED"),
	"LOADER_USE_UNSAFE_FILE_SEARCH": ("define", "LOADER_USE_UNSAFE_FILE_SEARCH"),
	"ENABLE_WIN10_ONECORE": None,
	"LOADER_CODEGEN": None,
	"CMAKE_INSTALL_PREFIX": None,
}

cmake = "\n".join(path.read_text(errors="replace") for path in sorted(loader.rglob("*")) if path.is_file() and (path.name == "CMakeLists.txt" or path.suffix == ".cmake"))
defined = {re.sub(r"^-D", "", define).split("=")[0] for define in defines if define.startswith("-D")}
for option, value in bootstrap["options"].items():
	if isinstance(value, dict):
		continue
	exists = option.startswith("CMAKE_") or re.search(rf"\b{re.escape(option)}\b", cmake) is not None
	if value == "absent":
		if exists:
			failures.append(f"{option} is recorded absent and the pinned revision has it - give it the value this configuration takes")
		continue
	if not exists:
		failures.append(f"{option} is pinned {value!r} and the pinned revision has no such option - record it \"absent\" rather than guess")
	if option not in EFFECTS:
		failures.append(f"{option} is pinned and this build does not know what it does - say so in the builder's table of effects")
		continue
	effect = EFFECTS[option]
	if effect is None:
		continue
	kind, token = effect
	if token not in cmake:
		failures.append(f"{option}'s effect {token} does not appear in the pinned revision's CMake - the table of effects is wrong")
	present = token in defined if kind == "define" else token in cflags
	if value not in ("ON", "OFF"):
		failures.append(f"{option} is pinned {value!r} - an option with an effect is ON or OFF")
	elif present != (value == "ON"):
		failures.append(f"{option} is pinned {value} and this build {'carries' if present else 'lacks'} its {kind} {token}")

# THE BUILD ITSELF, when it is the pinned one. A measurement build is a different build by intent.
if build_type == "pinned":
	build = bootstrap["build"]
	optimisations = [flag for flag in cflags if re.fullmatch(r"-O[0-9sz]?", flag)]
	if optimisations != [build["optimisation"]]:
		failures.append(f"the build optimises with {optimisations} and the lockfile pins {build['optimisation']}")
	if ("NDEBUG" in defined or "-DNDEBUG" in cflags) != (build["assertions"] == "off"):
		failures.append(f"assertions are {build['assertions']} in the lockfile and this build {'defines' if 'NDEBUG' in defined or '-DNDEBUG' in cflags else 'does not define'} NDEBUG")
	if ("-g" in cflags) != (build["debug_info"] == "full"):
		failures.append(f"debug information is {build['debug_info']} in the lockfile and this build {'emits' if '-g' in cflags else 'omits'} it")

# THE SOURCES. Upstream's list is read rather than restated, so what is checked is that it is the
# list the static-target part's archives were built from.
if any(not re.fullmatch(r"[A-Za-z0-9_]+", source) for source in sources):
	failures.append(f"the source list is not a list of C sources: {sources}")
for target, archive in static["archives"].items():
	if archive["objects"] != len(sources):
		failures.append(f"{target}: the static-target part froze {archive['objects']} objects and upstream's list is {len(sources)} sources")
		break

# THE GENERATED SOURCES ARE THE VENDORED ONES, by the digest the bootstrap part pins.
generated = bootstrap["generated_sources"]
directory = loader / generated["path"]
files = sorted(str(path.relative_to(directory)) for path in directory.rglob("*") if path.is_file())
if files != sorted(generated["files"]):
	failures.append(f"{generated['path']} holds {files} and the lockfile pins {sorted(generated['files'])}")
inner = hashlib.sha256()
for path in sorted(path for path in directory.rglob("*") if path.is_file()):
	inner.update(hashlib.sha256(path.read_bytes()).hexdigest().encode())
	inner.update(b"  ./" + str(path.relative_to(directory)).encode() + b"\n")
if inner.hexdigest() != generated["digest"]:
	failures.append(f"the generated sources are not the pinned ones ({inner.hexdigest()[:12]} is not {generated['digest'][:12]})")

# THE CROSS FILE DESCRIBES THE ABI THIS COMPILE IS GIVEN. No build runs CMake; what keeps the pinned
# cross file from being a second, drifting description of the ABI is that it must say exactly this.
cross = bootstrap["cross_files"][arch]
text = (root / cross["path"]).read_text()
described = re.findall(r'"(--target=[^"]+|-m[^"]+)"', text)
given = [flag for flag in cflags if flag.startswith("--target=") or flag.startswith("-m")]
if described != given:
	failures.append(f"{cross['path']} describes {described} and this compile is given {given}")
if f"--target={cross['target']}" not in given:
	failures.append(f"the lockfile pins {arch} to {cross['target']} and this compile targets {given[:1]}")

for failure in failures:
	print(f"build-foreign-static: lockfile: {failure}", file=sys.stderr)
raise SystemExit(1 if failures else 0)
PY

if [[ "$check_only" == 1 ]]; then
	echo "build-foreign-static: $arch: the $tree tree is the derivation of the pinned inputs, and this configuration is the lockfile's"
	exit 0
fi

# 6. COMPILE AND ARCHIVE.
rm -rf "$out"
mkdir -p "$out"
objects=()
for name in "${sources[@]}"; do
	object="$out/$name.o"
	"$compiler" \
		"${cflags[@]}" "${defines[@]}" \
		"${include[@]}" \
		-I "$LOADER/loader" -I "$LOADER/loader/generated" -I "$HEADERS/include" \
		-c "$LOADER/loader/$name.c" -o "$object"
	objects+=("$object")
done

archive="$out/libvulkan.a"
rm -f "$archive"
# `D` FOR DETERMINISTIC: zero timestamps, uids and modes in the archive members. Without it two
# archives of identical objects differ, and "builds reproducibly" becomes unprovable.
"$archiver" crsD "$archive" "${objects[@]}"

# 7. WHAT WAS COMPILED, WRITTEN DOWN BESIDE IT. The audit link takes the identity record's flags and its
# configure-input digest from this file rather than describing them in prose, and the artifact gate
# recomputes them from it - so a record cannot say one configuration while the archive is another.
# No path is in it: where the tree lives is not what was configured.
if [[ "$tree" == ported ]]; then
	series=("${ported_series[@]}")
	series_digests=("${ported_series_sha256[@]}")
else
	series=("${pinned_series[@]}")
	series_digests=("${pinned_series_sha256[@]}")
fi
{
	printf 'target=%s\n' "$target"
	printf 'tree=%s\n' "$tree"
	printf 'series='
	for index in "${!series[@]}"; do
		printf '%s%s:%s' "$([[ "$index" == 0 ]] || printf ' ')" "$(basename "${series[$index]}")" "${series_digests[$index]}"
	done
	printf '\n'
	printf 'sysroot=%s\n' "$sysroot"
	printf 'build-type=%s\n' "$build_type"
	printf 'cflags=%s\n' "${cflags[*]}"
	printf 'defines=%s\n' "${defines[*]}"
	printf 'sources=%s\n' "${sources[*]}"
} >"$out/configuration"

echo "build-foreign-static: $arch: ${#objects[@]} object(s), $sysroot sysroot -> $archive"
echo "build-foreign-static: $arch: $(sha256sum "$archive" | cut -d' ' -f1)"
