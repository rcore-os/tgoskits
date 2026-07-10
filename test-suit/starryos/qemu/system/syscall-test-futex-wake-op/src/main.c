#define _GNU_SOURCE

#include "test_framework.h"

#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdatomic.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif

#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif

#ifndef FUTEX_WAKE_OP
#define FUTEX_WAKE_OP 5
#endif

#ifndef FUTEX_PRIVATE_FLAG
#define FUTEX_PRIVATE_FLAG 128
#endif

#ifndef FUTEX_CLOCK_REALTIME
#define FUTEX_CLOCK_REALTIME 256
#endif

#ifndef FUTEX_OP_SET
#define FUTEX_OP_SET 0
#endif

#ifndef FUTEX_OP_ADD
#define FUTEX_OP_ADD 1
#endif

#ifndef FUTEX_OP_OR
#define FUTEX_OP_OR 2
#endif

#ifndef FUTEX_OP_ANDN
#define FUTEX_OP_ANDN 3
#endif

#ifndef FUTEX_OP_XOR
#define FUTEX_OP_XOR 4
#endif

#ifndef FUTEX_OP_OPARG_SHIFT
#define FUTEX_OP_OPARG_SHIFT 8
#endif

#ifndef FUTEX_OP_CMP_EQ
#define FUTEX_OP_CMP_EQ 0
#endif

#ifndef FUTEX_OP_CMP_NE
#define FUTEX_OP_CMP_NE 1
#endif

#ifndef FUTEX_OP_CMP_LT
#define FUTEX_OP_CMP_LT 2
#endif

#ifndef FUTEX_OP_CMP_LE
#define FUTEX_OP_CMP_LE 3
#endif

#ifndef FUTEX_OP_CMP_GT
#define FUTEX_OP_CMP_GT 4
#endif

#ifndef FUTEX_OP_CMP_GE
#define FUTEX_OP_CMP_GE 5
#endif

#define FUTEX_OP_ENCODE(op, oparg, cmp, cmparg) \
    ((((op) & 0xf) << 28) | (((cmp) & 0xf) << 24) | \
     (((oparg) & 0xfff) << 12) | ((cmparg) & 0xfff))

static uint32_t wake_word;
static uint32_t op_word;
static _Atomic int waiter1_ready;
static _Atomic int waiter2_ready;
static _Atomic int waiter1_ret;
static _Atomic int waiter2_ret;

struct waiter_args {
    uint32_t *word;
    uint32_t expected;
    _Atomic int *ready;
    _Atomic int *result;
};

static long raw_futex(uint32_t *uaddr, int op, uint32_t val,
                      const struct timespec *timeout, uint32_t *uaddr2,
                      uint32_t val3)
{
    errno = 0;
    return syscall(SYS_futex, uaddr, op, val, timeout, uaddr2, val3);
}

static const struct timespec *futex_count_arg(uint32_t count)
{
    return (const struct timespec *)(uintptr_t)count;
}

static void short_settle(void)
{
    const struct timespec ts = {
        .tv_sec = 0,
        .tv_nsec = 50 * 1000 * 1000,
    };
    nanosleep(&ts, NULL);
}

static void *futex_waiter_thread(void *arg)
{
    struct waiter_args *args = arg;
    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };

    atomic_store_explicit(args->ready, 1, memory_order_release);
    long ret = raw_futex(args->word, FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                         args->expected, &timeout, NULL, 0);
    atomic_store_explicit(args->result,
                          (int)((ret == 0) ? 0 : -errno),
                          memory_order_release);
    return NULL;
}

static void join_thread(pthread_t thread)
{
    int err = pthread_join(thread, NULL);
    CHECK(err == 0, "pthread_join succeeds");
    if (err != 0) {
        _exit(1);
    }
}

static void reset_words(uint32_t first, uint32_t second)
{
    wake_word = first;
    op_word = second;
    atomic_store_explicit(&waiter1_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&waiter2_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&waiter1_ret, INT_MIN, memory_order_relaxed);
    atomic_store_explicit(&waiter2_ret, INT_MIN, memory_order_relaxed);
}

static void wait_until_ready(const _Atomic int *ready)
{
    while (atomic_load_explicit(ready, memory_order_acquire) == 0) {
        sched_yield();
    }
}

static void test_wake_op_rmw_operations(void)
{
    printf("\n--- FUTEX_WAKE_OP read-modify-write operations ---\n");

    reset_words(0, 7);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_SET, 42,
                                        FUTEX_OP_CMP_EQ, 7)),
              0, "SET operation succeeds with no waiters");
    CHECK(op_word == 42, "SET stores the encoded operand");

    reset_words(0, 10);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 5,
                                        FUTEX_OP_CMP_EQ, 10)),
              0, "ADD operation succeeds with no waiters");
    CHECK(op_word == 15, "ADD uses the old futex word");

    reset_words(0, 0x10);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_OR, 0x03,
                                        FUTEX_OP_CMP_EQ, 0x10)),
              0, "OR operation succeeds with no waiters");
    CHECK(op_word == 0x13, "OR updates bits in uaddr2");

    reset_words(0, 0x1f);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ANDN, 0x05,
                                        FUTEX_OP_CMP_GE, 0)),
              0, "ANDN operation succeeds with no waiters");
    CHECK(op_word == 0x1a, "ANDN clears the encoded operand bits");

    reset_words(0, 0x55);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_XOR, 0x0f,
                                        FUTEX_OP_CMP_NE, 0)),
              0, "XOR operation succeeds with no waiters");
    CHECK(op_word == 0x5a, "XOR toggles the encoded operand bits");

    reset_words(0, 0);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_OR | FUTEX_OP_OPARG_SHIFT,
                                        5, FUTEX_OP_CMP_EQ, 0)),
              0, "OPARG_SHIFT operation succeeds with no waiters");
    CHECK(op_word == 32, "OPARG_SHIFT converts the operand to 1 << operand");
}

static void test_wake_op_wakes_both_futexes(void)
{
    printf("\n--- FUTEX_WAKE_OP wakes the primary and conditional futex ---\n");
    pthread_t first;
    pthread_t second;
    struct waiter_args first_args;
    struct waiter_args second_args;

    reset_words(0, 0);
    first_args = (struct waiter_args) {
        .word = &wake_word,
        .expected = 0,
        .ready = &waiter1_ready,
        .result = &waiter1_ret,
    };
    second_args = (struct waiter_args) {
        .word = &op_word,
        .expected = 0,
        .ready = &waiter2_ready,
        .result = &waiter2_ret,
    };

    int err = pthread_create(&first, NULL, futex_waiter_thread, &first_args);
    CHECK(err == 0, "pthread_create primary waiter succeeds");
    if (err != 0) {
        return;
    }
    err = pthread_create(&second, NULL, futex_waiter_thread, &second_args);
    CHECK(err == 0, "pthread_create secondary waiter succeeds");
    if (err != 0) {
        return;
    }
    wait_until_ready(&waiter1_ready);
    wait_until_ready(&waiter2_ready);
    short_settle();

    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 1,
                        futex_count_arg(1), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_EQ, 0)),
              2, "WAKE_OP wakes val waiters and val2 conditional waiters");

    join_thread(first);
    join_thread(second);
    CHECK(atomic_load_explicit(&waiter1_ret, memory_order_acquire) == 0,
          "primary waiter returns after WAKE_OP");
    CHECK(atomic_load_explicit(&waiter2_ret, memory_order_acquire) == 0,
          "secondary waiter returns when comparison matches");
    CHECK(op_word == 1, "WAKE_OP updates uaddr2 before returning");
}

static void test_wake_op_comparison_controls_second_wake(void)
{
    printf("\n--- FUTEX_WAKE_OP comparison controls the secondary wake ---\n");
    pthread_t first;
    pthread_t second;
    struct waiter_args first_args;
    struct waiter_args second_args;

    reset_words(0, 5);
    first_args = (struct waiter_args) {
        .word = &wake_word,
        .expected = 0,
        .ready = &waiter1_ready,
        .result = &waiter1_ret,
    };
    second_args = (struct waiter_args) {
        .word = &op_word,
        .expected = 5,
        .ready = &waiter2_ready,
        .result = &waiter2_ret,
    };

    int err = pthread_create(&first, NULL, futex_waiter_thread, &first_args);
    CHECK(err == 0, "pthread_create primary waiter for cmp-false case succeeds");
    if (err != 0) {
        return;
    }
    err = pthread_create(&second, NULL, futex_waiter_thread, &second_args);
    CHECK(err == 0, "pthread_create secondary waiter for cmp-false case succeeds");
    if (err != 0) {
        return;
    }
    wait_until_ready(&waiter1_ready);
    wait_until_ready(&waiter2_ready);
    short_settle();

    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 1,
                        futex_count_arg(1), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_EQ, 0)),
              1, "WAKE_OP still wakes uaddr when the uaddr2 comparison fails");
    CHECK(op_word == 6, "WAKE_OP still updates uaddr2 when comparison fails");
    short_settle();
    CHECK(atomic_load_explicit(&waiter2_ret, memory_order_acquire) == INT_MIN,
          "secondary waiter remains asleep when comparison fails");

    CHECK_RET(raw_futex(&op_word, FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL,
                        NULL, 0),
              1, "manual FUTEX_WAKE releases the secondary waiter");

    join_thread(first);
    join_thread(second);
    CHECK(atomic_load_explicit(&waiter1_ret, memory_order_acquire) == 0,
          "primary waiter returns after cmp-false WAKE_OP");
    CHECK(atomic_load_explicit(&waiter2_ret, memory_order_acquire) == 0,
          "secondary waiter returns after explicit wake");
}

static void test_wake_op_comparisons(void)
{
    printf("\n--- FUTEX_WAKE_OP comparison predicates ---\n");

    reset_words(0, 5);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_LT, 6)),
              0, "CMP_LT predicate is accepted");
    CHECK(op_word == 6, "CMP_LT case updates uaddr2");

    reset_words(0, 5);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_LE, 5)),
              0, "CMP_LE predicate is accepted");
    CHECK(op_word == 6, "CMP_LE case updates uaddr2");

    reset_words(0, 5);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_GT, 4)),
              0, "CMP_GT predicate is accepted");
    CHECK(op_word == 6, "CMP_GT case updates uaddr2");

    reset_words(0, 5);
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1,
                                        FUTEX_OP_CMP_GE, 5)),
              0, "CMP_GE predicate is accepted");
    CHECK(op_word == 6, "CMP_GE case updates uaddr2");
}

static void test_wake_op_validation(void)
{
    printf("\n--- FUTEX_WAKE_OP validation ---\n");
    uint32_t aligned[2] = {0, 0};
    uint32_t *unaligned = (uint32_t *)((uintptr_t)&aligned[0] + 1);
    uint32_t op = FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1, FUTEX_OP_CMP_EQ, 0);

    reset_words(0, 0);
    CHECK_ERR(raw_futex(unaligned, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word, op),
              EINVAL, "WAKE_OP rejects an unaligned uaddr");
    CHECK_ERR(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), unaligned, op),
              EINVAL, "WAKE_OP rejects an unaligned uaddr2");
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG,
                        (uint32_t)-1, futex_count_arg(0), &op_word, op),
              0, "WAKE_OP accepts a large unsigned val wake count");
    CHECK_RET(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg((uint32_t)-1), &op_word, op),
              0, "WAKE_OP accepts a large unsigned val2 wake count");
    CHECK_ERR(raw_futex(&wake_word,
                        FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG |
                            FUTEX_CLOCK_REALTIME,
                        0, futex_count_arg(0), &op_word, op),
              ENOSYS, "WAKE_OP rejects FUTEX_CLOCK_REALTIME as unsupported");
    CHECK_ERR(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), NULL, op),
              EFAULT, "WAKE_OP rejects a NULL uaddr2");
    CHECK_ERR(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(0xf, 1, FUTEX_OP_CMP_EQ, 0)),
              ENOSYS, "WAKE_OP rejects an unknown arithmetic op");
    CHECK_ERR(raw_futex(&wake_word, FUTEX_WAKE_OP | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), &op_word,
                        FUTEX_OP_ENCODE(FUTEX_OP_ADD, 1, 0xf, 0)),
              ENOSYS, "WAKE_OP rejects an unknown comparison op");
}

int main(void)
{
    TEST_START("futex WAKE_OP syscall");

    test_wake_op_rmw_operations();
    test_wake_op_wakes_both_futexes();
    test_wake_op_comparison_controls_second_wake();
    test_wake_op_comparisons();
    test_wake_op_validation();

    TEST_DONE();
}
