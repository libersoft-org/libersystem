/* C99 <math.h>, restricted to what the pinned configuration includes it for. */
#ifndef LIBERSYSTEM_MATH_H
#define LIBERSYSTEM_MATH_H
double floor(double x);
double ceil(double x);
double fabs(double x);
double pow(double base, double exponent);
double sqrt(double x);
double log(double x);
double exp(double x);
double fmod(double x, double y);
int isnan(double x);
int isinf(double x);
int isfinite(double x);
#endif
