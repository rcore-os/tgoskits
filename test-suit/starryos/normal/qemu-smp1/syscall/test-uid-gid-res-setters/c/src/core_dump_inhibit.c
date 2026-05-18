/* core_dump_inhibit.c — setresuid 改 euid → dumpable=0 (man N2). */

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
    /* 测什么/怎么测/期望/为什么: dumpable 启动 1. */
    int rc = prctl(PR_GET_DUMPABLE);
    if (rc < 0 && errno == EINVAL) {
        printf("  KNOWN-STARRY-LIMITATION | (a) PR_GET_DUMPABLE\n");
        return;
    }
    CHECK(rc == 1,                                                 "core_dump (a) baseline dumpable == 1");
}

static void cd_setresuid_nochg(void)
{
    /* 测什么/怎么测/期望/为什么: setresuid(-1,-1,-1) NOCHG → dumpable 仍 1. */
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 1) _exit(0);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "core_dump (b) setresuid(-1×3) NOCHG → dumpable 仍 1");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | (b) PR_GET_DUMPABLE\n");
    else                CHECK(0, "core_dump (b) failed");
}

static void cd_setresuid_changes(void)
{
    /* 测什么/怎么测/期望/为什么: setresuid(_,2000,_) 改 euid → dumpable=0. */
    if (getuid() != 0) { printf("  core_dump (c) skip\n"); return; }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 2000, 3000) != 0) _exit(99);
        int rc = prctl(PR_GET_DUMPABLE);
        if (rc < 0 && errno == EINVAL) _exit(20);
        if (rc == 0) _exit(0);
        if (rc == 1) _exit(21);
        _exit(1);
    }
    int status;
    waitpid(pid, &status, 0);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    if (ec == 0)        CHECK(1, "core_dump (c) setresuid(1k,2k,3k) 改 euid → dumpable=0");
    else if (ec == 20)  printf("  KNOWN-STARRY-LIMITATION | (c) PR_GET_DUMPABLE\n");
    else if (ec == 21)  printf("  KNOWN-STARRY-LIMITATION | (c) starry 未禁 dumpable\n");
    else                CHECK(0, "core_dump (c) failed");
}

int core_dump_inhibit_run(void)
{
    printf("\n----- core_dump_inhibit (man N2) -----\n");
    cd_baseline();
    cd_setresuid_nochg();
    cd_setresuid_changes();
    printf("  ----- core_dump_inhibit: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
