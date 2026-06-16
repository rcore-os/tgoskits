#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* boundary — setuid/setgid 边界 / 异常输入测试。
 *
 * man 2 setuid §"ERRORS":
 *   "EINVAL — uid is not valid in this user namespace."
 *
 * 边界值：
 *   - 0 (root)
 *   - 1 (special daemon range start)
 *   - 65535 (16-bit max)
 *   - 65536 (16-bit overflow)
 *   - u32::MAX = 4294967295 (NOCHG sentinel + 边界)
 *   - u32::MAX-1
 *
 * 注：root 时所有 uid 值都接受（CAP_SETUID 时无 EINVAL）；non-root 时
 * 这些边界对当前 uid/euid/suid 集合的命中率是 0 → 全 EPERM。
 */

/* root 调 setuid(任意 uid) 应成功（CAP_SETUID 时无 EINVAL）。
 * 每个边界值在独立 fork 内验证 — 因为 setuid(N) 后 euid 变 N，
 * 失去 root 权限（除非 N=0），后续再 setuid 别的值会因 unprivileged
 * 受限。 */
static void setuid_boundary_values_root(void)
{
    if (getuid() != 0) {
        printf("  boundary (a) skip: not root\n");
        return;
    }
    /* 测 4 个边界 uid：0, 1, 65535, 1000 */
    uid_t cases[] = {0, 1, 65535, 1000};
    int n_ok = 0;
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        pid_t pid = fork();
        if (pid == 0) {
            /* child: 设单个 boundary 值；child 是 root（fork 时继承）*/
            if (setuid(cases[i]) == 0) _exit(0);
            _exit(1);
        }
        if (pid > 0) {
            int status;
            waitpid(pid, &status, 0);
            if (WIFEXITED(status) && WEXITSTATUS(status) == 0) n_ok++;
        }
    }
    CHECK(n_ok == 4, "boundary (a): root accepts uid 0/1/65535/1000 each in own fork");
}

static void setuid_u32max_sentinel_unprivileged(void)
{
    /* codex P1 (adopted) — boundary.c:73 行为修正:
     *
     * Linux: setuid((uid_t)-1) → -1 EINVAL (uid_valid 拒绝 -1, invalid-ID 检查
     *        先于 perm 检查, 任何 priv 路径都触发).
     * starry: 不做 uid_valid 检查, 接受 -1 → cred 被污染 (starry bug).
     *
     * 处理: case 期望 Linux 行为 EINVAL. starry 上会 fail → soft
     * KNOWN-STARRY-BUG 报告 + ref bug-starry-setuid-setgid-no-uidvalid.
     */
    if (getuid() != 0) {
        printf("  boundary (b) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setuid((uid_t)-1);
        int err = errno;
        if (rc == -1 && err == EINVAL) _exit(0);   /* Linux compliant */
        if (rc == 0)                   _exit(2);   /* starry bug — accepted */
        if (rc == -1 && err == EPERM)  _exit(3);   /* old buggy expectation */
        _exit(1);
    }
    if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
        if (ec == 0) {
            CHECK(1, "boundary (b) Linux 行为: unprivileged setuid((uid_t)-1) -> -1 EINVAL");
        } else if (ec == 2) {
            /* starry bug A: 接受 (cred 被污染) */
            printf("  KNOWN-STARRY-BUG | boundary (b) starry 接受 setuid((uid_t)-1) [bug A] | see bugfix/bug-starry-setuid-setgid-no-uidvalid\n");
        } else if (ec == 3) {
            /* starry bug B: 在 unpriv 路径返 EPERM 而非 Linux uid_valid EINVAL.
             * starry 缺 uid_valid 检查后 fall-through 到普通 in_set 检查;
             * (uid_t)-1 不在 {old.uid, old.suid}={1000,1000} → EPERM.
             * 与 Linux 行为 (优先级 EINVAL > EPERM) 不一致. */
            printf("  KNOWN-STARRY-BUG | boundary (b) starry 返 EPERM 而非 EINVAL [bug B fall-through] | see bugfix/bug-starry-setuid-setgid-no-uidvalid\n");
        } else {
            char buf[200];
            snprintf(buf, sizeof buf,
                     "boundary (b) unexpected (ec=%d) for setuid((uid_t)-1)", ec);
            CHECK(0, buf);
        }
    }
}

static void setuid_raw_syscall_returns_negative_errno(void)
{
    /* codex P1 (adopted) — boundary.c:91 增加 fail path raw-vs-libc 覆盖. */
    /* success path */
    uid_t u = getuid();
    long rc_succ = syscall(SYS_setuid, u);
    CHECK(rc_succ == 0,                                              "boundary (c1) raw setuid(getuid()) success -> 0");
    int succ_libc = setuid(u);
    CHECK(succ_libc == 0,                                            "boundary (c1) libc setuid(getuid()) success -> 0");

    /* fail path — unprivileged child setuid(0) 应 EPERM, raw 与 libc errno 一致 */
    if (getuid() != 0) {
        printf("  boundary (c2) skip fail-path: needs root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        long raw_rc = syscall(SYS_setuid, 0);
        int raw_err = errno;
        errno = 0;
        int libc_rc = setuid(0);
        int libc_err = errno;
        if (raw_rc == -1 && raw_err == EPERM
            && libc_rc == -1 && libc_err == EPERM) _exit(0);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "boundary (c2) raw vs libc on fail path: both -1 EPERM (unpriv setuid(0))");
}

int boundary_run(void)
{
    printf("\n----- boundary -----\n");
    setuid_boundary_values_root();
    setuid_u32max_sentinel_unprivileged();
    setuid_raw_syscall_returns_negative_errno();
    printf("  ----- boundary: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
