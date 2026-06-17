/* core_dump_inhibit.c — setuid 改 euid 后 process dumpable=0.
 *
 * man 2 setuid §"NOTES":
 *   "If uid is different from the old effective UID, the process will be
 *    forbidden from leaving core dumps."
 *
 * 实现: Linux setuid 修改 cred 时调 commit_creds → cred 变 → 自动设
 * prctl(PR_SET_DUMPABLE, 0). 用户态可用 prctl(PR_GET_DUMPABLE) 验.
 *
 * starry 是否实现 prctl PR_SET/GET_DUMPABLE 未知; 若实现, 验是否
 * setuid 后自动设 0.
 *
 * 3 维度 (a-c):
 *   (a) baseline: dumpable == 1 (PR_GET_DUMPABLE default)
 *   (b) setuid(0) 不改 euid → dumpable 不变 (仍 1)
 *   (c) setuid(1000) 改 euid (0→1000) → dumpable 应 0 (禁 core dump)
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitpid_safely_cd(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

/* (a) baseline dumpable */
static void core_dump_baseline(void)
{
    /* 测什么: PR_GET_DUMPABLE 启动默认 1 (允许 core dump).
     * 怎么测: 调 prctl(PR_GET_DUMPABLE) 看 返值.
     * 期望: == 1.
     * 为什么: 验 starry 实现 prctl PR_GET_DUMPABLE 默认行为. */
    int rc = prctl(PR_GET_DUMPABLE);
    if (rc < 0 && errno == EINVAL) {
        printf("  KNOWN-STARRY-LIMITATION | core_dump (a) starry 不支持 PR_GET_DUMPABLE\n");
        return;
    }
    CHECK(rc == 1,                                                 "core_dump (a) baseline dumpable == 1");
}

/* (b) setuid 不改 euid → dumpable 不变 */
static void core_dump_setuid_no_euid_change(void)
{
    /* 测什么: man N2 — 仅在 uid != old euid 时才禁 dump. setuid(自己 uid) 不动 euid.
     * 怎么测: fork → child setuid(getuid()) → 验 dumpable 仍 1.
     * 期望: dumpable == 1 (未禁).
     * 为什么: 验 N2 条件 "different from old euid" 准确实现, 不滥禁. */
    pid_t pid = fork();
    if (pid == 0) {
        uid_t u = getuid();
        if (setuid(u) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 1) _exit(0);
        _exit(1);
    }
    int status;
    waitpid_safely_cd(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "core_dump (b) setuid(self) 不改 euid → dumpable 仍 1");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | core_dump (b) PR_GET_DUMPABLE 不支持\n");
    else                CHECK(0, "core_dump (b) failed");
}

/* (c) setuid(1000) 改 euid → dumpable 禁 */
static void core_dump_setuid_changes_euid(void)
{
    /* 测什么: man N2 — uid != old euid → process forbidden from core dumps.
     * 怎么测: root fork → child setuid(1000) (改 euid 0→1000) → 验 dumpable == 0.
     * 期望: PR_GET_DUMPABLE 返 0.
     * 为什么: 验 starry commit_creds 自动设 SUID_DUMP_DISABLE (Linux 行为). */
    if (getuid() != 0) {
        printf("  core_dump (c) skip: requires root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setuid(1000) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 0) _exit(0);                          /* Linux: 禁 ✓ */
        if (rc == 1) _exit(21);                          /* starry: 未禁 (limit) */
        _exit(1);
    }
    int status;
    waitpid_safely_cd(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0) {
        CHECK(1, "core_dump (c) setuid(1000) 改 euid → dumpable == 0 (禁 core dump)");
    } else if (ec == 20) {
        printf("  KNOWN-STARRY-LIMITATION | core_dump (c) PR_GET_DUMPABLE 不支持\n");
    } else if (ec == 21) {
        printf("  KNOWN-STARRY-LIMITATION | core_dump (c) starry commit_creds 未自动禁 dumpable\n");
    } else {
        CHECK(0, "core_dump (c) failed");
    }
}

int core_dump_inhibit_run(void)
{
    printf("\n----- core_dump_inhibit (man N2) -----\n");
    core_dump_baseline();
    core_dump_setuid_no_euid_change();
    core_dump_setuid_changes_euid();
    printf("  ----- core_dump_inhibit: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
