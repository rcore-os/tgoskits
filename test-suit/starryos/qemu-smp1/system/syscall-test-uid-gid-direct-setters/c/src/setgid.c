#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setgid(2) — set group identity. analogous to setuid for group IDs.
 *
 * man 2 setgid §"DESCRIPTION":
 *   "setgid() sets the effective group ID of the calling process. If the
 *    calling process is privileged (the process has the CAP_SETGID capability),
 *    the real GID and saved set-group-ID are also set."
 *
 * man §"ERRORS":
 *   "EINVAL — The group ID specified in gid is not valid in this user namespace."
 *   "EPERM — The calling process is not privileged (does not have the
 *    CAP_SETGID capability), and gid does not match the real group ID or
 *    saved set-group-ID of the calling process."
 *
 * 测试与 setuid 镜像（gid_t 替代 uid_t；EPERM 路径 setresgid 而非 setresuid）：
 *   (a) idempotent  (b) root sets all 3  (c) returns 0  (d) raw vs libc 成功
 *   (e) unpriv EPERM  (f) raw vs libc FAIL 路径  (g) 不可逆性  (h) fork 独立
 *   (i) setgid(getegid()) 不改变 cred
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setgid_idempotent_to_self(void)
{
    /* 测什么: man §DESCRIPTION 隐含 — setgid(getgid()) idempotent.
     *         man §EPERM "gid does not match the real GID or saved set-group-ID"
     *         的反面: gid 等于 real GID → 不触发 EPERM.
     * 怎么测: 调 setgid(getgid()).
     * 期望:   rc == 0.
     * 为什么: 验证 starry 不误把 idempotent call 当成 perm violation. */
    gid_t g = getgid();
    int rc = setgid(g);
    CHECK(rc == 0, "setgid (a) setgid(getgid()) -> 0 (idempotent)");
}

static void setgid_root_sets_all_three(void)
{
    /* 测什么: man §DESCRIPTION — "If the calling process is privileged (CAP_SETGID),
     *         the real GID and saved set-group-ID are also set." 设全 3.
     * 怎么测: root 调 setgid(0), 验返 0; getresgid 验 r=e=s=0.
     * 期望:   rc=0, r=e=s=0.
     * 为什么: 验证 starry has_cap_setgid 路径下设全三 IDs (不只 egid). */
    if (getuid() != 0) {
        printf("  setgid (b) skip: not running as root\n");
        return;
    }
    int rc = setgid(0);
    CHECK(rc == 0, "setgid (b) root setgid(0) -> 0");
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) == 0) {
        CHECK(r == 0 && e == 0 && s == 0, "setgid (b) root setgid(0): r=e=s=0");
    }
}

static void setgid_returns_zero_on_success(void)
{
    /* 测什么: man §RETURN VALUE — "On success, zero is returned."
     * 怎么测: setgid(getgid()) idempotent success.
     * 期望:   rc == 0.
     * 为什么: 显式断言 success path 返 0 (而非奇怪值). */
    int rc = setgid(getgid());
    CHECK(rc == 0, "setgid (c) returns 0 on success");
}

static void setgid_raw_syscall_matches_libc(void)
{
    /* 测什么: libc 应直接转发 syscall.
     *         man §HISTORY — libc 透明处理 16/32-bit setgid32 差异.
     * 怎么测: raw + libc 各跑 setgid(getgid()), 都应返 0.
     * 期望:   两次 rc == 0.
     * 为什么: 验证 starry sys_setgid ABI 与 libc 包装一致. */
    gid_t g = getgid();
    long rc1 = syscall(SYS_setgid, g);
    int rc2 = setgid(g);
    CHECK(rc1 == 0 && rc2 == 0, "setgid (d) raw syscall == libc (both = 0)");
}

static void setgid_unprivileged_eperm_in_child(void)
{
    /* 测什么: man §EPERM — "calling process is not privileged (CAP_SETGID)
     *         and gid does not match the real GID or saved set-group-ID."
     * 怎么测: root fork → child setresgid 切非 root cred (gid=1000) + setresuid
     *         (清 caps) → 尝试 setgid(0) — 0 不在 {1000,1000} 集合内 → EPERM.
     *         注意 setresgid 必须先于 setresuid (drop caps 后 setresgid EPERM).
     * 期望:   rc=-1, errno=EPERM.
     * 为什么: 验证 starry 非特权路径的 in_set 检查 + EPERM 返回. */
    if (getuid() != 0) {
        printf("  setgid (e) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 1000, 1000) != 0) _exit(99);
        /* drop uid too — setresuid AFTER setresgid, otherwise gid setting fails */
        if (setresuid(1000, 1000, 1000) != 0) _exit(98);
        errno = 0;
        int rc = setgid(0);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            int es = WEXITSTATUS(status);
            CHECK(WIFEXITED(status) && es == 0,
                  "setgid (e) unprivileged child setgid(0) -> -1 EPERM");
            if (es >= 98) printf("  (setresgid/setresuid setup failed in child rc=%d)\n", es);
        }
    } else {
        CHECK(0, "setgid (e) fork failed");
    }
}

/* (f) raw vs libc 失败路径 errno 一致性
 *
 * 测什么: raw syscall 和 libc 在 fail path 应返同样 -1 + EPERM.
 *         man §RETURN VALUE — 失败 rc=-1, errno 设. ABI 一致性.
 * 怎么测: 同 (e) 设置 unpriv child, 然后 raw + libc 各跑 setgid(0).
 *         比对两次 rc 和 errno.
 * 期望:   raw_rc=-1 raw_err=EPERM + libc_rc=-1 libc_err=EPERM.
 * 为什么: 验证 starry libc wrapper 在 fail path 的 errno 转换正确;
 *         防 wrapper 漏报 errno (如 raw 返 -EPERM 但 libc errno=0). */
static void setgid_raw_vs_libc_failed_path(void)
{
    if (getuid() != 0) {
        printf("  setgid (f) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 1000, 1000) != 0) _exit(99);
        if (setresuid(1000, 1000, 1000) != 0) _exit(98);

        errno = 0;
        long raw_rc = syscall(SYS_setgid, 0);
        int raw_err = errno;
        errno = 0;
        int libc_rc = setgid(0);
        int libc_err = errno;

        if (raw_rc == -1 && raw_err == EPERM
            && libc_rc == -1 && libc_err == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgid (f) raw syscall == libc on FAIL path (both -1 EPERM)");
        }
    }
}

/* (g) setgid 单独 NOT irreversible — 关键 capability 语义
 *
 * man 2 setgid §"DESCRIPTION":
 *   "If the calling process is privileged (the process has the CAP_SETGID
 *    capability), the real GID and saved set-group-ID are also set."
 *
 * CAP_SETGID 与 euid 绑定，setgid 调用不修改 euid → CAP_SETGID 保留 →
 * root → setgid(1000) → setgid(0) 应**成功**（caps 仍在）。
 *
 * 这与 setuid (g) 不同：setuid 修改 euid → 非 root euid → caps 被清 →
 * setuid(0) 失败 EPERM。
 *
 * 真正的 setgid 不可逆性需 同时 drop CAP_SETGID（通过 setuid 切非 root）
 * — 用 fork → setuid(1000) → setgid(1000) → setgid(0) → EPERM 验证。
 */
static void setgid_alone_not_irreversible_caps_intact(void)
{
    if (getuid() != 0) {
        printf("  setgid (g) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setgid(1000) != 0) _exit(99);
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(98);
        if (r != 1000 || e != 1000 || s != 1000) _exit(97);

        /* 仍是 root euid → CAP_SETGID 在 → setgid(0) 应成功 */
        errno = 0;
        int rc = setgid(0);
        if (rc == 0) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgid (g) NOT irreversible alone: root→setgid(1000)→setgid(0)→0 (CAP_SETGID 保留)");
        }
    }
}

/* (g2) setgid + setuid 组合后真不可逆 — 验证 saved-set-gid 在 caps drop 后生效 */
static void setgid_irreversible_after_setuid_drop(void)
{
    if (getuid() != 0) {
        printf("  setgid (g2) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setgid(1000) != 0) _exit(99);          /* gid=egid=sgid=1000，caps 仍在 */
        if (setuid(1000) != 0) _exit(98);          /* uid=euid=suid=1000，caps 清 */
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(97);
        if (r != 1000 || e != 1000 || s != 1000) _exit(96);

        errno = 0;
        int rc = setgid(0);
        /* 现在无 CAP_SETGID，0 不在 {rgid, sgid} = {1000} → EPERM */
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgid (g2) 真不可逆: root→setgid(1000)→setuid(1000)→setgid(0)→-1 EPERM");
        }
    }
}

/* (h) fork 后 child 独立 setgid 不影响 parent */
static void setgid_fork_child_independent(void)
{
    if (getuid() != 0) {
        printf("  setgid (h) skip: requires root\n");
        return;
    }
    gid_t parent_gid_before = getgid();
    pid_t pid = fork();
    if (pid == 0) {
        if (setgid(2000) != 0) _exit(1);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        waitpid_safely(pid, &status);
        CHECK(getgid() == parent_gid_before,
              "setgid (h) fork child setgid 不影响 parent");
    }
}

/* (i) setgid(getegid()) 不改变任何 ID */
static void setgid_self_egid_no_change(void)
{
    gid_t e0 = getegid();
    gid_t r0, eu0, s0;
    if (getresgid(&r0, &eu0, &s0) != 0) { CHECK(0, "skip"); return; }
    int rc = setgid(e0);
    CHECK(rc == 0, "setgid (i) setgid(getegid()) -> 0");
    gid_t r1, eu1, s1;
    if (getresgid(&r1, &eu1, &s1) == 0) {
        CHECK(r1 == r0 && eu1 == eu0 && s1 == s0,                  "setgid (i) setgid(getegid()) 不改变任何 ID");
    }
}

int setgid_run(void)
{
    printf("\n----- setgid -----\n");
    setgid_idempotent_to_self();
    setgid_root_sets_all_three();
    setgid_returns_zero_on_success();
    setgid_raw_syscall_matches_libc();
    setgid_unprivileged_eperm_in_child();
    setgid_raw_vs_libc_failed_path();
    setgid_alone_not_irreversible_caps_intact();   /* (g) — 替换错误的 irreversibility */
    setgid_irreversible_after_setuid_drop();        /* (g2) new — 真不可逆条件 */
    setgid_fork_child_independent();
    setgid_self_egid_no_change();
    printf("  ----- setgid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
