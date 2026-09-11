/* `dladdr` ONLY. There is no `dlopen`, `dlsym`, `dlclose` or `dlerror` here and there is no symbol
   for any of them: a provider is taken by reference out of a closure that was verified before the
   process started, which is the discovery item's decision, and the three upstream calls are removed
   by the platform port rather than backed. `dladdr` survives because it asks a question about an
   address that is already mapped, and answering it opens nothing. */
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
