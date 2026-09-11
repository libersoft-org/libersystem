/* C99 <errno.h>. The values are the Linux ones the pinned sources compare against; a different set
   would make a comparison silently false rather than fail to compile. */
#ifndef LIBERSYSTEM_ERRNO_H
#define LIBERSYSTEM_ERRNO_H
extern int *__liber_errno_location(void);
#define errno (*__liber_errno_location())
#define EPERM 1
#define ENOENT 2
#define EINTR 4
#define EIO 5
#define EBADF 9
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define ENODEV 19
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define ENFILE 23
#define EMFILE 24
#define ENOSPC 28
#define ERANGE 34
#define ENOSYS 38
#define ENOTSUP 95
#define EOVERFLOW 75
#endif
