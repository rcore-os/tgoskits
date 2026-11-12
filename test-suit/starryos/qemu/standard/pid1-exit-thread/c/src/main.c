#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#include <sys/syscall.h>
int main(void)
{
    setbuf(stdout, NULL);
    if (getpid() != 1) {
        puts("STARRY_PID1_EXIT_FAILED");
        return 1;
    }
    puts("STARRY_PID1_EXIT_BEGIN");
    syscall(SYS_exit, 37);
    puts("STARRY_PID1_EXIT_FAILED: exit returned");
    return 1;
}
