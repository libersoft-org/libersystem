/* C99 <inttypes.h>, restricted to the format macros the pinned configuration uses. The widths are
   LP64, which is what all three of this system's targets are. */
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
