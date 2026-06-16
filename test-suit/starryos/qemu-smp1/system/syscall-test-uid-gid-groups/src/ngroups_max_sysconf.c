/* ngroups_max_sysconf.c — NGROUPS_MAX 极限 + sysconf/procfs 报告一致性.
 *
 * man 2 getgroups §"NOTES":
 *   "A process can have up to NGROUPS_MAX supplementary group IDs in addition
 *    to the effective group ID. The constant NGROUPS_MAX is defined in
 *    <limits.h>."
 *   "The maximum number of supplementary group IDs can be found at run time
 *    using sysconf(_SC_NGROUPS_MAX)."
 *
 * man 2 setgroups §"DESCRIPTION":
 *   "The maximum number of supplementary group IDs is NGROUPS_MAX (32 on
 *    Linux 2.0.x, 65536 since Linux 2.4.x and the value is exported via
 *    /proc/sys/kernel/ngroups_max)."
 *
 * 验证 starry sys_getgroups/setgroups 与 sysconf/procfs 报告一致.
 * 若 starry sysconf 返 0 或 procfs 不暴露 ngroups_max → 应用程序无法
 * 动态分配 supplementary groups buffer.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void sysconf_ngroups_max_positive(void)
{
    /* 测什么: man NOTES — sysconf(_SC_NGROUPS_MAX) 返 NGROUPS_MAX (>0).
     * 怎么测: 调 sysconf, 验返值 > 0.
     * 期望:   rc > 0 (Linux 通常 65536 since 2.4).
     * 为什么: 验 starry libc 桥接 sysconf — 若返 -1 或 0, 用户态程序无法
     *         动态计算 supplementary groups buffer 上限. */
    long rc = sysconf(_SC_NGROUPS_MAX);
    CHECK(rc > 0, "sysconf (a) _SC_NGROUPS_MAX > 0 (kernel 报告 NGROUPS_MAX)");
    printf("  sysconf(_SC_NGROUPS_MAX) = %ld (Linux since 2.4 typically 65536)\n", rc);
}

static void sysconf_matches_procfs_ngroups_max(void)
{
    /* 测什么: man setgroups §DESCRIPTION — '/proc/sys/kernel/ngroups_max'
     *         应与 sysconf 一致.
     * 怎么测: 读 /proc/sys/kernel/ngroups_max, 比对 sysconf(_SC_NGROUPS_MAX).
     * 期望:   两值相等.
     * 为什么: 验 starry sysconf 数据源与 procfs 同源 — 不一致 = cred/proc
     *         子系统分裂 (用户态拿到的 ngroups_max 不可信). */
    FILE *f = fopen("/proc/sys/kernel/ngroups_max", "r");
    if (!f) {
        printf("  sysconf (b) skip: /proc/sys/kernel/ngroups_max 不可读\n");
        return;
    }
    long proc_val = -1;
    if (fscanf(f, "%ld", &proc_val) != 1) proc_val = -1;
    fclose(f);
    long sysconf_val = sysconf(_SC_NGROUPS_MAX);
    CHECK(proc_val > 0 && proc_val == sysconf_val,
          "sysconf (b) procfs ngroups_max == sysconf(_SC_NGROUPS_MAX)");
    printf("  procfs=%ld sysconf=%ld\n", proc_val, sysconf_val);
}

static void getgroups_with_huge_buf(void)
{
    /* 测什么: man §DESCRIPTION — getgroups 接受任意 size >= ngroups, 不拒大 size.
     * 怎么测: getgroups(8192, buf) — 远大于典型 ngroups (5-10).
     * 期望:   rc == ngroups (实际 size, 不是 8192).
     * 为什么: 验 starry sys_getgroups 不误把"size 远大于 ngroups"当 EINVAL
     *         拒绝. 与 boundary (c) (size 太小) 互补 — 这边测 size 富余. */
    int n_query = getgroups(0, NULL);
    if (n_query < 0) { CHECK(0, "sysconf (c) baseline getgroups failed"); return; }
    gid_t *buf = malloc(8192 * sizeof(gid_t));
    if (!buf) { CHECK(0, "sysconf (c) malloc failed"); return; }
    int rc = getgroups(8192, buf);
    CHECK(rc == n_query, "sysconf (c) getgroups(8192, buf) -> ngroups (不被大 size 拒)");
    free(buf);
}

static void setgroups_at_realistic_max_root(void)
{
    /* 测什么: setgroups 接受 size 接近 NGROUPS_MAX 但 < NGROUPS_MAX.
     * 怎么测: root fork → child setgroups(64, gids) — 64 是合理上限内典型 size
     *         (常见 nss 系统 user 5-30 groups, 64 是 safe margin).
     * 期望:   rc=0 + getgroups 返 64.
     * 为什么: 验 starry sys_setgroups 在合理 size 内不拒 (与 boundary (d) 的
     *         setgroups(8) 互补 — 这边测稍大但远 < max 的 size). */
    if (getuid() != 0) {
        printf("  sysconf (d) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t gids[64];
        for (int i = 0; i < 64; i++) gids[i] = 1000 + i;
        if (setgroups(64, gids) != 0) _exit(99);
        int n = getgroups(0, NULL);
        if (n == 64) _exit(0);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "sysconf (d) root setgroups(64, ...) 接受 + getgroups 返 64");
    }
}

static void setgroups_negative_size_einval(void)
{
    /* 测什么: man (隐含) — setgroups 接收 size_t, 但 raw syscall 可传任意值.
     *         传负值 → size 极大 (size_t 解释) → 应 EINVAL (> NGROUPS_MAX).
     * 怎么测: raw syscall(SYS_setgroups, -1, NULL).
     * 期望:   rc=-1 errno=EINVAL (>NGROUPS_MAX) 或 EFAULT (NULL).
     * 为什么: 验 starry sys_setgroups size 上限检查鲁棒 — 负数传入应被
     *         size > NGROUPS_MAX 提前 reject (避免 OOM 误分配 Vec);
     *         不应 silent ignore. */
    if (getuid() != 0) {
        printf("  sysconf (e) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        errno = 0;
        long rc = syscall(SYS_setgroups, (long)-1, NULL);
        int err = errno;
        if (rc == -1 && (err == EINVAL || err == EFAULT)) _exit(0);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "sysconf (e) raw setgroups(size=-1, NULL) -> -1 EINVAL/EFAULT (no kernel oops)");
    }
}

int ngroups_max_sysconf_run(void)
{
    printf("\n----- ngroups_max_sysconf (man NOTES + DESCRIPTION) -----\n");
    sysconf_ngroups_max_positive();
    sysconf_matches_procfs_ngroups_max();
    getgroups_with_huge_buf();
    setgroups_at_realistic_max_root();
    setgroups_negative_size_einval();
    printf("  ----- ngroups_max_sysconf: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
