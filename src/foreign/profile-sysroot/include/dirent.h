/* THE DIRECTORY READ, BACKED BY THE DISCOVERY ITEM AND NOT BY A FILESYSTEM. The three entry points
   are the ones the inventory names, and what stands behind them is an enumeration of package-owned
   candidate records - which is the whole reason the ambient scan this header's shape suggests is
   not what happens. */
#ifndef LIBERSYSTEM_DIRENT_H
#define LIBERSYSTEM_DIRENT_H
#include <sys/types.h>
#define NAME_MAX 255
struct dirent {
	ino_t d_ino;
	unsigned char d_type;
	char d_name[NAME_MAX + 1];
};
typedef struct _LiberSystemDir DIR;
DIR *opendir(const char *path);
struct dirent *readdir(DIR *dir);
int closedir(DIR *dir);
#endif
