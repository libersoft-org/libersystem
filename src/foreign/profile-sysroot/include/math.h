/* ONE FUNCTION. The pinned configuration's floating-point use is a magnitude comparison; `floor`,
   `ceil`, `pow`, `sqrt`, `log`, `exp`, `fmod` and the classification macros are not called by any
   object, and a substrate that provided them would be carrying a maths library for nobody. */
#ifndef LIBERSYSTEM_MATH_H
#define LIBERSYSTEM_MATH_H
double fabs(double x);
#endif
