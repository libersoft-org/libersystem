/* THE ACCESS-MODE CONSTANTS, AND NOT ONE FUNCTION. `access`, `readlink`, `close` and `read` were
   never called by any object. The four identity queries WERE, and are gone for a different reason:
   the loader compared a real and an effective id to decide whether to trust an environment search
   path, and the platform port removed the environment reading - so the comparison, and the four
   queries under it, are what the converged link no longer asks for.

   THE HEADER REMAINS BECAUSE THE SOURCES INCLUDE IT. Removing it would fail the include; declaring
   a function nothing requires would be surface nobody asked for. */
#ifndef LIBERSYSTEM_UNISTD_H
#define LIBERSYSTEM_UNISTD_H
#include <stddef.h>
#include <sys/types.h>
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
#endif
