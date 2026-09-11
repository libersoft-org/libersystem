# The aarch64 cross description for this milestone's pinned, AUDIT-ONLY configuration.
#
# WHY THIS FILE EXISTS AT ALL. Pinning compilers and flags does not give a C translation unit a
# target ABI or a place to find headers. Without a cross file CMake compiles for the HOST - and that
# failure is silent: the build succeeds against glibc's ABI on a hosted x86_64 Linux, and the symbol
# inventory derived from it describes a system this one is not. On this target it would not even be
# the right ARCHITECTURE.
#
# EVERY ABI VALUE HERE MATCHES WHAT THE RUST SIDE IS BUILT WITH, which for this target is
# `aarch64-unknown-none` with `-C relocation-model=pic` (see `src/tools/build-shared.sh`). Two
# descriptions of one ABI is how a C object and a Rust object end up disagreeing about a struct.
set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR aarch64)

set(LIBERSYSTEM_TARGET aarch64-unknown-none)
set(LIBERSYSTEM_SYSROOT "${CMAKE_CURRENT_LIST_DIR}/../bootstrap-sysroot")

set(CMAKE_C_COMPILER clang)
set(CMAKE_CXX_COMPILER clang++)
set(CMAKE_AR llvm-ar)
set(CMAKE_RANLIB llvm-ranlib)
set(CMAKE_LINKER ld.lld)

# TRY_COMPILE BUILDS A STATIC LIBRARY, NOT AN EXECUTABLE. There is no runtime to link against yet -
# that is the substrate this milestone has not built - so the default executable probe fails for a
# reason that says nothing about whether the compiler works.
set(CMAKE_TRY_COMPILE_TARGET_TYPE STATIC_LIBRARY)

# -nostdlibinc AND AN EXPLICIT INCLUDE PATH, in that order. Leaving the host include path reachable
# is the silent failure this file exists to prevent: a header the bootstrap sysroot does not have
# would be found in /usr/include, the compile would succeed, and the ABI measured would be the
# host's.
#
# `-nostdlibinc` AND NOT `-nostdinc`, and the difference is load-bearing. `-nostdinc` drops the
# COMPILER'S OWN resource headers as well - `stddef.h`, `stdint.h`, `stdarg.h`, `float.h`,
# `limits.h`, `stdbool.h` - and those describe the TARGET rather than the host. Dropping them forces
# this sysroot to restate the target's own type widths, which is a second description of the one
# ABI and exactly how a C object and a Rust object come to disagree about a type.
set(LIBERSYSTEM_C_ABI_FLAGS
	"--target=aarch64-unknown-none"
	"-ffreestanding"
	"-nostdlibinc"
	"-isystem" "${LIBERSYSTEM_SYSROOT}/include"
	# FLOATING POINT AND NEON ARE ON, because the Rust half has them on: `aarch64-unknown-none`
	# reports `target_feature="neon"`, which is the hardfloat AAPCS variant. An earlier version of
	# this file set `-mgeneral-regs-only` here on the assumption that a freestanding target must be
	# soft-float, and that was a SECOND description of the ABI - exactly the failure the paragraph
	# above warns about. It surfaced as `cJSON` refusing to compile: the pinned sources use `double`
	# and the ABI this file had declared could not pass one.
	"-fPIC"
)
string(JOIN " " LIBERSYSTEM_C_ABI_FLAGS_STR ${LIBERSYSTEM_C_ABI_FLAGS})
set(CMAKE_C_FLAGS_INIT "${LIBERSYSTEM_C_ABI_FLAGS_STR}")
set(CMAKE_CXX_FLAGS_INIT "${LIBERSYSTEM_C_ABI_FLAGS_STR} -nostdinc++ -fno-exceptions -fno-rtti")

# Nothing about the host is searched, for anything.
set(CMAKE_FIND_ROOT_PATH "${LIBERSYSTEM_SYSROOT}")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
