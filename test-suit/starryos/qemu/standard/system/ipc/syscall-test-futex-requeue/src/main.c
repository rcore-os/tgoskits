#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <unistd.h>

#define FUTEX_WAIT_PRIVATE 128
#define FUTEX_WAKE_PRIVATE 129
#define FUTEX_REQUEUE_PRIVATE 131
#define FUTEX_CMP_REQUEUE_PRIVATE 132
#define WAITERS 3

static int source;
static int target;

static void *wait_on_source(void *unused) {
    (void)unused;
    long result = syscall(SYS_futex, &source, FUTEX_WAIT_PRIVATE, 0,
                          NULL, NULL, 0);
    return (void *)(intptr_t)(result == 0 ? 0 : errno);
}

int main(void) {
    pthread_t threads[WAITERS];
    setvbuf(stdout, NULL, _IONBF, 0);
    /* The alarm only bounds failures; queue membership establishes readiness. */
    alarm(30);
    for (int i = 0; i < WAITERS; ++i) {
        if (pthread_create(&threads[i], NULL, wait_on_source, NULL) != 0)
            return 1;
    }

    int moved = 0;
    while (moved < WAITERS) {
        long result = syscall(SYS_futex, &source, FUTEX_CMP_REQUEUE_PRIVATE,
                              0, WAITERS - moved, &target, 0);
        if (result < 0 || result > WAITERS - moved) {
            perror("setup requeue");
            return 1;
        }
        moved += (int)result;
        sched_yield();
    }

    /* No wake or signal can remove these waiters before the count checks. */
    long requeued = syscall(SYS_futex, &target, FUTEX_REQUEUE_PRIVATE,
                            0, WAITERS, &target, 0);
    long compared = syscall(SYS_futex, &target, FUTEX_CMP_REQUEUE_PRIVATE,
                            0, WAITERS, &target, 0);
    long woken = syscall(SYS_futex, &target, FUTEX_WAKE_PRIVATE, WAITERS,
                         NULL, NULL, 0);
    int failed = requeued != WAITERS || compared != WAITERS || woken != WAITERS;
    for (int i = 0; i < WAITERS; ++i) {
        void *result = NULL;
        if (pthread_join(threads[i], &result) != 0 || result != NULL)
            failed = 1;
    }
    alarm(0);
    printf("same-key requeue=%ld cmp_requeue=%ld wake=%ld: %s\n",
           requeued, compared, woken, failed ? "FAIL" : "PASS");
    return failed;
}
