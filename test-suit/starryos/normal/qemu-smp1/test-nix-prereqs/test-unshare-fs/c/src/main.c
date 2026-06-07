/*
 * test-unshare-fs — verify unshare(CLONE_FS).
 *
 * nixpkgs 测试可能用到 — unshare(CLONE_FS) 是 Nix fetchTarball
 * 下载线程的前置依赖。
 *
 * Scenarios:
 *   1. unshare(CLONE_FS) on independent task → returns 0.
 *   2. clone(CLONE_FS) → child unshare(CLONE_FS) → cwd isolation.
 *   3. unshare(0xDEAD) → EINVAL.
 */

#define _GNU_SOURCE
#include <errno.h>
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

static void test_unshare_fs_basic(void) {
    int rc = unshare(CLONE_FS);
    check(rc == 0, "unshare(CLONE_FS) returns 0 (rc=%d, errno=%d)", rc, errno);

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "getcwd after unshare(CLONE_FS)");
    check(cwd[0] == '/', "cwd valid after unshare(CLONE_FS): %s", cwd);

    printf("UNSHARE_FS_BASIC_PASSED\n");
}

static void test_clone_fs_then_unshare(void) {
    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                       MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    check(shared != MAP_FAILED, "mmap shared page");

    shared[0] = 0; shared[1] = 0;
    mkdir("/tmp/unshare-fs-dirA", 0755);
    mkdir("/tmp/unshare-fs-dirB", 0755);

    pid_t pid = fork();
    check(pid >= 0, "fork for clone+unshare test");

    if (pid == 0) {
        int rc = unshare(CLONE_FS);
        check(rc == 0, "child unshare(CLONE_FS) (rc=%d, errno=%d)", rc, errno);

        rc = chdir("/tmp/unshare-fs-dirB");
        check(rc == 0, "child chdir to dirB");

        shared[0] = 1;
        while (shared[1] == 0) usleep(10000);
        _exit(0);
    }

    while (shared[0] == 0) usleep(10000);

    int rc = chdir("/tmp/unshare-fs-dirA");
    check(rc == 0, "parent chdir to dirA (cwd isolated)");

    char cwd[256];
    check(getcwd(cwd, sizeof(cwd)) != NULL, "parent getcwd");
    check(strstr(cwd, "unshare-fs-dirA") != NULL,
          "parent cwd is dirA: %s", cwd);

    shared[1] = 1;
    waitpid(pid, NULL, 0);
    rmdir("/tmp/unshare-fs-dirA");
    rmdir("/tmp/unshare-fs-dirB");
    munmap(shared, 4096);

    printf("UNSHARE_FS_CLONE_ISOLATION_PASSED\n");
}

static void test_unshare_invalid_flags(void) {
    int rc = unshare(0xDEAD);
    check(rc == -1 && errno == EINVAL,
          "unshare(0xDEAD) returns EINVAL (rc=%d, errno=%d)", rc, errno);
    printf("UNSHARE_FS_INVALID_PASSED\n");
}

int main(void) {
    test_unshare_fs_basic();
    test_clone_fs_then_unshare();
    test_unshare_invalid_flags();
    printf("UNSHARE_FS_ALL_PASSED\n");
    return 0;
}
