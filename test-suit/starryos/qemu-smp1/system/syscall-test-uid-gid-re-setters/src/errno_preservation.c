/* errno_preservation.c — setreuid/setregid 成功路径 errno 不被误改 (Group C Round 8).
 *
 * man 2 setreuid §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is
 *    set to indicate the error."
 *
 * Linux 实测: 成功路径 errno 不动. 验 starry sys_setreuid/setregid 在
 * success path 不误改 user errno (常见 starry vm_write/cred update 后误设
 * errno 残值的 bug 类型).
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

static void setreuid_nochg_errno_preserved(void)
{
    /* 测什么/怎么测/期望/为什么: setreuid(-1,-1) NOCHG 路径成功 + errno 不动. */
    errno = 4242;
    int rc = setreuid((uid_t)-1, (uid_t)-1);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (a) setreuid(-1,-1) -> 0");
    CHECK(err == 4242,                                            "errno_preserve (a) setreuid(-1,-1) 不改 errno (still 4242)");
    errno = 0;
}

static void setregid_nochg_errno_preserved(void)
{
    errno = 5555;
    int rc = setregid((gid_t)-1, (gid_t)-1);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (b) setregid(-1,-1) -> 0");
    CHECK(err == 5555,                                            "errno_preserve (b) setregid(-1,-1) 不改 errno (still 5555)");
    errno = 0;
}

static void setreuid_self_errno_preserved(void)
{
    /* 测什么: setreuid(self, self) 实际写 cred 路径 errno 不动. */
    uid_t u = getuid();
    uid_t e = geteuid();
    errno = 6666;
    int rc = setreuid(u, e);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (c) setreuid(self,self) -> 0");
    CHECK(err == 6666,                                            "errno_preserve (c) setreuid(self,self) 不改 errno (still 6666)");
    errno = 0;
}

static void setregid_self_errno_preserved(void)
{
    gid_t g = getgid();
    gid_t e = getegid();
    errno = 7777;
    int rc = setregid(g, e);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (d) setregid(self,self) -> 0");
    CHECK(err == 7777,                                            "errno_preserve (d) setregid(self,self) 不改 errno (still 7777)");
    errno = 0;
}

static void raw_setreuid_success_errno_preserved(void)
{
    /* 测什么: raw syscall 路径 errno 不动 (与 libc 路径分离测). */
    errno = 1111;
    long rc = syscall(SYS_setreuid, getuid(), geteuid());
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (e) raw setreuid(self,self) -> 0");
    CHECK(err == 1111,                                            "errno_preserve (e) raw setreuid(self,self) 不改 errno (still 1111)");
    errno = 0;
}

int errno_preservation_run(void)
{
    printf("\n----- errno_preservation (Round 8) -----\n");
    setreuid_nochg_errno_preserved();
    setregid_nochg_errno_preserved();
    setreuid_self_errno_preserved();
    setregid_self_errno_preserved();
    raw_setreuid_success_errno_preserved();
    printf("  ----- errno_preservation: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
