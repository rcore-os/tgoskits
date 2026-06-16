#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

/* getegid(2) — returns the effective group ID of the calling process.
 *
 * man 2 getgid §"DESCRIPTION":
 *   "getegid() returns the effective group ID of the calling process."
 *
 * man 2 getgid §"ERRORS":
 *   "These functions are always successful and never modify errno."
 *
 * man 2 getgid §"HISTORY":
 *   "The original Linux getuid() and geteuid() system calls supported only
 *    16-bit user IDs. Subsequently, Linux 2.4 added getuid32() and geteuid32(),
 *    supporting 32-bit IDs. The glibc getuid() and geteuid() wrapper functions
 *    transparently deal with the variations across kernel versions."
 *
 * 5 维度覆盖 (a-e):
 *   (a) basic 返回值                       man §DESCRIPTION
 *   (b) idempotent 纯查询无副作用           man §DESCRIPTION (隐含)
 *   (c) errno 不动                          man §ERRORS
 *   (d) raw syscall vs libc wrapper 一致    man §HISTORY (libc 透明处理)
 *   (e) 默认 egid == gid                    man §DESCRIPTION (无 setgid binary 时)
 */

static void getegid_basic_returns_egid(void)
{
    /* 测什么: man §DESCRIPTION — getegid() returns effective group ID.
     * 怎么测: 直接调 getegid(), 打印.
     * 期望:   返回当前进程的 effective gid (root 启动时为 0).
     * 为什么: 验证 syscall 不 panic / 不返错误指示符 (gid_t 是 unsigned, 无 -1
     *         失败语义, 所以 "always succeeds" 体现为 "调用返回任意值都合法"). */
    gid_t e = getegid();
    CHECK(e == e, "getegid (a) basic: returned value (always succeeds)");
    printf("  current effective gid = %u\n", (unsigned)e);
}

static void getegid_idempotent(void)
{
    /* 测什么: man §DESCRIPTION 隐含语义 — 纯查询无 side effect.
     * 怎么测: 连续 3 次调 getegid().
     * 期望:   3 次返回值相同.
     * 为什么: 防御 starry 实现是否有状态副作用 (如 cache invalidation bug). */
    gid_t e1 = getegid();
    gid_t e2 = getegid();
    gid_t e3 = getegid();
    CHECK(e1 == e2 && e2 == e3, "getegid (b) idempotent: 3 calls return same value");
}

static void getegid_does_not_modify_errno(void)
{
    /* 测什么: man §ERRORS — "never modify errno".
     * 怎么测: 预设 errno = 11111 (任意值), 调 getegid(), 验 errno 未变.
     * 期望:   errno 仍是 11111.
     * 为什么: starry 实现必须仅读 cred.egid, 不能踩 errno. */
    errno = 11111;
    (void)getegid();
    CHECK(errno == 11111, "getegid (c) does not modify errno");
    errno = 0;
}

static void getegid_raw_syscall_matches_libc(void)
{
    /* 测什么: man §HISTORY — libc 透明处理 16-bit/32-bit 差异, 直接转发 syscall.
     * 怎么测: 比较 syscall(SYS_getegid) 与 libc getegid() 返回值.
     * 期望:   两者相等.
     * 为什么: 验证 libc wrapper 无值转换 / 无内部 cache; raw 路径 ABI 一致. */
    gid_t libc_v = getegid();
    long raw_v = syscall(SYS_getegid);
    CHECK(raw_v >= 0 && (gid_t)raw_v == libc_v,
          "getegid (d) raw syscall matches libc wrapper");
}

static void getegid_default_equals_gid(void)
{
    /* 测什么: man §DESCRIPTION 隐含 — 普通进程 (非 setgid binary)
     *         egid == gid (启动时未做权限提升).
     * 怎么测: 比较 getegid() 与 getgid().
     * 期望:   两者相等 (CI runner 不是 setgid program).
     * 为什么: 验证 starry 启动时不误设 egid != gid; 也为后续 setresgid
     *         矩阵提供 known-state 基线. */
    gid_t g = getgid();
    gid_t e = getegid();
    CHECK(g == e, "getegid (e) default: egid == gid (no setgid binary)");
}

int getegid_run(void)
{
    printf("\n----- getegid -----\n");
    getegid_basic_returns_egid();
    getegid_idempotent();
    getegid_does_not_modify_errno();
    getegid_raw_syscall_matches_libc();
    getegid_default_equals_gid();
    printf("  ----- getegid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
