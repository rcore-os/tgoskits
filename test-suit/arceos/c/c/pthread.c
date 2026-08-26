#include "test.h"

#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static pthread_mutex_t BASIC_LOCK = PTHREAD_MUTEX_INITIALIZER;
static volatile int DETACH_STARTED;
static volatile int DETACH_RELEASE;
static volatile int DETACH_FINISHED;
static volatile uintptr_t STACK_PROBE_RESULT;

__attribute__((noinline)) static uintptr_t consume_stack(unsigned depth, uintptr_t seed)
{
    volatile unsigned char frame[16 * 1024];
    uintptr_t checksum = seed;

    for (size_t offset = 0; offset < sizeof(frame); offset += 4096) {
        frame[offset] = (unsigned char)(seed + depth + offset);
        checksum += frame[offset];
    }
    if (depth != 0) {
        checksum ^= consume_stack(depth - 1, checksum + depth);
    }
    return checksum + frame[(depth * 4096) % sizeof(frame)];
}

static void *return_arg_thread(void *arg)
{
    if (arg == NULL) {
        return NULL;
    }
    return arg;
}

static void *exit_thread(void *arg)
{
    (void)arg;
    pthread_exit("exit message");
    return NULL;
}

static void *increment_thread(void *arg)
{
    int *value = (int *)arg;

    pthread_mutex_lock(&BASIC_LOCK);
    *value += 1;
    pthread_mutex_unlock(&BASIC_LOCK);
    return NULL;
}

static void *self_join_thread(void *arg)
{
    (void)arg;
    return (void *)(uintptr_t)pthread_join(pthread_self(), NULL);
}

static void *controlled_detach_thread(void *arg)
{
    (void)arg;
    __atomic_store_n(&DETACH_STARTED, 1, __ATOMIC_RELEASE);
    while (!__atomic_load_n(&DETACH_RELEASE, __ATOMIC_ACQUIRE)) {
        usleep(1);
    }
    __atomic_store_n(&DETACH_FINISHED, 1, __ATOMIC_RELEASE);
    return NULL;
}

static void *configured_stack_thread(void *arg)
{
    STACK_PROBE_RESULT = consume_stack(12, 0x5a);
    return arg;
}

int arceos_c_test_pthread_basic(char *reason, size_t reason_len)
{
    enum { THREADS = 32 };
    pthread_t threads[THREADS];
    pthread_t t;
    void *result = NULL;
    char message[] = "child return message";
    pthread_attr_t attr;
    size_t stack_size = 0;
    int value = 0;

    CHECK_TRUE(pthread_self() != 0);
    CHECK_RET(pthread_mutex_lock(&BASIC_LOCK), 0);
    CHECK_RET(pthread_mutex_trylock(&BASIC_LOCK), EBUSY);
    CHECK_RET(pthread_mutex_unlock(&BASIC_LOCK), 0);
    CHECK_RET(pthread_create(&t, NULL, return_arg_thread, NULL), 0);
    CHECK_RET(pthread_join(t, NULL), 0);
    CHECK_RET(pthread_create(&t, NULL, return_arg_thread, message), 0);
    CHECK_RET(pthread_join(t, &result), 0);
    CHECK_TRUE(result == message);

    result = NULL;
    CHECK_RET(pthread_create(&t, NULL, exit_thread, NULL), 0);
    CHECK_RET(pthread_join(t, &result), 0);
    CHECK_TRUE(result != NULL);
    CHECK_RET(strcmp((const char *)result, "exit message"), 0);

    for (int i = 0; i < THREADS; i++) {
        CHECK_RET(pthread_create(&threads[i], NULL, increment_thread, &value), 0);
    }
    for (int i = 0; i < THREADS; i++) {
        CHECK_RET(pthread_join(threads[i], NULL), 0);
    }
    CHECK_RET(value, THREADS);

    CHECK_RET(pthread_attr_init(&attr), 0);
    CHECK_RET(pthread_attr_getstacksize(&attr, &stack_size), 0);
    CHECK_RET(stack_size, 128 * 1024);
    CHECK_RET(pthread_attr_setstacksize(&attr, PTHREAD_STACK_MIN - 1), EINVAL);
    CHECK_RET(pthread_attr_setstacksize(&attr, SIZE_MAX), EINVAL);
    CHECK_RET(pthread_attr_setstacksize(&attr, 384 * 1024), 0);
    CHECK_RET(pthread_attr_getstacksize(&attr, &stack_size), 0);
    CHECK_RET(stack_size, 384 * 1024);
    STACK_PROBE_RESULT = 0;
    CHECK_RET(pthread_create(&t, &attr, configured_stack_thread, message), 0);
    CHECK_RET(pthread_join(t, &result), 0);
    CHECK_TRUE(result == message);
    CHECK_TRUE(STACK_PROBE_RESULT != 0);

    result = NULL;
    CHECK_RET(pthread_create(&t, NULL, self_join_thread, NULL), 0);
    CHECK_RET(pthread_join(t, &result), 0);
    CHECK_RET((uintptr_t)result, EDEADLK);

    DETACH_STARTED = 0;
    DETACH_RELEASE = 0;
    DETACH_FINISHED = 0;
    CHECK_RET(pthread_create(&t, NULL, controlled_detach_thread, NULL), 0);
    while (!__atomic_load_n(&DETACH_STARTED, __ATOMIC_ACQUIRE)) {
        usleep(1);
    }
    CHECK_RET(pthread_detach(t), 0);
    CHECK_RET(pthread_detach(t), EINVAL);
    int detached_join = pthread_join(t, NULL);
    CHECK_TRUE(detached_join == EINVAL || detached_join == ESRCH);
    __atomic_store_n(&DETACH_RELEASE, 1, __ATOMIC_RELEASE);
    while (!__atomic_load_n(&DETACH_FINISHED, __ATOMIC_ACQUIRE)) {
        usleep(1);
    }

    puts("pthread_basic: pthread APIs OK");
    return 0;
}

#define NUM_DATA  4096
#define NUM_TASKS 8

static uint64_t VALUES[NUM_DATA];

static uint64_t sqrt_floor(uint64_t n)
{
    uint64_t x = n;

    if (n == 0) {
        return 0;
    }

    while (1) {
        if (x * x <= n && (x + 1) * (x + 1) > n) {
            return x;
        }
        x = (x + n / x) / 2;
    }
}

struct parallel_arg {
    int id;
    uint64_t partial;
};

static void *parallel_thread(void *arg)
{
    struct parallel_arg *param = (struct parallel_arg *)arg;
    int left = param->id * (NUM_DATA / NUM_TASKS);
    int right = left + (NUM_DATA / NUM_TASKS);

    for (int i = left; i < right; i++) {
        param->partial += sqrt_floor(VALUES[i]);
    }
    return NULL;
}

int arceos_c_test_pthread_parallel(char *reason, size_t reason_len)
{
    pthread_t threads[NUM_TASKS];
    struct parallel_arg args[NUM_TASKS];
    uint64_t expect = 0;
    uint64_t actual = 0;

    srand(0x1234);
    for (int i = 0; i < NUM_DATA; i++) {
        VALUES[i] = (uint64_t)rand();
        expect += sqrt_floor(VALUES[i]);
    }

    for (int i = 0; i < NUM_TASKS; i++) {
        args[i].id = i;
        args[i].partial = 0;
        CHECK_RET(pthread_create(&threads[i], NULL, parallel_thread, &args[i]), 0);
    }
    for (int i = 0; i < NUM_TASKS; i++) {
        CHECK_RET(pthread_join(threads[i], NULL), 0);
        actual += args[i].partial;
    }

    CHECK_RET(actual, expect);
    printf("pthread_parallel: actual sum = %llu\n", (unsigned long long)actual);
    return 0;
}
