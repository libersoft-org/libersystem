/* `assert` is a macro and is defined per inclusion, which is why the macro is not inside the include
   guard: <assert.h> is the one standard header that may be included more than once with a different
   NDEBUG each time. */
#include <stddef.h>
#undef assert
#ifdef NDEBUG
#define assert(expression) ((void)0)
#else
void __liber_assert_failed(const char *expression, const char *file, int line);
#define assert(expression) ((expression) ? (void)0 : __liber_assert_failed(#expression, __FILE__, __LINE__))
#endif
