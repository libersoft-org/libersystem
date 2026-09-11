/* The subset of <unistd.h> the pinned configuration includes it for.
   EVERY DECLARATION HERE IS A QUESTION FOR THE DISCOVERY ITEM, not an answer: this system has no
   ambient file namespace and no user ids, and whether a symbol backs any of these is decided by the
   derived inventory. A declaration is not a promise. */
#ifndef LIBERSYSTEM_UNISTD_H
#define LIBERSYSTEM_UNISTD_H
#include <stddef.h>
#include <sys/types.h>
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
int access(const char *path, int mode);
ssize_t readlink(const char *restrict path, char *restrict buffer, size_t size);
int close(int fd);
ssize_t read(int fd, void *buffer, size_t count);
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
#endif
