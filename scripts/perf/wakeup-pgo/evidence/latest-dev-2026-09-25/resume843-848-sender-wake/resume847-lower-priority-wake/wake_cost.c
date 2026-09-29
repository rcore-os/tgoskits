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

enum { WARMUP = 1000, SAMPLES = 20000, TOTAL = WARMUP + SAMPLES };

struct state {
    _Atomic int ready;
    _Atomic int armed;
    _Atomic int gate;
    _Atomic int done;
    _Atomic int empty;
    _Atomic int receiver_error;
    uint64_t empty_samples[SAMPLES];
    uint64_t parked_samples[SAMPLES];
    int missed;
};

static struct state state;

static uint64_t now_ns(void)
{
    struct timespec now;
    if (syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &now) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)now.tv_sec * 1000000000ULL + (uint64_t)now.tv_nsec;
}

static int wake_one(_Atomic int *word)
{
    return syscall(SYS_futex, word, FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
                   1, NULL, NULL, 0);
}

static int wait_until(_Atomic int *word, int sequence)
{
    for (;;) {
        int observed = atomic_load_explicit(word, memory_order_acquire);
        if (observed == sequence) {
            return 0;
        }
        if (observed > sequence) {
            return EPROTO;
        }
        struct timespec timeout = {.tv_sec = 10};
        if (syscall(SYS_futex, word, FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                    observed, &timeout, NULL, 0) < 0 &&
            errno != EAGAIN && errno != EINTR) {
            return errno;
        }
    }
}

static int pin_and_schedule(int priority)
{
    cpu_set_t cpus;
    CPU_ZERO(&cpus);
    CPU_SET(0, &cpus);
    int error = pthread_setaffinity_np(pthread_self(), sizeof(cpus), &cpus);
    if (error != 0) {
        return error;
    }
    struct sched_param param = {.sched_priority = priority};
    error = pthread_setschedparam(pthread_self(), SCHED_FIFO, &param);
    return error;
}

static void *receiver(void *unused)
{
    (void)unused;
    int error = pin_and_schedule(1);
    atomic_store_explicit(&state.receiver_error, error, memory_order_release);
    atomic_store_explicit(&state.ready, 1, memory_order_release);
    wake_one(&state.ready);
    if (error != 0) {
        return (void *)1;
    }
    for (int sequence = 1; sequence <= TOTAL; sequence++) {
        atomic_store_explicit(&state.armed, sequence, memory_order_release);
        wake_one(&state.armed);
        error = wait_until(&state.gate, sequence);
        if (error != 0) {
            atomic_store(&state.receiver_error, error);
            return (void *)1;
        }
        atomic_store_explicit(&state.done, sequence, memory_order_release);
        wake_one(&state.done);
    }
    return NULL;
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static void report(const char *name, uint64_t *samples)
{
    qsort(samples, SAMPLES, sizeof(*samples), compare_u64);
    printf("RESUME847_RESULT {\"case\":\"%s\",\"samples\":%d,"
           "\"p50_ns\":%llu,\"p99_ns\":%llu,\"p999_ns\":%llu}\n",
           name, SAMPLES,
           (unsigned long long)samples[(SAMPLES - 1) / 2],
           (unsigned long long)samples[((SAMPLES - 1) * 99) / 100],
           (unsigned long long)samples[((SAMPLES - 1) * 999) / 1000]);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    pthread_t thread;
    int error = pthread_create(&thread, NULL, receiver, NULL);
    if (error != 0) {
        fprintf(stderr, "pthread_create: %d\n", error);
        return 1;
    }
    error = wait_until(&state.ready, 1);
    if (error == 0) {
        error = atomic_load_explicit(&state.receiver_error, memory_order_acquire);
    }
    if (error == 0) {
        error = pin_and_schedule(80);
    }
    if (error != 0) {
        fprintf(stderr, "setup: %d\n", error);
        return 1;
    }
    printf("RESUME847_POLICY sender=%d receiver=%d cpu=%d\n",
           sched_getscheduler(0), SCHED_FIFO, sched_getcpu());

    for (int sequence = 1; sequence <= TOTAL; sequence++) {
        error = wait_until(&state.armed, sequence);
        if (error != 0) {
            fprintf(stderr, "armed: %d\n", error);
            return 1;
        }
        struct timespec settle = {.tv_nsec = 50000};
        while (nanosleep(&settle, &settle) != 0) {
            if (errno != EINTR) {
                perror("nanosleep");
                return 1;
            }
        }
        uint64_t start = now_ns();
        int empty_count = wake_one(&state.empty);
        uint64_t empty_elapsed = now_ns() - start;
        atomic_store_explicit(&state.gate, sequence, memory_order_release);
        start = now_ns();
        int parked_count = wake_one(&state.gate);
        uint64_t parked_elapsed = now_ns() - start;
        if (empty_count != 0 || parked_count != 1) {
            state.missed++;
        }
        if (sequence > WARMUP) {
            int index = sequence - WARMUP - 1;
            state.empty_samples[index] = empty_elapsed;
            state.parked_samples[index] = parked_elapsed;
        }
        error = wait_until(&state.done, sequence);
        if (error != 0) {
            fprintf(stderr, "done: %d\n", error);
            return 1;
        }
    }
    void *result;
    error = pthread_join(thread, &result);
    if (error != 0 || result != NULL || state.missed != 0) {
        printf("RESUME847_INVALID join=%d receiver=%d missed=%d\n",
               error, atomic_load(&state.receiver_error), state.missed);
        return 1;
    }
    report("empty", state.empty_samples);
    report("parked_lower_priority", state.parked_samples);
    puts("RESUME847_DONE 0");
    return 0;
}
