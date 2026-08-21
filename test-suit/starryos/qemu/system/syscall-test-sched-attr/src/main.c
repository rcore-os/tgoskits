#define _GNU_SOURCE
#include "test_framework.h"
#include <stdint.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

/*
 * Linux sched syscall numbers. Kept local because the guest musl headers may
 * not expose sched_setattr/sched_getattr on every architecture.
 */
#if defined(__x86_64__)
#define SYS_SCHED_SETPARAM_NR        142
#define SYS_SCHED_GET_PRIORITY_MAX_NR 146
#define SYS_SCHED_GET_PRIORITY_MIN_NR 147
#define SYS_SCHED_RR_GET_INTERVAL_NR 148
#define SYS_SCHED_SETATTR_NR         314
#define SYS_SCHED_GETATTR_NR         315
#elif defined(__riscv) || defined(__aarch64__) || defined(__loongarch64)
#define SYS_SCHED_SETPARAM_NR        118
#define SYS_SCHED_GET_PRIORITY_MAX_NR 125
#define SYS_SCHED_GET_PRIORITY_MIN_NR 126
#define SYS_SCHED_RR_GET_INTERVAL_NR 127
#define SYS_SCHED_SETATTR_NR         274
#define SYS_SCHED_GETATTR_NR         275
#else
#error "unsupported architecture for sched-attr test"
#endif

#define SCHED_NORMAL 0
#define SCHED_FIFO   1
#define SCHED_RR     2
#define SCHED_BATCH  3
#define SCHED_IDLE   5
#define SCHED_DEADLINE 6

struct sched_attr {
    uint32_t size;
    uint32_t sched_policy;
    uint64_t sched_flags;
    int32_t  sched_nice;
    uint32_t sched_priority;
    uint64_t sched_runtime;
    uint64_t sched_deadline;
    uint64_t sched_period;
    uint32_t sched_util_min;
    uint32_t sched_util_max;
};

int main(void)
{
    TEST_START("sched_setattr / sched_getattr / sched_setparam / priority helpers");

    /* sched_get_priority_max/min. */
    {
        CHECK_RET(syscall(SYS_SCHED_GET_PRIORITY_MAX_NR, SCHED_FIFO), 99,
                  "sched_get_priority_max(SCHED_FIFO) == 99");
        CHECK_RET(syscall(SYS_SCHED_GET_PRIORITY_MAX_NR, SCHED_RR), 99,
                  "sched_get_priority_max(SCHED_RR) == 99");
        CHECK_RET(syscall(SYS_SCHED_GET_PRIORITY_MAX_NR, SCHED_NORMAL), 0,
                  "sched_get_priority_max(SCHED_NORMAL) == 0");
        CHECK_RET(syscall(SYS_SCHED_GET_PRIORITY_MIN_NR, SCHED_FIFO), 1,
                  "sched_get_priority_min(SCHED_FIFO) == 1");
        CHECK_RET(syscall(SYS_SCHED_GET_PRIORITY_MIN_NR, SCHED_RR), 1,
                  "sched_get_priority_min(SCHED_RR) == 1");
        CHECK_ERR(syscall(SYS_SCHED_GET_PRIORITY_MAX_NR, 0xdead), EINVAL,
                  "sched_get_priority_max(bad policy) -> EINVAL");
    }

    /* sched_rr_get_interval. */
    {
        struct timespec ts;
        memset(&ts, 0, sizeof(ts));
        CHECK_RET(syscall(SYS_SCHED_RR_GET_INTERVAL_NR, 0, &ts), 0,
                  "sched_rr_get_interval(self)");
        CHECK(ts.tv_sec == 0 && ts.tv_nsec > 0 && ts.tv_nsec <= 1000000000,
              "rr interval is a positive timespec");
        CHECK_ERR(syscall(SYS_SCHED_RR_GET_INTERVAL_NR, 0, NULL), EINVAL,
                  "sched_rr_get_interval(NULL) -> EINVAL");
    }

    /* sched_setattr: full attribute, then getattr round-trip. */
    {
        struct sched_attr a;
        memset(&a, 0, sizeof(a));
        a.size = sizeof(a);
        a.sched_policy = SCHED_NORMAL;
        a.sched_nice = 0;
        CHECK_RET(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0), 0,
                  "sched_setattr(self, SCHED_NORMAL)");

        memset(&a, 0, sizeof(a));
        a.size = sizeof(a);
        CHECK_RET(syscall(SYS_SCHED_GETATTR_NR, 0, &a, sizeof(a), 0),
                  (long)sizeof(a), "sched_getattr returns struct size");
        CHECK(a.sched_policy == SCHED_NORMAL && a.sched_nice == 0,
              "getattr reflects SCHED_NORMAL/nice 0");

        /* ABI validation. */
        a.size = 8;
        CHECK_ERR(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0), EINVAL,
                  "sched_setattr(size too small) -> EINVAL");
        a.size = 4096;
        CHECK_ERR(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0), E2BIG,
                  "sched_setattr(size too large) -> E2BIG");
        a.size = sizeof(a);
        a.sched_policy = 0xdead;
        CHECK_ERR(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0), EINVAL,
                  "sched_setattr(bad policy) -> EINVAL");
        a.sched_policy = SCHED_NORMAL;
        a.sched_nice = 100;
        CHECK_ERR(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0), EINVAL,
                  "sched_setattr(nice out of range) -> EINVAL");
        a.sched_nice = 0;
        CHECK_ERR(syscall(SYS_SCHED_SETATTR_NR, 0, &a, 0x80000000u), EINVAL,
                  "sched_setattr(bad flags) -> EINVAL");
    }

    /* sched_setparam. */
    {
        int32_t prio = 0;
        CHECK_RET(syscall(SYS_SCHED_SETPARAM_NR, 0, &prio), 0,
                  "sched_setparam(self, prio 0)");
        prio = -1;
        CHECK_ERR(syscall(SYS_SCHED_SETPARAM_NR, 0, &prio), EINVAL,
                  "sched_setparam(prio -1) -> EINVAL");
        prio = 50;
        CHECK_ERR(syscall(SYS_SCHED_SETPARAM_NR, 0, &prio), EINVAL,
                  "sched_setparam(prio 50 on SCHED_NORMAL) -> EINVAL");
        CHECK_ERR(syscall(SYS_SCHED_SETPARAM_NR, 0, NULL), EINVAL,
                  "sched_setparam(NULL) -> EINVAL");
    }

    TEST_DONE();
}
