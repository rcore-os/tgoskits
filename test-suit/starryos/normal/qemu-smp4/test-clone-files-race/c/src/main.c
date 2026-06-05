/*
 * test-clone-files-race.c — Stresses the close_all_fds / clone(CLONE_FILES)
 * synchronization boundary under SMP.
 *
 * Two worker threads each rapidly create clone children with CLONE_FILES.
 * Every child verifies FD table integrity via fstat() on a pipe inherited
 * from the parent, then exits immediately.  The parent's threads share the
 * same FD_TABLE scope (CLONE_THREAD), so strong_count can never reach 1
 * while workers are alive — but the lock-based synchronization boundary
 * (FD_TABLE.read() in clone, FD_TABLE.write() in close_all_fds) is exercised
 * under heavy concurrent load.
 *
 * Note: on StarryOS, the 1→2 TOCTOU window (close_all_fds checking
 * strong_count==1 outside the lock while a concurrent clone bumps it to 2)
 * is architecturally prevented: close_all_fds only runs for the LAST
 * thread in a process (exit_group semantics), so no other thread can
 * concurrently call clone(CLONE_FILES) in the same scope.  The RWLock
 * synchronization is nonetheless required for correctness — it documents
 * the intended protocol and protects against future changes to the exit
 * model.
 *
 * A watchdog thread monitors progress; stall or corruption triggers FAIL.
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <signal.h>
#include <sched.h>
#include <unistd.h>

/* Keep the normal qemu case bounded; larger stress counts belong in stress. */
#define N_ITERATIONS 64
#define STACK_SIZE   (64 * 1024)
#define CLONE_SHARED_FILES_FLAGS \
    (CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | SIGCHLD)

static int  g_pipefd[2];
static volatile int g_corrupt;
static volatile int g_done;
static volatile int g_round;
static volatile int g_main_pid;
static volatile int g_worker1_pid;
static volatile int g_worker2_pid;

static int clone_child_fn(void *arg) {
    int slot = (int)(long)arg;
    struct stat st;
    if (fstat(g_pipefd[0], &st) < 0) {
        printf("  FAIL | %s:%d | child %d fstat errno=%d (%s)\n",
               __FILE__, __LINE__, slot, errno, strerror(errno));
        g_corrupt = 1;
        _exit(1);
    }
    _exit(0);
}

/* ─── Worker ────────────────────────────────────────────────────── */

static int worker_thread(void *arg) {
    int wid = (int)(long)arg;
    void *cld_stk = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (cld_stk == MAP_FAILED) return 1;

    while (!g_corrupt) {
        int slot = __atomic_fetch_add(&g_round, 1, __ATOMIC_RELAXED);
        if (slot >= N_ITERATIONS) break;

        int cld = clone(clone_child_fn,
                        (char *)cld_stk + STACK_SIZE,
                        CLONE_SHARED_FILES_FLAGS, (void *)(long)slot);
        if (cld < 0) {
            printf("  FAIL | %s:%d | w%d clone errno=%d (%s)\n",
                   __FILE__, __LINE__, wid, errno, strerror(errno));
            g_corrupt = 1;
            break;
        }
        int status = 0;
        if (waitpid(cld, &status, __WALL) < 0 ||
            !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            printf("  FAIL | %s:%d | w%d wait child=%d status=0x%x errno=%d (%s)\n",
                   __FILE__, __LINE__, wid, cld, status, errno, strerror(errno));
            g_corrupt = 1;
            break;
        }
    }

    munmap(cld_stk, STACK_SIZE);
    return 0;
}

/* ─── Watchdog ──────────────────────────────────────────────────── */

static void kill_if_valid(int pid) {
    if (pid > 0) {
        kill(pid, SIGKILL);
    }
}

static void watchdog_fail(const char *reason, int cur) {
    g_corrupt = 1;
    printf("  FAIL | %s:%d | %s (r=%d/%d)\n",
           __FILE__, __LINE__, reason, cur, N_ITERATIONS);
    kill_if_valid(g_worker1_pid);
    kill_if_valid(g_worker2_pid);
    kill_if_valid(g_main_pid);
    _exit(1);
}

static int watchdog_thread(void *arg) {
    (void)arg;
    int last = 0, stalls = 0;
    for (int sec = 0; sec < 180; sec++) {
        sleep(1);
        if (__atomic_load_n(&g_done, __ATOMIC_ACQUIRE)) {
            return 0;
        }
        int cur = __atomic_load_n(&g_round, __ATOMIC_RELAXED);
        if (g_corrupt) {
            watchdog_fail("corruption detected", cur);
        }
        if (cur == last) {
            if (++stalls >= 30) {
                watchdog_fail("stalled", cur);
            }
        } else { stalls = 0; last = cur; }
    }
    watchdog_fail("timeout", g_round);
    return 1;
}

/* ─── Main ──────────────────────────────────────────────────────── */

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("clone_files_race");

    {
        g_corrupt = 0;
        g_done = 0;
        g_round = 0;
        g_main_pid = getpid();
        g_worker1_pid = 0;
        g_worker2_pid = 0;
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

        int td = clone(watchdog_thread,
                       (char *)stk_wd + STACK_SIZE,
                       CLONE_SHARED_FILES_FLAGS, NULL);
        CHECK(td >= 0, "clone watchdog");

        int t1 = clone(worker_thread,
                       (char *)stk_w1 + STACK_SIZE,
                       CLONE_SHARED_FILES_FLAGS, (void *)0);
        g_worker1_pid = t1;
        CHECK(t1 >= 0, "clone worker1");

        int t2 = clone(worker_thread,
                       (char *)stk_w2 + STACK_SIZE,
                       CLONE_SHARED_FILES_FLAGS, (void *)1);
        g_worker2_pid = t2;
        CHECK(t2 >= 0, "clone worker2");

        if (t1 >= 0) { int s; waitpid(t1, &s, __WALL); }
        if (t2 >= 0) { int s; waitpid(t2, &s, __WALL); }
        __atomic_store_n(&g_done, 1, __ATOMIC_RELEASE);
        if (td >= 0) kill(td, SIGKILL);
        if (td >= 0) { int s; waitpid(td, &s, __WALL); }

        CHECK(!g_corrupt && g_round >= N_ITERATIONS,
              "no FD table corruption, all iterations done");

    cleanup:
        if (stk_w1 != MAP_FAILED) munmap(stk_w1, STACK_SIZE);
        if (stk_w2 != MAP_FAILED) munmap(stk_w2, STACK_SIZE);
        if (stk_wd != MAP_FAILED) munmap(stk_wd, STACK_SIZE);
        close(g_pipefd[0]);
        close(g_pipefd[1]);
    }

    TEST_DONE();
}
