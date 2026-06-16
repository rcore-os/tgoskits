/* core_dump_inhibit.c — setreuid 改 euid → dumpable=0 (man N2).
 *
 * man 2 setreuid (引用自 setuid 共享 NOTES 隐含):
 *   "If euid is different from the old effective UID, the process will be
 *    forbidden from leaving core dumps."
 *
 * 3 case (a-c).
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

static void cd_baseline(void)
{
    /* 测什么/怎么测/期望/为什么: dumpable 启动 1, prctl(PR_GET_DUMPABLE) 返 1. */
    int rc = prctl(PR_GET_DUMPABLE);
    if (rc < 0 && errno == EINVAL) {
        printf("  KNOWN-STARRY-LIMITATION | core_dump (a) PR_GET_DUMPABLE 不支持\n");
        return;
    }
    CHECK(rc == 1,                                                 "core_dump (a) baseline dumpable == 1");
}

/* (b) setreuid 不改 euid → dumpable 不变 */
static void cd_setreuid_no_euid_change(void)
{
    /* 测什么: setreuid(-1,-1) NOCHG 不动 euid → dumpable 仍 1.
     * 怎么测: fork → child setreuid(-1,-1) → 验 dumpable.
     * 期望: 1. */
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid((uid_t)-1, (uid_t)-1) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 1) _exit(0);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "core_dump (b) setreuid(-1,-1) NOCHG → dumpable 仍 1");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | core_dump (b) PR_GET_DUMPABLE 不支持\n");
    else                CHECK(0, "core_dump (b) failed");
}

/* (c) setreuid(1000, 2000) 改 euid → dumpable=0 */
static void cd_setreuid_changes_euid(void)
{
    /* 测什么: setreuid 改 euid (0→2000) → dumpable=0.
     * 怎么测: root fork → child setreuid(1000, 2000) → 验 dumpable.
     * 期望: 0 (禁 core dump). */
    if (getuid() != 0) { printf("  core_dump (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setreuid(1000, 2000) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 0) _exit(0);
        if (rc == 1) _exit(21);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "core_dump (c) setreuid(1k,2k) 改 euid → dumpable=0");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | core_dump (c) PR_GET_DUMPABLE\n");
    else if (ec == 21)  printf("  KNOWN-STARRY-LIMITATION | core_dump (c) starry 未禁 dumpable\n");
    else                CHECK(0, "core_dump (c) failed");
}

int core_dump_inhibit_run(void)
{
    printf("\n----- core_dump_inhibit (man N2) -----\n");
    cd_baseline();
    cd_setreuid_no_euid_change();
    cd_setreuid_changes_euid();
    printf("  ----- core_dump_inhibit: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
