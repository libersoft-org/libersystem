/* `fstat` AND THE STRUCTURE IT FILLS. `stat` is not declared: the inventory does not name it, so the
   substrate has no symbol for it, so a source that calls it stops here instead of at the link. What
   a path means on this system is the discovery item's answer and there is no ambient one. */
#ifndef LIBERSYSTEM_SYS_STAT_H
#define LIBERSYSTEM_SYS_STAT_H
#include <sys/types.h>
struct stat {
	dev_t st_dev;
	ino_t st_ino;
	mode_t st_mode;
	nlink_t st_nlink;
	uid_t st_uid;
	gid_t st_gid;
	off_t st_size;
	time_t st_mtime;
};
#define S_IFMT 0170000
#define S_IFDIR 0040000
#define S_IFREG 0100000
#define S_ISDIR(mode) (((mode) & S_IFMT) == S_IFDIR)
#define S_ISREG(mode) (((mode) & S_IFMT) == S_IFREG)
int fstat(int fd, struct stat *buffer);
#endif
