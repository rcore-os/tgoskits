#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setresuid(2) — set real, effective, saved user IDs.
 *
 * man 2 setresuid:
 *   "setresuid() sets the real user ID, the effective user ID, and the
 *    saved set-user-ID of the calling process."
 *   "An unprivileged process may change its real UID, effective UID, and
 *    saved set-user-ID, each to one of: the current real UID, the current
 *    effective UID, or the current saved set-user-ID."
 *   "Privileged processes ... may set ... to arbitrary values."
 *   "If one of the arguments equals -1, the corresponding value is not changed."
 *
 * 测试覆盖：
 *   (a) setresuid(-1,-1,-1) — 全 NOCHG，cred 不变
 *   (b) setresuid(self,self,self) — idempotent
 *   (c) root setresuid(任意三个不同值) → 真正三独立设置
 *   (d) unpriv setresuid 在 {uid,euid,suid} 集合内 → ok
 *   (e) unpriv setresuid 任一不在集合内 → -1 EPERM
 *   (f) raw vs libc
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setresuid_all_nochg(void)
{
    /* 测什么: man §DESCRIPTION — "If one of the arguments equals -1, the
     *         corresponding value is not changed." 三 -1 全 NOCHG → cred 不变.
     * 怎么测: 保存 baseline cred → 调 setresuid(-1,-1,-1) → 比对 cred.
     * 期望:   rc=0, cred 完全不变.
     * 为什么: 验证 starry NOCHG sentinel 处理 — 不能误把 -1 当 uid 写入 cred
     *         (这是 bug-starry-setuid-setgid-no-uidvalid 的 NOCHG 路径变种). */
    uid_t r0, e0, s0;
    getresuid(&r0, &e0, &s0);
    int rc = setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1);
    CHECK(rc == 0,                                                "setresuid (a) (-1,-1,-1) -> 0");
    uid_t r1, e1, s1;
    getresuid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1,                       "setresuid (a) (-1,-1,-1) cred unchanged");
}

static void setresuid_self_self_self(void)
{
    /* 测什么: man §DESCRIPTION 隐含 — set 到当前 cred (r,e,s) 应总成功
     *         (任一字段值已在 allowed set 内).
     * 怎么测: 取当前 (r,e,s) → 调 setresuid(r,e,s).
     * 期望:   rc=0.
     * 为什么: 验证 starry !has_cap 路径下 in_set 检查正向 case (每参数都
     *         在 {r,e,s} 集合内 → 不触发 EPERM). */
    uid_t r, e, s;
    getresuid(&r, &e, &s);
    int rc = setresuid(r, e, s);
    CHECK(rc == 0,                                                "setresuid (b) (r,e,s) idempotent");
}

static void setresuid_root_arbitrary_three_values(void)
{
    /* 测什么: man §DESCRIPTION — root (CAP_SETUID) 可设三参数为任意值.
     * 怎么测: root fork → child setresuid(2000,3000,4000) → 验 r/e/s 三字段精确.
     * 期望:   rc=0, r=2000 e=3000 s=4000.
     * 为什么: 验证 starry has_cap_setuid 路径下三参数独立 set, 互不影响 (不会
     *         如 setreuid 那样隐式触发 saved-set 规则). */
    if (getuid() != 0) {
        printf("  setresuid (c) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(2000, 3000, 4000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 3000 && s == 4000) _exit(0);
        printf("  got r=%u e=%u s=%u expected (2000,3000,4000)\n", r, e, s);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresuid (c) root setresuid(2000,3000,4000) → r=2000 e=3000 s=4000");
        }
    }
}

static void setresuid_unpriv_within_set_ok(void)
{
    /* 测什么: man §DESCRIPTION — unpriv 可设各字段为 {old.r, old.e, old.s}
     *         之一; 三字段都在集合 → 允许.
     * 怎么测: root fork → child setresuid(1000,2000,3000) 建集合 {1000,2000,3000}
     *         → 调 setresuid(2000,1000,3000) — 都在集合内 → 应成功.
     * 期望:   rc=0, 三字段重排为 r=2000 e=1000 s=3000.
     * 为什么: 验证 starry !has_cap 路径下三参数都通过 in_set 检查时的成功路径
     *         (允许重排 r/e/s). */
    if (getuid() != 0) {
        printf("  setresuid (d) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        if (setresuid(2000, 1000, 3000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 1000 && s == 3000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresuid (d) unpriv setresuid within {r,e,s} set → ok");
        }
    }
}

static void setresuid_unpriv_outside_set_eperm(void)
{
    /* 测什么: man §EPERM — "tried to change the IDs to values that are not
     *         permitted" (即任一非 NOCHG 参数不在 {old.r, old.e, old.s}).
     * 怎么测: root fork → child setresuid(1000,1000,1000) 集合={1000} → 调
     *         setresuid(2000,-1,-1) 因 2000 不在集合 → EPERM.
     * 期望:   rc=-1 errno=EPERM.
     * 为什么: 验证 starry !has_cap 路径下 in_set 检查负向 — 任一字段越界即拒. */
    if (getuid() != 0) {
        printf("  setresuid (e) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setresuid(2000, (uid_t)-1, (uid_t)-1);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresuid (e) unpriv setresuid(outside_set, -1, -1) → -1 EPERM");
        }
    }
}

static void setresuid_raw_matches_libc(void)
{
    /* 测什么: man §HISTORY — libc 透明处理 setresuid32 差异.
     * 怎么测: raw syscall(SYS_setresuid, -1, -1, -1) — NOCHG×3 success path.
     * 期望:   rc=0.
     * 为什么: 验证 starry sys_setresuid ABI 与 libc 包装一致. */
    long rc = syscall(SYS_setresuid, (uid_t)-1, (uid_t)-1, (uid_t)-1);
    CHECK(rc == 0,                                                "setresuid (f) raw syscall(NOCHG×3) -> 0");
}

int setresuid_run(void)
{
    printf("\n----- setresuid -----\n");
    setresuid_all_nochg();
    setresuid_self_self_self();
    setresuid_root_arbitrary_three_values();
    setresuid_unpriv_within_set_ok();
    setresuid_unpriv_outside_set_eperm();
    setresuid_raw_matches_libc();
    printf("  ----- setresuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
