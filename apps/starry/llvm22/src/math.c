/* math.c - clang -O2 with libm linkage (-lm). sqrt(2) = 1.4142... deterministic. */
#include <stdio.h>
#include <math.h>

int main(void)
{
    volatile double x = 2.0;
    printf("SQRT=%.4f\n", sqrt(x));
    return 0;
}
