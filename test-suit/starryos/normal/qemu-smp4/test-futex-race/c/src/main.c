/*
 * test-futex-race.c - Exerciser for futex table operations.
 *
 * Repeatedly creates and drops FutexTable entries by calling
 * FUTEX_WAIT (which immediately returns EAGAIN since the value
 * changes each iteration). This stresses the FutexGuard::drop
 * cleanup path which was fixed to check strong_count inside
 * the table lock, closing a TOCTOU window on SMP.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <errno.h>
#include <string.h>
#include <stdatomic.h>

#define FUTEX_WAIT 0
#define FUTEX_WAKE 1
#define N_ITERATIONS 10000

static atomic_int g_futex;

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("=== Futex Table Stress Test ===\n");

    atomic_store(&g_futex, 0);
    int errors = 0;

    for (int i = 0; i < N_ITERATIONS; i++) {
        /* Change value each iteration so FUTEX_WAIT returns EAGAIN.
           This creates a FutexGuard (via get_or_insert), checks the
           condition, and drops the guard — exercising the cleanup. */
        atomic_store(&g_futex, i);
        long rc = syscall(SYS_futex, &g_futex, FUTEX_WAIT, i + 1,
                          NULL, NULL, 0);
        if (rc != -1 || errno != EAGAIN) {
            printf("  FAIL iter %d: rc=%ld errno=%d (%s)\n",
                   i, rc, errno, strerror(errno));
            errors++;
            if (errors > 5) break;
        }
    }

    /* Also test FUTEX_WAKE on empty queue (another guard drop path) */
    for (int i = 0; i < 1000; i++) {
        atomic_store(&g_futex, i);
        long rc = syscall(SYS_futex, &g_futex, FUTEX_WAKE, 1,
                          NULL, NULL, 0);
        if (rc < 0) {
            printf("  FAIL wake iter %d: rc=%ld errno=%d (%s)\n",
                   i, rc, errno, strerror(errno));
            errors++;
            if (errors > 5) break;
        }
    }

    if (errors > 0) {
        printf("FAIL: %d errors\n", errors);
        return 1;
    }

    printf("PASS: futex operations completed (%d iterations)\n",
           N_ITERATIONS);
    return 0;
}
