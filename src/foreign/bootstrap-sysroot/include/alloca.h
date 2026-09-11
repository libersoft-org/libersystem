/* <alloca.h>. NOT C99 - it is a compiler builtin everywhere it exists, and the pinned sources use it
   for short-lived arrays whose size is a device count. The declaration maps to the builtin rather
   than to a symbol, so nothing in the substrate has to provide it. */
#ifndef LIBERSYSTEM_ALLOCA_H
#define LIBERSYSTEM_ALLOCA_H
#include <stddef.h>
#define alloca(size) __builtin_alloca(size)
#endif
