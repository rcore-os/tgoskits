#define _GNU_SOURCE
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>
#include <sys/syscall.h>
static void *peer(void *unused)
{
    (void)unused;
    for (;;) pause();
    return NULL;
}
int main(void)
{
    setbuf(stdout, NULL);
    if (getpid() != 1) {
        puts("STARRY_PID1_EXIT_FAILED");
        return 1;
    }
    pthread_t thread;
    if (pthread_create(&thread, NULL, peer, NULL) != 0) {
        puts("STARRY_PID1_EXIT_FAILED: create peer");
        return 1;
    }
    puts("STARRY_PID1_EXIT_BEGIN");
    syscall(SYS_exit_group, 37);
    puts("STARRY_PID1_EXIT_FAILED: exit returned");
    return 1;
}
