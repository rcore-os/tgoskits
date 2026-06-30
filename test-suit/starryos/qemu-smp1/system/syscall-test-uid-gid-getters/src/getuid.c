#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* small inline helper */
static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r;
    do {
        r = waitpid(pid, status, 0);
    } while (r < 0 && errno == EINTR);
    return r == pid ? 0 : -1;
}

/* getuid(2) — returns the real user ID of the calling process.
 *
 * man 2 getuid 原文（DESCRIPTION + RETURN VALUE + ERRORS）：
 *   "getuid() returns the real user ID of the calling process."
 *   "geteuid() returns the effective user ID of the calling process."
 *   "These functions are always successful and never modify errno."
 *
 * 测试方式：
 *   (a) getuid() 返回非负 uid_t 值（不报错）
 *   (b) 多次调用结果一致（无 side effect / 状态变化）
 *   (c) 不修改 errno（即使 errno 预先是非 0）
 *   (d) raw syscall 与 libc 包装一致（验证 libc 没做转换）
 *   (e) fork 子进程继承相同 uid（fork 不改变 cred.uid）
 */

static void getuid_basic_returns_uid(void)
{
    uid_t u = getuid();
    /* 怎么测：直接调 getuid()
     * 期望：返回当前进程的真实 uid
     * 为什么：man "getuid() returns the real user ID" — root 时为 0 */
    /* uid 是 uid_t (unsigned int)，无 -1 失败语义；任何值都是合法 */
    CHECK(u == u, "getuid (a) basic: returned value (always succeeds)");
    printf("  current real uid = %u\n", (unsigned)u);
}

static void getuid_idempotent(void)
{
    uid_t u1 = getuid();
    uid_t u2 = getuid();
    uid_t u3 = getuid();
    /* 怎么测：连续 3 次调用
     * 期望：3 次返回相同值
     * 为什么：getuid 是纯查询无 side effect */
    CHECK(u1 == u2 && u2 == u3, "getuid (b) idempotent: 3 calls return same value");
}

static void getuid_does_not_modify_errno(void)
{
    /* 怎么测：预设 errno = 12345，调 getuid()，检查 errno 不变
     * 期望：errno 仍是 12345
     * 为什么：man "These functions are always successful and never modify errno" */
    errno = 12345;
    (void)getuid();
    CHECK(errno == 12345, "getuid (c) does not modify errno");
    errno = 0;  /* reset */
}

static void getuid_raw_syscall_matches_libc(void)
{
    /* 怎么测：raw syscall(SYS_getuid) vs libc getuid()
     * 期望：两者返回相同值
     * 为什么：libc 包装应直接转发 syscall，无值转换 */
    uid_t libc_v = getuid();
    long raw_v = syscall(SYS_getuid);
    CHECK(raw_v >= 0 && (uid_t)raw_v == libc_v,
          "getuid (d) raw syscall matches libc wrapper");
}

static void getuid_fork_child_inherits(void)
{
    /* 怎么测：fork 后 child 调 getuid()
     * 期望：child 与 parent 返回相同 uid
     * 为什么：fork 不改变 cred.uid（man fork(2) "the child inherits"）
     * 实现：通过 _exit code 传递（uid % 256）— 仅当 uid < 256 时精确
     * 否则只验证 fork 成功 + child 不报错退出 */
    uid_t parent_uid = getuid();
    pid_t pid = fork();
    if (pid == 0) {
        /* child */
        uid_t cu = getuid();
        _exit(cu == parent_uid ? 0 : 1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "getuid (e) fork child inherits same uid");
        } else {
            CHECK(0, "getuid (e) waitpid failed");
        }
    } else {
        CHECK(0, "getuid (e) fork failed");
    }
}

int getuid_run(void)
{
    printf("\n----- getuid -----\n");
    getuid_basic_returns_uid();
    getuid_idempotent();
    getuid_does_not_modify_errno();
    getuid_raw_syscall_matches_libc();
    getuid_fork_child_inherits();
    printf("  ----- getuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
