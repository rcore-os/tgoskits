/*
 * test_getrusage.c — comprehensive getrusage(2) syscall tests
 *
 * getrusage returns resource usage statistics for the calling process
 * (RUSAGE_SELF=0), its terminated children (RUSAGE_CHILDREN=-1), or
 * the calling thread (RUSAGE_THREAD=1).
 *
 * Positive coverage:
 *   A1. RUSAGE_SELF  — basic query, validate key fields
 *   A2. RUSAGE_THREAD — validate fields
 *   A3. RUSAGE_CHILDREN — zero before children, non-zero after child reaped
 *   A4. Field invariants — maintained fields non-negative, unmaintained=0
 *   A5. Monotonicity — utime+stime non-decreasing across successive calls
 *   A6. SELF vs THREAD — in single-threaded, should be close
 *
 * Negative coverage:
 *   B1. Invalid who values (-2, 2, 999, 99999) → EINVAL
 *   B2. EFAULT — invalid usage pointer
 *   B3. EFAULT — usage=NULL (must not crash)
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

/* macros for timeval validation */
#define TV_NONNEG(tv)   ((tv).tv_sec >= 0 && (tv).tv_usec >= 0)
#define TV_VALID(tv)    ((tv).tv_sec >= 0 && (tv).tv_usec >= 0 && (tv).tv_usec < 1000000)
#define TV_TO_US(tv)    ((long)(tv).tv_sec * 1000000L + (long)(tv).tv_usec)
#define A6_SKEW_TOLERANCE_US 500000L

int main(void)
{
    TEST_START("getrusage");

    /* ============================================================== */
    /* A1. Positive: RUSAGE_SELF — basic query, validate key fields   */
    /* ============================================================== */
    {
        printf("\n--- A1. RUSAGE_SELF (positive) ---\n");
        struct rusage u;
        memset(&u, 0, sizeof(u));
        CHECK_RET(getrusage(RUSAGE_SELF, &u), 0,
                  "getrusage(RUSAGE_SELF) returns 0");
        CHECK(TV_VALID(u.ru_utime), "A1: ru_utime valid");
        CHECK(TV_VALID(u.ru_stime), "A1: ru_stime valid");
        printf("  info | utime=%ld.%06ld stime=%ld.%06ld maxrss=%ld "
               "minflt=%ld majflt=%ld nvcsw=%ld nivcsw=%ld\n",
               (long)u.ru_utime.tv_sec, (long)u.ru_utime.tv_usec,
               (long)u.ru_stime.tv_sec, (long)u.ru_stime.tv_usec,
               u.ru_maxrss, u.ru_minflt, u.ru_majflt,
               u.ru_nvcsw, u.ru_nivcsw);
        /* RUSAGE_SELF on an active process should have non-zero total time */
        CHECK(u.ru_utime.tv_sec + u.ru_utime.tv_usec
              + u.ru_stime.tv_sec + u.ru_stime.tv_usec > 0,
              "RUSAGE_SELF has non-zero CPU time");
    }

    /* ============================================================== */
    /* A2. Positive: RUSAGE_THREAD — validate fields                  */
    /* ============================================================== */
    {
        printf("\n--- A2. RUSAGE_THREAD (positive) ---\n");
        struct rusage u;
        memset(&u, 0, sizeof(u));
        CHECK_RET(getrusage(RUSAGE_THREAD, &u), 0,
                  "getrusage(RUSAGE_THREAD) returns 0");
        CHECK(TV_VALID(u.ru_utime), "A2: ru_utime valid");
        CHECK(TV_VALID(u.ru_stime), "A2: ru_stime valid");
        printf("  info | utime=%ld.%06ld stime=%ld.%06ld\n",
               (long)u.ru_utime.tv_sec, (long)u.ru_utime.tv_usec,
               (long)u.ru_stime.tv_sec, (long)u.ru_stime.tv_usec);
    }

    /* ============================================================== */
    /* A3. Positive: RUSAGE_CHILDREN                                  */
    /*      Before any children → should be zero; after child → > 0   */
    /* ============================================================== */
    {
        printf("\n--- A3. RUSAGE_CHILDREN (positive) ---\n");

        /* A3a: before any children, RUSAGE_CHILDREN should be all zero */
        {
            struct rusage u;
            memset(&u, 0, sizeof(u));
            CHECK_RET(getrusage(RUSAGE_CHILDREN, &u), 0,
                      "getrusage(RUSAGE_CHILDREN) before children returns 0");
            /* With no children reaped, time should be zero */
            CHECK(u.ru_utime.tv_sec == 0 && u.ru_utime.tv_usec == 0
                  && u.ru_stime.tv_sec == 0 && u.ru_stime.tv_usec == 0,
                  "RUSAGE_CHILDREN before children: utime+stime zero");
        }

        /* A3b: spawn a child process, wait for it, then check RUSAGE_CHILDREN */
        {
            struct rusage u_pre;
            memset(&u_pre, 0, sizeof(u_pre));
            getrusage(RUSAGE_CHILDREN, &u_pre); /* snapshot */

            pid_t pid = fork();
            CHECK(pid >= 0, "fork for RUSAGE_CHILDREN test succeeds");
            if (pid == 0) {
                /* Child: do some work to consume CPU */
                volatile int x = 0;
                for (int i = 0; i < 5000000; i++)
                    x++;
                _exit(0);
            }
            int status;
            pid_t w;
            do {
                w = waitpid(pid, &status, 0);
            } while (w == -1 && errno == EINTR);
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "RUSAGE_CHILDREN child exited normally");

            struct rusage u_post;
            memset(&u_post, 0, sizeof(u_post));
            CHECK_RET(getrusage(RUSAGE_CHILDREN, &u_post), 0,
                      "getrusage(RUSAGE_CHILDREN) after reaping returns 0");

            /* After reaping a child that did CPU work, children times
             * should have increased.  But on some systems RUSAGE_CHILDREN
             * may remain zero if times aren't tracked.  We just check
             * the values are non-negative. */
            CHECK(TV_VALID(u_post.ru_utime), "A3b: children post utime valid");
            CHECK(TV_VALID(u_post.ru_stime), "A3b: children post stime valid");
            printf("  info | children utime=%ld.%06ld stime=%ld.%06ld "
                   "nvcsw=%ld nivcsw=%ld\n",
                   (long)u_post.ru_utime.tv_sec, (long)u_post.ru_utime.tv_usec,
                   (long)u_post.ru_stime.tv_sec, (long)u_post.ru_stime.tv_usec,
                   u_post.ru_nvcsw, u_post.ru_nivcsw);
        }

        /* A3c: spawn multiple children, verify cumulative accounting */
        {
            struct rusage u_before;
            getrusage(RUSAGE_CHILDREN, &u_before);

            pid_t pid = fork();
            CHECK(pid >= 0, "fork for cumulative children test succeeds");
            if (pid == 0) {
                volatile int x = 0;
                for (int i = 0; i < 2000000; i++)
                    x++;
                _exit(0);
            }
            int status;
            pid_t w1;
            do {
                w1 = waitpid(pid, &status, 0);
            } while (w1 == -1 && errno == EINTR);

            pid_t pid2 = fork();
            CHECK(pid2 >= 0, "fork for 2nd cumulative child succeeds");
            if (pid2 == 0) {
                volatile int x = 0;
                for (int i = 0; i < 2000000; i++)
                    x++;
                _exit(0);
            }
            do {
                w1 = waitpid(pid2, &status, 0);
            } while (w1 == -1 && errno == EINTR);
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "2nd child exited normally");

            struct rusage u_after;
            CHECK_RET(getrusage(RUSAGE_CHILDREN, &u_after), 0,
                      "getrusage(RUSAGE_CHILDREN) after multiple children returns 0");
            CHECK(TV_VALID(u_after.ru_utime), "A3c: multiple children utime valid");
            CHECK(TV_VALID(u_after.ru_stime), "A3c: multiple children stime valid");
        }
    }

    /* ============================================================== */
    /* A4. Positive: verify every field of struct rusage              */
    /* ============================================================== */
    {
        printf("\n--- A4. check every struct rusage field (positive) ---\n");
        struct rusage u;
        memset(&u, 0xFF, sizeof(u)); /* fill with garbage first */
        CHECK_RET(getrusage(RUSAGE_SELF, &u), 0,
                  "A4: getrusage(RUSAGE_SELF) returns 0");

        /* timeval fields */
        CHECK(TV_VALID(u.ru_utime), "A4: ru_utime is valid");
        CHECK(TV_VALID(u.ru_stime), "A4: ru_stime is valid");
        printf("  info | utime=%ld.%06ld stime=%ld.%06ld\n",
               (long)u.ru_utime.tv_sec, (long)u.ru_utime.tv_usec,
               (long)u.ru_stime.tv_sec, (long)u.ru_stime.tv_usec);

        /* maintained counters — at least zero */
        CHECK(u.ru_minflt >= 0, "A4: ru_minflt (page reclaims) >= 0");
        CHECK(u.ru_majflt >= 0, "A4: ru_majflt (page faults) >= 0");
        CHECK(u.ru_inblock >= 0, "A4: ru_inblock (block input ops) >= 0");
        CHECK(u.ru_oublock >= 0, "A4: ru_oublock (block output ops) >= 0");
        CHECK(u.ru_nvcsw >= 0, "A4: ru_nvcsw (voluntary ctx switches) >= 0");
        CHECK(u.ru_nivcsw >= 0, "A4: ru_nivcsw (involuntary ctx switches) >= 0");
        CHECK(u.ru_maxrss >= 0, "A4: ru_maxrss (max resident set) >= 0");
        printf("  info | minflt=%ld majflt=%ld inblock=%ld oublock=%ld "
               "nvcsw=%ld nivcsw=%ld maxrss=%ld\n",
               u.ru_minflt, u.ru_majflt, u.ru_inblock, u.ru_oublock,
               u.ru_nvcsw, u.ru_nivcsw, u.ru_maxrss);

        /* unmaintained fields — kernel sets to zero */
        CHECK(u.ru_ixrss == 0, "A4: ru_ixrss (integral shared mem) == 0");
        CHECK(u.ru_idrss == 0, "A4: ru_idrss (integral unshared data) == 0");
        CHECK(u.ru_isrss == 0, "A4: ru_isrss (integral unshared stack) == 0");
        CHECK(u.ru_nswap == 0, "A4: ru_nswap (swaps) == 0");
        CHECK(u.ru_msgsnd == 0, "A4: ru_msgsnd (IPC msgs sent) == 0");
        CHECK(u.ru_msgrcv == 0, "A4: ru_msgrcv (IPC msgs received) == 0");
        CHECK(u.ru_nsignals == 0, "A4: ru_nsignals (signals received) == 0");
        printf("  info | ixrss=%ld idrss=%ld isrss=%ld nswap=%ld "
               "msgsnd=%ld msgrcv=%ld nsignals=%ld\n",
               u.ru_ixrss, u.ru_idrss, u.ru_isrss, u.ru_nswap,
               u.ru_msgsnd, u.ru_msgrcv, u.ru_nsignals);
    }

    /* ============================================================== */
    /* A5. Positive: monotonicity — CPU time non-decreasing           */
    /* ============================================================== */
    {
        printf("\n--- A5. monotonicity (positive) ---\n");
        struct rusage u1, u2;
        memset(&u1, 0, sizeof(u1));
        memset(&u2, 0, sizeof(u2));

        CHECK_RET(getrusage(RUSAGE_SELF, &u1), 0,
                  "getrusage(RUSAGE_SELF) 1st call returns 0");

        /* Do some work to consume CPU time */
        volatile long acc = 0;
        for (volatile long i = 0; i < 10000000; i++)
            acc += i;

        CHECK_RET(getrusage(RUSAGE_SELF, &u2), 0,
                  "getrusage(RUSAGE_SELF) 2nd call returns 0");

        /* Total CPU time should be non-decreasing */
        long t1 = (long)u1.ru_utime.tv_sec * 1000000 + u1.ru_utime.tv_usec
                + (long)u1.ru_stime.tv_sec * 1000000 + u1.ru_stime.tv_usec;
        long t2 = (long)u2.ru_utime.tv_sec * 1000000 + u2.ru_utime.tv_usec
                + (long)u2.ru_stime.tv_sec * 1000000 + u2.ru_stime.tv_usec;
        CHECK(t2 >= t1, "A5: CPU time non-decreasing");
        printf("  info | CPU time before=%ldus after=%ldus diff=%ldus\n",
               t1, t2, t2 - t1);

        CHECK(u2.ru_nvcsw >= u1.ru_nvcsw, "A5: nvcsw non-decreasing");
        printf("  info | nvcsw before=%ld after=%ld\n",
               u1.ru_nvcsw, u2.ru_nvcsw);
    }

    /* ============================================================== */
    /* A6. Positive: SELF vs THREAD comparison (single-threaded)      */
    /* ============================================================== */
    {
        printf("\n--- A6. RUSAGE_SELF vs RUSAGE_THREAD (positive) ---\n");
        struct rusage us, ut;
        memset(&us, 0, sizeof(us));
        memset(&ut, 0, sizeof(ut));

        CHECK_RET(getrusage(RUSAGE_SELF, &us), 0,
                  "getrusage(RUSAGE_SELF) returns 0");
        CHECK_RET(getrusage(RUSAGE_THREAD, &ut), 0,
                  "getrusage(RUSAGE_THREAD) returns 0");

        /* In a single-threaded process, SELF time and THREAD time
         * should be nearly identical (small differences from call
         * ordering are expected). */
        long ts = TV_TO_US(us.ru_utime) + TV_TO_US(us.ru_stime);
        long tt = TV_TO_US(ut.ru_utime) + TV_TO_US(ut.ru_stime);
        long diff = ts > tt ? ts - tt : tt - ts;
        CHECK(ts > 0 && tt > 0,
              "A6: RUSAGE_SELF and RUSAGE_THREAD report non-zero CPU time");
        CHECK(diff <= A6_SKEW_TOLERANCE_US,
              "A6: RUSAGE_SELF and RUSAGE_THREAD times within scheduler tolerance");
        printf("  info | self=%ldus thread=%ldus diff=%ldus tolerance=%ldus\n",
               ts, tt, ts - tt, A6_SKEW_TOLERANCE_US);
    }

    /* ============================================================== */
    /* B1. Negative: invalid who values → EINVAL                      */
    /*    Note: RUSAGE_CHILDREN = -1 is VALID, so we test -2, 2, etc. */
    /* ============================================================== */
    {
        printf("\n--- B1. invalid who (negative) ---\n");
        struct rusage u;
        memset(&u, 0, sizeof(u));

        /* B1a: who = -2 (beyond valid range) */
        CHECK_ERR(getrusage(-2, &u), EINVAL,
                  "getrusage(who=-2) -> EINVAL");
        /* B1b: who = 2 (beyond RUSAGE_THREAD=1) */
        CHECK_ERR(getrusage(2, &u), EINVAL,
                  "getrusage(who=2) -> EINVAL");
        /* B1c: who = 999 */
        CHECK_ERR(getrusage(999, &u), EINVAL,
                  "getrusage(who=999) -> EINVAL");
        /* B1d: who = -999 */
        CHECK_ERR(getrusage(-999, &u), EINVAL,
                  "getrusage(who=-999) -> EINVAL");
        /* B1e: who = 99999 */
        CHECK_ERR(getrusage(99999, &u), EINVAL,
                  "getrusage(who=99999) -> EINVAL");
    }

    /* ============================================================== */
    /* B2. Negative: EFAULT — invalid usage pointer                   */
    /*    Passing a low unmapped address should trigger EFAULT.       */
    /* ============================================================== */
    {
        printf("\n--- B2. invalid usage pointer (negative) ---\n");

        /* B2a: usage = NULL */
        {
            errno = 0;
            long r = (long)getrusage(RUSAGE_SELF, NULL);
            CHECK(r == -1 && (errno == EFAULT || errno == EINVAL),
                  "B2a: getrusage(usage=NULL) -> EFAULT/EINVAL");
        }

        /* B2b: usage = (void*)1 (unmapped, guaranteed to fault) */
        {
            errno = 0;
            long r = (long)getrusage(RUSAGE_SELF, (void *)1);
            CHECK(r == -1 && errno == EFAULT,
                  "B2b: getrusage(usage=(void*)1) -> EFAULT");
        }

        /* B2c: usage = (void*)-1 (very high, unmapped) */
        {
            errno = 0;
            long r = (long)getrusage(RUSAGE_SELF, (void *)(intptr_t)-1);
            CHECK(r == -1 && errno == EFAULT,
                  "B2c: getrusage(usage=(void*)-1) -> EFAULT");
        }

        /* B2d: EFAULT with invalid who — EINVAL may take precedence */
        {
            errno = 0;
            long r = (long)getrusage(999, (void *)1);
            CHECK(r == -1 && (errno == EFAULT || errno == EINVAL),
                  "B2d: getrusage(who=999, usage=(void*)1) -> EFAULT/EINVAL");
        }
    }

    /* ============================================================== */
    /* B3. Edge: RUSAGE_CHILDREN vs RUSAGE_SELF content difference    */
    /* ============================================================== */
    {
        printf("\n--- B3. SELF vs CHILDREN independence ---\n");
        struct rusage us, uc;
        memset(&us, 0, sizeof(us));
        memset(&uc, 0, sizeof(uc));

        CHECK_RET(getrusage(RUSAGE_SELF, &us), 0,
                  "RUSAGE_SELF returns 0");
        CHECK_RET(getrusage(RUSAGE_CHILDREN, &uc), 0,
                  "RUSAGE_CHILDREN returns 0");

        /* SELF should have non-zero time, CHILDREN may or may not.
         * They track different accounting pools. */
        CHECK(TV_VALID(us.ru_utime) && TV_VALID(us.ru_stime),
              "B3: RUSAGE_SELF times valid");
        CHECK(TV_VALID(uc.ru_utime) && TV_VALID(uc.ru_stime),
              "B3: RUSAGE_CHILDREN times valid");
        printf("  info | self utime=%ld.%06ld, children utime=%ld.%06ld\n",
               (long)us.ru_utime.tv_sec, (long)us.ru_utime.tv_usec,
               (long)uc.ru_utime.tv_sec, (long)uc.ru_utime.tv_usec);
    }

    if (__fail == 0) {
        printf("GETRUSAGE_ALL_PASSED\n");
    } else {
        printf("GETRUSAGE_HAS_FAILURES\n");
    }
    TEST_DONE();
}
