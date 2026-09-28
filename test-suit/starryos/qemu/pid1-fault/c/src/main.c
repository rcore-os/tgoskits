#define _GNU_SOURCE
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
int main(void)
{
    setbuf(stdout, NULL);
    volatile char *page = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (getpid() != 1 || page == MAP_FAILED) {
        puts("STARRY_PID1_FAULT_FAILED: setup");
        return 1;
    }
    puts("STARRY_PID1_FAULT_BEGIN");
    *page = 1;
    puts("STARRY_PID1_FAULT_FAILED: inaccessible page was writable");
    return 1;
}
