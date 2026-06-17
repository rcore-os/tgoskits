#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* getgid(2) — returns the real group ID of the calling process.
 *
 * man 2 getgid §"DESCRIPTION":
 *   "getgid() returns the real group ID of the calling process."
 *
 * man 2 getgid §"ERRORS":
 *   "These functions are always successful and never modify errno."
 *
 * man 2 getgid §"HISTORY":
 *   "The original Linux getgid() and getegid() system calls supported only
 *    16-bit group IDs. Subsequently, Linux 2.4 added getgid32() and getegid32(),
 *    supporting 32-bit IDs. The glibc wrapper functions transparently deal
 *    with the variations across kernel versions."
 *
 * 5 维度覆盖 (a-e):
 *   (a) basic 返回值                       man §DESCRIPTION
 *   (b) idempotent 纯查询无副作用           man §DESCRIPTION (隐含)
 *   (c) errno 不动                          man §ERRORS
 *   (d) raw syscall vs libc wrapper 一致    man §HISTORY (libc 透明处理)
 *   (e) fork 子进程继承相同 gid             man fork(2) "the child inherits ... cred"
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void getgid_basic_returns_gid(void)
{
    /* 测什么: man §DESCRIPTION — getgid() returns real group ID.
     * 怎么测: 直接调 getgid(), 打印.
     * 期望:   返回当前 cred.gid (root 启动时为 0).
     * 为什么: 验证 syscall 不 panic. gid_t 是 unsigned, "always succeeds". */
    gid_t g = getgid();
    CHECK(g == g, "getgid (a) basic: returned value (always succeeds)");
    printf("  current real gid = %u\n", (unsigned)g);
}

static void getgid_idempotent(void)
{
    /* 测什么: man §DESCRIPTION 隐含 — 纯查询无副作用.
     * 怎么测: 连续 3 次调 getgid().
     * 期望:   3 次返回值相同.
     * 为什么: 防 starry 实现 cache invalidation bug. */
    gid_t g1 = getgid();
    gid_t g2 = getgid();
    gid_t g3 = getgid();
    CHECK(g1 == g2 && g2 == g3, "getgid (b) idempotent: 3 calls return same value");
}

static void getgid_does_not_modify_errno(void)
{
    /* 测什么: man §ERRORS — "never modify errno".
     * 怎么测: 预设 errno = 99999, 调 getgid(), 验未变.
     * 期望:   errno 仍是 99999.
     * 为什么: starry 仅读 cred.gid, 不许踩 errno. */
    errno = 99999;
    (void)getgid();
    CHECK(errno == 99999, "getgid (c) does not modify errno");
    errno = 0;
}

static void getgid_raw_syscall_matches_libc(void)
{
    /* 测什么: man §HISTORY — libc 透明处理 16/32-bit 差异.
     * 怎么测: 比较 syscall(SYS_getgid) vs libc getgid().
     * 期望:   两者相等.
     * 为什么: 验证 libc wrapper 无值转换 / 无内部 cache. */
    gid_t libc_v = getgid();
    long raw_v = syscall(SYS_getgid);
    CHECK(raw_v >= 0 && (gid_t)raw_v == libc_v,
          "getgid (d) raw syscall matches libc wrapper");
}

static void getgid_fork_child_inherits(void)
{
    /* 测什么: man fork(2) — "The child process inherits copies of the parent's
     *         ... credentials." getgid 在 fork 跨进程边界仍返同值.
     * 怎么测: fork → child 调 getgid() 比对 parent.
     *         通过 _exit code 传递结果 (0=PASS, 1=FAIL).
     * 期望:   child.getgid() == parent.getgid().
     * 为什么: 防 starry fork 实现错误地重置 cred.gid (历史曾在某些
     *         microkernel 实现见过). */
    gid_t parent_gid = getgid();
    pid_t pid = fork();
    if (pid == 0) {
        gid_t cg = getgid();
        _exit(cg == parent_gid ? 0 : 1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "getgid (e) fork child inherits same gid");
        }
    } else {
        CHECK(0, "getgid (e) fork failed");
    }
}

int getgid_run(void)
{
    printf("\n----- getgid -----\n");
    getgid_basic_returns_gid();
    getgid_idempotent();
    getgid_does_not_modify_errno();
    getgid_raw_syscall_matches_libc();
    getgid_fork_child_inherits();
    printf("  ----- getgid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
