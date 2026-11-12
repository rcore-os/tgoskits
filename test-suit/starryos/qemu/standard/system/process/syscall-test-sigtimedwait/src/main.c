#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_rt_tgsigqueueinfo
#ifdef __NR_rt_tgsigqueueinfo
#define SYS_rt_tgsigqueueinfo __NR_rt_tgsigqueueinfo
#endif
#endif

static int passed;
static int failed;

#define CHECK(label, cond, fmt, ...)                                      \
    do {                                                                  \
        if (cond) {                                                       \
            printf("  PASS | rt_sigtimedwait | %s\n", label);            \
            passed++;                                                     \
        } else {                                                          \
            printf("  FAIL | rt_sigtimedwait | %s | " fmt "\n", label,   \
                   ##__VA_ARGS__);                                        \
            failed++;                                                     \
        }                                                                 \
    } while (0)

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-sigtimedwait: timeout, nullable info, siginfo ===\n");

    int signo = SIGRTMIN + 3;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, signo);
    CHECK("block waited signal", sigprocmask(SIG_BLOCK, &set, NULL) == 0,
          "errno=%d (%s)", errno, strerror(errno));

    struct timespec short_timeout = { .tv_sec = 0, .tv_nsec = 1000000 };
    errno = 0;
    int got = sigtimedwait(&set, NULL, &short_timeout);
    int saved_errno = errno;
    CHECK("timeout returns -1/EAGAIN", got == -1 && saved_errno == EAGAIN,
          "got=%d errno=%d (%s)", got, saved_errno, strerror(saved_errno));

    struct timespec bad_timeout = { .tv_sec = 0, .tv_nsec = 1000000000L };
    errno = 0;
    got = sigtimedwait(&set, NULL, &bad_timeout);
    saved_errno = errno;
    CHECK("invalid timeout nsec is rejected", got == -1 && saved_errno == EINVAL,
          "got=%d errno=%d (%s)", got, saved_errno, strerror(saved_errno));

    pid_t tid = (pid_t)syscall(SYS_gettid);
    CHECK("tgkill for nullable info case",
          syscall(SYS_tgkill, (long)getpid(), (long)tid, (long)signo) == 0,
          "errno=%d (%s)", errno, strerror(errno));
    struct timespec wait_timeout = { .tv_sec = 5, .tv_nsec = 0 };
    errno = 0;
    got = sigtimedwait(&set, NULL, &wait_timeout);
    saved_errno = errno;
    CHECK("pending signal can be consumed with info NULL", got == signo,
          "got=%d want=%d errno=%d (%s)", got, signo, saved_errno,
          strerror(saved_errno));

    int value = 0x7156;
    siginfo_t send_info;
    memset(&send_info, 0, sizeof(send_info));
    send_info.si_signo = signo;
    send_info.si_code = SI_QUEUE;
    send_info.si_pid = getpid();
    send_info.si_uid = getuid();
    send_info.si_value.sival_int = value;
    CHECK("rt_tgsigqueueinfo for siginfo case",
          syscall(SYS_rt_tgsigqueueinfo, (long)getpid(), (long)tid, (long)signo,
                  &send_info) == 0,
          "errno=%d (%s)", errno, strerror(errno));
    siginfo_t info;
    memset(&info, 0, sizeof(info));
    errno = 0;
    got = sigtimedwait(&set, &info, &wait_timeout);
    saved_errno = errno;
    CHECK("pending signal returns through sigtimedwait", got == signo,
          "got=%d want=%d errno=%d (%s)", got, signo, saved_errno,
          strerror(saved_errno));
    CHECK("siginfo signo is preserved", info.si_signo == signo,
          "got=%d want=%d", info.si_signo, signo);
    CHECK("siginfo value is preserved", info.si_value.sival_int == value,
          "got=%d want=%d", info.si_value.sival_int, value);

    errno = 0;
    got = sigtimedwait(&set, NULL, &short_timeout);
    saved_errno = errno;
    CHECK("consumed signal is not returned twice", got == -1 && saved_errno == EAGAIN,
          "got=%d errno=%d (%s)", got, saved_errno, strerror(saved_errno));

    sigprocmask(SIG_UNBLOCK, &set, NULL);

    printf("=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED: test-sigtimedwait\n");
    } else {
        printf("TEST FAILED: test-sigtimedwait\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
