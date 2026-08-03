#define _GNU_SOURCE

#include <errno.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifdef BENCH_INIT
#include <linux/reboot.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#endif

enum {
    WARMUP_ITERATIONS = 200,
    MEASURED_ITERATIONS = 2000,
    MEASURED_RUNS = 7,
};

struct ping_pong_state {
    _Atomic int turn;
    _Atomic int ready;
    _Atomic int start;
    int iterations;
};

static int futex_wait_private(_Atomic int *word, int expected)
{
    for (;;) {
        long result = syscall(SYS_futex, word, FUTEX_WAIT_PRIVATE, expected,
                              NULL, NULL, 0);
        if (result == 0 || errno == EAGAIN) {
            return 0;
        }
        if (errno != EINTR) {
            return -1;
        }
    }
}

static int futex_wake_private(_Atomic int *word)
{
    return syscall(SYS_futex, word, FUTEX_WAKE_PRIVATE, 1, NULL, NULL, 0) < 0
               ? -1
               : 0;
}

static int pin_current_thread(int cpu)
{
    cpu_set_t cpuset;
    CPU_ZERO(&cpuset);
    CPU_SET(cpu, &cpuset);
    return pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);
}

static uint64_t monotonic_ns(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)now.tv_sec * 1000000000ULL + (uint64_t)now.tv_nsec;
}

static void wait_for_value(_Atomic int *word, int value)
{
    while (atomic_load_explicit(word, memory_order_acquire) != value) {
        int observed = atomic_load_explicit(word, memory_order_relaxed);
        if (observed != value && futex_wait_private(word, observed) != 0) {
            perror("futex wait");
            exit(1);
        }
    }
}

static void *pong_thread(void *argument)
{
    struct ping_pong_state *state = argument;
    int affinity_error = pin_current_thread(1);
    if (affinity_error != 0) {
        atomic_store_explicit(&state->ready, -affinity_error,
                              memory_order_release);
        (void)futex_wake_private(&state->ready);
        return (void *)(uintptr_t)1;
    }
    atomic_store_explicit(&state->ready, 1, memory_order_release);
    if (futex_wake_private(&state->ready) != 0) {
        return (void *)(uintptr_t)2;
    }
    wait_for_value(&state->start, 1);

    for (int iteration = 0; iteration < state->iterations; iteration++) {
        wait_for_value(&state->turn, 1);
        atomic_store_explicit(&state->turn, 0, memory_order_release);
        if (futex_wake_private(&state->turn) != 0) {
            return (void *)(uintptr_t)3;
        }
    }
    return NULL;
}

static uint64_t run_ping_pong(int iterations)
{
    struct ping_pong_state state = {
        .turn = 0,
        .ready = 0,
        .start = 0,
        .iterations = iterations,
    };
    pthread_t pong;
    int error = pthread_create(&pong, NULL, pong_thread, &state);
    if (error != 0) {
        errno = error;
        perror("pthread_create");
        exit(1);
    }
    while (atomic_load_explicit(&state.ready, memory_order_acquire) == 0) {
        if (futex_wait_private(&state.ready, 0) != 0) {
            perror("futex ready wait");
            exit(1);
        }
    }
    int ready = atomic_load_explicit(&state.ready, memory_order_acquire);
    if (ready != 1) {
        fprintf(stderr, "worker affinity failed: %d\n", -ready);
        exit(1);
    }

    atomic_store_explicit(&state.start, 1, memory_order_release);
    if (futex_wake_private(&state.start) != 0) {
        perror("futex start wake");
        exit(1);
    }
    uint64_t started = monotonic_ns();
    for (int iteration = 0; iteration < iterations; iteration++) {
        atomic_store_explicit(&state.turn, 1, memory_order_release);
        if (futex_wake_private(&state.turn) != 0) {
            perror("futex ping wake");
            exit(1);
        }
        wait_for_value(&state.turn, 0);
    }
    uint64_t elapsed = monotonic_ns() - started;

    void *thread_result = NULL;
    error = pthread_join(pong, &thread_result);
    if (error != 0 || thread_result != NULL) {
        fprintf(stderr, "pthread_join failed: error=%d worker=%zu\n", error,
                (size_t)(uintptr_t)thread_result);
        exit(1);
    }
    return elapsed;
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static int run_benchmark(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (sysconf(_SC_NPROCESSORS_ONLN) < 2) {
        fprintf(stderr, "need at least two online CPUs\n");
        return 1;
    }
    int affinity_error = pin_current_thread(0);
    if (affinity_error != 0) {
        errno = affinity_error;
        perror("pin main thread");
        return 1;
    }

    (void)run_ping_pong(WARMUP_ITERATIONS);
    uint64_t samples[MEASURED_RUNS];
    for (int run = 0; run < MEASURED_RUNS; run++) {
        samples[run] = run_ping_pong(MEASURED_ITERATIONS);
        printf("FUTEX_PING_PONG_RUN run=%d iterations=%d elapsed_ns=%llu "
               "handoff_ns=%llu\n",
               run, MEASURED_ITERATIONS,
               (unsigned long long)samples[run],
               (unsigned long long)(samples[run] /
                                    (2ULL * MEASURED_ITERATIONS)));
    }
    qsort(samples, MEASURED_RUNS, sizeof(samples[0]), compare_u64);
    uint64_t median = samples[MEASURED_RUNS / 2];
    printf("FUTEX_PING_PONG_RESULT runs=%d iterations=%d median_elapsed_ns=%llu "
           "median_handoff_ns=%llu min_handoff_ns=%llu max_handoff_ns=%llu\n",
           MEASURED_RUNS, MEASURED_ITERATIONS,
           (unsigned long long)median,
           (unsigned long long)(median / (2ULL * MEASURED_ITERATIONS)),
           (unsigned long long)(samples[0] /
                                (2ULL * MEASURED_ITERATIONS)),
           (unsigned long long)(samples[MEASURED_RUNS - 1] /
                                (2ULL * MEASURED_ITERATIONS)));
    printf("FUTEX_PING_PONG_PASSED\n");
    return 0;
}

int main(void)
{
#ifdef BENCH_INIT
    (void)mount("proc", "/proc", "proc", 0, NULL);
    printf("LINUX_RT_BENCH_BOOTED\n");
#endif
    int status = run_benchmark();
#ifdef BENCH_INIT
    sync();
    reboot(LINUX_REBOOT_CMD_POWER_OFF);
    for (;;) {
        pause();
    }
#endif
    return status;
}
