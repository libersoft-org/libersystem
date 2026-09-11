/* The string and memory entry points the inventory names.
   GONE: `memchr`, `strcat`, `strnlen`, `strtok` and `strdup`. `strtok` in particular is not merely
   uncalled - the pinned sources call `thread_safe_strtok`, which their own headers declare. */
#ifndef LIBERSYSTEM_STRING_H
#define LIBERSYSTEM_STRING_H
#include <stddef.h>
void *memcpy(void *restrict dest, const void *restrict src, size_t n);
void *memmove(void *dest, const void *src, size_t n);
void *memset(void *s, int c, size_t n);
int memcmp(const void *a, const void *b, size_t n);
char *strcpy(char *restrict dest, const char *restrict src);
char *strncpy(char *restrict dest, const char *restrict src, size_t n);
char *strncat(char *restrict dest, const char *restrict src, size_t n);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, size_t n);
size_t strlen(const char *s);
char *strchr(const char *s, int c);
char *strrchr(const char *s, int c);
char *strstr(const char *haystack, const char *needle);
char *strerror(int errnum);
#endif
