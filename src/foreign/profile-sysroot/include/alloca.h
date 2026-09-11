/* <alloca.h>. A COMPILER BUILTIN AND NOT A SYMBOL: the declaration maps to `__builtin_alloca`, so
   nothing in the substrate provides it and nothing in the inventory names it. The pinned sources use
   it for short-lived arrays whose size is a device count. */
#ifndef LIBERSYSTEM_ALLOCA_H
#define LIBERSYSTEM_ALLOCA_H
#include <stddef.h>
#define alloca(size) __builtin_alloca(size)
#endif
