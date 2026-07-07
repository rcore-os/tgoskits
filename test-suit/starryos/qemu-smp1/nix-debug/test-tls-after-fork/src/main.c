/*
 * test-tls-after-fork — glibc uses TLS for per-thread malloc arenas.
 * After fork, TLS must be correctly copied to child. If StarryOS
 * doesn't properly handle TLS in fork, glibc's arena pointers are stale.
 *
 * Tests __thread variable values across fork.
 */

#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static __thread int tls_value = 42;

int main(void)
{
    printf("NIX_TLS_FORK_BEGIN\n");
    TEST_START("tls-after-fork: __thread variable across fork");

    /* Set TLS value */
    tls_value = 0xDEAD;
    CHECK(tls_value == 0xDEAD, "TLS value set in parent");

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) return 1;

    if (child == 0) {
        /* Child: TLS should be copied from parent */
        int val = tls_value;
        printf("CHILD_TLS: value=0x%x expected=0xDEAD\n", val);

        /* Child modifies its own TLS */
        tls_value = 0xBEEF;
        printf("CHILD_TLS_MODIFIED: value=0x%x\n", tls_value);

        _exit(val == 0xDEAD ? 0 : 1);
    }

    int status = 0;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child TLS value correct");

    /* Parent: TLS should be unchanged by child */
    CHECK(tls_value == 0xDEAD,
          "parent TLS unchanged after child fork+modify");

    /* ── Multiple TLS variables ── */
    static __thread int tls_a = 100;
    static __thread int tls_b = 200;
    static __thread char tls_buf[64] = "hello tls";

    tls_a = 111;
    tls_b = 222;
    strcpy(tls_buf, "fork test");

    pid_t child2 = fork();
    CHECK(child2 >= 0, "second fork succeeds");
    if (child2 == 0) {
        int ok = (tls_a == 111) && (tls_b == 222) &&
                 (strcmp(tls_buf, "fork test") == 0);
        printf("CHILD2_TLS: a=%d b=%d buf=%s ok=%d\n", tls_a, tls_b, tls_buf, ok);
        _exit(ok ? 0 : 2);
    }
    waitpid(child2, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "second fork TLS correct");

    CHECK(tls_a == 111 && tls_b == 222 && strcmp(tls_buf, "fork test") == 0,
          "parent multi-TLS unchanged");

    printf("NIX_TLS_FORK_PASSED\n");
    return 0;
}
