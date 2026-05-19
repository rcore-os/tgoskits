/*
 * bug-tgkill-sigwait-ignored.c
 *
 * Regression test for: ThreadSignalManager::send_signal() drops a blocked
 * default-ignored signal via is_ignore() without consulting sigwait_set.
 *
 * Bug path (StarryOS):
 *   tgkill(tid, SIGURG)
 *     → ThreadSignalManager::send_signal(SIGURG)
 *       → actions[SIGURG].is_ignore(SIGURG)  ← returns true (default-ignore)
 *         → return None   ← signal silently dropped
 *   sigtimedwait({SIGURG}) never wakes → hangs until alarm fires.
 *
 * Linux / POSIX requirement:
 *   A signal that is blocked in the target thread MUST be queued as pending
 *   even if its default disposition is to ignore it.  sigtimedwait() /
 *   sigwaitinfo() can then synchronously consume it.
 *   (POSIX.1-2017 sigtimedwait, sigwaitinfo; Linux signal(7))
 *
 * Test uses SIGURG (default-ignored, not real-time) as the probe signal.
 * A second case uses SIGWINCH for variety.
 *
 * Sequence:
 *   1. Block SIGURG (and SIGWINCH) in the main thread.
 *   2. Send SIGURG to the thread itself via tgkill(gettid(), SIGURG).
 *   3. sigtimedwait({SIGURG}, &si, 5s) must return SIGURG immediately.
 *   4. Repeat with SIGWINCH.
 *   5. A 10-second SIGALRM acts as a hard hang-guard.
 *
 * Expected on a correct kernel: TEST PASSED
 * Expected on buggy StarryOS:   TEST FAILED (sigtimedwait times out)
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static int passed;
static int failed;
static volatile int timed_out;

#define CHECK(label, cond, fmt, ...)                                        \
    do {                                                                    \
        if (cond) {                                                         \
            printf("  [OK]   %s\n", label);                                \
            passed++;                                                       \
        } else {                                                            \
            printf("  [FAIL] %s: " fmt "\n", label, ##__VA_ARGS__);        \
            failed++;                                                       \
        }                                                                   \
    } while (0)

static void alarm_handler(int sig)
{
    (void)sig;
    timed_out = 1;
}

/*
 * Send signo to this thread via tgkill, then consume it with sigtimedwait.
 * Returns 1 on success, 0 on failure (including timeout).
 */
static int test_signal(int signo, const char *name)
{
    if (timed_out)
        return 0;

    printf("--- testing %s (signo=%d) ---\n", name, signo);

    /* Block the signal so it queues rather than being delivered. */
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, signo);
    if (sigprocmask(SIG_BLOCK, &set, NULL) < 0) {
        printf("  [FAIL] sigprocmask SIG_BLOCK: %s\n", strerror(errno));
        failed++;
        return 0;
    }

    /*
     * Use the raw tgkill syscall to send the signal directly to this thread.
     * This exercises ThreadSignalManager::send_signal(), not the process-level
     * path that was already fixed.
     */
    pid_t tid = (pid_t)syscall(SYS_gettid);
    pid_t pid = getpid();
    long rc = syscall(SYS_tgkill, (long)pid, (long)tid, (long)signo);
    CHECK("tgkill returns 0", rc == 0,
          "got %ld (errno=%d %s)", rc, errno, strerror(errno));
    if (rc != 0) {
        sigprocmask(SIG_UNBLOCK, &set, NULL);
        return 0;
    }

    /*
     * sigtimedwait with a 5-second timeout.
     * On a correct kernel the signal is already pending → returns instantly.
     * On the buggy kernel the signal was dropped → times out after 5s.
     */
    struct timespec ts = { .tv_sec = 5, .tv_nsec = 0 };
    siginfo_t si;
    int got = sigtimedwait(&set, &si, &ts);

    if (got < 0 && errno == EAGAIN) {
        printf("  [FAIL] sigtimedwait(%s) timed out — signal was dropped "
               "by is_ignore() in ThreadSignalManager::send_signal()\n", name);
        failed++;
        sigprocmask(SIG_UNBLOCK, &set, NULL);
        return 0;
    }

    CHECK("sigtimedwait returns signal number", got == signo,
          "got %d, want %d", got, signo);
    CHECK("si_signo matches", si.si_signo == signo,
          "got %d, want %d", si.si_signo, signo);

    sigprocmask(SIG_UNBLOCK, &set, NULL);
    return (got == signo && si.si_signo == signo) ? 1 : 0;
}

int main(void)
{
    printf("=== bug-tgkill-sigwait-ignored ===\n");
    printf("Tests that tgkill() to a thread blocked in sigtimedwait()\n");
    printf("correctly queues default-ignored signals (SIGURG, SIGWINCH).\n\n");

    /* Hard timeout: if the whole test hangs, SIGALRM fires after 10s. */
    struct sigaction sa_alarm = { .sa_handler = alarm_handler };
    sigemptyset(&sa_alarm.sa_mask);
    sigaction(SIGALRM, &sa_alarm, NULL);
    alarm(10);

    test_signal(SIGURG,   "SIGURG");
    test_signal(SIGWINCH, "SIGWINCH");

    alarm(0);

    if (timed_out) {
        printf("\n  [FAIL] hard alarm fired — test hung\n");
        failed++;
    }

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
