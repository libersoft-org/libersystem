/* The subset of <fcntl.h> the pinned configuration includes it for. See <unistd.h>: these describe
   an interface the discovery item decides the fate of, and declaring one does not provide it. */
#ifndef LIBERSYSTEM_FCNTL_H
#define LIBERSYSTEM_FCNTL_H
#include <sys/types.h>
#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000
int open(const char *path, int flags, ...);
#endif
