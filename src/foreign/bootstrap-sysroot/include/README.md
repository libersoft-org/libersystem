# The bootstrap sysroot

THIS IS NOT A LIBC, and the distinction is the whole point. It holds the C99 declarations the pinned
configuration needs in order to COMPILE, and nothing else. It is sized by the OPTION SET - a question
that can be answered by reading the configuration - and not by the symbol inventory, which is a
measurement that cannot exist until something has compiled against these headers.

WHY A HEADER WITHOUT A SYMBOL IS THE FAILURE TO AVOID, stated here because it is the mistake this
directory invites: a header that declares a function the substrate has no symbol for produces a
compile that succeeds and a link that fails. That is the general POSIX layer arriving through the
include path instead of through the symbol list, one declaration at a time, and it is exactly what
`P02M0135` forbids. Every declaration below is here because the pinned sources include the header it
lives in; whether the substrate will PROVIDE it is decided later, by the derived inventory, and a
declaration here is not a promise that it will.

THE PROFILE SYSROOT IS A DIFFERENT THING AND COMES LATER. That one is sized by what the inventory
names. This one exists so the inventory can be measured at all.

THE COMPILER'S OWN FREESTANDING HEADERS ARE NOT DUPLICATED HERE. `stddef.h`, `stdint.h`, `stdarg.h`,
`float.h`, `limits.h` and `stdbool.h` describe the TARGET and come from clang; a second copy in this
directory would be a second description of the same ABI, which is how a C object and a Rust object
come to disagree about a type.
