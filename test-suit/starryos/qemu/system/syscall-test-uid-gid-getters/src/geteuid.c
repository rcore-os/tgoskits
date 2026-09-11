#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* geteuid(2) — returns the effective user ID of the calling process.
 *
 * man 2 getuid §"DESCRIPTION":
 *   "geteuid() returns the effective user ID of the calling process."
 *
 * man 2 getuid §"ERRORS":
 *   "These functions are always successful and never modify errno."
 *
 * man 2 getuid §"HISTORY":
 *   "Linux 2.4 added getuid32() and geteuid32(), supporting 32-bit IDs.
 *    The glibc getuid() and geteuid() wrapper functions transparently
 *    deal with the variations across kernel versions."
 *
 * 4 维度覆盖 (b-e):
 *   (b) idempotent 纯查询无副作用           man §DESCRIPTION (隐含)
 *   (c) errno 不动                          man §ERRORS
 *   (d) raw syscall vs libc wrapper 一致    man §HISTORY (libc 透明处理)
 *   (e) 默认 euid == uid                    man fork+exec 继承 (无 setuid binary 时)
 */


static void geteuid_idempotent(void)
{
    /* 测什么: man §DESCRIPTION 隐含 — 纯查询无 side effect.
     * 怎么测: 连续 3 次调 geteuid().
     * 期望:   3 次返回值相同.
     * 为什么: 防御 starry 实现是否有状态副作用 (如 cache invalidation bug). */
    uid_t e1 = geteuid();
    uid_t e2 = geteuid();
    uid_t e3 = geteuid();
    CHECK(e1 == e2 && e2 == e3, "geteuid (b) idempotent: 3 calls return same value");
}

static void geteuid_does_not_modify_errno(void)
{
    /* 测什么: man §ERRORS — "never modify errno".
     * 怎么测: 预设 errno = 67890 (任意值), 调 geteuid(), 验 errno 未变.
     * 期望:   errno 仍是 67890.
     * 为什么: starry 实现必须仅读 cred.euid, 不能踩 errno. */
    errno = 67890;
    (void)geteuid();
    CHECK(errno == 67890, "geteuid (c) does not modify errno");
    errno = 0;
}

static void geteuid_raw_syscall_matches_libc(void)
{
    /* 测什么: man §HISTORY — libc 透明处理 16/32-bit syscall 差异.
     * 怎么测: 比较 syscall(SYS_geteuid) 与 libc geteuid() 返回值.
     * 期望:   两者相等.
     * 为什么: 验证 libc wrapper 无值转换 / 无内部 cache; raw 路径 ABI 一致.
     *         Alpha 上是 getxuid() 单 syscall 返二元组 — libc 也透明处理. */
    uid_t libc_v = geteuid();
    long raw_v = syscall(SYS_geteuid);
    CHECK(raw_v >= 0 && (uid_t)raw_v == libc_v,
          "geteuid (d) raw syscall matches libc wrapper");
}

static void geteuid_default_equals_uid(void)
{
    /* 测什么: man fork(2) + execve(2) — fork+exec 继承不改变 cred;
     *         setuid binary 才会改 euid; CI runner 不是 setuid 程序.
     * 怎么测: 比较 geteuid() vs getuid().
     * 期望:   两者相等 (启动时 euid == uid).
     * 为什么: 为后续 setresuid 矩阵 (Group D) 提供 known-state 基线;
     *         也验证 starry 启动时不误设 euid != uid. */
    uid_t u = getuid();
    uid_t e = geteuid();
    CHECK(u == e, "geteuid (e) default: euid == uid (no setuid binary)");
}

int geteuid_run(void)
{
    printf("\n----- geteuid -----\n");
    geteuid_idempotent();
    geteuid_does_not_modify_errno();
    geteuid_raw_syscall_matches_libc();
    geteuid_default_equals_uid();
    printf("  ----- geteuid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
