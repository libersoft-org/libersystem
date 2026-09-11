# The riscv64 cross description for this milestone's pinned, AUDIT-ONLY configuration.
#
# WHY THIS FILE EXISTS AT ALL. Pinning compilers and flags does not give a C translation unit a
# target ABI or a place to find headers. Without a cross file CMake compiles for the HOST - and that
# failure is silent: the build succeeds against glibc's ABI on a hosted x86_64 Linux, and the symbol
# inventory derived from it describes a system this one is not. On this target it would not even be
# the right ARCHITECTURE.
#
# EVERY ABI VALUE HERE MATCHES WHAT THE RUST SIDE IS BUILT WITH, which for this target is
# `riscv64gc-unknown-none-elf` with `-C relocation-model=pic -C code-model=medium` (see
# `src/tools/build-shared.sh`). Two descriptions of one ABI is how a C object and a Rust object end
# up disagreeing about a struct - or, on this target, about how far a symbol may be from its
# reference.
set(CMAKE_SYSTEM_NAME Generic)
set(CMAKE_SYSTEM_PROCESSOR riscv64)

set(LIBERSYSTEM_TARGET riscv64-unknown-none-elf)
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
	"--target=riscv64-unknown-none-elf"
	"-ffreestanding"
	"-nostdlibinc"
	"-isystem" "${LIBERSYSTEM_SYSROOT}/include"
	# `gc` IS THE RUST TRIPLE'S OWN EXTENSION SET, spelled out because clang's triple does not carry
	# it: IMAFD plus compressed instructions, with the double-float ABI that implies.
	"-march=rv64gc"
	"-mabi=lp64d"
	# `code-model=medium` in the Rust target. A C object built `small` cannot reach a symbol the
	# Rust half placed beyond its range, and the link that discovers it names a relocation rather
	# than a decision.
	"-mcmodel=medany"
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
