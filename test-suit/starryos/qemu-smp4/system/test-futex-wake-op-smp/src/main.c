#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef FUTEX_WAKE_OP
#define FUTEX_WAKE_OP 5
#endif

#ifndef FUTEX_PRIVATE_FLAG
#define FUTEX_PRIVATE_FLAG 128
#endif

#ifndef FUTEX_OP_ADD
#define FUTEX_OP_ADD 1
#endif

#ifndef FUTEX_OP_CMP_EQ
#define FUTEX_OP_CMP_EQ 0
#endif

#define FUTEX_OP_ENCODE(op, oparg, cmp, cmparg) \
    ((((op) & 0xf) << 28) | (((cmp) & 0xf) << 24) | \
     (((oparg) & 0xfff) << 12) | ((cmparg) & 0xfff))

enum {
    THREAD_COUNT = 8,
    ITERATIONS = 10000,
};

static uint32_t wake_word;
static uint32_t op_word;
static atomic_int ready_count;
static atomic_int start_flag;
static atomic_int first_errno;

static long raw_futex(uint32_t *uaddr, int op, uint32_t val,
                      const void *timeout_or_count, uint32_t *uaddr2,
                      uint32_t val3)
{
    errno = 0;
    return syscall(SYS_futex, uaddr, op, val, timeout_or_count, uaddr2, val3);
}

static const void *futex_count_arg(uint32_t count)
{
    return (const void *)(uintptr_t)count;
}

static void record_errno_once(int err)
{
    int expected = 0;
    atomic_compare_exchange_strong_explicit(&first_errno, &expected, err,
                                            memory_order_release,
                                            memory_order_relaxed);
}

static void *wake_op_worker(void *arg)
{
    (void)arg;

    const uint32_t encoded_add =
        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1, FUTEX_OP_CMP_EQ, 0xfff);

    atomic_fetch_add_explicit(&ready_count, 1, memory_order_release);
    while (atomic_load_explicit(&start_flag, memory_order_acquire) == 0) {
        sched_yield();
    }

    for (int i = 0; i < ITERATIONS; i++) {
        long ret = raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                             futex_count_arg(0), &op_word, encoded_add);
        if (ret != 0) {
            record_errno_once(errno);
            return NULL;
        }
    }

    return NULL;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);

    pthread_t threads[THREAD_COUNT];
    for (int i = 0; i < THREAD_COUNT; i++) {
        int err = pthread_create(&threads[i], NULL, wake_op_worker, NULL);
        if (err != 0) {
            printf("FAIL: pthread_create[%d] errno=%d\n", i, err);
            return 1;
        }
    }

    while (atomic_load_explicit(&ready_count, memory_order_acquire) !=
           THREAD_COUNT) {
        sched_yield();
    }
    atomic_store_explicit(&start_flag, 1, memory_order_release);

    for (int i = 0; i < THREAD_COUNT; i++) {
        int err = pthread_join(threads[i], NULL);
        if (err != 0) {
            printf("FAIL: pthread_join[%d] errno=%d\n", i, err);
            return 1;
        }
    }

    int futex_errno = atomic_load_explicit(&first_errno, memory_order_acquire);
    if (futex_errno != 0) {
        printf("FAIL: FUTEX_WAKE_OP syscall errno=%d\n", futex_errno);
        return 1;
    }

    const uint32_t expected = THREAD_COUNT * ITERATIONS;
    if (op_word != expected) {
        printf("FAIL: FUTEX_WAKE_OP SMP counter got %u, expected %u\n",
               op_word, expected);
        return 1;
    }

    printf("PASS: FUTEX_WAKE_OP SMP atomic ADD %u/%u\n", op_word, expected);
    return 0;
}
