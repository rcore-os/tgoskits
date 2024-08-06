#include <stdlib.h>

extern int main();

int __start_main()
{
    exit(main());
    return 0;
}
