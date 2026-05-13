/*
 * test-futex-race.c — SMP futex TOCTOU race reproducer.
 *
 * Uses fork() + mmap(MAP_SHARED) for the futex word because StarryOS
 * futex WAIT/WAKE does not work across CLONE_VM threads (WAKE returns
 * 0 for CLONE_VM waiters).  The parent process is the blocker: stores
 * 0, calls FUTEX_WAIT(val=0) → actually enqueues.  The waker child
 * sets the value to 1 and calls FUTEX_WAKE.  This exercises concurrent
 * FutexGuard create/drop on the same key with real enqueue/dequeue.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <errno.h>
#include <string.h>

#define FUTEX_WAIT      0
#define FUTEX_WAKE      1
#define ROUNDS          30

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);

    int *futex = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                      MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (futex == MAP_FAILED) {
        printf("FAIL: mmap\n");
        return 1;
    }
    *futex = 0;

    /* Progress counters in shared memory */
    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    volatile int *block_cnt = &shared[0];
    volatile int *wake_cnt  = &shared[1];
    *block_cnt = 0;
    *wake_cnt  = 0;

    pid_t waker = fork();
    if (waker < 0) {
        printf("FAIL: fork\n");
        return 1;
    }

    if (waker == 0) {
        /* Child: waker */
        while (*block_cnt < ROUNDS) {
            *futex = 1;
            syscall(SYS_futex, futex, FUTEX_WAKE, 1, NULL, NULL, 0);
            __sync_fetch_and_add(wake_cnt, 1);
            usleep(10000);
        }
        _exit(0);
    }

    /* Parent: blocker */
    int enqueued = 0;
    for (int i = 0; i < ROUNDS; i++) {
        *futex = 0;

        long rc = syscall(SYS_futex, futex, FUTEX_WAIT, 0, NULL, NULL, 0);
        if (rc == -1 && errno != EAGAIN) {
            printf("  FAIL | blocker errno=%d (%s)\n", errno, strerror(errno));
            kill(waker, SIGKILL);
            waitpid(waker, NULL, 0);
            return 1;
        }
        if (rc == 0) enqueued++;
        __sync_fetch_and_add(block_cnt, 1);
    }

    /* Brief wait for waker to see block_cnt and exit */
    usleep(50000);

    int status;
    waitpid(waker, &status, 0);

    printf("PASS: %d rounds, %d enqueued, %d wakes\n",
           ROUNDS, enqueued, *wake_cnt);

    if (*block_cnt != ROUNDS) {
        printf("FAIL: only %d/%d rounds\n", *block_cnt, ROUNDS);
        return 1;
    }
    if (enqueued == 0) {
        printf("FAIL: never entered queue\n");
        return 1;
    }
    return 0;
}
