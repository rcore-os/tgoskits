/*
 * test-pthread-atfork — glibc registers pthread_atfork handlers to
 * reinitialize malloc arenas after fork. If StarryOS fork doesn't
 * properly invoke atfork handlers, glibc's post-fork malloc state
 * is corrupted.
 *
 * Tests pthread_atfork prepare/parent/child handler execution order.
 */

#include "test_framework.h"

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static int prepare_called  = 0;
static int parent_called   = 0;
static int child_called    = 0;

static void prepare_handler(void)  { prepare_called++; }
static void parent_handler(void)   { parent_called++;  }
static void child_handler(void)    { child_called++;   }

int main(void)
{
    printf("NIX_ATFORK_BEGIN\n");
    TEST_START("pthread-atfork: handler execution across fork");

    /* Register handlers */
    int rc = pthread_atfork(prepare_handler, parent_handler, child_handler);
    CHECK(rc == 0, "pthread_atfork succeeds");

    /* Reset counters (pthread_atfork itself doesn't call handlers) */
    prepare_called = 0;
    parent_called  = 0;
    child_called   = 0;

    pid_t child = fork();
    CHECK(child >= 0, "fork succeeds");
    if (child < 0) return 1;

    if (child == 0) {
        /* Child: verify child handler was called, parent handler NOT called */
        printf("CHILD_ATFORK: prepare=%d parent=%d child=%d\n",
               prepare_called, parent_called, child_called);

        int ok = (prepare_called > 0) && (parent_called == 0) && (child_called > 0);
        _exit(ok ? 0 : 1);
    }

    /* Parent: wait for child */
    int status = 0;
    waitpid(child, &status, 0);
    CHECK(WIFEXITED(status), "child exited normally");
    CHECK(WEXITSTATUS(status) == 0, "child atfork handlers correct");

    /* Parent: verify prepare + parent handlers called, child NOT called */
    printf("PARENT_ATFORK: prepare=%d parent=%d child=%d\n",
           prepare_called, parent_called, child_called);
    CHECK(prepare_called > 0, "prepare handler called in parent");
    CHECK(parent_called > 0, "parent handler called in parent");
    CHECK(child_called == 0, "child handler NOT called in parent");

    /* ── Multiple fork: verify handlers fire each time ── */
    prepare_called = 0;
    parent_called  = 0;
    child_called   = 0;

    pid_t child2 = fork();
    CHECK(child2 >= 0, "second fork succeeds");
    if (child2 == 0) {
        int ok2 = (prepare_called > 0) && (parent_called == 0) && (child_called > 0);
        _exit(ok2 ? 0 : 2);
    }
    waitpid(child2, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "second fork atfork correct");
    CHECK(prepare_called > 0 && parent_called > 0 && child_called == 0,
          "second fork: parent handlers correct");

    printf("NIX_ATFORK_PASSED\n");
    return 0;
}
