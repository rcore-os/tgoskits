#include <stdio.h>

#ifndef STARRY_SKIP_MESSAGE
#define STARRY_SKIP_MESSAGE "Starry SMP system test skipped on this architecture"
#endif

int main(void)
{
    puts(STARRY_SKIP_MESSAGE);
    return 0;
}
