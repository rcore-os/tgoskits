#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setregid(2) — set real and/or effective group ID. Analogous to setreuid.
 *
 * man 2 setreuid:
 *   "setregid() does the same for the group IDs."
 *
 * 测试与 setreuid 镜像（gid_t 替代）, 含 (g) supp groups invariant.
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setregid_both_nochg(void)
{
    gid_t r0, e0, s0;
    getresgid(&r0, &e0, &s0);
    int rc = setregid((gid_t)-1, (gid_t)-1);
    CHECK(rc == 0, "setregid (a) (-1, -1) -> 0");
    gid_t r1, e1, s1;
    getresgid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1, "setregid (a) leaves cred unchanged");
}

static void setregid_rgid_self(void)
{
    /* 测什么: man §DESCRIPTION 镜像 setreuid — 部分 NOCHG (rgid=self, egid=-1).
     * 怎么测: setregid(getgid(), -1), 验 rc=0 + rgid 不变.
     * 期望:   rc=0, rgid 仍 == self.
     * 为什么: 验证 starry NOCHG 在 egid 槽 + rgid set-to-self 不 EPERM. */
    gid_t g = getgid();
    int rc = setregid(g, (gid_t)-1);
    CHECK(rc == 0, "setregid (b) (gid, -1) -> 0");
    CHECK(getgid() == g, "setregid (b) rgid still == self");
}

static void setregid_egid_self(void)
{
    /* 测什么: man §DESCRIPTION 镜像 — 部分 NOCHG (rgid=-1, egid=self).
     * 怎么测: setregid(-1, getegid()), 验 rc=0 + egid 不变.
     * 期望:   rc=0, egid 不变.
     * 为什么: 镜像 (b) — NOCHG 在 rgid 槽 + egid set-to-self 不 EPERM. */
    gid_t e = getegid();
    int rc = setregid((gid_t)-1, e);
    CHECK(rc == 0, "setregid (c) (-1, egid) -> 0");
    CHECK(getegid() == e, "setregid (c) egid still == self");
}

static void setregid_root_arbitrary(void)
{
    /* 测什么: man §DESCRIPTION — root (CAP_SETGID) 可设任意 rgid/egid.
     * 怎么测: root fork → child setregid(2000, 3000) → 验 r=2000, e=3000.
     * 期望:   rc=0, r=2000, e=3000.
     * 为什么: 验证 starry has_cap_setgid 路径下两参数都被 set. */
    if (getuid() != 0) {
        printf("  setregid (d) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setregid(2000, 3000) != 0) _exit(1);
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 3000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setregid (d) root setregid(2000, 3000) → r=2000, e=3000");
        }
    }
}

static void setregid_unpriv_eperm(void)
{
    /* 测什么: man §EPERM — unpriv + rgid 不在 {real, effective} → EPERM.
     * 怎么测: root fork → child setresgid + setresuid 切到非 root 状态 (cred=1000)
     *         → setregid(2000, -1) 因 2000 不在 {1000} → EPERM.
     *         注意 setresgid 必须先, 否则 caps drop 后无法设 gid.
     * 期望:   rc=-1 errno=EPERM.
     * 为什么: 验证 starry !has_cap_setgid 路径下 rgid in {r, e} 检查. */
    if (getuid() != 0) {
        printf("  setregid (e) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 1000, 1000) != 0) _exit(99);
        if (setresuid(1000, 1000, 1000) != 0) _exit(98);
        errno = 0;
        int rc = setregid(2000, (gid_t)-1);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setregid (e) unpriv setregid(2000, -1) → -1 EPERM");
        }
    }
}

static void setregid_raw_matches_libc(void)
{
    /* 测什么: libc 应直接转发 syscall (镜像 setreuid).
     * 怎么测: raw syscall(SYS_setregid, getgid(), getegid()).
     * 期望:   rc=0.
     * 为什么: 验证 starry sys_setregid ABI 与 libc 包装契约一致. */
    long rc = syscall(SYS_setregid, getgid(), getegid());
    CHECK(rc == 0, "setregid (f) raw syscall(self, self) -> 0");
}

static void setregid_keeps_supp_groups(void)
{
    /* 测什么: Linux kernel `sys_setregid` 只改 cred.gid/egid/sgid,
     *         不动 cred.group_info. setregid 与 setgid/setresgid 一致:
     *         supplementary groups 不变. 仅 setgroups/initgroups 改 supp.
     * 怎么测: root fork → child setgroups([7,8,9]) → setregid(2000, 2000)
     *         drop → getgroups 应仍 = [7,8,9].
     * 期望:   getgroups 返 3 个, 值 [7,8,9].
     * 为什么: Linux invariant 镜像 setreuid (g). starry 应同款不动 supp groups. */
    if (getuid() != 0) {
        printf("  setregid (g) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t pre[3] = {7, 8, 9};
        if (setgroups(3, pre) != 0) _exit(98);
        if (setregid(2000, 2000) != 0) _exit(99);
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
                  "setregid (g) supp groups preserved after setregid(2000, 2000)");
        }
    }
}

int setregid_run(void)
{
    printf("\n----- setregid -----\n");
    setregid_both_nochg();
    setregid_rgid_self();
    setregid_egid_self();
    setregid_root_arbitrary();
    setregid_unpriv_eperm();
    setregid_raw_matches_libc();
    setregid_keeps_supp_groups();
    printf("  ----- setregid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
