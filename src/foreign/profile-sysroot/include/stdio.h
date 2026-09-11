/* The formatting and stream entry points the inventory names.
   GONE: `printf`, `fprintf`, `vfprintf`, `fflush`, `fseek`, `ftell` and `stdout`. The loader's
   logging goes through `fputs`/`fputc` on `stderr` and through `snprintf` into its own buffers, so
   the rest of the family is surface nobody requires - and `stdout` is a stream this system does not
   give a foreign artifact at all. */
#ifndef LIBERSYSTEM_STDIO_H
#define LIBERSYSTEM_STDIO_H
#include <stddef.h>
#include <stdarg.h>
typedef struct _LiberSystemFile FILE;
extern FILE *stderr;
int snprintf(char *restrict s, size_t n, const char *restrict format, ...);
int vsnprintf(char *restrict s, size_t n, const char *restrict format, va_list args);
int fputs(const char *restrict s, FILE *restrict stream);
int fputc(int c, FILE *stream);
/* `sscanf` READS A STRING AND TOUCHES NO FILE, which is why it survives the trim while the rest of
   the scanning family never appeared: the pinned sources parse a version out of a buffer with it. */
int sscanf(const char *restrict s, const char *restrict format, ...);
/* THE MANIFEST READ. What backs these is the discovery item's: a bounded read of a package-owned
   record, not an ambient file namespace. `SEEK_*` stay because the sources name them; the seeking
   functions do not, because the objects never call them. */
FILE *fopen(const char *restrict path, const char *restrict mode);
int fclose(FILE *stream);
size_t fread(void *restrict buffer, size_t size, size_t count, FILE *restrict stream);
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
int fileno(FILE *stream);
#endif
