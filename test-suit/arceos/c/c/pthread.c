#include "test.h"

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static pthread_mutex_t BASIC_LOCK = PTHREAD_MUTEX_INITIALIZER;

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

int arceos_c_test_pthread_basic(char *reason, size_t reason_len)
{
    enum { THREADS = 32 };
    pthread_t threads[THREADS];
    pthread_t t;
    void *result = NULL;
    char message[] = "child return message";
    int value = 0;

    CHECK_TRUE(pthread_self() != 0);
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
