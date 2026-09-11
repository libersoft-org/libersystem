/* The subset of <dlfcn.h> the pinned configuration includes it for.
   THE AMBIENT LIBRARY SEARCH IS WHAT THE DISCOVERY ITEM REPLACES. These declare the interface the
   pinned sources call so that pass 1 can SEE those calls; whether the substrate provides a symbol
   for any of them is that item's decision, and a declaration here is not a promise that it will. */
#ifndef LIBERSYSTEM_DLFCN_H
#define LIBERSYSTEM_DLFCN_H
typedef struct {
	const char *dli_fname;
	void *dli_fbase;
	const char *dli_sname;
	void *dli_saddr;
} Dl_info;
int dladdr(const void *address, Dl_info *info);
#endif
