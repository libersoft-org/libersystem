/* The subset of <dirent.h> the pinned configuration includes it for.
   THE AMBIENT DIRECTORY SCAN IS WHAT THE DISCOVERY ITEM EXISTS TO REPLACE, so this header is the
   clearest case of the rule the sysroot README states: it declares an interface, and whether the
   substrate provides a symbol for any of it is that item's decision rather than this file's. */
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
