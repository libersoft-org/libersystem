/* ONE FUNCTION, AND THAT IS THE MEASUREMENT'S ANSWER. The bootstrap header declared ten because the
   pinned sources include <ctype.h>; the objects call exactly one of them. The other nine are the
   clearest case of surface a header can invent: each would compile anywhere and link nowhere. */
#ifndef LIBERSYSTEM_CTYPE_H
#define LIBERSYSTEM_CTYPE_H
int tolower(int c);
#endif
