/* The format macros the pinned configuration uses. The widths are LP64; `target/` asserts that for
   the architecture being compiled rather than leaving it to this comment. */
#ifndef LIBERSYSTEM_INTTYPES_H
#define LIBERSYSTEM_INTTYPES_H
#include <stdint.h>
#define PRId32 "d"
#define PRIi32 "i"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"
#define PRId64 "ld"
#define PRIi64 "li"
#define PRIu64 "lu"
#define PRIx64 "lx"
#define PRIX64 "lX"
#define PRIdPTR "ld"
#define PRIuPTR "lu"
#define PRIxPTR "lx"
#endif
