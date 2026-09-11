/* The subset of <sys/stat.h> the pinned configuration includes it for. See <dirent.h>. */
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
int stat(const char *restrict path, struct stat *restrict buffer);
int fstat(int fd, struct stat *buffer);
#endif
