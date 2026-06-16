#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_rt_sigqueueinfo
#ifdef __NR_rt_sigqueueinfo
#define SYS_rt_sigqueueinfo __NR_rt_sigqueueinfo
#endif
#endif

static int passed;
static int failed;
static uid_t sender_uid;

#define CHECK(label, cond, fmt, ...)                                      \
    do {                                                                  \
        if (cond) {                                                       \
            printf("  PASS | rt_sigqueueinfo | %s\n", label);            \
            passed++;                                                     \
        } else {                                                          \
            printf("  FAIL | rt_sigqueueinfo | %s | " fmt "\n", label,   \
                   ##__VA_ARGS__);                                        \
            failed++;                                                     \
        }                                                                 \
    } while (0)

static void fill_siginfo(siginfo_t *si, int signo, int value)
{
    memset(si, 0, sizeof(*si));
    si->si_signo = signo;
    si->si_code = SI_QUEUE;
    si->si_pid = getpid();
    si->si_uid = getuid();
    si->si_value.sival_int = value;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-sigqueueinfo: rt_sigqueueinfo siginfo preservation ===\n");

    int signo = SIGRTMIN + 1;
    int value = 0x5149;
    sender_uid = getuid();
    int ready_pipe[2];
    CHECK("create ready pipe", pipe(ready_pipe) == 0, "errno=%d (%s)", errno,
          strerror(errno));

    pid_t child = fork();
    if (child < 0) {
        CHECK("fork receiver child", 0, "child=%ld errno=%d (%s)",
              (long)child, errno, strerror(errno));
    } else if (child > 0) {
        CHECK("fork receiver child", 1, "child=%ld", (long)child);
    }
    if (child == 0) {
        close(ready_pipe[0]);

        int child_failed = 0;
        printf("  INFO | rt_sigqueueinfo | child receiver pid=%ld\n",
               (long)getpid());

        sigset_t set;
        sigemptyset(&set);
        sigaddset(&set, signo);
        if (sigprocmask(SIG_BLOCK, &set, NULL) != 0) {
            printf("  FAIL | rt_sigqueueinfo | child blocks signal | errno=%d (%s)\n",
                   errno, strerror(errno));
            _exit(2);
        }

        char byte = 'r';
        if (write(ready_pipe[1], &byte, 1) != 1) {
            printf("  FAIL | rt_sigqueueinfo | child ready write | errno=%d (%s)\n",
                   errno, strerror(errno));
            _exit(2);
        }
        close(ready_pipe[1]);

        struct timespec timeout = { .tv_sec = 5, .tv_nsec = 0 };
        siginfo_t recv_info;
        memset(&recv_info, 0, sizeof(recv_info));
        errno = 0;
        int got = sigtimedwait(&set, &recv_info, &timeout);
        int saved_errno = errno;

        if (got != signo) {
            printf("  FAIL | rt_sigqueueinfo | child receives queued signal | "
                   "expected=%d observed=%d errno=%d (%s)\n",
                   signo, got, saved_errno, strerror(saved_errno));
            child_failed++;
        }
        if (recv_info.si_signo != signo) {
            printf("  FAIL | rt_sigqueueinfo | child si_signo | expected=%d observed=%d\n",
                   signo, recv_info.si_signo);
            child_failed++;
        }
        if (recv_info.si_code != SI_QUEUE) {
            printf("  FAIL | rt_sigqueueinfo | child si_code | expected=%d observed=%d\n",
                   SI_QUEUE, recv_info.si_code);
            child_failed++;
        }
        if (recv_info.si_pid != getppid()) {
            printf("  FAIL | rt_sigqueueinfo | child si_pid | expected=%ld observed=%ld\n",
                   (long)getppid(), (long)recv_info.si_pid);
            child_failed++;
        }
        if (recv_info.si_uid != sender_uid) {
            printf("  FAIL | rt_sigqueueinfo | child si_uid | expected=%ld observed=%ld\n",
                   (long)sender_uid, (long)recv_info.si_uid);
            child_failed++;
        }
        if (recv_info.si_value.sival_int != value) {
            printf("  FAIL | rt_sigqueueinfo | child si_value | expected=%d observed=%d\n",
                   value, recv_info.si_value.sival_int);
            child_failed++;
        }
        if (child_failed == 0) {
            printf("  INFO | rt_sigqueueinfo | child received preserved siginfo\n");
        }
        _exit(child_failed == 0 ? 0 : 1);
    }

    close(ready_pipe[1]);
    char byte;
    CHECK("receiver child reports ready", read(ready_pipe[0], &byte, 1) == 1,
          "errno=%d (%s)", errno, strerror(errno));
    close(ready_pipe[0]);

    siginfo_t send_info;
    fill_siginfo(&send_info, signo, value);
    errno = 0;
    long rc = syscall(SYS_rt_sigqueueinfo, (long)child, (long)signo,
                      &send_info);
    CHECK("rt_sigqueueinfo queues to child process", rc == 0,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    int status;
    CHECK("wait receiver child", waitpid(child, &status, 0) == child,
          "errno=%d (%s)", errno, strerror(errno));
    CHECK("receiver child validates siginfo",
          WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "status=0x%x", status);

    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, signo);

    siginfo_t bad_info;
    fill_siginfo(&bad_info, SIGRTMAX + 1, 7);
    errno = 0;
    rc = syscall(SYS_rt_sigqueueinfo, (long)getpid(), (long)(SIGRTMAX + 1),
                 &bad_info);
    CHECK("invalid signal number is rejected", rc == -1 && errno == EINVAL,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    fill_siginfo(&bad_info, signo, 8);
    errno = 0;
    rc = syscall(SYS_rt_sigqueueinfo, (long)child, (long)signo, &bad_info);
    CHECK("exited receiver pid is rejected", rc == -1 && errno == ESRCH,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    sigprocmask(SIG_UNBLOCK, &set, NULL);

    printf("=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED: test-sigqueueinfo\n");
    } else {
        printf("TEST FAILED: test-sigqueueinfo\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
