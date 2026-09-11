/* The allocation, conversion and termination entry points the inventory names.
   GONE FROM HERE AND DELIBERATELY: `exit`, `qsort`, `aligned_alloc` and `strtol`, which no object
   calls; and `getenv` and `strtoul`, which the PLATFORM PORT removed. The ported loader reads no
   environment at all - so the one function that read it and the one that parsed the number it
   carried are symbols the converged link does not ask for, and a symbol nothing requires is not
   declared and not built. */
#ifndef LIBERSYSTEM_STDLIB_H
#define LIBERSYSTEM_STDLIB_H
#include <stddef.h>
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
double strtod(const char *restrict s, char **restrict end);
int atoi(const char *s);
void abort(void);
#endif
