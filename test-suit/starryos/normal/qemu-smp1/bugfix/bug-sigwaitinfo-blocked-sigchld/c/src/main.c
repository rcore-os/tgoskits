/*
 * bug-sigwaitinfo-blocked-sigchld.c
 *
 * Bug: sigwaitinfo() stalls forever in StarryOS when waiting for a signal
 * that is blocked via sigprocmask() before the call.
 *
 * Root cause: send_signal_to_process() calls proc_data.signal.send_signal()
 * which queues the signal as pending but returns None (no thread to
 * interrupt) when the signal is blocked.  As a result task.interrupt() is
 * never called, so the task sleeping in rt_sigtimedwait is never woken.
 *
 * POSIX semantics: sigwaitinfo(set, ...) atomically unblocks and waits for
 * any signal in `set`.  The signal must be blocked before the call so it
 * is queued rather than delivered asynchronously.  sigwaitinfo() must
 * return once the signal is pending, regardless of whether the signal was
 * already blocked when it arrived.
 *
 * Minimal reproduction:
 *   1. Block SIGCHLD with sigprocmask(SIG_BLOCK)
 *   2. fork() a child that exits immediately (sends SIGCHLD to parent)
 *   3. sigwaitinfo({SIGCHLD}, &info) must return with si_signo == SIGCHLD
 *      within a reasonable timeout — it must NOT stall forever.
 *
 * We use a 5-second alarm as a hard timeout so the test fails cleanly
 * instead of hanging the test harness.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;
static volatile int timed_out;

#define CHECK(cond, msg)                                                \
    do {                                                                \
        if (cond) {                                                     \
            printf("  [OK]   %s\n", (msg));                            \
            passed++;                                                   \
        } else {                                                        \
            printf("  [FAIL] %s (errno=%d %s)\n",                      \
                   (msg), errno, strerror(errno));                      \
            failed++;                                                   \
        }                                                               \
    } while (0)

static void alarm_handler(int sig)
{
    (void)sig;
    timed_out = 1;
}

int main(void)
{
    printf("=== bug-sigwaitinfo-blocked-sigchld ===\n");

    /*
     * Install a 5-second alarm so the test fails cleanly if sigwaitinfo
     * stalls instead of hanging the harness indefinitely.
     */
    struct sigaction sa_alarm = { .sa_handler = alarm_handler };
    sigemptyset(&sa_alarm.sa_mask);
    sigaction(SIGALRM, &sa_alarm, NULL);
    alarm(5);

    /*
     * Block SIGCHLD before fork so it is queued as pending rather than
     * delivered asynchronously.  sigwaitinfo() requires the signal to be
     * blocked in the calling thread's signal mask.
     */
    sigset_t mask, oldmask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGCHLD);
    if (sigprocmask(SIG_BLOCK, &mask, &oldmask) < 0) {
        perror("sigprocmask");
        return EXIT_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }
    if (child == 0) {
        /* Child exits immediately, sending SIGCHLD to parent. */
        _exit(0);
    }

    /*
     * sigwaitinfo() must return with SIGCHLD.
     * On a correct kernel this returns almost immediately.
     * On the buggy kernel this stalls until SIGALRM fires.
     */
    siginfo_t si;
    int sig = sigwaitinfo(&mask, &si);

    alarm(0); /* cancel alarm */

    if (timed_out) {
        printf("  [FAIL] sigwaitinfo(SIGCHLD) timed out after 5s — "
               "task was never woken\n");
        failed++;
    } else {
        CHECK(sig == SIGCHLD,
              "sigwaitinfo() returns SIGCHLD");
        CHECK(si.si_signo == SIGCHLD,
              "si_signo == SIGCHLD");
        CHECK(si.si_pid == child,
              "si_pid == child pid");
    }

    /* Reap the child to avoid leaving a zombie. */
    int status;
    waitpid(child, &status, 0);

    /* Restore signal mask. */
    sigprocmask(SIG_SETMASK, &oldmask, NULL);

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
