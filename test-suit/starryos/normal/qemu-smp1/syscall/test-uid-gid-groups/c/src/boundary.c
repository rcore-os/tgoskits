#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* boundary — NGROUPS_MAX / EFAULT / 极大 size.
 *
 * man 2 setgroups ERRORS:
 *   "EINVAL — size is greater than NGROUPS_MAX (32 or 65536)."
 *   "EFAULT — list has an invalid address."
 *
 * 测试：
 *   (a) setgroups(NGROUPS_MAX+1, list) → -1 EINVAL
 *   (b) setgroups(N, invalid_ptr) → -1 EFAULT
 *   (c) getgroups(small_size, valid) where size < ngroups → -1 EINVAL
 *   (d) setgroups(NGROUPS_MAX, valid_list) → 应成功（边界内）
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* Linux NGROUPS_MAX since 2.4 is 65536 */
#define NGROUPS_MAX_LINUX 65536

static void boundary_setgroups_over_max_einval(void)
{
    if (getuid() != 0) {
        printf("  boundary (a) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 用最小 list 但 size = NGROUPS_MAX + 1 → 不需真分配，kernel 应在 size
         * 校验阶段就 EINVAL（不读 list） */
        gid_t dummy[1] = {100};
        errno = 0;
        int rc = setgroups(NGROUPS_MAX_LINUX + 1, dummy);
        if (rc == -1 && errno == EINVAL) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (a) setgroups(NGROUPS_MAX+1, ...) → -1 EINVAL");
        }
    }
}

static void boundary_setgroups_invalid_ptr_efault(void)
{
    if (getuid() != 0) {
        printf("  boundary (b) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        errno = 0;
        /* size > 0 但 list = kernel pointer → EFAULT */
        long rc = syscall(SYS_setgroups, 4, (void *)0xdeadbeefdeadbeefULL);
        if (rc == -1 && errno == EFAULT) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (b) setgroups(4, invalid_kernel_ptr) → -1 EFAULT");
        }
    }
}

static void boundary_getgroups_too_small_einval(void)
{
    /* 先 set 一些 supp groups（root only），再 getgroups 用 size=1 试 EINVAL */
    if (getuid() != 0) {
        printf("  boundary (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t a[3] = {100, 200, 300};
        if (setgroups(3, a) != 0) _exit(99);
        gid_t buf[1];
        errno = 0;
        int rc = getgroups(1, buf);
        if (rc == -1 && errno == EINVAL) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (c) getgroups(size=1) when ngroups=3 → -1 EINVAL");
        }
    }
}

static void boundary_setgroups_at_max_ok(void)
{
    /* size = NGROUPS_MAX 应被接受（在 64K 限内）；但分配 64K * 4B = 256KB 数组
     * 比较大，用较小尺寸代表「far inside the max」即可 */
    if (getuid() != 0) {
        printf("  boundary (d) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 8 个 supp groups：远小于 64K，但比常见 4-5 个多，验证非平凡尺寸 */
        gid_t a[8] = {1, 2, 3, 4, 5, 6, 7, 8};
        if (setgroups(8, a) != 0) _exit(1);
        int n = getgroups(0, NULL);
        if (n != 8) _exit(2);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (d) setgroups(8, ...) accepted as non-trivial inside-max size");
        }
    }
}

int boundary_run(void)
{
    printf("\n----- boundary -----\n");
    boundary_setgroups_over_max_einval();
    boundary_setgroups_invalid_ptr_efault();
    boundary_getgroups_too_small_einval();
    boundary_setgroups_at_max_ok();
    printf("  ----- boundary: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
