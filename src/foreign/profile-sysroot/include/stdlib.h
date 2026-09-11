/* The allocation, conversion and termination entry points the inventory names.
   GONE FROM HERE AND DELIBERATELY: `exit`, `qsort`, `aligned_alloc` and `strtol`. The bootstrap
   header declared all four because the pinned sources include <stdlib.h>; the objects call none of
   them, so the substrate has no symbol for any of them and declaring one would be a link failure
   waiting for a caller. */
#ifndef LIBERSYSTEM_STDLIB_H
#define LIBERSYSTEM_STDLIB_H
#include <stddef.h>
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
unsigned long strtoul(const char *restrict s, char **restrict end, int base);
double strtod(const char *restrict s, char **restrict end);
int atoi(const char *s);
void abort(void);
/* `getenv` IS BACKED, AND WHAT BACKS IT ANSWERS NO ENVIRONMENT. The discovery item's decision is
   that this system has no ambient environment to read, so the substrate's symbol reports every name
   as unset - which is a definite answer the loader's own code paths already handle, and is why the
   symbol exists rather than the declaration being removed. */
char *getenv(const char *name);
#endif
