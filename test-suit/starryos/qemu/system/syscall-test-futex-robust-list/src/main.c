#define _GNU_SOURCE

#include "test_framework.h"

#include <stddef.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif

#ifndef FUTEX_WAKE
#define FUTEX_WAKE 1
#endif

#ifndef FUTEX_WAIT_BITSET
#define FUTEX_WAIT_BITSET 9
#endif

#ifndef FUTEX_WAKE_BITSET
#define FUTEX_WAKE_BITSET 10
#endif

#ifndef FUTEX_REQUEUE
#define FUTEX_REQUEUE 3
#endif

#ifndef FUTEX_CMP_REQUEUE
#define FUTEX_CMP_REQUEUE 4
#endif

#ifndef FUTEX_PRIVATE_FLAG
#define FUTEX_PRIVATE_FLAG 128
#endif

#ifndef FUTEX_CLOCK_REALTIME
#define FUTEX_CLOCK_REALTIME 256
#endif

#ifndef FUTEX_BITSET_MATCH_ANY
#define FUTEX_BITSET_MATCH_ANY 0xffffffffu
#endif

#ifndef FUTEX_OWNER_DIED
#define FUTEX_OWNER_DIED 0x40000000u
#endif

#ifndef FUTEX_WAITERS
#define FUTEX_WAITERS 0x80000000u
#endif

#ifndef FUTEX_TID_MASK
#define FUTEX_TID_MASK 0x3fffffffu
#endif

#define FUTEX_INVALID_OP 0x7f
#define FUTEX_UNKNOWN_OPTION 0x200
#define NS_PER_SEC 1000000000L

struct local_robust_list {
    struct local_robust_list *next;
};

struct local_robust_list_head {
    struct local_robust_list list;
    long futex_offset;
    struct local_robust_list *list_op_pending;
};

struct robust_test_node {
    struct local_robust_list list;
    _Atomic uint32_t futex_word;
};

static _Atomic uint32_t futex_word = 0;
static _Atomic uint32_t requeue_src = 0;
static _Atomic uint32_t requeue_dst = 0;
static _Atomic int waiter_ready = 0;
static _Atomic int target_waiter_ready = 0;
static _Atomic int source_waiter_ready = 0;
static _Atomic int cmp_requeue_ready[3];
static _Atomic int cmp_requeue_ret[3];
static _Atomic int bitset_waiter_ret = 0;

static struct local_robust_list_head robust_head;
static struct local_robust_list_head robust_syscall_head;
static struct robust_test_node robust_node;
static struct local_robust_list_head robust_pending_head;
static struct robust_test_node robust_pending_node;
static _Atomic int robust_owner_ready = 0;
static _Atomic int robust_owner_can_exit = 0;
static _Atomic int robust_waiter_ready = 0;
static _Atomic int robust_waiter_ret = 0;
static _Atomic uint32_t robust_owner_tid = 0;
static _Atomic int robust_pending_owner_ready = 0;
static _Atomic int robust_pending_owner_can_exit = 0;
static _Atomic uint32_t robust_pending_owner_tid = 0;
static _Atomic uint32_t robust_wait_expected = 0;
static _Atomic int robust_owner_mode = 0;
static _Atomic int robust_query_ready = 0;
static _Atomic int robust_query_can_exit = 0;
static _Atomic uint32_t robust_query_tid = 0;
static struct local_robust_list_head robust_query_head;

static long raw_futex(uint32_t *uaddr, int op, uint32_t val,
                      const struct timespec *timeout, uint32_t *uaddr2,
                      uint32_t val3)
{
    errno = 0;
    return syscall(SYS_futex, uaddr, op, val, timeout, uaddr2, val3);
}

static long raw_set_robust_list(struct local_robust_list_head *head, size_t size)
{
    errno = 0;
    return syscall(SYS_set_robust_list, head, size);
}

static long raw_get_robust_list(pid_t tid, struct local_robust_list_head **head,
                                size_t *size)
{
    errno = 0;
    return syscall(SYS_get_robust_list, tid, head, size);
}

static void wait_child_ok(pid_t pid, const char *msg)
{
    int status = 0;
    pid_t waited;

    do {
        waited = waitpid(pid, &status, 0);
    } while (waited == -1 && errno == EINTR);

    CHECK(waited == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0, msg);
}

static void add_ms_to_timespec(struct timespec *ts, long ms)
{
    ts->tv_sec += ms / 1000;
    ts->tv_nsec += (ms % 1000) * 1000000L;
    if (ts->tv_nsec >= NS_PER_SEC) {
        ts->tv_sec++;
        ts->tv_nsec -= NS_PER_SEC;
    }
}

static struct timespec monotonic_deadline_ms(long ms)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    add_ms_to_timespec(&ts, ms);
    return ts;
}

static struct timespec realtime_deadline_ms(long ms)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    add_ms_to_timespec(&ts, ms);
    return ts;
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

static long elapsed_ms(const struct timespec *start, const struct timespec *end)
{
    return (end->tv_sec - start->tv_sec) * 1000L +
           (end->tv_nsec - start->tv_nsec) / 1000000L;
}

static uint32_t shared_atomic_load(uint32_t *word)
{
    return __sync_fetch_and_add(word, 0);
}

static void *basic_waiter_thread(void *arg)
{
    (void)arg;
    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };

    atomic_store_explicit(&waiter_ready, 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&futex_word, FUTEX_WAIT | FUTEX_PRIVATE_FLAG,
                         0, &timeout, NULL, 0);
    return (void *)(intptr_t)((ret == 0) ? 0 : -errno);
}

static void *bitset_waiter_thread(void *arg)
{
    (void)arg;
    struct timespec deadline = monotonic_deadline_ms(5000);

    atomic_store_explicit(&waiter_ready, 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&futex_word,
                         FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG, 0,
                         &deadline, NULL, 0x2);
    atomic_store_explicit(&bitset_waiter_ret,
                          (int)((ret == 0) ? 0 : -errno),
                          memory_order_release);
    return NULL;
}

static void *requeue_target_waiter_thread(void *arg)
{
    (void)arg;
    const struct timespec timeout = {
        .tv_sec = 0,
        .tv_nsec = 800 * 1000 * 1000,
    };

    atomic_store_explicit(&target_waiter_ready, 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&requeue_dst,
                         FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &timeout, NULL, 0);
    return (void *)(intptr_t)((ret == 0) ? 0 : -errno);
}

static void *requeue_source_waiter_thread(void *arg)
{
    (void)arg;
    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };

    atomic_store_explicit(&source_waiter_ready, 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&requeue_src,
                         FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &timeout, NULL, 0);
    return (void *)(intptr_t)((ret == 0) ? 0 : -errno);
}

static void *cmp_requeue_source_waiter_thread(void *arg)
{
    int index = *(int *)arg;
    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };

    atomic_store_explicit(&cmp_requeue_ready[index], 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&requeue_src,
                         FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &timeout, NULL, 0);
    atomic_store_explicit(&cmp_requeue_ret[index],
                          (int)((ret == 0) ? 0 : -errno),
                          memory_order_release);
    return NULL;
}

static void join_thread(pthread_t thread, void **result)
{
    int err = pthread_join(thread, result);
    CHECK(err == 0, "pthread_join succeeds");
    if (err != 0) {
        printf("pthread_join failed: %d (%s)\n", err, strerror(err));
        exit(1);
    }
}

static void test_futex_basic(void)
{
    printf("\n--- futex basic semantics ---\n");
    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);

    CHECK_RET(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0),
              0, "FUTEX_WAKE with no waiters returns 0");

    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0),
              EAGAIN, "FUTEX_WAIT returns EAGAIN when value differs");

    const struct timespec timeout = {
        .tv_sec = 0,
        .tv_nsec = 20 * 1000 * 1000,
    };
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &timeout, NULL, 0),
              ETIMEDOUT, "FUTEX_WAIT relative timeout returns ETIMEDOUT");

    const struct timespec invalid_timeout = {
        .tv_sec = 0,
        .tv_nsec = NS_PER_SEC,
    };
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &invalid_timeout,
                        NULL, 0),
              EINVAL, "FUTEX_WAIT rejects tv_nsec >= 1000000000");

    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_INVALID_OP, 0, NULL,
                        NULL, 0),
              ENOSYS, "invalid futex operation returns ENOSYS");

    CHECK_ERR(raw_futex(NULL, FUTEX_WAIT, 0, NULL, NULL, 0),
              EFAULT, "FUTEX_WAIT rejects a NULL user pointer");
}

static void test_futex_op_flag_validation(void)
{
    printf("\n--- futex op/flag validation (Linux ABI) ---\n");
    struct timespec rel_timeout = {
        .tv_sec = 0,
        .tv_nsec = 1,
    };
    struct timespec realtime_past = realtime_deadline_ms(-1000);

    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);

    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_INVALID_OP, 0, NULL,
                        NULL, 0),
              ENOSYS, "Linux ABI: unknown futex op returns ENOSYS");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG | FUTEX_UNKNOWN_OPTION,
                        0, &rel_timeout, NULL, 0),
              EINVAL, "Linux ABI: unknown futex option bits return EINVAL");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME,
                        0, &rel_timeout, NULL, 0),
              ETIMEDOUT,
              "Linux ABI: FUTEX_WAIT accepts CLOCK_REALTIME with relative timeout");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG |
                            FUTEX_CLOCK_REALTIME,
                        0, &realtime_past, NULL, FUTEX_BITSET_MATCH_ANY),
              ETIMEDOUT,
              "Linux ABI: FUTEX_WAIT_BITSET accepts CLOCK_REALTIME absolute timeout");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE | FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME,
                        1, NULL, NULL, 0),
              EINVAL, "Linux ABI: FUTEX_WAKE rejects CLOCK_REALTIME");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE_BITSET | FUTEX_PRIVATE_FLAG |
                            FUTEX_CLOCK_REALTIME,
                        1, NULL, NULL, FUTEX_BITSET_MATCH_ANY),
              EINVAL, "Linux ABI: FUTEX_WAKE_BITSET rejects CLOCK_REALTIME");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_REQUEUE | FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME,
                        0, futex_count_arg(0), (uint32_t *)&requeue_dst, 0),
              EINVAL, "Linux ABI: FUTEX_REQUEUE rejects CLOCK_REALTIME");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &rel_timeout,
                        NULL, 0),
              ETIMEDOUT,
              "Linux ABI: FUTEX_WAIT without CLOCK_REALTIME uses relative timeout");
}

static void test_futex_address_and_count_validation(void)
{
    printf("\n--- futex address/alignment/count validation (Linux ABI) ---\n");
    uint32_t aligned_words[2] = {0, 0};
    uint32_t *unaligned = (uint32_t *)((uintptr_t)&aligned_words[0] + 1);
    uint32_t *bad_page = mmap(NULL, 4096, PROT_NONE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(bad_page != MAP_FAILED, "mmap inaccessible futex page succeeds");
    if (bad_page == MAP_FAILED) {
        return;
    }

    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);
    atomic_store_explicit(&requeue_dst, 0, memory_order_relaxed);

    CHECK_ERR(raw_futex(NULL, FUTEX_WAKE, 1, NULL, NULL, 0),
              EFAULT, "Linux ABI: FUTEX_WAKE(NULL) returns EFAULT");
    CHECK_ERR(raw_futex(unaligned, FUTEX_WAKE, 1, NULL, NULL, 0),
              EINVAL, "Linux ABI: FUTEX_WAKE rejects an unaligned uaddr");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_WAKE,
                        (uint32_t)-1, NULL, NULL, 0),
              EINVAL, "Linux ABI: FUTEX_WAKE rejects a negative count");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_REQUEUE,
                        (uint32_t)-1, futex_count_arg(0),
                        (uint32_t *)&requeue_dst, 0),
              EINVAL, "Linux ABI: FUTEX_REQUEUE rejects negative val");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_REQUEUE, 0,
                        futex_count_arg((uint32_t)-1),
                        (uint32_t *)&requeue_dst, 0),
              EINVAL, "Linux ABI: FUTEX_REQUEUE rejects negative val2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_REQUEUE, 0,
                        futex_count_arg(0), NULL, 0),
              EFAULT, "Linux ABI: FUTEX_REQUEUE rejects NULL uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_REQUEUE, 0,
                        futex_count_arg(0), unaligned, 0),
              EINVAL, "Linux ABI: FUTEX_REQUEUE rejects unaligned uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_REQUEUE, 0,
                        futex_count_arg(0), bad_page, 0),
              EFAULT, "Linux ABI: FUTEX_REQUEUE rejects inaccessible uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_CMP_REQUEUE, 0,
                        futex_count_arg(0), NULL, 0),
              EFAULT, "Linux ABI: FUTEX_CMP_REQUEUE rejects NULL uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_CMP_REQUEUE, 0,
                        futex_count_arg(0), unaligned, 0),
              EINVAL, "Linux ABI: FUTEX_CMP_REQUEUE rejects unaligned uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_CMP_REQUEUE, 0,
                        futex_count_arg(0), bad_page, 0),
              EFAULT, "Linux ABI: FUTEX_CMP_REQUEUE rejects inaccessible uaddr2");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_CMP_REQUEUE,
                        (uint32_t)-1, futex_count_arg(0),
                        (uint32_t *)&requeue_dst, 0),
              EINVAL, "Linux ABI: FUTEX_CMP_REQUEUE rejects negative val");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word, FUTEX_CMP_REQUEUE, 0,
                        futex_count_arg((uint32_t)-1),
                        (uint32_t *)&requeue_dst, 0),
              EINVAL, "Linux ABI: FUTEX_CMP_REQUEUE rejects negative val2");

    CHECK(munmap(bad_page, 4096) == 0, "munmap inaccessible futex page succeeds");
}

static void test_futex_timeout_duration(void)
{
    printf("\n--- FUTEX_WAIT timeout duration ---\n");
    struct timespec start;
    struct timespec end;
    const struct timespec timeout = {
        .tv_sec = 0,
        .tv_nsec = 100 * 1000 * 1000,
    };

    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);
    CHECK(clock_gettime(CLOCK_MONOTONIC, &start) == 0, "clock_gettime start succeeds");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 0, &timeout, NULL, 0),
              ETIMEDOUT, "FUTEX_WAIT timeout returns ETIMEDOUT");
    CHECK(clock_gettime(CLOCK_MONOTONIC, &end) == 0, "clock_gettime end succeeds");
    CHECK(elapsed_ms(&start, &end) >= 50,
          "FUTEX_WAIT waits for a meaningful timeout duration");
}

static void test_futex_wait_wake(void)
{
    printf("\n--- FUTEX_WAIT/FUTEX_WAKE handoff ---\n");
    pthread_t waiter;

    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);
    atomic_store_explicit(&waiter_ready, 0, memory_order_relaxed);

    int err = pthread_create(&waiter, NULL, basic_waiter_thread, NULL);
    CHECK(err == 0, "pthread_create waiter succeeds");
    if (err != 0) {
        exit(1);
    }

    while (atomic_load_explicit(&waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    short_settle();

    atomic_store_explicit(&futex_word, 1, memory_order_release);
    CHECK_RET(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0),
              1, "FUTEX_WAKE wakes one waiter");

    void *result = NULL;
    join_thread(waiter, &result);
    CHECK((int)(intptr_t)result == 0, "FUTEX_WAIT waiter returns 0 after wake");
}

static void test_futex_shared_fork(void)
{
    printf("\n--- shared FUTEX_WAIT/FUTEX_WAKE across fork ---\n");
    uint32_t *shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(shared != MAP_FAILED, "mmap shared futex word succeeds");
    if (shared == MAP_FAILED) {
        return;
    }

    *shared = 1;
    pid_t pid = fork();
    CHECK(pid >= 0, "fork child succeeds");
    if (pid == 0) {
        usleep(50 * 1000);
        *shared = 2;
        (void)raw_futex(shared, FUTEX_WAKE, 1, NULL, NULL, 0);
        _exit(0);
    }

    const struct timespec timeout = {
        .tv_sec = 2,
        .tv_nsec = 0,
    };
    long ret = raw_futex(shared, FUTEX_WAIT, 1, &timeout, NULL, 0);
    CHECK(ret == 0 || (ret == -1 && errno == EAGAIN),
          "parent wait either blocks until child wake or observes child update");
    CHECK(*shared == 2, "shared futex word updated by child");
    wait_child_ok(pid, "fork child exits successfully");
    CHECK(munmap(shared, sizeof(*shared)) == 0, "munmap shared futex word succeeds");
}

static void test_futex_shared_private_key_fork(void)
{
    printf("\n--- shared mapping private/shared futex key across fork ---\n");
    struct fork_words {
        uint32_t futex;
        uint32_t stage;
    };
    struct fork_words *shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
                                     MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(shared != MAP_FAILED, "mmap shared fork words succeeds");
    if (shared == MAP_FAILED) {
        return;
    }

    shared->futex = 1;
    shared->stage = 0;
    pid_t pid = fork();
    CHECK(pid >= 0, "fork shared-key child succeeds");
    if (pid == 0) {
        while (shared_atomic_load(&shared->stage) == 0) {
            sched_yield();
        }
        for (int i = 0; i < 20000; i++) {
            long woke = raw_futex(&shared->futex, FUTEX_WAKE, 1, NULL, NULL, 0);
            if (woke == 1) {
                shared->futex = 2;
                _exit(0);
            }
            sched_yield();
        }
        _exit(1);
    }

    const struct timespec timeout = {
        .tv_sec = 2,
        .tv_nsec = 0,
    };
    __sync_fetch_and_add(&shared->stage, 1);
    CHECK_RET(raw_futex(&shared->futex, FUTEX_WAIT, 1, &timeout, NULL, 0), 0,
              "Linux ABI: MAP_SHARED futex without PRIVATE wakes across fork");
    wait_child_ok(pid, "shared-key child observes and wakes parent");
    CHECK(shared->futex == 2, "shared-key child updated shared futex word");

    shared->futex = 1;
    shared->stage = 0;
    pid = fork();
    CHECK(pid >= 0, "fork private-key child succeeds");
    if (pid == 0) {
        while (shared_atomic_load(&shared->stage) == 0) {
            sched_yield();
        }
        for (int i = 0; i < 1000; i++) {
            (void)raw_futex(&shared->futex, FUTEX_WAKE | FUTEX_PRIVATE_FLAG,
                            1, NULL, NULL, 0);
            sched_yield();
        }
        _exit(0);
    }

    const struct timespec private_timeout = {
        .tv_sec = 0,
        .tv_nsec = 200 * 1000 * 1000,
    };
    __sync_fetch_and_add(&shared->stage, 1);
    CHECK_ERR(raw_futex(&shared->futex, FUTEX_WAIT | FUTEX_PRIVATE_FLAG, 1,
                        &private_timeout, NULL, 0),
              ETIMEDOUT,
              "Linux ABI: FUTEX_PRIVATE_FLAG does not wake across processes");
    wait_child_ok(pid, "private-key child exits successfully");
    CHECK(munmap(shared, sizeof(*shared)) == 0, "munmap shared fork words succeeds");
}

static void test_futex_shared_wake_count(void)
{
    printf("\n--- shared FUTEX_WAKE count across forked waiters ---\n");
    uint32_t *shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    uint32_t *ready_count = mmap(NULL, sizeof(*ready_count), PROT_READ | PROT_WRITE,
                                 MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    uint32_t *returned_count = mmap(NULL, sizeof(*returned_count), PROT_READ | PROT_WRITE,
                                    MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(shared != MAP_FAILED && ready_count != MAP_FAILED && returned_count != MAP_FAILED,
          "mmap shared futex/count words succeeds");
    if (shared == MAP_FAILED || ready_count == MAP_FAILED || returned_count == MAP_FAILED) {
        return;
    }

    *shared = 1;
    *ready_count = 0;
    *returned_count = 0;
    pid_t pids[3];
    for (size_t i = 0; i < 3; i++) {
        pids[i] = fork();
        CHECK(pids[i] >= 0, "fork waiter child succeeds");
        if (pids[i] == 0) {
            const struct timespec timeout = {
                .tv_sec = 2,
                .tv_nsec = 0,
            };

            __sync_fetch_and_add(ready_count, 1);
            long ret = raw_futex(shared, FUTEX_WAIT, 1, &timeout, NULL, 0);
            if (ret == 0 || (ret == -1 && errno == EAGAIN)) {
                __sync_fetch_and_add(returned_count, 1);
                _exit(0);
            }
            _exit(1);
        }
    }

    while (shared_atomic_load(ready_count) < 3) {
        sched_yield();
    }
    usleep(100 * 1000);
    long woke = raw_futex(shared, FUTEX_WAKE, 2, NULL, NULL, 0);
    CHECK(woke >= 1 && woke <= 2, "FUTEX_WAKE(2) wakes at most two waiters");

    usleep(100 * 1000);
    uint32_t first_returned_count = shared_atomic_load(returned_count);
    CHECK(first_returned_count >= 1 && first_returned_count <= 2,
          "only the requested subset of waiters returns before final wake");

    *shared = 2;
    (void)raw_futex(shared, FUTEX_WAKE, 3, NULL, NULL, 0);
    for (size_t i = 0; i < 3; i++) {
        wait_child_ok(pids[i], "fork waiter child exits successfully");
    }
    CHECK(shared_atomic_load(returned_count) == 3, "all forked waiters eventually return");

    CHECK(munmap(shared, sizeof(*shared)) == 0, "munmap shared futex word succeeds");
    CHECK(munmap(ready_count, sizeof(*ready_count)) == 0, "munmap shared ready word succeeds");
    CHECK(munmap(returned_count, sizeof(*returned_count)) == 0, "munmap shared count word succeeds");
}

static void test_futex_requeue_id_collision_regression(void)
{
    printf("\n--- FUTEX_REQUEUE id collision regression ---\n");
    pthread_t target_waiter;
    pthread_t source_waiter;

    atomic_store_explicit(&requeue_src, 0, memory_order_relaxed);
    atomic_store_explicit(&requeue_dst, 0, memory_order_relaxed);
    atomic_store_explicit(&target_waiter_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&source_waiter_ready, 0, memory_order_relaxed);

    int err = pthread_create(&target_waiter, NULL, requeue_target_waiter_thread, NULL);
    CHECK(err == 0, "pthread_create target waiter succeeds");
    if (err != 0) {
        exit(1);
    }
    while (atomic_load_explicit(&target_waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    short_settle();

    err = pthread_create(&source_waiter, NULL, requeue_source_waiter_thread, NULL);
    CHECK(err == 0, "pthread_create source waiter succeeds");
    if (err != 0) {
        exit(1);
    }
    while (atomic_load_explicit(&source_waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    short_settle();

    long requeued = raw_futex((uint32_t *)&requeue_src,
                              FUTEX_REQUEUE | FUTEX_PRIVATE_FLAG, 0,
                              (const struct timespec *)(uintptr_t)1,
                              (uint32_t *)&requeue_dst, 0);
    CHECK_RET(requeued, 1, "FUTEX_REQUEUE moves one source waiter to target futex");

    void *target_result = NULL;
    join_thread(target_waiter, &target_result);
    CHECK((int)(intptr_t)target_result == -ETIMEDOUT,
          "original target waiter times out without removing requeued waiter");

    CHECK_RET(raw_futex((uint32_t *)&requeue_dst,
                        FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 1, NULL, NULL, 0),
              1, "target FUTEX_WAKE still sees the requeued source waiter");

    void *source_result = NULL;
    join_thread(source_waiter, &source_result);
    CHECK((int)(intptr_t)source_result == 0,
          "requeued source waiter returns after target futex wake");
}

static void test_futex_cmp_requeue_semantics(void)
{
    printf("\n--- FUTEX_CMP_REQUEUE Linux ABI semantics ---\n");
    pthread_t waiters[3];
    int indexes[3] = {0, 1, 2};

    atomic_store_explicit(&requeue_src, 0, memory_order_relaxed);
    atomic_store_explicit(&requeue_dst, 0, memory_order_relaxed);

    CHECK_ERR(raw_futex((uint32_t *)&requeue_src,
                        FUTEX_CMP_REQUEUE | FUTEX_PRIVATE_FLAG, 0,
                        futex_count_arg(0), (uint32_t *)&requeue_dst, 1),
              EAGAIN, "Linux ABI: FUTEX_CMP_REQUEUE compare mismatch returns EAGAIN");

    for (size_t i = 0; i < 3; i++) {
        atomic_store_explicit(&cmp_requeue_ready[i], 0, memory_order_relaxed);
        atomic_store_explicit(&cmp_requeue_ret[i], INT32_MIN, memory_order_relaxed);
        int err = pthread_create(&waiters[i], NULL, cmp_requeue_source_waiter_thread,
                                 &indexes[i]);
        CHECK(err == 0, "pthread_create CMP_REQUEUE source waiter succeeds");
        if (err != 0) {
            exit(1);
        }
    }
    for (size_t i = 0; i < 3; i++) {
        while (atomic_load_explicit(&cmp_requeue_ready[i],
                                    memory_order_acquire) == 0) {
            sched_yield();
        }
    }
    short_settle();

    CHECK_RET(raw_futex((uint32_t *)&requeue_src,
                        FUTEX_CMP_REQUEUE | FUTEX_PRIVATE_FLAG, 1,
                        futex_count_arg(2), (uint32_t *)&requeue_dst, 0),
              3,
              "Linux ABI: CMP_REQUEUE wakes val waiters and requeues val2 waiters");

    int direct_wake_count = 0;
    for (int spin = 0; spin < 20000 && direct_wake_count != 1; spin++) {
        direct_wake_count = 0;
        for (size_t i = 0; i < 3; i++) {
            if (atomic_load_explicit(&cmp_requeue_ret[i],
                                     memory_order_acquire) == 0) {
                direct_wake_count++;
            }
        }
        if (direct_wake_count != 1) {
            sched_yield();
        }
    }
    CHECK(direct_wake_count == 1,
          "Linux ABI: exactly one source waiter returns before target wake");

    CHECK_RET(raw_futex((uint32_t *)&requeue_dst,
                        FUTEX_WAKE | FUTEX_PRIVATE_FLAG, 2, NULL, NULL, 0),
              2, "Linux ABI: target FUTEX_WAKE wakes the two requeued waiters");

    for (size_t i = 0; i < 3; i++) {
        join_thread(waiters[i], NULL);
        CHECK(atomic_load_explicit(&cmp_requeue_ret[i],
                                   memory_order_acquire) == 0,
              "Linux ABI: CMP_REQUEUE source waiter eventually returns 0");
    }
}

static void test_futex_bitset(void)
{
    printf("\n--- FUTEX_WAIT_BITSET/FUTEX_WAKE_BITSET ---\n");
    pthread_t waiter;

    atomic_store_explicit(&futex_word, 0, memory_order_relaxed);
    atomic_store_explicit(&waiter_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&bitset_waiter_ret, INT32_MIN, memory_order_relaxed);

    int err = pthread_create(&waiter, NULL, bitset_waiter_thread, NULL);
    CHECK(err == 0, "pthread_create bitset waiter succeeds");
    if (err != 0) {
        exit(1);
    }

    while (atomic_load_explicit(&waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    short_settle();

    CHECK_RET(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE_BITSET | FUTEX_PRIVATE_FLAG, 1, NULL,
                        NULL, 0x4),
              0, "WAKE_BITSET with a disjoint mask wakes no waiters");

    CHECK(atomic_load_explicit(&bitset_waiter_ret, memory_order_acquire) == INT32_MIN,
          "waiter remains blocked after disjoint WAKE_BITSET");

    CHECK_RET(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE_BITSET | FUTEX_PRIVATE_FLAG, 1, NULL,
                        NULL, 0x2),
              1, "WAKE_BITSET with an intersecting mask wakes one waiter");

    join_thread(waiter, NULL);
    CHECK(atomic_load_explicit(&bitset_waiter_ret, memory_order_acquire) == 0,
          "WAIT_BITSET waiter returns 0 after matching wake");

    struct timespec past = monotonic_deadline_ms(-1000);
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG, 0, &past,
                        NULL, 0xffffffffu),
              ETIMEDOUT, "WAIT_BITSET uses absolute timeout and past time expires");

    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG, 0, &past,
                        NULL, 0),
              EINVAL, "WAIT_BITSET rejects val3 == 0");

    struct timespec realtime_past = realtime_deadline_ms(-1000);
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAIT_BITSET | FUTEX_PRIVATE_FLAG |
                            FUTEX_CLOCK_REALTIME,
                        0, &realtime_past, NULL, FUTEX_BITSET_MATCH_ANY),
              ETIMEDOUT,
              "Linux ABI: WAIT_BITSET|CLOCK_REALTIME accepts realtime absolute timeout");
    CHECK_ERR(raw_futex((uint32_t *)&futex_word,
                        FUTEX_WAKE_BITSET | FUTEX_PRIVATE_FLAG |
                            FUTEX_CLOCK_REALTIME,
                        1, NULL, NULL, FUTEX_BITSET_MATCH_ANY),
              EINVAL, "Linux ABI: WAKE_BITSET|CLOCK_REALTIME is invalid");
}

static void *robust_query_thread(void *arg)
{
    (void)arg;

    pid_t tid = (pid_t)syscall(SYS_gettid);
    robust_query_head.list.next = &robust_query_head.list;
    robust_query_head.futex_offset = 0;
    robust_query_head.list_op_pending = NULL;
    long ret = raw_set_robust_list(&robust_query_head, sizeof(robust_query_head));
    if (ret != 0) {
        atomic_store_explicit(&robust_query_tid, UINT32_MAX, memory_order_release);
    } else {
        atomic_store_explicit(&robust_query_tid, (uint32_t)tid, memory_order_release);
    }
    atomic_store_explicit(&robust_query_ready, 1, memory_order_release);

    while (atomic_load_explicit(&robust_query_can_exit, memory_order_acquire) == 0) {
        sched_yield();
    }
    return NULL;
}

static void test_robust_list_syscalls(void)
{
    printf("\n--- set_robust_list/get_robust_list ABI ---\n");
    struct local_robust_list_head *got_head = NULL;
    size_t got_size = 0;
    pthread_t query_thread;

    robust_syscall_head.list.next = &robust_syscall_head.list;
    robust_syscall_head.futex_offset = 0;
    robust_syscall_head.list_op_pending = NULL;

    CHECK_RET(raw_set_robust_list(&robust_syscall_head, sizeof(robust_syscall_head)), 0,
              "set_robust_list accepts a valid head and size");
    CHECK_RET(raw_get_robust_list(0, &got_head, &got_size), 0,
              "get_robust_list(0) succeeds");
    CHECK(got_head == &robust_syscall_head, "get_robust_list returns the head just set");
    CHECK(got_size == sizeof(robust_syscall_head),
          "get_robust_list returns sizeof(struct robust_list_head)");

    CHECK_ERR(raw_set_robust_list(&robust_syscall_head, sizeof(robust_syscall_head) - 1), EINVAL,
              "set_robust_list rejects an invalid size");
    CHECK_ERR(raw_get_robust_list(0, (struct local_robust_list_head **)1,
                                  &got_size),
              EFAULT, "get_robust_list rejects an invalid head output pointer");
    CHECK_ERR(raw_get_robust_list(0, &got_head, (size_t *)1),
              EFAULT, "Linux ABI: get_robust_list rejects invalid size output pointer");
    CHECK_ERR(raw_get_robust_list(0x3ffffffe, &got_head, &got_size), ESRCH,
              "get_robust_list rejects a nonexistent tid");

    atomic_store_explicit(&robust_query_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_query_can_exit, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_query_tid, 0, memory_order_relaxed);
    int err = pthread_create(&query_thread, NULL, robust_query_thread, NULL);
    CHECK(err == 0, "pthread_create robust query thread succeeds");
    if (err != 0) {
        exit(1);
    }
    while (atomic_load_explicit(&robust_query_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    uint32_t query_tid = atomic_load_explicit(&robust_query_tid,
                                              memory_order_acquire);
    CHECK(query_tid != UINT32_MAX, "other pthread set robust list");
    got_head = NULL;
    got_size = 0;
    CHECK_RET(raw_get_robust_list((pid_t)query_tid, &got_head, &got_size), 0,
              "Linux ABI: get_robust_list can query another pthread tid in the process");
    CHECK(got_head == &robust_query_head,
          "Linux ABI: get_robust_list returns the other pthread robust head");
    CHECK(got_size == sizeof(robust_query_head),
          "Linux ABI: get_robust_list returns the other pthread robust head size");
    atomic_store_explicit(&robust_query_can_exit, 1, memory_order_release);
    join_thread(query_thread, NULL);
}

static void *robust_owner_thread(void *arg)
{
    (void)arg;

    pid_t tid = (pid_t)syscall(SYS_gettid);
    int mode = atomic_load_explicit(&robust_owner_mode, memory_order_acquire);

    robust_head.futex_offset = (long)offsetof(struct robust_test_node, futex_word);
    robust_node.list.next = &robust_head.list;
    if (mode == 1) {
        robust_head.list.next = &robust_head.list;
        robust_head.list_op_pending = &robust_node.list;
    } else {
        robust_head.list.next = &robust_node.list;
        robust_head.list_op_pending = (mode == 2) ? &robust_node.list : NULL;
    }

    uint32_t futex_value =
        (((mode == 3) ? (uint32_t)tid + 1 : (uint32_t)tid) & FUTEX_TID_MASK) |
        FUTEX_WAITERS;
    atomic_store_explicit(&robust_node.futex_word, futex_value, memory_order_release);
    atomic_store_explicit(&robust_wait_expected, futex_value, memory_order_release);

    long ret = raw_set_robust_list(&robust_head, sizeof(robust_head));
    if (ret != 0) {
        atomic_store_explicit(&robust_owner_tid, UINT32_MAX, memory_order_release);
        atomic_store_explicit(&robust_owner_ready, 1, memory_order_release);
        return (void *)(intptr_t)-errno;
    }

    atomic_store_explicit(&robust_owner_tid, (uint32_t)tid, memory_order_release);
    atomic_store_explicit(&robust_owner_ready, 1, memory_order_release);

    while (atomic_load_explicit(&robust_owner_can_exit, memory_order_acquire) == 0) {
        sched_yield();
    }

    return NULL;
}

static void *robust_waiter_thread(void *arg)
{
    (void)arg;
    uint32_t expected = atomic_load_explicit(&robust_wait_expected, memory_order_acquire);
    const struct timespec timeout = {
        .tv_sec = 5,
        .tv_nsec = 0,
    };

    atomic_store_explicit(&robust_waiter_ready, 1, memory_order_release);
    long ret = raw_futex((uint32_t *)&robust_node.futex_word,
                         FUTEX_WAIT | FUTEX_PRIVATE_FLAG, expected, &timeout,
                         NULL, 0);
    atomic_store_explicit(&robust_waiter_ret,
                          (int)((ret == 0) ? 0 : -errno),
                          memory_order_release);
    return NULL;
}

static void run_robust_owner_death_case(int mode, const char *label,
                                        int expected_wait_ret,
                                        int expect_owner_died)
{
    pthread_t owner;
    pthread_t waiter;

    printf("  CASE | %s\n", label);
    atomic_store_explicit(&robust_owner_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_owner_can_exit, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_waiter_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_waiter_ret, INT32_MIN, memory_order_relaxed);
    atomic_store_explicit(&robust_owner_tid, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_wait_expected, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_owner_mode, mode, memory_order_release);
    atomic_store_explicit(&robust_node.futex_word, 0, memory_order_relaxed);

    int err = pthread_create(&owner, NULL, robust_owner_thread, NULL);
    CHECK(err == 0, "pthread_create robust owner succeeds");
    if (err != 0) {
        exit(1);
    }

    while (atomic_load_explicit(&robust_owner_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    CHECK(atomic_load_explicit(&robust_owner_tid, memory_order_acquire) != UINT32_MAX,
          "owner thread set robust list");

    err = pthread_create(&waiter, NULL, robust_waiter_thread, NULL);
    CHECK(err == 0, "pthread_create robust waiter succeeds");
    if (err != 0) {
        exit(1);
    }

    while (atomic_load_explicit(&robust_waiter_ready, memory_order_acquire) == 0) {
        sched_yield();
    }
    short_settle();

    atomic_store_explicit(&robust_owner_can_exit, 1, memory_order_release);
    join_thread(owner, NULL);
    join_thread(waiter, NULL);

    int wait_ret = atomic_load_explicit(&robust_waiter_ret, memory_order_acquire);
    uint32_t word = atomic_load_explicit(&robust_node.futex_word, memory_order_acquire);

    printf("  INFO | robust waiter ret=%d futex_word=0x%08x owner_tid=%u\n",
           wait_ret, word,
           atomic_load_explicit(&robust_owner_tid, memory_order_acquire));

    CHECK(wait_ret == expected_wait_ret,
          "Linux ABI: robust-list owner death waiter result matches expectation");
    if (expect_owner_died) {
        CHECK((word & FUTEX_OWNER_DIED) != 0,
              "Linux ABI: futex word has FUTEX_OWNER_DIED after owner exit");
        CHECK((word & FUTEX_TID_MASK) == 0,
              "Linux ABI: owner TID bits are cleared after robust owner death");
    } else {
        CHECK((word & FUTEX_OWNER_DIED) == 0,
              "Linux ABI: owner mismatch does not set FUTEX_OWNER_DIED");
    }
}

static void test_robust_list_owner_death(void)
{
    printf("\n--- robust-list owner death wakes futex waiter ---\n");

    run_robust_owner_death_case(0, "node linked in robust list", 0, 1);
    run_robust_owner_death_case(1, "list_op_pending points to node while list is empty",
                                0, 1);
    run_robust_owner_death_case(2, "list_op_pending points to node also linked in list",
                                0, 1);
    run_robust_owner_death_case(3, "futex owner TID mismatch", -ETIMEDOUT, 0);
}

static void test_robust_list_bad_chain_does_not_hang(void)
{
    printf("\n--- robust-list bad/cyclic chain hang guard ---\n");
    pid_t pid = fork();
    CHECK(pid >= 0, "fork bad robust-list child succeeds");
    if (pid == 0) {
        pid_t tid = (pid_t)syscall(SYS_gettid);
        robust_head.list.next = &robust_node.list;
        robust_head.futex_offset = (long)offsetof(struct robust_test_node, futex_word);
        robust_head.list_op_pending = NULL;
        robust_node.list.next = &robust_node.list;
        atomic_store_explicit(&robust_node.futex_word, (uint32_t)tid,
                              memory_order_release);
        if (raw_set_robust_list(&robust_head, sizeof(robust_head)) != 0) {
            _exit(2);
        }
        _exit(0);
    }

    int status = 0;
    /*
     * This guard only proves the kernel does NOT infinite-loop on a bad/cyclic
     * robust list; the exact reap latency is irrelevant. A fixed iteration cap
     * (~2s) is fragile under slow aarch64-TCG + real SMP concurrency, so bound
     * the wait by a generous wall-clock deadline instead.
     */
    struct timespec start;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &start) == 0, "clock_gettime start succeeds");
    const long deadline_ms = 60000;
    for (;;) {
        pid_t waited = waitpid(pid, &status, WNOHANG);
        if (waited == pid) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "Linux ABI: cyclic robust-list child exits without hanging cleanup");
            return;
        }
        CHECK(waited == 0 || (waited == -1 && errno == EINTR),
              "waitpid bad robust-list child is pending or interrupted");
        struct timespec now;
        if (clock_gettime(CLOCK_MONOTONIC, &now) == 0 &&
            elapsed_ms(&start, &now) > deadline_ms) {
            break;
        }
        const struct timespec pause = {
            .tv_sec = 0,
            .tv_nsec = 1000 * 1000,
        };
        nanosleep(&pause, NULL);
    }

    kill(pid, SIGKILL);
    (void)waitpid(pid, &status, 0);
    CHECK(0, "Linux ABI: bad/cyclic robust list must not permanently hang exit");
}

static void *robust_pending_owner_thread(void *arg)
{
    (void)arg;

    /*
     * Linux stops the robust-list walk after a bad list pointer, so pending
     * cleanup needs a valid list head and bad-head tolerance is checked below.
     */
    pid_t tid = (pid_t)syscall(SYS_gettid);
    robust_pending_head.list.next = &robust_pending_head.list;
    robust_pending_head.futex_offset =
        (long)offsetof(struct robust_test_node, futex_word);
    robust_pending_head.list_op_pending = &robust_pending_node.list;
    robust_pending_node.list.next = &robust_pending_head.list;
    atomic_store_explicit(&robust_pending_node.futex_word,
                          (uint32_t)tid & FUTEX_TID_MASK,
                          memory_order_release);

    long ret = raw_set_robust_list(&robust_pending_head,
                                   sizeof(robust_pending_head));
    if (ret != 0) {
        atomic_store_explicit(&robust_pending_owner_tid, UINT32_MAX,
                              memory_order_release);
        atomic_store_explicit(&robust_pending_owner_ready, 1,
                              memory_order_release);
        return (void *)(intptr_t)-errno;
    }

    atomic_store_explicit(&robust_pending_owner_tid, (uint32_t)tid,
                          memory_order_release);
    atomic_store_explicit(&robust_pending_owner_ready, 1, memory_order_release);

    while (atomic_load_explicit(&robust_pending_owner_can_exit,
                                memory_order_acquire) == 0) {
        sched_yield();
    }

    return NULL;
}

static void test_robust_list_pending_owner_death(void)
{
    printf("\n--- robust-list pending cleanup ---\n");
    pthread_t owner;

    atomic_store_explicit(&robust_pending_owner_ready, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_pending_owner_can_exit, 0,
                          memory_order_relaxed);
    atomic_store_explicit(&robust_pending_owner_tid, 0, memory_order_relaxed);
    atomic_store_explicit(&robust_pending_node.futex_word, 0,
                          memory_order_relaxed);

    int err = pthread_create(&owner, NULL, robust_pending_owner_thread, NULL);
    CHECK(err == 0, "pthread_create robust pending owner succeeds");
    if (err != 0) {
        exit(1);
    }

    while (atomic_load_explicit(&robust_pending_owner_ready,
                                memory_order_acquire) == 0) {
        sched_yield();
    }
    CHECK(atomic_load_explicit(&robust_pending_owner_tid,
                               memory_order_acquire) != UINT32_MAX,
          "pending owner thread set robust list");

    atomic_store_explicit(&robust_pending_owner_can_exit, 1,
                          memory_order_release);
    join_thread(owner, NULL);

    uint32_t word =
        atomic_load_explicit(&robust_pending_node.futex_word, memory_order_acquire);

    CHECK((word & FUTEX_OWNER_DIED) != 0,
          "pending robust futex is marked owner-dead");
    CHECK((word & FUTEX_TID_MASK) == 0,
          "pending robust futex owner TID is cleared");
}

static void *robust_bad_head_owner_thread(void *arg)
{
    (void)arg;

    robust_pending_head.list.next = (struct local_robust_list *)(uintptr_t)1;
    robust_pending_head.futex_offset =
        (long)offsetof(struct robust_test_node, futex_word);
    robust_pending_head.list_op_pending = NULL;

    long ret = raw_set_robust_list(&robust_pending_head,
                                   sizeof(robust_pending_head));
    if (ret != 0) {
        return (void *)(intptr_t)-errno;
    }
    return NULL;
}

static void test_robust_list_bad_head_is_tolerated(void)
{
    printf("\n--- robust-list bad list head is tolerated ---\n");
    pthread_t owner;

    int err = pthread_create(&owner, NULL, robust_bad_head_owner_thread, NULL);
    CHECK(err == 0, "pthread_create robust bad-head owner succeeds");
    if (err != 0) {
        exit(1);
    }

    void *result = NULL;
    join_thread(owner, &result);
    CHECK((int)(intptr_t)result == 0,
          "bad robust-list head does not abort thread exit");
}

int main(void)
{
    TEST_START("futex and robust-list syscalls");

    test_futex_basic();
    test_futex_op_flag_validation();
    test_futex_address_and_count_validation();
    test_futex_timeout_duration();
    test_futex_wait_wake();
    test_futex_shared_fork();
    test_futex_shared_private_key_fork();
    test_futex_shared_wake_count();
    test_futex_requeue_id_collision_regression();
    test_futex_cmp_requeue_semantics();
    test_futex_bitset();
    test_robust_list_syscalls();
    test_robust_list_owner_death();
    test_robust_list_pending_owner_death();
    test_robust_list_bad_head_is_tolerated();
    test_robust_list_bad_chain_does_not_hang();

    TEST_DONE();
}
