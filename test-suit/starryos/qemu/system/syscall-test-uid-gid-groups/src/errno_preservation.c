/* errno_preservation.c — getgroups/setgroups 成功路径 errno 不被误改 (Group E Round 8).
 *
 * man 2 getgroups §"RETURN VALUE":
 *   "On success, getgroups() returns the number of supplementary group IDs.
 *    On error, -1 is returned, and errno is set to indicate the error."
 *   "On success, setgroups() returns 0. On error, -1 is returned, and errno
 *    is set to indicate the error."
 *
 * Linux 实测: 成功路径 errno 不动. 验 starry sys_getgroups/setgroups 在
 * success path 不误改 user errno.
 *
 * getgroups 特殊性: 返 ngroups (非 0/非 -1) 而非 0 — 验 starry 返非负
 * 值时也不动 errno.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void getgroups_query_errno_preserved(void)
{
    /* 测什么/怎么测/期望/为什么: getgroups(0, NULL) 查询模式 → errno 不动. */
    errno = 4242;
    int n = getgroups(0, NULL);
    int err = errno;
    CHECK(n >= 0,                                                 "errno_preserve (a) getgroups(0, NULL) >= 0");
    CHECK(err == 4242,                                            "errno_preserve (a) getgroups 不改 errno (still 4242)");
    errno = 0;
}

static void getgroups_fill_errno_preserved(void)
{
    /* 测什么: getgroups(N, buf) 实际填模式 → errno 不动. */
    gid_t buf[256];
    errno = 5555;
    int n = getgroups(256, buf);
    int err = errno;
    CHECK(n >= 0,                                                 "errno_preserve (b) getgroups(256, buf) >= 0");
    CHECK(err == 5555,                                            "errno_preserve (b) getgroups(256) 不改 errno (still 5555)");
    errno = 0;
}

static void setgroups_clear_errno_preserved(void)
{
    /* 测什么: root setgroups(0, NULL) 清空模式 → errno 不动. */
    if (getuid() != 0) {
        printf("  errno_preserve (c) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        errno = 6666;
        int rc = setgroups(0, NULL);
        int err = errno;
        if (rc == 0 && err == 6666) _exit(0);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "errno_preserve (c) root setgroups(0, NULL) → 0 + errno preserved (6666)");
    }
}

static void setgroups_set_errno_preserved(void)
{
    /* 测什么: root setgroups(N, list) 实际写模式 → errno 不动. */
    if (getuid() != 0) {
        printf("  errno_preserve (d) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t g[] = {300, 400, 500};
        errno = 7777;
        int rc = setgroups(3, g);
        int err = errno;
        if (rc == 0 && err == 7777) _exit(0);
        _exit(1);
    }
    int status;
    if (waitpid_safely(pid, &status) == 0) {
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "errno_preserve (d) root setgroups(3, ...) → 0 + errno preserved (7777)");
    }
}

static void raw_getgroups_errno_preserved(void)
{
    /* 测什么: raw syscall(SYS_getgroups, ...) 成功路径 errno 不动. */
    gid_t buf[64];
    errno = 1111;
    long rc = syscall(SYS_getgroups, 64, buf);
    int err = errno;
    CHECK(rc >= 0,                                                "errno_preserve (e) raw getgroups(64, buf) >= 0");
    CHECK(err == 1111,                                            "errno_preserve (e) raw getgroups 不改 errno (still 1111)");
    errno = 0;
}

int errno_preservation_run(void)
{
    printf("\n----- errno_preservation (Round 8) -----\n");
    getgroups_query_errno_preserved();
    getgroups_fill_errno_preserved();
    setgroups_clear_errno_preserved();
    setgroups_set_errno_preserved();
    raw_getgroups_errno_preserved();
    printf("  ----- errno_preservation: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
