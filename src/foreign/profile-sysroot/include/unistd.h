/* THE FOUR IDENTITY QUERIES, AND NOTHING THAT TOUCHES A FILE. `access`, `readlink`, `close` and
   `read` are gone: the objects call none of them, and each would be an ambient file operation this
   system does not have. The four that remain are what the loader's own security check compares, and
   what backs them is a fixed unprivileged identity - a definite answer, so the check runs rather
   than being patched out. */
#ifndef LIBERSYSTEM_UNISTD_H
#define LIBERSYSTEM_UNISTD_H
#include <stddef.h>
#include <sys/types.h>
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
#endif
