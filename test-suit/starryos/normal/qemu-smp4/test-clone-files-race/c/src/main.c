/*
 * test-clone-files-race.c — close_all_fds vs clone(CLONE_FILES) race.
 *
 * Two worker threads inside the main process each continuously create
 * clone children with CLONE_FILES.  Every child verifies the FD table
 * is intact by calling fstat() on a pipe FD inherited from the parent,
 * then exits immediately.  When a child exits, close_all_fds checks
 * Arc::strong_count(&FD_TABLE); if at that moment no other clone
 * children exist, strong_count == 1.
 *
 * Under the OLD (buggy) code, close_all_fds checked strong_count
 * OUTSIDE the FD_TABLE lock.  On SMP, the other worker thread could
 * concurrently create a new clone child (bumping strong_count to 2)
 * between that check and the lock acquisition.  The exiting child
 * would then clear a now-shared FD table, causing the newly created
 * child to get EBADF on fstat.
 *
 * With the fix, clone(CLONE_FILES) holds FD_TABLE.read() during the
 * clone, and close_all_fds holds FD_TABLE.write(), so strong_count
 * is always accurate.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <sched.h>
#include <unistd.h>

#define N_ITERATIONS 2000
#define STACK_SIZE   (64 * 1024)

static int  g_pipefd[2];
static volatile int g_corrupt;  /* child got EBADF */
static volatile int g_done[N_ITERATIONS];  /* per-iteration completion */
static volatile int g_round;    /* next iteration slot */

/* ─── Clone child: verify FD then exit ──────────────────────────── */

static int clone_child_fn(void *arg) {
    int slot = (int)(long)arg;
    struct stat st;
    int rc = fstat(g_pipefd[0], &st);
    if (rc < 0) {
        printf("  FAIL | %s:%d | child slot %d fstat errno=%d (%s)\n",
               __FILE__, __LINE__, slot, errno, strerror(errno));
        g_corrupt = 1;
        _exit(1);
    }
    g_done[slot] = 1;
    _exit(0);
}

/* ─── Worker: keep spawning clone children ──────────────────────── */

static int worker_thread(void *arg) {
    int wid = (int)(long)arg;
    (void)wid;

    /* Reusable child stack — children are sequential per worker */
    void *cld_stk = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cld_stk == MAP_FAILED) return 1;

    int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

    while (!g_corrupt) {
        int slot = __atomic_fetch_add(&g_round, 1, __ATOMIC_RELAXED);
        if (slot >= N_ITERATIONS) break;

        int cld = clone(clone_child_fn,
                        (char *)cld_stk + STACK_SIZE,
                        flags, (void *)(long)slot);
        if (cld < 0) {
            printf("  FAIL | %s:%d | worker %d clone errno=%d (%s)\n",
                   __FILE__, __LINE__, wid, errno, strerror(errno));
            g_corrupt = 1;
            break;
        }
        /* Reap immediately so the child's close_all_fds runs.
           While we wait here, the OTHER worker runs on another
           core and may be creating its next clone child, opening
           the TOCTOU window. */
        int status;
        waitpid(cld, &status, __WALL);
    }

    munmap(cld_stk, STACK_SIZE);
    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static int watchdog_thread(void *arg) {
    (void)arg;
    for (int sec = 0; sec < 120; sec++) {
        sleep(1);
        if (g_corrupt) _exit(1);

        int done = 0;
        for (int i = 0; i < N_ITERATIONS; i++)
            if (g_done[i]) done++;
        if (done >= N_ITERATIONS) return 0;
    }
    printf("  FAIL | %s:%d | timeout 120s\n", __FILE__, __LINE__);
    _exit(1);
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("clone_files_race");

    {
        g_corrupt = 0;
        g_round = 0;
        for (int i = 0; i < N_ITERATIONS; i++) g_done[i] = 0;

        CHECK(pipe(g_pipefd) == 0, "create pipe");

        void *stk_w1 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_w2 = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        void *stk_wd = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                            MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        CHECK(stk_w1 != MAP_FAILED, "stack w1");
        CHECK(stk_w2 != MAP_FAILED, "stack w2");
        CHECK(stk_wd != MAP_FAILED, "stack wd");
        if (stk_w1 == MAP_FAILED || stk_w2 == MAP_FAILED ||
            stk_wd == MAP_FAILED) goto cleanup;

        int f = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;

        int twd = clone(watchdog_thread,
                        (char *)stk_wd + STACK_SIZE, f, NULL);
        CHECK(twd >= 0, "clone watchdog");

        int tw1 = clone(worker_thread,
                        (char *)stk_w1 + STACK_SIZE, f, (void *)0);
        CHECK(tw1 >= 0, "clone worker1");

        int tw2 = clone(worker_thread,
                        (char *)stk_w2 + STACK_SIZE, f, (void *)1);
        CHECK(tw2 >= 0, "clone worker2");

        if (tw1 >= 0) { int s; waitpid(tw1, &s, __WALL); }
        if (tw2 >= 0) { int s; waitpid(tw2, &s, __WALL); }
        if (twd >= 0) { int s; waitpid(twd, &s, __WALL); }

        int done = 0;
        for (int i = 0; i < N_ITERATIONS; i++) if (g_done[i]) done++;
        CHECK(!g_corrupt,  "no FD table corruption");
        CHECK(done == N_ITERATIONS, "all iterations completed");

    cleanup:
        if (stk_w1 != MAP_FAILED) munmap(stk_w1, STACK_SIZE);
        if (stk_w2 != MAP_FAILED) munmap(stk_w2, STACK_SIZE);
        if (stk_wd != MAP_FAILED) munmap(stk_wd, STACK_SIZE);
        close(g_pipefd[0]);
        close(g_pipefd[1]);
    }

    TEST_DONE();
}
