#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#define FUTEX_WAIT_BITSET 9
#define FUTEX_PRIVATE_FLAG 128
#define FUTEX_CLOCK_REALTIME 256
#define FUTEX_BITSET_MATCH_ANY UINT32_MAX

struct wait_probe {
    int sleep;
    struct timespec deadline;
    _Atomic uint32_t word;
    _Atomic int ready;
    _Atomic int done;
    long result;
    int error;
};

static void require(int condition, const char *operation)
{
    if (!condition) {
        fprintf(stderr, "FAIL: clock-step wait: %s errno=%d\n", operation, errno);
        puts("STARRY_GROUPED_TEST_FAILED: syscall-test-clock-settime");
        fflush(NULL);
        _exit(EXIT_FAILURE);
    }
}

static void *waiter(void *arg)
{
    struct wait_probe *probe = arg;
    struct sched_param priority = {.sched_priority = 80};
    require(syscall(SYS_sched_setscheduler, 0, SCHED_FIFO, &priority) == 0,
            "set waiter FIFO priority");
    atomic_store_explicit(&probe->ready, 1, memory_order_release);
    errno = 0;
    if (probe->sleep) {
        probe->result = syscall(SYS_clock_nanosleep, CLOCK_REALTIME,
                                TIMER_ABSTIME, &probe->deadline, NULL);
    } else {
        probe->result = syscall(SYS_futex, &probe->word,
                                FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG |
                                    FUTEX_CLOCK_REALTIME,
                                0, &probe->deadline, NULL, FUTEX_BITSET_MATCH_ANY);
    }
    probe->error = errno;
    atomic_store_explicit(&probe->done, 1, memory_order_release);
    return NULL;
}

/* The caller restores the original realtime clock after these probes. */
void check_clock_step_waits(void)
{
    cpu_set_t saved_affinity, single_cpu;
    struct sched_param saved_priority;
    int saved_policy = syscall(SYS_sched_getscheduler, 0);
    require(saved_policy >= 0, "read controller policy");
    require(syscall(SYS_sched_getparam, 0, &saved_priority) == 0,
            "read controller priority");
    require(syscall(SYS_sched_getaffinity, 0, sizeof(saved_affinity),
                    &saved_affinity) >= 0, "read controller affinity");
    CPU_ZERO(&single_cpu);
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (CPU_ISSET(cpu, &saved_affinity)) {
            CPU_SET(cpu, &single_cpu);
            break;
        }
    }
    require(syscall(SYS_sched_setaffinity, 0, sizeof(single_cpu), &single_cpu) == 0,
            "pin controller and inherited waiter affinity");
    struct sched_param priority = {.sched_priority = 0};
    require(syscall(SYS_sched_setscheduler, 0, SCHED_OTHER, &priority) == 0,
            "set controller normal policy");

    /* This bounds broken notifier implementations; timeouts are not a pass. */
    alarm(5);
    for (int sleep = 0; sleep <= 1; sleep++) {
        struct timespec base;
        require(syscall(SYS_clock_gettime, CLOCK_REALTIME, &base) == 0,
                "read realtime");
        base.tv_sec += 240;
        require(syscall(SYS_clock_settime, CLOCK_REALTIME, &base) == 0,
                "establish room for a backward clock step");
        struct wait_probe probe = {.sleep = sleep, .deadline = base};
        probe.deadline.tv_sec += 60;
        pthread_t worker;
        require(pthread_create(&worker, NULL, waiter, &probe) == 0,
                "create clock-domain waiter");
        while (!atomic_load_explicit(&probe.ready, memory_order_acquire)) {
            syscall(SYS_sched_yield);
        }
        /* FIFO 80 runs until blocked before this normal-policy controller resumes.
         * Changing the word without waking must not turn a queued wait into
         * EAGAIN when the wall clock notification rebuilds its deadline. */
        atomic_store_explicit(&probe.word, 1, memory_order_release);
        struct timespec backward = base;
        backward.tv_sec -= 120;
        require(syscall(SYS_clock_settime, CLOCK_REALTIME, &backward) == 0,
                "move the pending absolute deadline farther away");
        require(!atomic_load_explicit(&probe.done, memory_order_acquire),
                "backward step must keep the original waiter pending");
        struct timespec forward = probe.deadline;
        forward.tv_sec++;
        require(syscall(SYS_clock_settime, CLOCK_REALTIME, &forward) == 0,
                "move realtime beyond the original absolute deadline");
        require(pthread_join(worker, NULL) == 0, "join expired waiter");
        if (sleep) {
            require(probe.result == 0, "absolute clock_nanosleep completes");
        } else {
            require(probe.result == -1 && probe.error == ETIMEDOUT,
                    "queued futex keeps its registration and expires without EAGAIN");
        }
    }
    alarm(0);
    require(syscall(SYS_sched_setscheduler, 0, saved_policy, &saved_priority) == 0,
            "restore controller policy");
    require(syscall(SYS_sched_setaffinity, 0, sizeof(saved_affinity), &saved_affinity) == 0,
            "restore controller affinity");
    puts("clock-step waits: queued futex and absolute clock_nanosleep passed");
}
