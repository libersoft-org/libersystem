/* C99 <stdio.h>, restricted to what the pinned configuration includes it for.
   NO FILE STREAMS BEYOND WHAT IS INCLUDED FOR: this system has no ambient file namespace, and a
   complete <stdio.h> here would declare an interface the substrate must then refuse to back. */
#ifndef LIBERSYSTEM_STDIO_H
#define LIBERSYSTEM_STDIO_H
#include <stddef.h>
#include <stdarg.h>
typedef struct _LiberSystemFile FILE;
extern FILE *stdout;
extern FILE *stderr;
int printf(const char *restrict format, ...);
int fprintf(FILE *restrict stream, const char *restrict format, ...);
int snprintf(char *restrict s, size_t n, const char *restrict format, ...);
int vsnprintf(char *restrict s, size_t n, const char *restrict format, va_list args);
int vfprintf(FILE *restrict stream, const char *restrict format, va_list args);
int fputs(const char *restrict s, FILE *restrict stream);
int fputc(int c, FILE *stream);
int fflush(FILE *stream);
/* `sscanf` READS A STRING AND TOUCHES NO FILE, which is why it is here while the rest of the
   scanning family is not: the pinned sources use it to parse a version out of a buffer. */
int sscanf(const char *restrict s, const char *restrict format, ...);
/* The pinned sources read a manifest through these. See <dirent.h>: declaring the interface is not
   providing it, and what backs it is the discovery item's decision. */
FILE *fopen(const char *restrict path, const char *restrict mode);
int fclose(FILE *stream);
size_t fread(void *restrict buffer, size_t size, size_t count, FILE *restrict stream);
int fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
/* `fileno` IS POSIX AND NOT C99, and it is here for the same reason the rest of this file is: the
   pinned sources call it. What a descriptor means on this system is the discovery item's answer. */
int fileno(FILE *stream);
#endif
