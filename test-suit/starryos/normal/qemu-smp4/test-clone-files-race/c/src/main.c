/*
 * test-clone-files-race.c — Stress test for close_all_fds vs clone(CLONE_FILES).
 *
 * The parent creates a pipe.  It spawns a clone worker that repeatedly
 * forks children sharing the FD table via CLONE_FILES.  Each child
 * verifies FD table integrity by calling fstat() on the pipe read end —
 * if the FD table was cleared by a racing close_all_fds, fstat returns
 * EBADF.  An exit_racer thread concurrently calls _exit() to trigger
 * close_all_fds close to the clone worker's CLONE_FILES operations.
 *
 * Under the OLD code, close_all_fds checked strong_count outside the
 * FD_TABLE lock, allowing a concurrent clone to bump the count between
 * the check and lock acquisition.  The exiting thread would then clear
 * a shared FD table.
 *
 * With the fix, clone(CLONE_FILES) acquires FD_TABLE.read() before
 * cloning the Arc, creating a shared synchronization boundary with
 * close_all_fds' FD_TABLE.write().
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <sched.h>
#include <unistd.h>

#define N_ITERATIONS 2000
#define STACK_SIZE   (64 * 1024)

static int  g_pipefd[2];
static volatile int g_ebadf;   /* child got EBADF on fstat */
static volatile int g_done;

static int clone_child_fn(void *arg) {
    int i = (int)(long)arg;
    struct stat st;
    int rc = fstat(g_pipefd[0], &st);
    if (rc < 0) {
        printf("  FAIL | %s:%d | child %d fstat errno=%d (%s)\n",
               __FILE__, __LINE__, i, errno, strerror(errno));
        g_ebadf = 1;
        _exit(1);
    }
    _exit(0);
}

static int clone_worker(void *arg) {
    (void)arg;

    /* One reusable stack for clone children (sequential use). */
    void *cld_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cld_stack == MAP_FAILED) {
        printf("  FAIL | %s:%d | cld_stack mmap errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        g_ebadf = 1;
        return 1;
    }

    int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

    for (int i = 0; i < N_ITERATIONS && !g_ebadf; i++) {
        int cld_pid = clone(clone_child_fn,
                            (char *)cld_stack + STACK_SIZE,
                            flags, (void *)(long)i);
        if (cld_pid < 0) {
            printf("  FAIL | %s:%d | clone %d errno=%d (%s)\n",
                   __FILE__, __LINE__, i, errno, strerror(errno));
            g_ebadf = 1;
            break;
        }
        int status;
        waitpid(cld_pid, &status, __WALL);
    }

    munmap(cld_stack, STACK_SIZE);
    g_done = 1;
    return 0;
}

/* ─── Exit racer ────────────────────────────────────────────────── */

static int exit_racer(void *arg) {
    (void)arg;
    for (int i = 0; i < 10; i++)
        sched_yield();
    _exit(0);
    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static int watchdog_thread(void *arg) {
    (void)arg;
    for (int sec = 0; sec < 180; sec++) {
        sleep(1);
        if (g_ebadf) _exit(1);
        if (g_done) return 0;
    }
    printf("  FAIL | %s:%d | timeout 180s\n", __FILE__, __LINE__);
    _exit(1);
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("clone_files_race");

    {
        g_ebadf = 0;
        g_done = 0;

        CHECK(pipe(g_pipefd) == 0, "create pipe");

        void *stk_wrk  = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_exit = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_wdog = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

        CHECK(stk_wrk  != MAP_FAILED, "stack_wrk mmap");
        CHECK(stk_exit != MAP_FAILED, "stack_exit mmap");
        CHECK(stk_wdog != MAP_FAILED, "stack_wdog mmap");

        if (stk_wrk == MAP_FAILED || stk_exit == MAP_FAILED ||
            stk_wdog == MAP_FAILED)
            goto cleanup;

        int f = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

        int tw = clone(watchdog_thread,
                       (char *)stk_wdog + STACK_SIZE, f, NULL);
        CHECK(tw >= 0, "clone watchdog");

        int te = clone(exit_racer,
                       (char *)stk_exit + STACK_SIZE, f, NULL);
        CHECK(te >= 0, "clone exit_racer");

        int tr = clone(clone_worker,
                       (char *)stk_wrk + STACK_SIZE, f, NULL);
        CHECK(tr >= 0, "clone worker");

        if (tr >= 0) { int st; waitpid(tr, &st, __WALL); }
        if (te >= 0) { int st; waitpid(te, &st, __WALL); }
        if (tw >= 0) { int st; waitpid(tw, &st, __WALL); }

        CHECK(!g_ebadf, "no FD table corruption");
        CHECK(g_done,    "all iterations completed");

    cleanup:
        if (stk_wrk  != MAP_FAILED) munmap(stk_wrk,  STACK_SIZE);
        if (stk_exit != MAP_FAILED) munmap(stk_exit, STACK_SIZE);
        if (stk_wdog != MAP_FAILED) munmap(stk_wdog, STACK_SIZE);
        close(g_pipefd[0]);
        close(g_pipefd[1]);
    }

    TEST_DONE();
}
