// SPDX-License-Identifier: Apache-2.0
// Focused regression: pidfd poll readiness is reported only after the target
// process exits. This matches Linux pidfd semantics and catches inverted
// readiness that can confuse event loops supervising child builders.

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h>"
#endif

static int tests_pass;
static int tests_fail;

#define TEST(cond, msg)                                                        \
    do {                                                                      \
        if (cond) {                                                           \
            tests_pass++;                                                     \
            printf("  PASS: %s\n", msg);                                      \
        } else {                                                              \
            tests_fail++;                                                     \
            printf("  FAIL: %s (%s:%d errno=%d)\n", msg, __FILE__, __LINE__,  \
                   errno);                                                    \
        }                                                                     \
    } while (0)

static int x_pidfd_open(pid_t pid, unsigned int flags) {
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static void test_pidfd_poll_exit_readiness(void) {
    printf("Test 1: pidfd poll becomes readable only after child exit\n");

    int sync_pipe[2];
    TEST(pipe(sync_pipe) == 0, "sync pipe created");

    pid_t child = fork();
    TEST(child >= 0, "fork succeeded");
    if (child == 0) {
        close(sync_pipe[1]);
        char c;
        (void)!read(sync_pipe[0], &c, 1);
        close(sync_pipe[0]);
        _exit(42);
    }

    close(sync_pipe[0]);

    int pfd = x_pidfd_open(child, 0);
    TEST(pfd >= 0, "pidfd_open(child) succeeded before exit");

    struct pollfd p = {.fd = pfd, .events = POLLIN};
    errno = 0;
    int ret = poll(&p, 1, 50);
    TEST(ret == 0, "pidfd is not readable while child is alive");
    TEST(p.revents == 0, "pidfd has no revents while child is alive");

    TEST(write(sync_pipe[1], "x", 1) == 1, "released child");
    close(sync_pipe[1]);

    p.revents = 0;
    errno = 0;
    ret = poll(&p, 1, 1000);
    TEST(ret == 1, "pidfd poll returns after child exits");
    TEST((p.revents & POLLIN) != 0, "pidfd reports POLLIN after child exit");
    fprintf(stderr, "  INFO: pidfd revents=0x%x (POLLIN=0x%x)\n", p.revents,
            POLLIN);

    int status = 0;
    TEST(waitpid(child, &status, 0) == child, "waitpid reaped child");
    TEST(WIFEXITED(status) && WEXITSTATUS(status) == 42,
         "child exit status preserved");

    close(pfd);
}

int main(void) {
    printf("=== pidfd-poll-exit regression ===\n");

    test_pidfd_poll_exit_readiness();

    printf("\n=== Results: %d pass, %d fail ===\n", tests_pass, tests_fail);
    if (tests_fail == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return tests_fail > 0 ? 1 : 0;
}
