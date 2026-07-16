/*
 * test-unshare-fs — verify unshare(CLONE_FS).
 *
 * nixpkgs 测试可能用到 — unshare(CLONE_FS) 是 Nix fetchTarball
 * 下载线程的前置依赖。
 *
 * Scenarios:
 *   1. unshare(CLONE_FS) on independent task → returns 0.
 *   2. unshare(CLONE_FILES) in one thread leaves sibling fd tables unchanged.
 *   3. clone(CLONE_FS) → share cwd → child unshare(CLONE_FS) → cwd
 *      isolation: child chdir must not affect parent cwd.
 *   4. unshare(0xDEAD) → EINVAL.
 *
 * Note: uses clone(CLONE_FS | SIGCHLD), NOT fork().  In this kernel
 * fork() does NOT share FS_CONTEXT, so a fork-based test would pass
 * even when CLONE_FS sharing + unshare isolation is broken.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define fail(fmt, ...) do { \
    fprintf(stderr, "FAIL | %s:%d | " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__); \
    exit(1); \
} while(0)

#define pass(fmt, ...) \
    printf("  PASS | %s:%d | " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__)

#define check(cond, fmt, ...) do { \
    if (cond) pass(fmt, ##__VA_ARGS__); else fail(fmt, ##__VA_ARGS__); \
} while(0)

/* Minimal stack for clone(CLONE_FS) child — 64KiB should be enough for
   chdir / getcwd / _exit; SIGCHLD so waitpid works. */
#define STACK_SIZE (64 * 1024)

struct clone_arg {
    int *shared;
    int barrier;
};

struct files_unshare_arg {
    int fd;
    int unshare_rc;
    int unshare_errno;
    int close_rc;
};

static void *unshare_files_thread(void *opaque) {
    struct files_unshare_arg *arg = opaque;

    errno = 0;
    arg->unshare_rc = unshare(CLONE_FILES);
    arg->unshare_errno = errno;
    arg->close_rc = arg->unshare_rc == 0 ? close(arg->fd) : -1;
    return NULL;
}

static void test_thread_unshare_files_isolation(void) {
    int fd = open("/dev/null", O_RDONLY);
    check(fd >= 0, "open fd shared by pthreads");

    struct files_unshare_arg arg = {
        .fd = fd,
        .unshare_rc = -1,
        .unshare_errno = 0,
        .close_rc = -1,
    };
    pthread_t thread;
    int rc = pthread_create(&thread, NULL, unshare_files_thread, &arg);
    check(rc == 0, "create CLONE_FILES-sharing pthread (rc=%d)", rc);
    if (rc == 0)
        check(pthread_join(thread, NULL) == 0, "join CLONE_FILES pthread");

    check(arg.unshare_rc == 0,
          "pthread unshare(CLONE_FILES) succeeds (rc=%d, errno=%d)",
          arg.unshare_rc, arg.unshare_errno);
    check(arg.close_rc == 0, "pthread closes fd in its private table");

    errno = 0;
    check(fcntl(fd, F_GETFD) >= 0,
          "calling thread retains fd after sibling unshare+close (errno=%d)", errno);
    close(fd);

    printf("UNSHARE_FILES_THREAD_ISOLATION_PASSED\n");
}

static int clone_child(void *arg) {
    struct clone_arg *a = (struct clone_arg *)arg;

    /* After clone(CLONE_FS), parent and child share the same
       FS_CONTEXT.  chdir here should be visible to the parent. */
    int rc = chdir("/tmp");
    check(rc == 0, "clone child (shared FS) chdir to /tmp (rc=%d, errno=%d)",
          rc, errno);

    /* Signal parent: I've chdir'd. */
    a->shared[0] = 1;
    while (a->shared[1] == 0) usleep(10000);

    /* Parent should have observed /tmp while FS_CONTEXT was shared. */
    while (a->shared[2] == 0) usleep(10000);

    /* Now unshare(CLONE_FS) — break the shared FS_CONTEXT. */
    rc = unshare(CLONE_FS);
    check(rc == 0, "clone child unshare(CLONE_FS) (rc=%d, errno=%d)", rc, errno);

    /* Now cwd changes here must be invisible to parent. */
    rc = chdir("/usr");
    check(rc == 0, "child chdir to /usr after unshare(CLONE_FS) (rc=%d, errno=%d)",
          rc, errno);

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "child getcwd after unshare");
    check(strcmp(cwd, "/usr") == 0,
          "child cwd is /usr after unshare+chdir: %s", cwd);

    /* Tell parent: child finished its side. */
    a->shared[3] = 1;
    _exit(0);
    return 0;
}

static void test_unshare_fs_basic(void) {
    int rc = unshare(CLONE_FS);
    check(rc == 0, "unshare(CLONE_FS) on independent task (rc=%d, errno=%d)",
          rc, errno);

    rc = unshare(CLONE_FILES);
    check(rc == 0, "unshare(CLONE_FILES) on independent task (rc=%d, errno=%d)",
          rc, errno);

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "getcwd after unshare(CLONE_FS)");
    check(cwd[0] == '/', "cwd valid after unshare(CLONE_FS): %s", cwd);

    printf("UNSHARE_FS_BASIC_PASSED\n");
}

static void test_clone_fs_unshare_isolation(void) {
    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check(shared != MAP_FAILED, "mmap shared page");
    shared[0] = 0; shared[1] = 0; shared[2] = 0; shared[3] = 0;

    /* Allocate stack on heap for the clone child. */
    char *stack = mmap(NULL, STACK_SIZE, PROT_READ | PROT_WRITE,
                       MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    check(stack != MAP_FAILED, "mmap child stack");
    /* clone(2) takes the *top* of the stack on x86_64. */
    char *stack_top = stack + STACK_SIZE;

    /* Set up parent baseline cwd. */
    mkdir("/tmp/unshare-fs-dirP", 0755);
    int rc = chdir("/tmp/unshare-fs-dirP");
    check(rc == 0, "parent chdir to /tmp/unshare-fs-dirP");

    struct clone_arg arg = { .shared = shared, .barrier = 0 };
    pid_t pid = clone(clone_child, (void *)stack_top,
                      CLONE_FS | CLONE_VM | SIGCHLD, &arg);
    check(pid > 0, "clone(CLONE_FS|CLONE_VM|SIGCHLD) returned pid=%d", pid);

    /* Wait for child to chdir to /tmp (shared FS_CONTEXT). */
    while (shared[0] == 0) usleep(10000);

    /* Verify parent sees child's chdir — proving FS_CONTEXT was shared. */
    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "parent getcwd after child shared chdir");
    check(strcmp(cwd, "/tmp") == 0,
          "parent observes child chdir (shared FS): cwd=%s", cwd);

    /* Tell child to proceed with unshare. */
    shared[1] = 1;
    shared[2] = 1;

    /* Wait for child to unshare(CLONE_FS) + chdir /usr. */
    while (shared[3] == 0) usleep(10000);

    /* Verify parent cwd is still /tmp — isolation works. */
    check(getcwd(cwd, sizeof(cwd)) != NULL, "parent getcwd after child unshare");
    check(strcmp(cwd, "/tmp") == 0,
          "parent cwd unchanged after child unshare+chdir: %s", cwd);

    waitpid(pid, NULL, 0);
    munmap(stack, STACK_SIZE);
    munmap(shared, 4096);
    rmdir("/tmp/unshare-fs-dirP");

    printf("UNSHARE_FS_CLONE_ISOLATION_PASSED\n");
}

static void test_unshare_invalid_flags(void) {
    int rc = unshare(0xDEAD);
    check(rc == -1 && errno == EINVAL,
          "unshare(0xDEAD) returns EINVAL (rc=%d, errno=%d)", rc, errno);
    printf("UNSHARE_FS_INVALID_PASSED\n");
}

int main(void) {
    test_thread_unshare_files_isolation();
    test_unshare_fs_basic();
    test_clone_fs_unshare_isolation();
    test_unshare_invalid_flags();
    printf("UNSHARE_FS_ALL_PASSED\n");
    return 0;
}
