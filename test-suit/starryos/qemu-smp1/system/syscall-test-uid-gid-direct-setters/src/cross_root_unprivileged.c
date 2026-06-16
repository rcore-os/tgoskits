#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* cross_root_unprivileged — setuid/setgid 在 root 与非 root 下的关键行为对照。
 *
 * 验证 starry CAP_SETUID/CAP_SETGID 模拟（用 euid==0 近似）的正确性：
 *   - root: setuid(任意值) 全成功；ruid/euid/suid 全更新（不可逆）
 *   - non-root: setuid 仅可设 ruid/suid 中已有值；EPERM 否则
 *
 * 这些是 setuid/setgid 实现的核心区别 — 一致性 bug 会导致权限突破。
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* 测什么: man §DESCRIPTION — root (CAP_SETUID) 时 setuid(任意值) 设全 3 IDs.
 *         man §D4 — root setuid 后不可逆 ("After this has occurred, it is
 *         impossible for the program to regain root privileges").
 * 怎么测: root fork → child setuid(2000) → 验 getresuid 三个槽都 == 2000.
 * 期望:   r=e=s=2000.
 * 为什么: 验证 starry has_cap_setuid 路径全设 r/e/s; 不可逆性间接体现
 *         (setuid 后 child 已非 root, 但 child 也立即退出, 不深测可逆性 —
 *         那由 setuid (g) irreversibility case 专门测). */
static void root_setuid_arbitrary_updates_all(void)
{
    if (getuid() != 0) {
        printf("  cross root (a) skip: not running as root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* child: 在子进程中改变 cred 不影响 parent */
        if (setuid(2000) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 2000 && s == 2000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "cross root (a): root setuid(2000) sets r=e=s=2000 (irreversible)");
        }
    }
}

/* 测什么: man §EPERM 反面 — unpriv 但 input == 当前 uid (in {r, s}) 应成功.
 * 怎么测: root fork → child setresuid(1000,1000,1000) → setuid(1000).
 * 期望:   rc=0 (uid 已在 {r,s} 集合内, 不触发 EPERM).
 * 为什么: 验证 starry !has_cap 路径下 in_set 判断正向case (input matches
 *         current uid → success). */
static void unprivileged_setuid_self_succeeds(void)
{
    if (getuid() != 0) {
        printf("  cross root (b) skip: requires root setup\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        /* setuid 设回自己 uid (1000) 应成功 */
        if (setuid(1000) != 0) _exit(1);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "cross root (b): unprivileged setuid(self) succeeds");
        }
    }
}

/* 测什么: man §EPERM — unpriv + input 不在 {r,e,s} → EPERM.
 * 怎么测: root fork → child setresuid(1000,1000,1000) → setuid(2000)
 *         (2000 不在 {1000} 集合内).
 * 期望:   rc=-1 errno=EPERM.
 * 为什么: 验证 starry !has_cap 路径下 in_set 判断负向 case (input不属于
 *         {r,e,s} → 拒绝). 与 (b) 共同覆盖 in_set 决策的两侧分支. */
static void unprivileged_setuid_arbitrary_eperm(void)
{
    if (getuid() != 0) {
        printf("  cross root (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setuid(2000);  /* 不属于 {1000, 1000, 1000} */
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "cross root (c): unprivileged setuid(arbitrary) -> -1 EPERM");
        }
    }
}

int cross_root_unprivileged_run(void)
{
    printf("\n----- cross_root_unprivileged -----\n");
    root_setuid_arbitrary_updates_all();
    unprivileged_setuid_self_succeeds();
    unprivileged_setuid_arbitrary_eperm();
    printf("  ----- cross_root_unprivileged: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
