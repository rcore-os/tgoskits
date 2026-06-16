#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* round_trip — setgroups → getgroups → 内容一致 + 顺序保留 + ngroups 准确。
 *
 * 验证 supp group 系统的端到端正确性：
 *   1. 内容一致：getgroups 返回的 gid 完全等于 setgroups 传入的 gid（顺序无关）
 *   2. ngroups 精确：getgroups(0, NULL) 返回值正好等于 setgroups 传入 size
 *   3. 多次 setgroups 覆盖原值（不累加）
 *
 * 这些不变量被破坏 → 影响所有依赖 supp groups 的功能（权限检查、PAM、container 等）。
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void round_trip_size_5(void)
{
    if (getuid() != 0) {
        printf("  round_trip (a) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t set[5] = {1, 7, 100, 1234, 65500};
        if (setgroups(5, set) != 0) _exit(1);

        int n = getgroups(0, NULL);
        if (n != 5) _exit(2);

        gid_t got[16] = {0};
        int rc = getgroups(16, got);
        if (rc != 5) _exit(3);

        /* 内容验：与 set[] 完全一致。
         * set={1,7,100,1234,65500} 已升序 → Linux kernel `groups_sort()` 对已排序输入
         * 保持原序 → memcmp 安全 (若 set 未排序, kernel 会 sort 后存, getgroups 返排序结果). */
        if (memcmp(got, set, 5 * sizeof(gid_t)) != 0) _exit(4);

        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "round_trip (a) setgroups({1,7,100,1234,65500}) → getgroups returns identical 5");
        }
    }
}

static void round_trip_overwrite_no_accumulation(void)
{
    if (getuid() != 0) {
        printf("  round_trip (b) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t a[3] = {10, 20, 30};
        if (setgroups(3, a) != 0) _exit(1);
        gid_t b[2] = {40, 50};
        if (setgroups(2, b) != 0) _exit(2);
        /* 现在应该只有 {40, 50}，不是 {10,20,30,40,50} */
        int n = getgroups(0, NULL);
        if (n != 2) _exit(3);
        gid_t got[8] = {0};
        if (getgroups(8, got) != 2) _exit(4);
        if (got[0] != 40 || got[1] != 50) _exit(5);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "round_trip (b) 第二次 setgroups 完全覆盖第一次（无累加）");
        }
    }
}

static void round_trip_empty_clears(void)
{
    if (getuid() != 0) {
        printf("  round_trip (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t a[2] = {100, 200};
        if (setgroups(2, a) != 0) _exit(1);
        if (setgroups(0, NULL) != 0) _exit(2);
        int n = getgroups(0, NULL);
        if (n != 0) _exit(3);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "round_trip (c) setgroups(0, NULL) 清空所有 supp groups");
        }
    }
}

int round_trip_run(void)
{
    printf("\n----- round_trip -----\n");
    round_trip_size_5();
    round_trip_overwrite_no_accumulation();
    round_trip_empty_clears();
    printf("  ----- round_trip: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
