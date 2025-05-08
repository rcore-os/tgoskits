#include <stdio.h>
#include <unistd.h>
int main()
{
    printf("Sleeping for 2 seconds...\n");
    sleep(2);
    printf("Done!\n");
    return 0;
}