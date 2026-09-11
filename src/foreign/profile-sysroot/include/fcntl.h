/* THE FLAGS, AND NO `open`. The pinned sources include this header for the constants; the objects
   never call `open`, so the substrate has no symbol for it and none is declared. A source that
   starts calling it stops the compile here, which is the decision being made deliberately. */
#ifndef LIBERSYSTEM_FCNTL_H
#define LIBERSYSTEM_FCNTL_H
#include <sys/types.h>
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000
#endif
