/*
 * Multi-threaded execve regression test.
 *
 * Phase 1: a failed execve from a multi-threaded process must leave the
 *          thread group intact (POSIX says execve failures preserve the
 *          process state).
 * Phase 2: execve from a non-leader thread is currently rejected with
 *          EPERM (full Linux de_thread leader transfer is TODO); the
 *          process must remain intact and joinable after that rejection.
 * Phase 3: a successful execve from the leader of a multi-threaded
 *          process must tear down the sibling threads and run the new
 *          image; the new image must observe gettid() == getpid().
 */

#include "test_framework.h"

#include <pthread.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

static int sibling_ready = 0;
static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;

#define SIBLING_SENTINEL ((void *)0xfeedbeefL)

static pid_t my_gettid(void) { return (pid_t)syscall(SYS_gettid); }

static void *sibling_quick(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&mtx);
    sibling_ready++;
    pthread_cond_broadcast(&cond);
    pthread_mutex_unlock(&mtx);

    /* Stay alive long enough for the main thread to run the bad execve.
     * 100 ms total is plenty without slowing the test down. */
    for (int i = 0; i < 100; i++) {
        struct timespec ts = { 0, 1000000 }; /* 1 ms */
        nanosleep(&ts, NULL);
    }
    return SIBLING_SENTINEL;
}

static void *sibling_block(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&mtx);
    sibling_ready++;
    pthread_cond_broadcast(&cond);
    pthread_mutex_unlock(&mtx);

    /* Block until the exec path zaps us. */
    for (;;) {
        pause();
    }
    return NULL;
}

struct nonleader_exec_result {
    int ret;
    int saved_errno;
};

static void *nonleader_exec_thread(void *arg)
{
    struct nonleader_exec_result *r = arg;
    /* Bad path keeps this safe even if the EPERM check ever regresses:
     * we'd then fall through to path resolution and get ENOENT instead
     * of actually replacing the process image. */
    char *av[] = { (char *)"/this/path/does/not/exist/mt-execve", NULL };
    char *ev[] = { NULL };
    errno = 0;
    r->ret = execve("/this/path/does/not/exist/mt-execve", av, ev);
    r->saved_errno = errno;
    return NULL;
}

static void wait_for_siblings(int n)
{
    pthread_mutex_lock(&mtx);
    while (sibling_ready < n) pthread_cond_wait(&cond, &mtx);
    pthread_mutex_unlock(&mtx);
}

int main(int argc, char *argv[])
{
    /* Unbuffered so phase markers reach the test runner before execve. */
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    /* Re-entry after a successful execve from the multi-threaded parent. */
    if (argc >= 2 && strcmp(argv[1], "child-after-exec") == 0) {
        /* The new image must be single-threaded with the original TGID
         * as its identity. If sibling teardown leaked or the leader
         * identity drifted, this would fail. */
        pid_t tid = my_gettid();
        pid_t pid = getpid();
        if (tid != pid) {
            fprintf(stderr,
                    "FAIL: post-exec gettid(%d) != getpid(%d)\n",
                    (int)tid, (int)pid);
            return 1;
        }
        printf("EXECVE_CHILD_OK\n");
        return 0;
    }

    TEST_START("multi-thread execve");

    /* Phase 1: failed execve preserves thread group. */
    sibling_ready = 0;
    pthread_t qt1, qt2;
    CHECK(pthread_create(&qt1, NULL, sibling_quick, NULL) == 0,
          "spawn quick sibling 1");
    CHECK(pthread_create(&qt2, NULL, sibling_quick, NULL) == 0,
          "spawn quick sibling 2");
    wait_for_siblings(2);

    char *bad_argv[] = { (char *)"/this/path/does/not/exist/mt-execve", NULL };
    char *bad_envp[] = { NULL };
    errno = 0;
    int br = execve("/this/path/does/not/exist/mt-execve", bad_argv, bad_envp);
    CHECK(br == -1, "execve to nonexistent path returns -1");
    CHECK(errno == ENOENT,
          "errno is ENOENT after failed execve");

    void *r1 = NULL, *r2 = NULL;
    CHECK(pthread_join(qt1, &r1) == 0, "sibling 1 joinable after failed execve");
    CHECK(pthread_join(qt2, &r2) == 0, "sibling 2 joinable after failed execve");
    CHECK(r1 == SIBLING_SENTINEL, "sibling 1 returned its sentinel");
    CHECK(r2 == SIBLING_SENTINEL, "sibling 2 returned its sentinel");

    if (__fail > 0) {
        TEST_DONE();
    }

    /* Phase-1 marker. The success regex ANDs this with the post-exec marker
     * so the test only passes if all phases work. */
    printf("PHASE1_OK\n");

    /* Phase 2: non-leader execve is rejected with EPERM and does not
     * disturb the process. Until we implement de_thread leader transfer,
     * the kernel returns EPERM for any execve where caller_tid != tgid. */
    struct nonleader_exec_result nl = { 0, 0 };
    pthread_t nlt;
    CHECK(pthread_create(&nlt, NULL, nonleader_exec_thread, &nl) == 0,
          "spawn non-leader exec attempt");
    CHECK(pthread_join(nlt, NULL) == 0, "non-leader exec thread joinable");
    CHECK(nl.ret == -1, "non-leader execve returns -1");
    CHECK(nl.saved_errno == EPERM,
          "non-leader execve sets errno to EPERM");
    /* Process must still be the same one (TGID unchanged, leader alive). */
    CHECK(my_gettid() == getpid(),
          "leader identity intact after non-leader exec attempt");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE2_OK\n");

    /* Phase 3: successful execve from the leader of a multi-threaded
     * process. Siblings block in pause() and are zapped during de_thread;
     * control then jumps into the new image. */
    sibling_ready = 0;
    pthread_t lt1, lt2, lt3;
    CHECK(pthread_create(&lt1, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 1");
    CHECK(pthread_create(&lt2, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 2");
    CHECK(pthread_create(&lt3, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 3");
    wait_for_siblings(3);

    if (__fail > 0) {
        TEST_DONE();
    }

    char *good_argv[] = { argv[0], (char *)"child-after-exec", NULL };
    char *good_envp[] = { NULL };
    execve(argv[0], good_argv, good_envp);

    /* Should not be reached on success. */
    fprintf(stderr, "FAIL: good execve returned, errno=%d (%s)\n",
            errno, strerror(errno));
    return 1;
}
