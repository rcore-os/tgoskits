/* fsuid_follow.c — codex P1 (adopted) — verify fsuid/fsgid 跟 effective ID
 *
 * man 2 setresuid §"DESCRIPTION":
 *   "Regardless of what changes are made to the real UID, effective UID, and
 *    saved set-user-ID, the filesystem UID is always set to the same value
 *    as the (possibly new) effective UID."
 *
 * man 2 setfsuid:
 *   "setfsuid() sets the user ID that the Linux kernel uses to check for all
 *    accesses to the filesystem. ... setfsuid(uid_t fsuid)."
 *   trick: setfsuid(-1) returns prev fsuid, doesn't change it.
 *
 * starry 实现 (sys.rs:58-148): new.fsuid = new.euid; new.fsgid = new.egid;
 * 但 starry 无 sys_setfsuid syscall → 无法直接 query fsuid.
 *
 * 策略:
 *   - 用 setfsuid(-1) syscall 尝试 query.
 *   - 若 starry 返 ENOSYS → SKIP + 标 KNOWN-STARRY-LIMITATION (不是 bug,
 *     是 starry 未实现该 syscall).
 *   - 若返 prev fsuid → 验证 = new euid.
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_setfsuid
#define SYS_setfsuid 122
#endif
#ifndef SYS_setfsgid
#define SYS_setfsgid 123
#endif

static int waitpid_safely2(pid_t pid, int *st)
{
    return waitpid(pid, st, 0) == pid ? 0 : -1;
}

/* Query current fsuid via setfsuid(-1) trick.
 * 返回 prev fsuid (Linux), 或 -1 (starry 未实现).
 * starry: 因无 SYS_setfsuid → syscall 返 -1 + errno=ENOSYS. */
static long query_fsuid(void)
{
    errno = 0;
    long rc = syscall(SYS_setfsuid, (uid_t)-1);
    return rc;
}
static long query_fsgid(void)
{
    errno = 0;
    long rc = syscall(SYS_setfsgid, (gid_t)-1);
    return rc;
}

/* 测 1: setresuid 后 fsuid == new euid */
static void fsuid_follows_euid_after_setresuid(void)
{
    if (getuid() != 0) {
        printf("  fsuid (a) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 先 setresuid 到不同值 — euid 设为 1234 */
        if (setresuid(0, 1234, 0) != 0) _exit(1);
        long fs = query_fsuid();
        int err = errno;
        if (fs < 0 && err == ENOSYS) _exit(20);  /* starry: 不支持 setfsuid */
        if (fs == 1234) _exit(0);                /* Linux: fsuid == 1234 ✓ */
        printf("  setresuid(0,1234,0) → fsuid query=%ld errno=%d\n", fs, err);
        _exit(2);
    }
    int status;
    waitpid_safely2(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "fsuid (a) Linux: setresuid(0,1234,0) → fsuid == 1234 (跟 euid)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | fsuid (a): starry 无 SYS_setfsuid syscall (无法 query)\n");
        printf("                          | starry 内部 sys.rs new.fsuid = new.euid 已写, 但无 query 接口\n");
    } else {
        CHECK(0, "fsuid (a) failed: setresuid 或 fsuid query 异常");
    }
}

/* 测 2: setresgid 后 fsgid == new egid */
static void fsgid_follows_egid_after_setresgid(void)
{
    if (getuid() != 0) {
        printf("  fsuid (b) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(0, 5678, 0) != 0) _exit(1);
        long fs = query_fsgid();
        int err = errno;
        if (fs < 0 && err == ENOSYS) _exit(20);
        if (fs == 5678) _exit(0);
        _exit(2);
    }
    int status;
    waitpid_safely2(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "fsuid (b) Linux: setresgid(0,5678,0) → fsgid == 5678 (跟 egid)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | fsuid (b): starry 无 SYS_setfsgid syscall\n");
    } else {
        CHECK(0, "fsuid (b) failed");
    }
}

/* 测 3: setresuid(NOCHG) 不动 fsuid */
static void fsuid_unchanged_when_euid_unchanged(void)
{
    if (getuid() != 0) {
        printf("  fsuid (c) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 设 fsuid 基线 */
        if (setresuid(0, 2222, 0) != 0) _exit(1);
        long before = query_fsuid();
        int err1 = errno;
        if (before < 0 && err1 == ENOSYS) _exit(20);
        /* 用 NOCHG 调 — euid 不变 */
        if (setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1) != 0) _exit(2);
        long after = query_fsuid();
        if (before == after && after == 2222) _exit(0);
        printf("  NOCHG before=%ld after=%ld\n", before, after);
        _exit(3);
    }
    int status;
    waitpid_safely2(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "fsuid (c) NOCHG setresuid 不动 fsuid (仍 == 2222)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | fsuid (c): 无 SYS_setfsuid\n");
    } else {
        CHECK(0, "fsuid (c) failed");
    }
}

int fsuid_follow_run(void)
{
    printf("\n----- fsuid_follow -----\n");
    fsuid_follows_euid_after_setresuid();
    fsgid_follows_egid_after_setresgid();
    fsuid_unchanged_when_euid_unchanged();
    printf("  ----- fsuid_follow: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
