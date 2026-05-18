/*
 * bug-sigchld-si-code-exit-group.c
 *
 * Regression test for incorrect SIGCHLD si_code/si_status when a child
 * exits via exit_group().
 *
 * Bug: do_exit() uses the `group_exit` flag to decide whether the child
 * was killed by a signal.  sys_exit_group() calls do_exit(code<<8, true),
 * so group_exit=true even for a normal exit.  The buggy branch decodes
 * the wait-status-encoded exit_code as a signal number and reports
 * CLD_KILLED with si_status=0 instead of CLD_EXITED with si_status=5.
 *
 * POSIX / Linux requirement:
 *   child calls exit_group(5)  →  parent sigwaitinfo sees:
 *     si_signo == SIGCHLD
 *     si_code  == CLD_EXITED   (1)
 *     si_status == 5
 *
 * Test steps:
 *   1. Block SIGCHLD so it is queued rather than delivered asynchronously.
 *   2. fork() a child that calls syscall(SYS_exit_group, 5).
 *   3. sigwaitinfo({SIGCHLD}, &si) in the parent.
 *   4. Assert si_code == CLD_EXITED and si_status == 5.
 *   5. A 5-second alarm acts as a hard timeout.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile int timed_out = 0;
static int failed = 0;

#define CHECK(label, cond, fmt, ...)                                    \
    do {                                                                \
        if (!(cond)) {                                                  \
            printf("  [FAIL] %s: " fmt "\n", label, ##__VA_ARGS__);    \
            failed++;                                                   \
        } else {                                                        \
            printf("  [PASS] %s\n", label);                            \
        }                                                               \
    } while (0)

static void alarm_handler(int sig)
{
    (void)sig;
    timed_out = 1;
}

int main(void)
{
    printf("=== bug-sigchld-si-code-exit-group ===\n");

    /* Hard timeout so the test fails cleanly instead of hanging. */
    struct sigaction sa_alarm = { .sa_handler = alarm_handler };
    sigemptyset(&sa_alarm.sa_mask);
    sigaction(SIGALRM, &sa_alarm, NULL);
    alarm(5);

    /* Block SIGCHLD before fork so it is queued, not delivered. */
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGCHLD);
    if (sigprocmask(SIG_BLOCK, &mask, NULL) < 0) {
        perror("sigprocmask");
        return EXIT_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }
    if (child == 0) {
        /*
         * Use the raw syscall to guarantee exit_group(5) is issued,
         * bypassing any libc wrapper that might call plain exit().
         */
        syscall(SYS_exit_group, 5);
        _exit(5); /* unreachable, but keeps the compiler happy */
    }

    /* Wait for SIGCHLD. */
    siginfo_t si = {0};
    int sig = sigwaitinfo(&mask, &si);

    alarm(0);

    if (timed_out) {
        printf("  [FAIL] sigwaitinfo(SIGCHLD) timed out after 5s\n");
        printf("\nTEST FAILED\n");
        return EXIT_FAILURE;
    }

    if (sig < 0) {
        perror("sigwaitinfo");
        printf("\nTEST FAILED\n");
        return EXIT_FAILURE;
    }

    /* Reap the child so it doesn't linger as a zombie. */
    int wstatus = 0;
    waitpid(child, &wstatus, WNOHANG);

    printf("  si_signo=%d  si_code=%d  si_pid=%d  si_status=%d\n",
           si.si_signo, si.si_code, si.si_pid, si.si_status);

    CHECK("si_signo == SIGCHLD",
          si.si_signo == SIGCHLD,
          "got %d, want %d", si.si_signo, SIGCHLD);

    CHECK("si_code == CLD_EXITED (1)",
          si.si_code == CLD_EXITED,
          "got %d, want CLD_EXITED (%d)", si.si_code, CLD_EXITED);

    CHECK("si_status == 5",
          si.si_status == 5,
          "got %d, want 5", si.si_status);

    CHECK("si_pid == child",
          si.si_pid == child,
          "got %d, want %d", si.si_pid, child);

    if (failed) {
        printf("\nTEST FAILED\n");
        return EXIT_FAILURE;
    }
    printf("\nTEST PASSED\n");
    return EXIT_SUCCESS;
}
