/* The types the declared surface uses. LP64 on all three targets; `target/` asserts it rather than
   trusting this comment, because a width that disagrees with the Rust half is a struct layout
   nobody checks. */
#ifndef LIBERSYSTEM_SYS_TYPES_H
#define LIBERSYSTEM_SYS_TYPES_H
#include <stddef.h>
typedef long ssize_t;
typedef long off_t;
typedef unsigned int mode_t;
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef unsigned long dev_t;
typedef unsigned long ino_t;
typedef unsigned long nlink_t;
typedef long time_t;
#endif
