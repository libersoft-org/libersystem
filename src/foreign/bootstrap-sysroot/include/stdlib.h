/* C99 <stdlib.h>, restricted to what the pinned configuration includes it for. */
#ifndef LIBERSYSTEM_STDLIB_H
#define LIBERSYSTEM_STDLIB_H
#include <stddef.h>
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
/* ALIGNED ALLOCATION IS IN THE CLOSURE and is named here because the loader asks for it directly;
   an allocator that ignored the alignment would be a fault the inventory could not see. */
void *aligned_alloc(size_t alignment, size_t size);
long strtol(const char *restrict s, char **restrict end, int base);
unsigned long strtoul(const char *restrict s, char **restrict end, int base);
double strtod(const char *restrict s, char **restrict end);
int atoi(const char *s);
void abort(void);
void exit(int status);
/* `getenv` IS DECLARED AND THE SUBSTRATE IS NOT OBLIGED TO PROVIDE IT. The loader's environment
   reading is what the discovery item exists to replace; whether a symbol backs this is that item's
   decision, and a declaration here does not make it. */
char *getenv(const char *name);
void qsort(void *base, size_t count, size_t size, int (*compare)(const void *, const void *));
#endif
