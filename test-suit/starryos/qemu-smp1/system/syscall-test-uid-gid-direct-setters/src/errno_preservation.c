/* errno_preservation.c — setuid/setgid 成功路径 errno 不被误改 (Group B Round 8).
 *
 * man 2 setuid §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is
 *    set to indicate the error."
 *
 * POSIX 不明确 setter 成功路径是否保留 errno, 但 Linux 实测 不动 (符合
 * "errno is set ON error" 的隐含语义). 验 starry sys_setuid/setgid 在成功
 * 路径不误改 errno (常见 starry bug: vm_write 后 errno 被设残值).
 *
 * Round 8 = 二轮 final iteration, 重点补正路径副作用 (errno) 检查.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static void setuid_self_errno_preserved(void)
{
    /* 测什么: setuid(self) 成功路径 errno 不被改.
     * 怎么测: 预设 errno=4242 → setuid(getuid()) → 验 errno 仍 4242.
     * 期望:   rc=0 + errno==4242.
     * 为什么: starry sys_setuid 成功路径若误调 errno=0 / errno=残值,
     *         用户态 errno-then-call 模式 (常见 libc pattern) 会失效. */
    errno = 4242;
    int rc = setuid(getuid());
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (a) setuid(self) -> 0");
    CHECK(err == 4242,                                            "errno_preserve (a) setuid(self) 不改 errno (still 4242)");
    errno = 0;
}

static void setgid_self_errno_preserved(void)
{
    /* 测什么/怎么测/期望/为什么: 同 (a), 但 GID 维度. */
    errno = 5555;
    int rc = setgid(getgid());
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (b) setgid(self) -> 0");
    CHECK(err == 5555,                                            "errno_preserve (b) setgid(self) 不改 errno (still 5555)");
    errno = 0;
}

static void setuid_root_errno_preserved(void)
{
    /* 测什么: root setuid(0) (cred 实际改变, 但仍 success) errno 不动.
     * 怎么测: root, errno=8888, setuid(0), 验 errno 仍 8888.
     * 期望:   rc=0 + errno==8888.
     * 为什么: root 路径走 has_cap_setuid 分支 (改 r/e/s), 区别于 unpriv 路径,
     *         独立验 errno 不动. */
    if (getuid() != 0) {
        printf("  errno_preserve (c) skip: not root\n");
        return;
    }
    errno = 8888;
    int rc = setuid(0);  /* idempotent for root */
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (c) root setuid(0) -> 0");
    CHECK(err == 8888,                                            "errno_preserve (c) root setuid(0) 不改 errno (still 8888)");
    errno = 0;
}

static void setgid_root_errno_preserved(void)
{
    if (getuid() != 0) {
        printf("  errno_preserve (d) skip: not root\n");
        return;
    }
    errno = 9999;
    int rc = setgid(0);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (d) root setgid(0) -> 0");
    CHECK(err == 9999,                                            "errno_preserve (d) root setgid(0) 不改 errno (still 9999)");
    errno = 0;
}

static void raw_setuid_success_errno_preserved(void)
{
    /* 测什么: raw syscall 成功路径 errno 不动 (与 libc 路径分离测).
     *         libc 可能在 wrapper 内 errno=0 但不该如此, raw 走 kernel 直达.
     * 怎么测: errno=1111 → syscall(SYS_setuid, getuid()) → 验 errno=1111.
     * 期望:   rc=0 + errno==1111.
     * 为什么: 验 starry kernel syscall 入口 + return 路径不动 user errno. */
    errno = 1111;
    long rc = syscall(SYS_setuid, getuid());
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (e) raw setuid(self) -> 0");
    CHECK(err == 1111,                                            "errno_preserve (e) raw setuid(self) 不改 errno (still 1111)");
    errno = 0;
}

int errno_preservation_run(void)
{
    printf("\n----- errno_preservation (Round 8) -----\n");
    setuid_self_errno_preserved();
    setgid_self_errno_preserved();
    setuid_root_errno_preserved();
    setgid_root_errno_preserved();
    raw_setuid_success_errno_preserved();
    printf("  ----- errno_preservation: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
