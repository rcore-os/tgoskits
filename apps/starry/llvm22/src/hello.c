/* hello.c - clang C front-end test: C -> LLVM IR (-emit-llvm) and C -> native
 * executable (compile + link + run). Deterministic stdout for exact assertion. */
#include <stdio.h>

int main(void)
{
    printf("CLANG22 OK\n");
    return 0;
}
