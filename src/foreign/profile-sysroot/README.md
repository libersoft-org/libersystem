# The profile sysroot

THIS IS NOT A LIBC EITHER, and it is not the bootstrap sysroot. The two differ in what SIZES them,
which is the only distinction that matters:

  the BOOTSTRAP sysroot   is sized by the OPTION SET - the headers the pinned sources include - and
                            exists so that something can compile at all, which is what makes a symbol
                            inventory possible. It declares interfaces the substrate may never back.
  the PROFILE sysroot     is sized by the INVENTORY. Every function declared here has a symbol in the
                            substrate; a function the substrate does not provide is not declared, so a
                            source that calls it fails to COMPILE instead of failing to link.

WHY THAT INVERSION IS THE WHOLE POINT. A header declaring a function nothing provides is the general
POSIX layer arriving through the include path: the compile succeeds, the link fails much later, and
the only record of the decision is a diagnostic nobody attributed. Measured against the bootstrap
headers, that gap was 43 declarations - `open`, `read`, `close`, `exit`, `printf`, `qsort`, `strdup`,
the whole `<ctype.h>` classification family, most of `<math.h>` - every one of which the pinned
configuration includes a header for and never calls. They are gone from here. If the configuration
ever does call one, the compile stops on it and the decision is made deliberately, by adding a
substrate symbol, rather than discovered at link time.

WHAT IS HERE, EXACTLY. Forty-six declarations: the fifty-eight symbols the inventory names, less the
twelve the loader's own patched headers declare for themselves - the eleven `loader_platform_*`
entry points and `thread_safe_strtok`. Beside them are the types, constants and macros those
declarations need and the sources compare against, and nothing else.

THE TARGET IS DESCRIBED AND CHECKED, NOT ASSUMED. `arch/` carries one header per architecture -
the triple, the data model, and static assertions over every type size, alignment and struct layout
the declarations below depend on. It is force-included by the build rather than included by a source,
so the assertions run on every translation unit and a disagreement between the C half and the Rust
half stops the compile at the file that disagrees. `include/` is architecture-independent by
construction: a declaration whose shape depends on the target belongs in `arch/`.

THE COMPILER'S OWN FREESTANDING HEADERS ARE STILL NOT DUPLICATED. `stddef.h`, `stdint.h`, `stdarg.h`,
`float.h`, `limits.h` and `stdbool.h` describe the target and come from clang. What clang cannot tell
this system is which builtin CALLS it may emit, so `arch/` names those too: they are symbols the
substrate provides without any header here declaring them.
