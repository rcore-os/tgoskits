#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;
static volatile sig_atomic_t got_usr1;
static volatile sig_atomic_t got_alarm;

#define CHECK(label, cond, fmt, ...)                                      \
    do {                                                                  \
        if (cond) {                                                       \
            printf("  PASS | rt_sigsuspend | %s\n", label);              \
            passed++;                                                     \
        } else {                                                          \
            printf("  FAIL | rt_sigsuspend | %s | " fmt "\n", label,     \
                   ##__VA_ARGS__);                                        \
            failed++;                                                     \
        }                                                                 \
    } while (0)

static void usr1_handler(int signo)
{
    if (signo == SIGUSR1) {
        got_usr1 = 1;
    }
}

static void alarm_handler(int signo)
{
    (void)signo;
    got_alarm = 1;
}

static int mask_has(int signo)
{
    sigset_t current;
    if (sigprocmask(SIG_SETMASK, NULL, &current) != 0) {
        return -1;
    }
    return sigismember(&current, signo);
}

static int pending_has(int signo)
{
    sigset_t pending;
    if (sigpending(&pending) != 0) {
        return -1;
    }
    return sigismember(&pending, signo);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-sigsuspend: rt_sigsuspend mask restore and EINTR ===\n");

    struct sigaction sa_usr1;
    memset(&sa_usr1, 0, sizeof(sa_usr1));
    sa_usr1.sa_handler = usr1_handler;
    sigemptyset(&sa_usr1.sa_mask);
    CHECK("install SIGUSR1 handler", sigaction(SIGUSR1, &sa_usr1, NULL) == 0,
          "errno=%d (%s)", errno, strerror(errno));

    struct sigaction sa_alarm;
    memset(&sa_alarm, 0, sizeof(sa_alarm));
    sa_alarm.sa_handler = alarm_handler;
    sigemptyset(&sa_alarm.sa_mask);
    CHECK("install SIGALRM guard", sigaction(SIGALRM, &sa_alarm, NULL) == 0,
          "errno=%d (%s)", errno, strerror(errno));

    sigset_t block_usr1;
    sigemptyset(&block_usr1);
    sigaddset(&block_usr1, SIGUSR1);
    sigaddset(&block_usr1, SIGUSR2);
    CHECK("block SIGUSR1 and SIGUSR2 before sigsuspend",
          sigprocmask(SIG_BLOCK, &block_usr1, NULL) == 0,
          "errno=%d (%s)", errno, strerror(errno));
    CHECK("SIGUSR1 is blocked before wait", mask_has(SIGUSR1) == 1,
          "mask_has=%d", mask_has(SIGUSR1));
    CHECK("SIGUSR2 is also blocked before wait", mask_has(SIGUSR2) == 1,
          "mask_has=%d", mask_has(SIGUSR2));

    pid_t child = fork();
    if (child < 0) {
        CHECK("fork sender child", 0, "child=%ld errno=%d (%s)", (long)child,
              errno, strerror(errno));
    } else if (child > 0) {
        CHECK("fork sender child", 1, "child=%ld", (long)child);
    }
    if (child == 0) {
        usleep(100000);
        kill(getppid(), SIGUSR1);
        _exit(0);
    }

    sigset_t wait_mask;
    sigemptyset(&wait_mask);
    alarm(5);
    errno = 0;
    int rc = sigsuspend(&wait_mask);
    int saved_errno = errno;
    alarm(0);

    CHECK("sigsuspend returns -1", rc == -1, "got=%d errno=%d (%s)", rc,
          saved_errno, strerror(saved_errno));
    CHECK("sigsuspend reports EINTR", saved_errno == EINTR,
          "got errno=%d (%s)", saved_errno, strerror(saved_errno));
    CHECK("SIGUSR1 handler ran instead of timeout", got_usr1 && !got_alarm,
          "got_usr1=%d got_alarm=%d", got_usr1, got_alarm);
    CHECK("old mask is restored after sigsuspend", mask_has(SIGUSR1) == 1,
          "mask_has=%d", mask_has(SIGUSR1));
    CHECK("unrelated blocked signal is restored after sigsuspend",
          mask_has(SIGUSR2) == 1, "mask_has=%d", mask_has(SIGUSR2));
    CHECK("wakeup signal is not left pending after handler",
          pending_has(SIGUSR1) == 0, "pending_has=%d", pending_has(SIGUSR1));

    if (child > 0) {
        int status;
        waitpid(child, &status, 0);
        CHECK("sender child exits normally",
              WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "status=0x%x", status);
    }

    sigprocmask(SIG_UNBLOCK, &block_usr1, NULL);

    printf("=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED: test-sigsuspend\n");
    } else {
        printf("TEST FAILED: test-sigsuspend\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
