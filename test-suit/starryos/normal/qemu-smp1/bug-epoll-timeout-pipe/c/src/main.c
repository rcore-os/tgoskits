/*
 * bug-epoll-timeout-pipe.c — Reproducer for the block_on lost-wakeup race
 *
 * This test targets the race in block_on's future_blocked_resched path:
 *   1. poll() returns Pending — timer waker is registered
 *   2. Timer IRQ fires, calls AxWaker::wake_by_ref → unblock_task
 *   3. But if the task hasn't entered blocked state yet, the wakeup is lost
 *   4. Task sleeps forever in future_blocked_resched
 *
 * Unlike bug-epoll-timeout (which uses stdin and triggers the Manual-mode
 * infinite loop), this test uses a PIPE with no writer. The pipe's poll()
 * returns empty (no IN events), and its register() does NOT immediately wake
 * the waker. So the task genuinely reaches the !woke branch in block_on.
 *
 * The timeout is the ONLY thing that should wake the task. If the race exists,
 * the timeout wakeup is lost and epoll_wait stalls.
 *
 * Strategy: Create a pipe, keep write end open but never write.
 * The read end will have no data and no HUP — pure timeout test.
 *
 * To increase the chance of hitting the race, we use very short timeouts (1ms)
 * and run many iterations. The race window is small but with 1000 iterations
 * at 1ms each, we should complete in ~1-2 seconds on a correct kernel.
 * On a buggy kernel, some iterations will stall (taking 9+ seconds total).
 *
 * Pass criteria: all 1000 iterations complete in < 2000ms total.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <time.h>
#include <unistd.h>

static long elapsed_ms(struct timespec *start) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (now.tv_sec - start->tv_sec) * 1000L +
           (now.tv_nsec - start->tv_nsec) / 1000000;
}

int main(void) {
    int pipefd[2];
    if (pipe(pipefd) < 0) { perror("pipe"); return 1; }
    /* pipefd[0] = read end, pipefd[1] = write end (kept open, never written) */

    int epfd = epoll_create1(0);
    if (epfd < 0) { perror("epoll_create1"); return 1; }

    struct epoll_event ev = { .events = EPOLLIN, .data.fd = pipefd[0] };
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, pipefd[0], &ev) < 0) {
        perror("epoll_ctl");
        return 1;
    }

    printf("Testing epoll_wait timeout on pipe (no stdin, no Manual-mode TTY)\n");
    printf("Each iteration: epoll_wait(pipe_read_end, timeout=1ms)\n");
    printf("Expected: returns 0 (timeout) in ~1ms each\n\n");

    const int timeout_ms = 1;
    const int iterations = 1000;
    int failures = 0;
    int stalls = 0;

    struct timespec total_start;
    clock_gettime(CLOCK_MONOTONIC, &total_start);

    for (int i = 0; i < iterations; i++) {
        struct timespec start;
        clock_gettime(CLOCK_MONOTONIC, &start);

        struct epoll_event events[1];
        int n = epoll_wait(epfd, events, 1, timeout_ms);

        long dt = elapsed_ms(&start);

        /* n must be 0 (timeout), elapsed should be 1-50ms */
        if (n != 0) {
            failures++;
            if (i < 20) printf("  iter %4d: n=%d dt=%ldms [FAIL: unexpected event]\n", i, n, dt);
        } else if (dt > 50) {
            stalls++;
            if (stalls <= 10) printf("  iter %4d: n=%d dt=%ldms [FAIL: timeout too slow]\n", i, n, dt);
        }
    }

    long total_ms = elapsed_ms(&total_start);

    close(epfd);
    close(pipefd[0]);
    close(pipefd[1]);

    printf("\nCompleted %d iterations in %ld ms (expected ~%d ms)\n",
           iterations, total_ms, iterations * timeout_ms);
    printf("Failures (wrong n): %d\n", failures);
    printf("Stalls (dt > 50ms): %d\n", stalls);

    if (failures == 0 && stalls == 0 && total_ms < 2000) {
        printf("TEST PASSED\n");
        return 0;
    } else {
        printf("TEST FAILED\n");
        return 1;
    }
}
