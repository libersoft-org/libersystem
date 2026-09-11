/* The riscv64 profile target. See x86_64.h for why this is force-included.
   `rv64gc` WITH THE `lp64d` ABI AND THE MEDIUM-ANY CODE MODEL, mirroring the cross file exactly. */
#ifndef LIBERSYSTEM_PROFILE_TARGET_H
#define LIBERSYSTEM_PROFILE_TARGET_H
#define LIBER_PROFILE_TRIPLE "riscv64-unknown-none-elf"
#define LIBER_PROFILE_ARCH "riscv64"
/* Plain `char` is UNSIGNED on riscv64, as it is on aarch64 - so x86_64 is the odd target and not,
   as this file first said, the other two. The assertion below is what corrected it: the value was
   written from the wrong assumption and the compile stopped on the file that disagreed, which is
   exactly why the data model is asserted in every translation unit rather than described here. */
#define LIBER_PROFILE_CHAR_SIGNED 0

/* THE DATA MODEL, ASSERTED RATHER THAN DESCRIBED. Everything below is a width, an alignment or a
   layout that a declaration in `include/` depends on, or that the Rust half of the substrate assumes
   when it implements one. A comment claiming LP64 is worth nothing; a failing `_Static_assert` names
   the translation unit where the two halves stopped agreeing. */
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <dirent.h>
#include <dlfcn.h>

_Static_assert(sizeof(void *) == 8, "the profile is LP64 on every target");
_Static_assert(sizeof(size_t) == 8, "size_t is the pointer width");
_Static_assert(sizeof(ptrdiff_t) == 8, "ptrdiff_t is the pointer width");
_Static_assert(sizeof(int) == 4, "int is 32 bits");
_Static_assert(sizeof(short) == 2, "short is 16 bits");
_Static_assert(sizeof(long) == 8, "long is 64 bits - this is LP64 and not LLP64");
_Static_assert(sizeof(long long) == 8, "long long is 64 bits");
_Static_assert(sizeof(float) == 4 && sizeof(double) == 8, "IEEE binary32 and binary64");
_Static_assert(_Alignof(double) == 8, "double is eight-byte aligned; the archive carries doubles");
_Static_assert(((char)-1 < 0) == LIBER_PROFILE_CHAR_SIGNED, "plain char signedness differs by target and the substrate must know which");

_Static_assert(sizeof(ssize_t) == 8 && sizeof(off_t) == 8, "the signed sizes are the pointer width");
_Static_assert(sizeof(uid_t) == 4 && sizeof(gid_t) == 4, "the identity queries return 32-bit ids");
_Static_assert(sizeof(mode_t) == 4, "st_mode is 32 bits, which is what S_IS* mask against");
_Static_assert(sizeof(struct stat) == 56, "the layout `fstat` fills, agreed with the Rust half");
_Static_assert(offsetof(struct stat, st_mode) == 16, "S_ISREG reads this offset");
_Static_assert(offsetof(struct stat, st_size) == 40, "the manifest read reads this offset");
_Static_assert(sizeof(struct dirent) == 272, "the layout `readdir` fills, agreed with the Rust half");
_Static_assert(offsetof(struct dirent, d_name) == 9, "the candidate name is read from this offset");
_Static_assert(sizeof(Dl_info) == 32, "the layout `dladdr` fills, agreed with the Rust half");

/* THE BUILTIN CALLS THIS TARGET'S COMPILER MAY EMIT, named because no header here declares them and
   the substrate provides them anyway. They are not inventory surface the sources asked for; they are
   what the code generator turns a structure assignment or an array initialisation into. */
#define LIBER_PROFILE_BUILTIN_CALLS "memcpy,memmove,memset,memcmp"

#endif
