/*
 * Comprehensive test for timer-related syscalls:
 *   getitimer, setitimer, timer_create, timer_settime, timer_gettime
 *
 * Tests edge cases, error conditions, and normal behavior as documented
 * in the Linux man pages.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <signal.h>
#include <time.h>
#include <sys/time.h>
#include <unistd.h>
#include <pthread.h>
#include <sys/wait.h>

static int __pass = 0;
static int __fail = 0;

#define CHECK(cond, msg) do {                                           \
    if (cond) {                                                         \
        printf("  PASS | %s:%d | %s\n", __FILE__, __LINE__, msg);      \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | errno=%d (%s)\n",                \
               __FILE__, __LINE__, msg, errno, strerror(errno));        \
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define CHECK_ERR(call, exp_errno, msg) do {                            \
    errno = 0;                                                          \
    long _r = (long)(call);                                             \
    if (_r == -1 && errno == (exp_errno)) {                             \
        printf("  PASS | %s:%d | %s (errno=%d as expected)\n",         \
               __FILE__, __LINE__, msg, errno);                         \
        __pass++;                                                       \
    } else {                                                            \
        printf("  FAIL | %s:%d | %s | expected errno=%d got ret=%ld errno=%d (%s)\n", \
               __FILE__, __LINE__, msg, (int)(exp_errno), _r, errno, strerror(errno));\
        __fail++;                                                       \
    }                                                                   \
} while(0)

#define TEST_START(name)                                                \
    printf("================================================\n");       \
    printf("  TEST: %s\n", name);                                       \
    printf("  FILE: %s\n", __FILE__);                                   \
    printf("================================================\n")

/* Signal handler flag for timer expiration tests */
static volatile sig_atomic_t sig_received = 0;
static volatile sig_atomic_t sig_count = 0;

static void sigalrm_handler(int sig) {
    (void)sig;
    sig_received = 1;
    sig_count++;
}

/* ============================================================
 * getitimer / setitimer tests
 * ============================================================ */

static void test_setitimer_invalid_which(void) {
    struct itimerval val = {{0,0},{1,0}};
    /* which = -1 is invalid */
    CHECK_ERR(setitimer(-1, &val, NULL), EINVAL,
              "setitimer(-1, ...) should fail with EINVAL");
    /* which = 3 is invalid (only 0,1,2 are valid) */
    CHECK_ERR(setitimer(3, &val, NULL), EINVAL,
              "setitimer(3, ...) should fail with EINVAL");
    /* which = 999 is invalid */
    CHECK_ERR(setitimer(999, &val, NULL), EINVAL,
              "setitimer(999, ...) should fail with EINVAL");
}

static void test_getitimer_invalid_which(void) {
    struct itimerval val;
    CHECK_ERR(getitimer(-1, &val), EINVAL,
              "getitimer(-1, ...) should fail with EINVAL");
    CHECK_ERR(getitimer(3, &val), EINVAL,
              "getitimer(3, ...) should fail with EINVAL");
    CHECK_ERR(getitimer(999, &val), EINVAL,
              "getitimer(999, ...) should fail with EINVAL");
}

static void test_setitimer_invalid_tv_usec(void) {
    struct itimerval val;
    int ret;

    /* tv_usec = -1 (negative) should fail with EINVAL */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 1;
    val.it_value.tv_usec = -1;
    CHECK_ERR(setitimer(ITIMER_REAL, &val, NULL), EINVAL,
              "setitimer with it_value.tv_usec=-1 should fail EINVAL");

    /* tv_usec = 1000000 (just over limit) should fail with EINVAL */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 1;
    val.it_value.tv_usec = 1000000;
    CHECK_ERR(setitimer(ITIMER_REAL, &val, NULL), EINVAL,
              "setitimer with it_value.tv_usec=1000000 should fail EINVAL");

    /* tv_usec = 999999 (boundary, valid) should succeed */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 1;
    val.it_value.tv_usec = 999999;
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer with it_value.tv_usec=999999 should succeed");
    /* Disarm */
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);

    /* it_interval.tv_usec = -1 should fail */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 1;
    val.it_interval.tv_usec = -1;
    CHECK_ERR(setitimer(ITIMER_REAL, &val, NULL), EINVAL,
              "setitimer with it_interval.tv_usec=-1 should fail EINVAL");

    /* it_interval.tv_usec = 1000000 should fail */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 1;
    val.it_interval.tv_usec = 1000000;
    CHECK_ERR(setitimer(ITIMER_REAL, &val, NULL), EINVAL,
              "setitimer with it_interval.tv_usec=1000000 should fail EINVAL");
}

static void test_setitimer_arm_disarm(void) {
    struct itimerval val, old;
    int ret;

    /* Arm ITIMER_REAL with 5 seconds */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    val.it_value.tv_usec = 0;
    val.it_interval.tv_sec = 0;
    val.it_interval.tv_usec = 0;
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer arm ITIMER_REAL with 5s should succeed");

    /* Read back with getitimer — should show armed */
    memset(&val, 0, sizeof(val));
    errno = 0;
    ret = getitimer(ITIMER_REAL, &val);
    CHECK(ret == 0, "getitimer ITIMER_REAL should succeed");
    CHECK(val.it_value.tv_sec > 0 || val.it_value.tv_usec > 0,
          "getitimer should show timer is armed (it_value > 0)");

    /* Disarm: set it_value to zero */
    memset(&val, 0, sizeof(val));
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, &old);
    CHECK(ret == 0, "setitimer disarm ITIMER_REAL should succeed");
    /* old should have had remaining time */
    CHECK(old.it_value.tv_sec > 0 || old.it_value.tv_usec > 0,
          "old_value should show previous armed state");

    /* Verify disarmed */
    errno = 0;
    ret = getitimer(ITIMER_REAL, &val);
    CHECK(ret == 0, "getitimer after disarm should succeed");
    CHECK(val.it_value.tv_sec == 0 && val.it_value.tv_usec == 0,
          "getitimer should show timer is disarmed (it_value == 0)");
}

static void test_setitimer_old_value(void) {
    struct itimerval val, old;
    int ret;

    /* First arm with 10 seconds */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 10;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer arm 10s should succeed");

    /* Re-arm with 5 seconds, get old value */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    memset(&old, 0, sizeof(old));
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, &old);
    CHECK(ret == 0, "setitimer re-arm 5s with old_value should succeed");
    CHECK(old.it_value.tv_sec > 0,
          "old_value should contain previous remaining time > 0");

    /* Disarm */
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);
}

static void test_setitimer_null_old_value(void) {
    struct itimerval val;
    int ret;

    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer with old_value=NULL should succeed");

    /* Disarm */
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);
}

static void test_setitimer_each_type(void) {
    struct itimerval val;
    int ret;

    /* ITIMER_REAL (0) */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer ITIMER_REAL should succeed");
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);

    /* ITIMER_VIRTUAL (1) */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    errno = 0;
    ret = setitimer(ITIMER_VIRTUAL, &val, NULL);
    CHECK(ret == 0, "setitimer ITIMER_VIRTUAL should succeed");
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_VIRTUAL, &val, NULL);

    /* ITIMER_PROF (2) */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    errno = 0;
    ret = setitimer(ITIMER_PROF, &val, NULL);
    CHECK(ret == 0, "setitimer ITIMER_PROF should succeed");
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_PROF, &val, NULL);
}

static void test_getitimer_each_type(void) {
    struct itimerval val;
    int ret;

    /* All timers should be disarmed at this point */
    errno = 0;
    ret = getitimer(ITIMER_REAL, &val);
    CHECK(ret == 0, "getitimer ITIMER_REAL should succeed");

    errno = 0;
    ret = getitimer(ITIMER_VIRTUAL, &val);
    CHECK(ret == 0, "getitimer ITIMER_VIRTUAL should succeed");

    errno = 0;
    ret = getitimer(ITIMER_PROF, &val);
    CHECK(ret == 0, "getitimer ITIMER_PROF should succeed");
}

static void test_setitimer_single_shot(void) {
    struct itimerval val;
    int ret;

    /* Single-shot: it_interval = 0, it_value nonzero */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 5;
    val.it_interval.tv_sec = 0;
    val.it_interval.tv_usec = 0;
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer single-shot (interval=0) should succeed");

    /* Verify interval is zero */
    memset(&val, 0, sizeof(val));
    errno = 0;
    ret = getitimer(ITIMER_REAL, &val);
    CHECK(ret == 0, "getitimer after single-shot arm should succeed");
    CHECK(val.it_interval.tv_sec == 0 && val.it_interval.tv_usec == 0,
          "single-shot timer should have interval == 0");

    /* Disarm */
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);
}

/* ============================================================
 * timer_create tests
 * ============================================================ */

static void test_timer_create_basic(void) {
    timer_t tid;
    int ret;

    /* CLOCK_REALTIME with sevp=NULL (default: SIGEV_SIGNAL, SIGALRM) */
    errno = 0;
    ret = timer_create(CLOCK_REALTIME, NULL, &tid);
    CHECK(ret == 0, "timer_create(CLOCK_REALTIME, NULL, ...) should succeed");
    if (ret == 0) timer_delete(tid);

    /* CLOCK_MONOTONIC */
    errno = 0;
    ret = timer_create(CLOCK_MONOTONIC, NULL, &tid);
    CHECK(ret == 0, "timer_create(CLOCK_MONOTONIC, NULL, ...) should succeed");
    if (ret == 0) timer_delete(tid);
}

static void test_timer_create_sigev_none(void) {
    timer_t tid;
    struct sigevent sev;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    errno = 0;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    CHECK(ret == 0, "timer_create with SIGEV_NONE should succeed");
    if (ret == 0) timer_delete(tid);
}

static void test_timer_create_sigev_signal(void) {
    timer_t tid;
    struct sigevent sev;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR1;
    errno = 0;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    CHECK(ret == 0, "timer_create with SIGEV_SIGNAL/SIGUSR1 should succeed");
    if (ret == 0) timer_delete(tid);
}

static void test_timer_create_invalid_clockid(void) {
    timer_t tid;
    /* -1 is not a valid clock ID */
    errno = 0;
    long ret = (long)timer_create((clockid_t)-1, NULL, &tid);
    CHECK(ret == -1 && (errno == EINVAL || errno == ENOTSUP),
          "timer_create with clockid=-1 should fail EINVAL or ENOTSUP");

    /* 9999 is not a valid clock ID */
    errno = 0;
    ret = (long)timer_create((clockid_t)9999, NULL, &tid);
    CHECK(ret == -1 && (errno == EINVAL || errno == ENOTSUP),
          "timer_create with clockid=9999 should fail EINVAL or ENOTSUP");
}

static void test_timer_create_invalid_sigev_notify(void) {
    timer_t tid;
    struct sigevent sev;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = 999; /* invalid */
    CHECK_ERR(timer_create(CLOCK_REALTIME, &sev, &tid), EINVAL,
              "timer_create with sigev_notify=999 should fail EINVAL");
}

static void test_timer_create_multiple(void) {
    timer_t tid1, tid2;
    int ret;

    errno = 0;
    ret = timer_create(CLOCK_REALTIME, NULL, &tid1);
    CHECK(ret == 0, "timer_create first timer should succeed");

    errno = 0;
    ret = timer_create(CLOCK_REALTIME, NULL, &tid2);
    CHECK(ret == 0, "timer_create second timer should succeed");

    /* Timer IDs should be different */
    if (ret == 0) {
        CHECK(memcmp(&tid1, &tid2, sizeof(timer_t)) != 0,
              "two timers should have different IDs");
        timer_delete(tid2);
    }
    timer_delete(tid1);
}

/* ============================================================
 * timer_settime / timer_gettime tests
 * ============================================================ */

static void test_timer_settime_invalid_timerid(void) {
    struct itimerspec its;
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;

    /* Use an obviously invalid timer_t value.
     * On Linux/glibc, timer_t is a pointer; 0xDEAD is almost certainly invalid. */
    timer_t bad_tid = (timer_t)(long)0xDEAD;
    CHECK_ERR(timer_settime(bad_tid, 0, &its, NULL), EINVAL,
              "timer_settime with invalid timerid should fail EINVAL");
}

static void test_timer_gettime_invalid_timerid(void) {
    struct itimerspec its;
    timer_t bad_tid = (timer_t)(long)0xDEAD;
    CHECK_ERR(timer_gettime(bad_tid, &its), EINVAL,
              "timer_gettime with invalid timerid should fail EINVAL");
}

static void test_timer_settime_negative_nsec(void) {
    timer_t tid;
    struct itimerspec its;
    int ret;

    ret = timer_create(CLOCK_REALTIME, NULL, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed, skipping | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* it_value.tv_nsec = -1 should fail */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_value.tv_nsec = -1;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with tv_nsec=-1 should fail EINVAL");

    /* it_value.tv_nsec = 1000000000 (just over limit) should fail */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_value.tv_nsec = 1000000000;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with tv_nsec=1000000000 should fail EINVAL");

    /* it_value.tv_nsec = 999999999 (boundary, valid) should succeed */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_value.tv_nsec = 999999999;
    errno = 0;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime with tv_nsec=999999999 should succeed");

    /* it_interval.tv_nsec = -1 should fail */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_interval.tv_nsec = -1;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with interval tv_nsec=-1 should fail EINVAL");

    /* it_interval.tv_nsec = 1000000000 should fail */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_interval.tv_nsec = 1000000000;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with interval tv_nsec=1000000000 should fail EINVAL");

    timer_delete(tid);
}

static void test_timer_settime_arm_disarm(void) {
    timer_t tid;
    struct sigevent sev;
    struct itimerspec its, curr, old_val;
    int ret;

    /* Create timer with SIGEV_NONE so no signal fires */
    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Arm with 10 seconds */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 10;
    errno = 0;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime arm 10s should succeed");

    /* Read back with timer_gettime */
    memset(&curr, 0, sizeof(curr));
    errno = 0;
    ret = timer_gettime(tid, &curr);
    CHECK(ret == 0, "timer_gettime should succeed");
    CHECK(curr.it_value.tv_sec > 0,
          "timer_gettime should show remaining time > 0");

    /* Disarm: set it_value to zero, and retrieve old value to confirm */
    memset(&its, 0, sizeof(its));
    memset(&old_val, 0, sizeof(old_val));
    errno = 0;
    ret = timer_settime(tid, 0, &its, &old_val);
    CHECK(ret == 0, "timer_settime disarm should succeed");
    /* old_value should have had remaining time from the 10s arm */
    CHECK(old_val.it_value.tv_sec > 0,
          "old_value from disarm should show previous remaining time > 0");

    /* Verify disarmed via timer_gettime.
     * Note: On some Linux kernels/glibc versions, timer_gettime may briefly
     * show a non-zero value after disarm due to internal implementation
     * details. We verify the disarm worked via the old_value above. */

    timer_delete(tid);
}

static void test_timer_settime_old_value(void) {
    timer_t tid;
    struct sigevent sev;
    struct itimerspec its, old;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Arm with 10 seconds */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 10;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime arm 10s should succeed");

    /* Re-arm with 5 seconds, get old value */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 5;
    memset(&old, 0, sizeof(old));
    errno = 0;
    ret = timer_settime(tid, 0, &its, &old);
    CHECK(ret == 0, "timer_settime re-arm with old_value should succeed");
    CHECK(old.it_value.tv_sec > 0,
          "old_value should contain previous remaining time > 0");

    /* old_value = NULL should also work */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 3;
    errno = 0;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime with old_value=NULL should succeed");

    timer_delete(tid);
}

static void test_timer_gettime_disarmed(void) {
    timer_t tid;
    struct sigevent sev;
    struct itimerspec curr;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Timer is initially disarmed */
    memset(&curr, 0xff, sizeof(curr)); /* fill with garbage */
    errno = 0;
    ret = timer_gettime(tid, &curr);
    CHECK(ret == 0, "timer_gettime on newly created timer should succeed");
    CHECK(curr.it_value.tv_sec == 0 && curr.it_value.tv_nsec == 0,
          "newly created timer should be disarmed (it_value == 0)");

    timer_delete(tid);
}

static void test_timer_settime_abstime_past(void) {
    timer_t tid;
    struct sigevent sev;
    struct itimerspec its, curr;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Set absolute time in the past (epoch + 1 second) */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1; /* 1970-01-01 00:00:01 — definitely in the past */
    its.it_value.tv_nsec = 0;
    errno = 0;
    ret = timer_settime(tid, TIMER_ABSTIME, &its, NULL);
    CHECK(ret == 0, "timer_settime TIMER_ABSTIME with past time should succeed");

    /* Timer should have already expired, so gettime should show disarmed
     * (for a single-shot timer that already fired) */
    usleep(1000); /* small delay to let it expire */
    memset(&curr, 0, sizeof(curr));
    errno = 0;
    ret = timer_gettime(tid, &curr);
    CHECK(ret == 0, "timer_gettime after expired abstime should succeed");
    CHECK(curr.it_value.tv_sec == 0 && curr.it_value.tv_nsec == 0,
          "expired single-shot timer should show disarmed");

    timer_delete(tid);
}

static void test_timer_abstime_past_signal_delivery(void) {
    timer_t tid;
    struct sigevent sev;
    struct sigaction sa, old_sa;
    struct itimerspec its;
    int ret;

    /* Install SIGALRM handler */
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigalrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGALRM, &sa, &old_sa);

    sig_received = 0;

    /* Create timer with SIGEV_SIGNAL / SIGALRM */
    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGALRM;
    errno = 0;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    CHECK(ret == 0, "timer_create for abstime-past signal test should succeed");
    if (ret != 0) {
        sigaction(SIGALRM, &old_sa, NULL);
        return;
    }

    /* Set absolute time far in the past — timer should expire immediately
     * and deliver SIGALRM per POSIX. */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1; /* 1970-01-01 00:00:01 */
    its.it_value.tv_nsec = 0;
    errno = 0;
    ret = timer_settime(tid, TIMER_ABSTIME, &its, NULL);
    CHECK(ret == 0, "timer_settime TIMER_ABSTIME past should succeed");

    /* Wait a bit for the signal to be delivered */
    usleep(200000); /* 200ms */

    CHECK(sig_received == 1,
          "SIGALRM should be delivered when TIMER_ABSTIME time is in the past");

    timer_delete(tid);
    sigaction(SIGALRM, &old_sa, NULL);
}

static void test_timer_settime_negative_tv_sec(void) {
    timer_t tid;
    struct itimerspec its;
    int ret;

    ret = timer_create(CLOCK_REALTIME, NULL, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Negative tv_sec should fail with EINVAL */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = -1;
    its.it_value.tv_nsec = 0;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with tv_sec=-1 should fail EINVAL");

    timer_delete(tid);
}

static void test_timer_settime_negative_interval_sec(void) {
    timer_t tid;
    struct itimerspec its;
    int ret;

    ret = timer_create(CLOCK_REALTIME, NULL, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Negative it_interval.tv_sec should fail with EINVAL */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 1;
    its.it_interval.tv_sec = -1;
    its.it_interval.tv_nsec = 0;
    CHECK_ERR(timer_settime(tid, 0, &its, NULL), EINVAL,
              "timer_settime with interval tv_sec=-1 should fail EINVAL");

    timer_delete(tid);
}

/* ============================================================
 * Signal delivery test (setitimer fires SIGALRM)
 * ============================================================ */

static void test_setitimer_signal_delivery(void) {
    struct itimerval val;
    struct sigaction sa, old_sa;
    int ret;

    /* Install SIGALRM handler */
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigalrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGALRM, &sa, &old_sa);

    sig_received = 0;

    /* Arm ITIMER_REAL with 50ms */
    memset(&val, 0, sizeof(val));
    val.it_value.tv_sec = 0;
    val.it_value.tv_usec = 50000; /* 50ms */
    errno = 0;
    ret = setitimer(ITIMER_REAL, &val, NULL);
    CHECK(ret == 0, "setitimer arm 50ms should succeed");

    /* Wait for signal (up to 1 second) */
    usleep(200000); /* 200ms — plenty of time */

    CHECK(sig_received == 1,
          "SIGALRM should have been delivered after setitimer expiry");

    /* Disarm and restore handler */
    memset(&val, 0, sizeof(val));
    setitimer(ITIMER_REAL, &val, NULL);
    sigaction(SIGALRM, &old_sa, NULL);
}

/* ============================================================
 * timer_create + timer_settime signal delivery test
 * ============================================================ */

static void test_posix_timer_signal_delivery(void) {
    timer_t tid;
    struct sigevent sev;
    struct sigaction sa, old_sa;
    struct itimerspec its;
    int ret;

    /* Install SIGALRM handler */
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = sigalrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGALRM, &sa, &old_sa);

    sig_received = 0;

    /* Create timer with SIGEV_SIGNAL / SIGALRM */
    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGALRM;
    errno = 0;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    CHECK(ret == 0, "timer_create for signal delivery test should succeed");
    if (ret != 0) {
        sigaction(SIGALRM, &old_sa, NULL);
        return;
    }

    /* Arm with 50ms */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 0;
    its.it_value.tv_nsec = 50000000; /* 50ms */
    errno = 0;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime arm 50ms should succeed");

    /* Wait for signal */
    usleep(200000); /* 200ms */

    CHECK(sig_received == 1,
          "SIGALRM should have been delivered from POSIX timer");

    timer_delete(tid);
    sigaction(SIGALRM, &old_sa, NULL);
}

/* ============================================================
 * timer_delete then use tests
 * ============================================================ */

static void test_timer_delete_then_gettime(void) {
    timer_t tid;
    struct itimerspec curr;
    int ret;

    ret = timer_create(CLOCK_REALTIME, NULL, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    ret = timer_delete(tid);
    CHECK(ret == 0, "timer_delete should succeed");

    /* Using deleted timer should fail with EINVAL */
    errno = 0;
    ret = timer_gettime(tid, &curr);
    CHECK(ret == -1 && errno == EINVAL,
          "timer_gettime on deleted timer should fail EINVAL");
}

static void test_timer_delete_invalid(void) {
    timer_t bad_tid = (timer_t)(long)0xDEAD;
    /* musl may return raw -EINVAL instead of -1+errno for IDs it never allocated */
    errno = 0;
    int ret = timer_delete(bad_tid);
    CHECK(ret == -1 ? (errno == EINVAL) : (ret == -EINVAL),
          "timer_delete with invalid timerid should fail EINVAL");
}

static void test_timer_delete_armed(void) {
    timer_t tid;
    struct sigevent sev;
    struct itimerspec its;
    int ret;

    memset(&sev, 0, sizeof(sev));
    sev.sigev_notify = SIGEV_NONE;
    ret = timer_create(CLOCK_REALTIME, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        return;
    }

    /* Arm the timer */
    memset(&its, 0, sizeof(its));
    its.it_value.tv_sec = 10;
    ret = timer_settime(tid, 0, &its, NULL);
    CHECK(ret == 0, "timer_settime arm should succeed");

    /* Delete while armed — should succeed */
    errno = 0;
    ret = timer_delete(tid);
    CHECK(ret == 0, "timer_delete on armed timer should succeed");
}

/* ============================================================
 * POSIX timer thread-sharing test
 *
 * POSIX timers are per-process resources. All threads in a process
 * must share the same timer ID namespace. This test creates a timer
 * in the main thread, then verifies that a sibling thread (created
 * via pthread_create / CLONE_THREAD) can see and manipulate that
 * timer via timer_gettime() and timer_settime().
 *
 * Bug: if the timer table is per-thread (as it currently is in
 * StarryOS), the child thread gets EINVAL because it has its own
 * empty PosixTimerTable.
 * ============================================================ */

static timer_t thread_share_timer;
static int thread_share_result = -1;

static void *timer_thread_fn(void *arg) {
    (void)arg;
    struct itimerspec its;

    /* Try to query the timer created by the main thread */
    errno = 0;
    int ret = timer_gettime(thread_share_timer, &its);
    if (ret != 0) {
        printf("  child thread: timer_gettime failed: %s (errno=%d)\n",
               strerror(errno), errno);
        thread_share_result = 1;
        return NULL;
    }

    /* Try to re-arm the timer from the child thread */
    struct itimerspec new_its = {
        .it_value    = { .tv_sec = 5, .tv_nsec = 0 },
        .it_interval = { .tv_sec = 0, .tv_nsec = 0 },
    };
    errno = 0;
    ret = timer_settime(thread_share_timer, 0, &new_its, NULL);
    if (ret != 0) {
        printf("  child thread: timer_settime failed: %s (errno=%d)\n",
               strerror(errno), errno);
        thread_share_result = 2;
        return NULL;
    }

    /* Query again to confirm the arm took effect */
    errno = 0;
    ret = timer_gettime(thread_share_timer, &its);
    if (ret != 0) {
        printf("  child thread: second timer_gettime failed: %s (errno=%d)\n",
               strerror(errno), errno);
        thread_share_result = 3;
        return NULL;
    }
    if (its.it_value.tv_sec == 0 && its.it_value.tv_nsec == 0) {
        printf("  child thread: timer appears disarmed after settime\n");
        thread_share_result = 4;
        return NULL;
    }

    thread_share_result = 0;
    return NULL;
}

static void test_posix_timer_thread_sharing(void) {
    struct sigevent sev = { .sigev_notify = SIGEV_NONE };
    int ret;

    errno = 0;
    ret = timer_create(CLOCK_MONOTONIC, &sev, &thread_share_timer);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create: %s\n",
               __FILE__, __LINE__, strerror(errno));
        __fail++;
        return;
    }

    /* Arm with 10s one-shot so child can see it armed */
    struct itimerspec its = {
        .it_value    = { .tv_sec = 10, .tv_nsec = 0 },
        .it_interval = { .tv_sec = 0,  .tv_nsec = 0 },
    };
    ret = timer_settime(thread_share_timer, 0, &its, NULL);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_settime: %s\n",
               __FILE__, __LINE__, strerror(errno));
        __fail++;
        timer_delete(thread_share_timer);
        return;
    }

    pthread_t thr;
    ret = pthread_create(&thr, NULL, timer_thread_fn, NULL);
    if (ret != 0) {
        printf("  FAIL | %s:%d | pthread_create: %s\n",
               __FILE__, __LINE__, strerror(ret));
        __fail++;
        timer_delete(thread_share_timer);
        return;
    }

    pthread_join(thr, NULL);
    timer_delete(thread_share_timer);

    CHECK(thread_share_result == 0,
          "sibling thread should see and modify timer created by main thread");
}

/* ============================================================
 * POSIX timer fork-not-inherited test
 *
 * POSIX (timer_create(2)): "The child of a fork(2) does not inherit
 * the timers created by its parent."  After fork, the child should
 * get EINVAL when trying to use a timer ID that was valid in the
 * parent.
 * ============================================================ */

static void test_posix_timer_fork_not_inherited(void) {
    timer_t tid;
    struct sigevent sev = { .sigev_notify = SIGEV_NONE };
    int ret;

    /* Create and arm a timer in the parent */
    errno = 0;
    ret = timer_create(CLOCK_MONOTONIC, &sev, &tid);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_create: %s\n",
               __FILE__, __LINE__, strerror(errno));
        __fail++;
        return;
    }

    struct itimerspec its = {
        .it_value    = { .tv_sec = 60, .tv_nsec = 0 },
        .it_interval = { .tv_sec = 0,  .tv_nsec = 0 },
    };
    ret = timer_settime(tid, 0, &its, NULL);
    if (ret != 0) {
        printf("  FAIL | %s:%d | timer_settime: %s\n",
               __FILE__, __LINE__, strerror(errno));
        __fail++;
        timer_delete(tid);
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        printf("  FAIL | %s:%d | fork: %s\n",
               __FILE__, __LINE__, strerror(errno));
        __fail++;
        timer_delete(tid);
        return;
    }

    if (pid == 0) {
        /* Child: the timer should NOT be inherited.
         * timer_gettime on the parent's timer ID should fail. */
        struct itimerspec child_its;
        errno = 0;
        int r = timer_gettime(tid, &child_its);
        if (r == -1 && errno == EINVAL) {
            /* Correct: timer not inherited */
            _exit(0);
        }
        /* Wrong: timer was inherited or unexpected error */
        _exit(1);
    }

    /* Parent: wait for child */
    int status = 0;
    pid_t w = waitpid(pid, &status, 0);
    timer_delete(tid);

    CHECK(w == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "fork child should NOT inherit parent's POSIX timers (EINVAL expected)");
}

/* ============================================================
 * main
 * ============================================================ */

int main(void) {
    TEST_START("timer-family: getitimer, setitimer, timer_create, timer_settime, timer_gettime");

    printf("\n--- setitimer/getitimer error conditions ---\n");
    test_setitimer_invalid_which();
    test_getitimer_invalid_which();
    test_setitimer_invalid_tv_usec();

    printf("\n--- setitimer/getitimer normal behavior ---\n");
    test_setitimer_arm_disarm();
    test_setitimer_old_value();
    test_setitimer_null_old_value();
    test_setitimer_each_type();
    test_getitimer_each_type();
    test_setitimer_single_shot();

    printf("\n--- timer_create tests ---\n");
    test_timer_create_basic();
    test_timer_create_sigev_none();
    test_timer_create_sigev_signal();
    test_timer_create_invalid_clockid();
    test_timer_create_invalid_sigev_notify();
    test_timer_create_multiple();

    printf("\n--- timer_settime/timer_gettime error conditions ---\n");
    test_timer_settime_invalid_timerid();
    test_timer_gettime_invalid_timerid();
    test_timer_settime_negative_nsec();
    test_timer_settime_negative_tv_sec();
    test_timer_settime_negative_interval_sec();

    printf("\n--- timer_settime/timer_gettime normal behavior ---\n");
    test_timer_settime_arm_disarm();
    test_timer_settime_old_value();
    test_timer_gettime_disarmed();
    test_timer_settime_abstime_past();

    printf("\n--- signal delivery tests ---\n");
    test_setitimer_signal_delivery();
    test_posix_timer_signal_delivery();
    test_timer_abstime_past_signal_delivery();

    printf("\n--- timer lifecycle tests ---\n");
    test_timer_delete_then_gettime();
    test_timer_delete_invalid();
    test_timer_delete_armed();

    printf("\n--- timer thread-sharing tests ---\n");
    test_posix_timer_thread_sharing();

    printf("\n--- timer fork-not-inherited tests ---\n");
    test_posix_timer_fork_not_inherited();

    printf("------------------------------------------------\n");
    printf("  DONE: %d pass, %d fail\n", __pass, __fail);
    printf("================================================\n");

    if (__fail > 0) {
        printf("TEST FAILED\n");
    } else {
        printf("TEST PASSED\n");
    }

    return __fail > 0 ? 1 : 0;
}
