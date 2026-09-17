#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int setup_failed(const char *operation)
{
    fprintf(stderr, "RTTIME %s failed: errno=%d\n", operation, errno);
    return 1;
}

static void on_timeout(int signal)
{
    (void)signal;
    _exit(124);
}

static void *observe_process_signal(void *observed)
{
    sigset_t pending;
    *(int *)observed = sigpending(&pending) == 0 &&
                       sigismember(&pending, SIGXCPU) == 1;
    return NULL;
}

static int exercise_soft_limit(void)
{
    struct sigaction action = { .sa_handler = on_timeout };
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGALRM, &action, NULL) != 0)
        return setup_failed("SIGALRM handler");
    const struct itimerval timeout = { .it_value = { .tv_sec = 10 } };
    if (syscall(SYS_setitimer, ITIMER_REAL, &timeout, NULL) != 0)
        return setup_failed("setitimer timeout");
    sigset_t blocked, pending;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGXCPU);
    if (sigprocmask(SIG_BLOCK, &blocked, NULL) != 0)
        return setup_failed("block SIGXCPU");

    /* Keep another CPU available for deferred signal/accounting work. */
    cpu_set_t allowed, selected;
    CPU_ZERO(&allowed);
    if (syscall(SYS_sched_getaffinity, 0, sizeof(allowed), &allowed) < 0)
        return setup_failed("sched_getaffinity");
    int cpu = -1;
    for (int i = 0; i < CPU_SETSIZE; i++) {
        if (CPU_ISSET(i, &allowed)) {
            cpu = i;
            if (i != 0)
                break;
        }
    }
    if (cpu < 0)
        return 1;
    CPU_ZERO(&selected);
    CPU_SET(cpu, &selected);
    if (syscall(SYS_sched_setaffinity, 0, sizeof(selected), &selected) != 0)
        return setup_failed("sched_setaffinity");

    const rlim_t initial_soft = 1000;
    struct rlimit limit = { .rlim_cur = initial_soft, .rlim_max = 5000000 };
    if (syscall(SYS_prlimit64, 0, RLIMIT_RTTIME, &limit, NULL) != 0)
        return setup_failed("prlimit64 set");
    struct sched_param policy = { .sched_priority = 1 };
    if (syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &policy) != 0)
        return setup_failed("sched_setscheduler FIFO");
    struct timespec started, now;
    if (clock_gettime(CLOCK_MONOTONIC, &started) != 0)
        return setup_failed("clock_gettime start");
    /* No blocking or yield: a true wake would reset the RT watchdog. */
    do {
        if (sigpending(&pending) != 0)
            return setup_failed("sigpending");
        if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
            return setup_failed("clock_gettime progress");
        if (now.tv_sec - started.tv_sec >= 15) {
            sigset_t mask;
            struct itimerval remaining;
            if (sigprocmask(SIG_BLOCK, NULL, &mask) != 0 ||
                syscall(SYS_getitimer, ITIMER_REAL, &remaining) != 0)
                return setup_failed("timeout state");
            fprintf(stderr, "RTTIME no SIGXCPU: SIGALRM pending=%d blocked=%d "
                    "remaining=%lld.%06lld\n",
                    sigismember(&pending, SIGALRM), sigismember(&mask, SIGALRM),
                    (long long)remaining.it_value.tv_sec,
                    (long long)remaining.it_value.tv_usec);
            return 1;
        }
    } while (!sigismember(&pending, SIGXCPU));
    policy.sched_priority = 0;
    if (syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &policy) != 0)
        return setup_failed("sched_setscheduler OTHER");
    if (syscall(SYS_prlimit64, 0, RLIMIT_RTTIME, NULL, &limit) != 0)
        return setup_failed("prlimit64 query");
    printf("RTTIME after SIGXCPU: soft=%llu hard=%llu\n",
           (unsigned long long)limit.rlim_cur,
           (unsigned long long)limit.rlim_max);
    /* A new thread inherits the blocked mask, not private pending signals. */
    pthread_t observer;
    int observed = 0;
    int error = pthread_create(&observer, NULL, observe_process_signal, &observed);
    if (error) {
        errno = error;
        return setup_failed("pthread_create");
    }
    error = pthread_join(observer, NULL);
    if (error) {
        errno = error;
        return setup_failed("pthread_join");
    }
    alarm(0);
    printf("RTTIME SIGXCPU visible to sibling: %d\n", observed);
    /* Delayed observation may include further one-second notifications. */
    return limit.rlim_cur < initial_soft + 1000000 ||
           (limit.rlim_cur - initial_soft) % 1000000 != 0 ||
           !observed;
}

int test_rttime_soft_limit(void)
{
    fflush(NULL);
    pid_t child = fork();
    if (child < 0)
        return 1;
    if (child == 0) {
        int result = exercise_soft_limit();
        if (result)
            fprintf(stderr, "RTTIME child failure: errno=%d\n", errno);
        fflush(NULL);
        _exit(result);
    }
    int status;
    pid_t waited;
    do {
        waited = waitpid(child, &status, 0);
    } while (waited < 0 && errno == EINTR);
    if (waited != child)
        return 1;
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "RTTIME child wait status=%d\n", status);
        return 1;
    }
    return 0;
}
