/* errno_preservation.c — setresuid/setresgid 成功路径 errno 不被误改 (Group D Round 8).
 *
 * man 2 setresuid §"RETURN VALUE":
 *   "On success, zero is returned. On error, -1 is returned, and errno is
 *    set to indicate the error."
 *
 * Linux 实测: 成功路径 errno 不动. 验 starry sys_setresuid/setresgid 在
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

static void setresuid_nochg_errno_preserved(void)
{
    /* 测什么/怎么测/期望/为什么: setresuid(-1,-1,-1) NOCHG → errno 不动. */
    errno = 4242;
    int rc = setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (a) setresuid(-1,-1,-1) -> 0");
    CHECK(err == 4242,                                            "errno_preserve (a) setresuid(-1,-1,-1) 不改 errno (still 4242)");
    errno = 0;
}

static void setresgid_nochg_errno_preserved(void)
{
    errno = 5555;
    int rc = setresgid((gid_t)-1, (gid_t)-1, (gid_t)-1);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (b) setresgid(-1,-1,-1) -> 0");
    CHECK(err == 5555,                                            "errno_preserve (b) setresgid(-1,-1,-1) 不改 errno (still 5555)");
    errno = 0;
}

static void setresuid_self_errno_preserved(void)
{
    /* 测什么: setresuid(self.r, self.e, self.s) 实际写 cred + errno 不动. */
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    errno = 6666;
    int rc = setresuid(r, e, s);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (c) setresuid(self triplet) -> 0");
    CHECK(err == 6666,                                            "errno_preserve (c) setresuid(self) 不改 errno (still 6666)");
    errno = 0;
}

static void setresgid_self_errno_preserved(void)
{
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    errno = 7777;
    int rc = setresgid(r, e, s);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (d) setresgid(self triplet) -> 0");
    CHECK(err == 7777,                                            "errno_preserve (d) setresgid(self) 不改 errno (still 7777)");
    errno = 0;
}

static void raw_setresuid_success_errno_preserved(void)
{
    /* 测什么: raw syscall 路径 errno 不动. */
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) { CHECK(0, "skip"); return; }
    errno = 1111;
    long rc = syscall(SYS_setresuid, r, e, s);
    int err = errno;
    CHECK(rc == 0,                                                "errno_preserve (e) raw setresuid(self triplet) -> 0");
    CHECK(err == 1111,                                            "errno_preserve (e) raw setresuid(self) 不改 errno (still 1111)");
    errno = 0;
}

int errno_preservation_run(void)
{
    printf("\n----- errno_preservation (Round 8) -----\n");
    setresuid_nochg_errno_preserved();
    setresgid_nochg_errno_preserved();
    setresuid_self_errno_preserved();
    setresgid_self_errno_preserved();
    raw_setresuid_success_errno_preserved();
    printf("  ----- errno_preservation: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
