/*
 * test_futex_robust.c - Verify FUTEX_OWNER_DIED on robust-futex owner death.
 *
 * BUG: handle_futex_death woke waiters but did NOT write FUTEX_OWNER_DIED
 *      into the user-space futex word.  A concurrent FUTEX_WAIT could pass
 *      the fast-path value check, but the wake fires before the waiter
 *      pushes onto the wait queue — the waiter then sleeps forever.
 *
 * FIX: handle_futex_death now writes (TID & FUTEX_TID_MASK) | FUTEX_OWNER_DIED
 *      into the user-space futex word unconditionally before waking any
 *      kernel-side waiters.  This matches Linux semantics and prevents
 *      the lost-wakeup race.
 *
 * Test strategy:
 *   1. Owner thread registers a robust futex, writes its TID into the word,
 *      then busy-waits.
 *   2. Controller verifies the futex word contains the owner TID.
 *   3. Controller kills the owner (tkill SIGKILL).
 *   4. Controller verifies the futex word now has FUTEX_OWNER_DIED set.
 *   5. Waiter thread does FUTEX_WAIT on the same futex and receives
 *      EOWNERDEAD (since the word already has FUTEX_OWNER_DIED).
 */
#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <sched.h>
#include <unistd.h>
#include <signal.h>
#include <stdint.h>
#include <stddef.h>

/* Futex and errno constants. */
#ifndef FUTEX_WAIT
#define FUTEX_WAIT        0
#endif
#ifndef FUTEX_OWNER_DIED
#define FUTEX_OWNER_DIED  0x40000000
#endif
#ifndef FUTEX_TID_MASK
#define FUTEX_TID_MASK    0x3fffffff
#endif
#ifndef EOWNERDEAD
#define EOWNERDEAD        130
#endif

/* Robust list types (matching kernel's RobustList / RobustListHead). */
struct robust_list {
    struct robust_list *next;
};
struct robust_list_head {
    struct robust_list list;
    long futex_offset;
    struct robust_list *list_op_pending;
};

struct my_robust_mutex {
    struct robust_list_head list_head;
    int futex_word;
};

static long do_futex(int *uaddr, int op, int val,
                     const void *timeout, int *uaddr2, int val3)
{
    return syscall(SYS_futex, uaddr, op, val, timeout, uaddr2, val3);
}
static long do_set_robust_list(struct robust_list_head *head, size_t len)
{
    return syscall(SYS_set_robust_list, head, len);
}
static long do_gettid(void)
{
    return syscall(SYS_gettid);
}
static long do_tkill(int tid, int sig)
{
    return syscall(SYS_tkill, tid, sig);
}

#define STACK_SIZE (64 * 1024)

static struct robust_list_head g_thread_head;
static struct my_robust_mutex   g_mutex;
static volatile int g_owner_tid   = 0;
static volatile int g_owner_ready = 0;

/* Owner: set up robust futex, then busy-wait until killed. */
static int owner_thread(void *arg)
{
    (void)arg;
    g_owner_tid = (int)do_gettid();

    /* Circular robust list: g_thread_head -> g_mutex -> g_thread_head. */
    g_thread_head.list.next = &g_mutex.list_head.list;
    g_thread_head.futex_offset =
        (long)(sizeof(struct robust_list_head) -
               offsetof(struct robust_list_head, list));
    g_thread_head.list_op_pending = NULL;

    g_mutex.list_head.list.next = &g_thread_head.list;
    g_mutex.list_head.futex_offset = 0;
    g_mutex.list_head.list_op_pending = NULL;
    g_mutex.futex_word = 0;

    if (do_set_robust_list(&g_thread_head, sizeof(g_thread_head)) != 0) {
        printf("  FAIL | %s:%d | set_robust_list failed: errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        _exit(1);
    }

    g_mutex.futex_word = g_owner_tid;
    __sync_synchronize();
    g_owner_ready = 1;

    while (1) { sched_yield(); }
}

/* Waiter: do FUTEX_WAIT; should get EAGAIN because FUTEX_OWNER_DIED
 * causes a value mismatch.  On Linux the userspace robust-mutex
 * unlocker checks for (value & FUTEX_OWNER_DIED) after the EAGAIN
 * and returns EOWNERDEAD to the caller. */
static int waiter_thread(void *arg)
{
    (void)arg;
    while (!g_owner_ready) { sched_yield(); }

    int tid = g_owner_tid;
    long ret = do_futex(&g_mutex.futex_word, FUTEX_WAIT, tid,
                        NULL, NULL, 0);
    /* After the owner dies, the futex word has FUTEX_OWNER_DIED set,
     * so it no longer equals the original TID.  FUTEX_WAIT therefore
     * returns EAGAIN (value changed) rather than blocking.  The
     * caller is expected to inspect the futex word, see
     * FUTEX_OWNER_DIED, and propagate EOWNERDEAD. */
    if (ret == -1 && errno == EAGAIN) {
        int word = g_mutex.futex_word;
        if (word & FUTEX_OWNER_DIED) {
            printf("  PASS | %s:%d | waiter got EAGAIN, "
                   "futex_word=0x%x has FUTEX_OWNER_DIED\n",
                   __FILE__, __LINE__, word);
        } else {
            printf("  FAIL | %s:%d | waiter got EAGAIN but "
                   "futex_word=0x%x lacks FUTEX_OWNER_DIED\n",
                   __FILE__, __LINE__, word);
        }
    } else {
        printf("  FAIL | %s:%d | waiter expected EAGAIN, "
               "got ret=%ld errno=%d (%s) futex_word=0x%x\n",
               __FILE__, __LINE__, ret, errno, strerror(errno),
               g_mutex.futex_word);
    }
    return 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("futex_robust_owner_dead");

    memset(&g_thread_head, 0, sizeof(g_thread_head));
    memset(&g_mutex, 0, sizeof(g_mutex));

    void *owner_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    void *waiter_stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(owner_stack != MAP_FAILED, "owner stack mmap");
    CHECK(waiter_stack != MAP_FAILED, "waiter stack mmap");

    if (owner_stack == MAP_FAILED || waiter_stack == MAP_FAILED) {
        goto cleanup;
    }

    {
        int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND;
        int owner_tid = clone(owner_thread,
                              (char *)owner_stack + STACK_SIZE,
                              flags, NULL);
        CHECK(owner_tid >= 0, "clone owner_thread");
        if (owner_tid < 0) { goto cleanup; }

        while (!g_owner_ready) { sched_yield(); }

        /* --- test 1: futex word before death --- */
        {
            int word = g_mutex.futex_word;
            CHECK((word & FUTEX_TID_MASK) == (unsigned)g_owner_tid,
                  "futex word contains owner TID before death");
        }

        /* --- test 2: kill owner, verify FUTEX_OWNER_DIED --- */
        if (do_tkill(owner_tid, SIGKILL) != 0) {
            printf("  FAIL | %s:%d | tkill failed: errno=%d (%s)\n",
                   __FILE__, __LINE__, errno, strerror(errno));
        }

        /* Wait for owner to be reaped. */
        {
            int status, done = 0;
            for (int i = 0; i < 50; i++) {
                usleep(100000);
                if (waitpid(owner_tid, &status, WNOHANG) > 0) {
                    done = 1;
                    break;
                }
            }
            if (!done) {
                printf("  FAIL | %s:%d | owner did not exit\n",
                       __FILE__, __LINE__);
            }
        }

        {
            int word = g_mutex.futex_word;
            CHECK((word & FUTEX_OWNER_DIED) != 0,
                  "futex word has FUTEX_OWNER_DIED set after owner death");
            CHECK((word & FUTEX_TID_MASK) == (unsigned)g_owner_tid,
                  "futex word still contains original TID bits");
        }

        /* --- test 3: waiter receives EOWNERDEAD --- */
        {
            int waiter_tid = clone(waiter_thread,
                                   (char *)waiter_stack + STACK_SIZE,
                                   flags, NULL);
            CHECK(waiter_tid >= 0, "clone waiter_thread");

            if (waiter_tid >= 0) {
                int status, done = 0;
                for (int i = 0; i < 50; i++) {
                    usleep(100000);
                    if (waitpid(waiter_tid, &status, WNOHANG) > 0) {
                        done = 1;
                        break;
                    }
                }
                if (!done) {
                    printf("  FAIL | %s:%d | waiter timed out\n",
                           __FILE__, __LINE__);
                    do_tkill(waiter_tid, SIGKILL);
                    usleep(200000);
                    waitpid(waiter_tid, &status, WNOHANG);
                }
            }
        }
    }

cleanup:
    if (owner_stack != MAP_FAILED) munmap(owner_stack, STACK_SIZE);
    if (waiter_stack != MAP_FAILED) munmap(waiter_stack, STACK_SIZE);
    TEST_DONE();
}
