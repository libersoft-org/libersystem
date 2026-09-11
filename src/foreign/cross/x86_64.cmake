# The x86_64 cross description for this milestone's pinned, AUDIT-ONLY configuration.
#
# WHY THIS FILE EXISTS AT ALL. Pinning compilers and flags does not give a C translation unit a
# target ABI or a place to find headers. Without a cross file CMake compiles for the HOST - and that
# failure is silent: the build succeeds, the objects are built against glibc's ABI on a hosted
# x86_64 Linux, and the symbol inventory derived from them describes a system this one is not.
#
# EVERY ABI VALUE HERE IS TAKEN FROM `src/user/x86_64-unknown-none.json`, which is what the Rust side
# of this system is actually built with. Two descriptions of one ABI is how a C object and a Rust
# object end up disagreeing about a struct.
set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR x86_64)

set(LIBERSYSTEM_TARGET x86_64-unknown-none-elf)
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
	"--target=x86_64-unknown-none-elf"
	"-ffreestanding"
	"-nostdlibinc"
	"-isystem" "${LIBERSYSTEM_SYSROOT}/include"
	# The Rust target's own feature line, matched exactly: SSE2 is the baseline this system assumes
	# and nothing above it is enabled, because a C object using AVX in a system whose Rust half does
	# not save that state is a corruption nobody traces back to a compiler flag.
	"-mno-mmx" "-msse" "-msse2" "-mno-sse3" "-mno-ssse3" "-mno-sse4.1" "-mno-sse4.2" "-mno-avx" "-mno-avx2" "-mfxsr"
	# `disable-redzone` in the Rust target. The red zone is unusable where an interrupt can land on
	# the current stack, and half a system observing that rule is worse than neither half doing so.
	"-mno-red-zone"
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
