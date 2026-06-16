#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

#ifndef SYS_rt_tgsigqueueinfo
#ifdef __NR_rt_tgsigqueueinfo
#define SYS_rt_tgsigqueueinfo __NR_rt_tgsigqueueinfo
#endif
#endif

static int passed;
static int failed;
static int ready_pipe[2];
static volatile pid_t worker_tid;
static int worker_got;
static int worker_value;
static int worker_code;
static pid_t worker_sender_pid;
static uid_t worker_sender_uid;
static int worker_errno;

#define CHECK(label, cond, fmt, ...)                                      \
    do {                                                                  \
        if (cond) {                                                       \
            printf("  PASS | rt_tgsigqueueinfo | %s\n", label);          \
            passed++;                                                     \
        } else {                                                          \
            printf("  FAIL | rt_tgsigqueueinfo | %s | " fmt "\n", label, \
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

static void *waiter_thread(void *arg)
{
    int signo = *(int *)arg;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, signo);
    if (sigprocmask(SIG_BLOCK, &set, NULL) != 0) {
        worker_errno = errno;
        return NULL;
    }

    worker_tid = (pid_t)syscall(SYS_gettid);
    char byte = 'r';
    if (write(ready_pipe[1], &byte, 1) != 1) {
        worker_errno = errno;
        return NULL;
    }

    struct timespec timeout = { .tv_sec = 5, .tv_nsec = 0 };
    siginfo_t info;
    memset(&info, 0, sizeof(info));
    errno = 0;
    worker_got = sigtimedwait(&set, &info, &timeout);
    worker_errno = errno;
    worker_value = info.si_value.sival_int;
    worker_code = info.si_code;
    worker_sender_pid = info.si_pid;
    worker_sender_uid = info.si_uid;
    return NULL;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== test-tgsigqueueinfo: rt_tgsigqueueinfo targets one thread ===\n");

    int signo = SIGRTMIN + 2;
    int value = 0x5447;
    CHECK("create ready pipe", pipe(ready_pipe) == 0, "errno=%d (%s)", errno,
          strerror(errno));

    pthread_t thread;
    CHECK("create waiter thread", pthread_create(&thread, NULL, waiter_thread,
                                                 &signo) == 0,
          "errno=%d (%s)", errno, strerror(errno));

    char byte;
    CHECK("waiter reports ready", read(ready_pipe[0], &byte, 1) == 1,
          "errno=%d (%s)", errno, strerror(errno));
    CHECK("waiter tid captured", worker_tid > 0, "worker_tid=%ld",
          (long)worker_tid);

    siginfo_t info;
    fill_siginfo(&info, signo, value);
    errno = 0;
    long rc = syscall(SYS_rt_tgsigqueueinfo, (long)getpid(), (long)worker_tid,
                      (long)signo, &info);
    CHECK("rt_tgsigqueueinfo sends to waiter tid", rc == 0,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    CHECK("join waiter", pthread_join(thread, NULL) == 0, "errno=%d (%s)",
          errno, strerror(errno));
    CHECK("target thread received signal", worker_got == signo,
          "got=%d want=%d errno=%d (%s)", worker_got, signo, worker_errno,
          strerror(worker_errno));
    CHECK("target thread received siginfo value", worker_value == value,
          "got=%d want=%d", worker_value, value);
    CHECK("target thread received SI_QUEUE code", worker_code == SI_QUEUE,
          "got=%d want=%d", worker_code, SI_QUEUE);
    CHECK("target thread received sender pid", worker_sender_pid == getpid(),
          "got=%ld want=%ld", (long)worker_sender_pid, (long)getpid());
    CHECK("target thread received sender uid", worker_sender_uid == getuid(),
          "got=%ld want=%ld", (long)worker_sender_uid, (long)getuid());

    fill_siginfo(&info, signo, 9);
    errno = 0;
    rc = syscall(SYS_rt_tgsigqueueinfo, (long)getpid(), (long)(worker_tid + 999999),
                 (long)signo, &info);
    CHECK("invalid tid is rejected", rc == -1 && errno == ESRCH,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    fill_siginfo(&info, signo, 10);
    errno = 0;
    rc = syscall(SYS_rt_tgsigqueueinfo, (long)(getpid() + 999999),
                 (long)worker_tid, (long)signo, &info);
    CHECK("invalid tgid is rejected", rc == -1 && errno == ESRCH,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    fill_siginfo(&info, SIGRTMAX + 1, 11);
    errno = 0;
    rc = syscall(SYS_rt_tgsigqueueinfo, (long)getpid(), (long)syscall(SYS_gettid),
                 (long)(SIGRTMAX + 1), &info);
    CHECK("invalid signal number is rejected", rc == -1 && errno == EINVAL,
          "got=%ld errno=%d (%s)", rc, errno, strerror(errno));

    close(ready_pipe[0]);
    close(ready_pipe[1]);

    printf("=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED: test-tgsigqueueinfo\n");
    } else {
        printf("TEST FAILED: test-tgsigqueueinfo\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
