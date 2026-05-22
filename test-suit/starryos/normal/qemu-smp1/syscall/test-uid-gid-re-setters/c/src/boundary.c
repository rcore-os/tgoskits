#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* boundary — setreuid/setregid 边界 + 异常输入。
 *
 * 边界值：
 *   - (-1, -1) sentinel — 已在 setreuid (a) 测；这里测 raw syscall 直传 u32::MAX
 *   - (uid, -1) / (-1, uid) — 单参数 NOCHG
 *   - root setreuid(u32::MAX-1, u32::MAX-1) — 极大值，root 接受
 *   - 错误输入：raw syscall 传无效组合
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* raw syscall 直传 u32::MAX (NOCHG) — kernel 应识别为 NOCHG */
static void boundary_raw_u32max_nochg(void)
{
    uid_t r0, e0, s0;
    getresuid(&r0, &e0, &s0);
    long rc = syscall(SYS_setreuid, (uid_t)-1, (uid_t)-1);
    CHECK(rc == 0,                                                "boundary (a) raw setreuid(u32::MAX, u32::MAX) -> 0");
    uid_t r1, e1, s1;
    getresuid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1,                       "boundary (a) cred unchanged after NOCHG sentinel");
}

/* root: 极大值 setreuid 应成功（CAP_SETUID 时无 EINVAL）*/
static void boundary_root_extreme_values(void)
{
    if (getuid() != 0) {
        printf("  boundary (b) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 0xFFFFFFFE = u32::MAX - 1，避免与 NOCHG 混淆 */
        if (setreuid(0xFFFFFFFE, 0xFFFFFFFE) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0xFFFFFFFE && e == 0xFFFFFFFE) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (b) root setreuid(u32::MAX-1) accepted (no EINVAL)");
        }
    }
}

/* unpriv: setreuid(u32::MAX-1, -1) 应 EPERM (不在自身 ID 集) */
static void boundary_unpriv_extreme_eperm(void)
{
    if (getuid() != 0) {
        printf("  boundary (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setreuid(0xFFFFFFFE, (uid_t)-1);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (c) unpriv setreuid(u32::MAX-1, -1) -> -1 EPERM");
        }
    }
}

int boundary_run(void)
{
    printf("\n----- boundary -----\n");
    boundary_raw_u32max_nochg();
    boundary_root_extreme_values();
    boundary_unpriv_extreme_eperm();
    printf("  ----- boundary: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
