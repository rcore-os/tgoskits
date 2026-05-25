#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setreuid(2) — set real and/or effective user ID.
 *
 * man 2 setreuid:
 *   "setreuid() sets real and effective user IDs of the calling process."
 *   "Supplying a value of -1 for either the real or effective user ID forces
 *    the system to leave that ID unchanged."
 *   "Unprivileged processes may only set the effective user ID to the real
 *    user ID, the effective user ID, or the saved set-user-ID."
 *   "Unprivileged users may only set the real user ID to the real user ID
 *    or the effective user ID."
 *
 * 测试覆盖：
 *   (a) setreuid(-1, -1) — 都不变（NOCHG sentinel）
 *   (b) setreuid(getuid(), -1) — ruid 设回自己
 *   (c) setreuid(-1, geteuid()) — euid 设回自己
 *   (d) root setreuid(任意值) → 任意成功
 *   (e) unpriv setreuid(任意, 任意) → EPERM 当不在允许集合
 *   (f) raw vs libc
 *   (g) Linux invariant: setreuid 不动 supplementary groups (host gcc 实测验证)
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setreuid_both_nochg_idempotent(void)
{
    /* 怎么测：setreuid(-1, -1) — 全 NOCHG sentinel
     * 期望：返回 0，cred 不变
     * 为什么：man "Supplying a value of -1 for either ... forces the system
     *           to leave that ID unchanged" */
    uid_t r0, e0, s0;
    getresuid(&r0, &e0, &s0);
    int rc = setreuid((uid_t)-1, (uid_t)-1);
    CHECK(rc == 0,                                                "setreuid (a) (-1, -1) -> 0");
    uid_t r1, e1, s1;
    getresuid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1,                       "setreuid (a) (-1, -1) leaves cred unchanged");
}

static void setreuid_ruid_self(void)
{
    /* 测什么: man §DESCRIPTION — 部分 NOCHG (ruid=self, euid=-1) 应成功且不动 e.
     * 怎么测: setreuid(getuid(), -1), 验 rc=0 + ruid 仍 == self.
     * 期望:   rc=0, ruid 不变.
     * 为什么: 验证 starry NOCHG 在 euid 槽生效 (即 sentinel 路径 +
     *         ruid set-to-self 不触发 EPERM). */
    uid_t u = getuid();
    int rc = setreuid(u, (uid_t)-1);
    CHECK(rc == 0,                                                "setreuid (b) (uid, -1) -> 0");
    CHECK(getuid() == u,                                          "setreuid (b) ruid still == self");
}

static void setreuid_euid_self(void)
{
    /* 测什么: man §DESCRIPTION — 部分 NOCHG (ruid=-1, euid=self) 应成功.
     * 怎么测: setreuid(-1, geteuid()), 验 rc=0 + euid 仍 == self.
     * 期望:   rc=0, euid 不变.
     * 为什么: 镜像 (b) — 验 NOCHG 在 ruid 槽 + euid set-to-self 不触发 EPERM. */
    uid_t e = geteuid();
    int rc = setreuid((uid_t)-1, e);
    CHECK(rc == 0,                                                "setreuid (c) (-1, euid) -> 0");
    CHECK(geteuid() == e,                                         "setreuid (c) euid still == self");
}

static void setreuid_root_arbitrary(void)
{
    /* 测什么: man §DESCRIPTION — root (CAP_SETUID) 可设任意 ruid/euid.
     * 怎么测: root fork → child setreuid(2000, 3000) → 验 r=2000, e=3000.
     * 期望:   rc=0, r=2000, e=3000.
     * 为什么: 验证 starry has_cap_setuid 路径下两参数都被 set. */
    if (getuid() != 0) {
        printf("  setreuid (d) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid(2000, 3000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 3000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setreuid (d) root setreuid(2000, 3000) → r=2000, e=3000");
        }
    }
}

static void setreuid_unpriv_eperm(void)
{
    /* 测什么: man §EPERM — unpriv + ruid 不在 {real, effective} 触发 EPERM.
     * 怎么测: root fork → child setresuid(1000,1000,1000) → setreuid(2000, -1).
     *         2000 不在 {1000} 集合内 → EPERM.
     * 期望:   rc=-1 errno=EPERM.
     * 为什么: 验证 starry !has_cap 路径下 ruid 的 in_set {r, e} 检查
     *         (注: ruid 仅 {r, e}, 不含 s — 与 euid 的 {r, e, s} 不同). */
    if (getuid() != 0) {
        printf("  setreuid (e) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setreuid(2000, (uid_t)-1);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setreuid (e) unpriv setreuid(2000, -1) → -1 EPERM");
        }
    }
}

static void setreuid_raw_matches_libc(void)
{
    /* 测什么: man §HISTORY — libc 透明处理 setreuid32 差异.
     * 怎么测: raw syscall(SYS_setreuid, getuid(), geteuid()) 应同 libc 路径成功.
     * 期望:   rc=0.
     * 为什么: 验证 starry sys_setreuid ABI 与 libc 包装契约一致. */
    long rc = syscall(SYS_setreuid, getuid(), geteuid());
    CHECK(rc == 0,                                                "setreuid (f) raw syscall(self, self) -> 0");
}

static void setreuid_keeps_supp_groups(void)
{
    /* 测什么: Linux kernel `sys_setreuid` (kernel/sys.c) 只改 cred.uid/euid/suid,
     *         不动 cred.group_info. setreuid 与 setuid/setresuid 一致:
     *         supplementary groups 不变. 仅 setgroups/initgroups 改 supp.
     * 怎么测: root fork → child setgroups([7,8,9]) → setreuid(1000,1000)
     *         drop → getgroups 应仍 = [7,8,9].
     * 期望:   getgroups 返 3 个, 值 [7,8,9].
     * 为什么: Linux invariant — man 未显式但 kernel 行为如此 (host gcc 实测 PASS).
     *         starry 应同款不动 supp groups, 否则 setreuid 会污染 supp 列表. */
    if (getuid() != 0) {
        printf("  setreuid (g) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t pre[3] = {7, 8, 9};
        if (setgroups(3, pre) != 0) _exit(98);
        if (setreuid(1000, 1000) != 0) _exit(99);
        gid_t got[16];
        int n = getgroups(16, got);
        if (n != 3) _exit(100 + (n < 0 ? 0 : n));
        if (got[0] != 7 || got[1] != 8 || got[2] != 9) _exit(120);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setreuid (g) supp groups preserved after setreuid(1000, 1000)");
        }
    }
}

int setreuid_run(void)
{
    printf("\n----- setreuid -----\n");
    setreuid_both_nochg_idempotent();
    setreuid_ruid_self();
    setreuid_euid_self();
    setreuid_root_arbitrary();
    setreuid_unpriv_eperm();
    setreuid_raw_matches_libc();
    setreuid_keeps_supp_groups();
    printf("  ----- setreuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
