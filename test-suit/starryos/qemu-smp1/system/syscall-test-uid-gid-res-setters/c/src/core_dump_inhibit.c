#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SYS_setfsuid
#if defined(__x86_64__)
#define SYS_setfsuid 122
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__) || defined(__loongarch64)
#define SYS_setfsuid 151
#else
#error "unknown architecture: define SYS_setfsuid"
#endif
#endif
#ifndef SYS_setfsgid
#if defined(__x86_64__)
#define SYS_setfsgid 123
#elif defined(__aarch64__) || defined(__riscv) || defined(__loongarch__) || defined(__loongarch64)
#define SYS_setfsgid 152
#else
#error "unknown architecture: define SYS_setfsgid"
#endif
#endif

static int wait_ok(pid_t pid)
{
    int status = 0;
    return waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static void check_baseline_dumpable(void)
{
    int rc = prctl(PR_GET_DUMPABLE);
    CHECK(rc == 1, "core_dump (a) baseline dumpable == 1");
}

static void check_dumpable_prctl_roundtrip(void)
{
    if (prctl(PR_SET_DUMPABLE, 0) != 0) {
        CHECK(0, "core_dump (b) PR_SET_DUMPABLE(0) failed");
        return;
    }
    CHECK(prctl(PR_GET_DUMPABLE) == 0, "core_dump (b) PR_SET_DUMPABLE(0) -> 0");

    CHECK(prctl(PR_SET_DUMPABLE, 1) == 0, "core_dump (b) PR_SET_DUMPABLE(1) -> 0");
    CHECK(prctl(PR_GET_DUMPABLE) == 1, "core_dump (b) PR_SET_DUMPABLE(1) -> 1");

    errno = 0;
    CHECK(prctl(PR_SET_DUMPABLE, 2) == -1 && errno == EINVAL,
          "core_dump (b) PR_SET_DUMPABLE(2) returns EINVAL");
}

static void check_nochange_keeps_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (c) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(1);
        if (setresuid((uid_t)-1, (uid_t)-1, (uid_t)-1) != 0) _exit(2);
        if (prctl(PR_GET_DUMPABLE) != 1) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (c) setresuid(-1,-1,-1) keeps dumpable");
}

static void check_uid_change_resets_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (d) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(1);
        if (setresuid(1000, 2000, 3000) != 0) _exit(2);
        if (prctl(PR_GET_DUMPABLE) != 0) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (d) setresuid change resets dumpable");
}

static void check_gid_change_resets_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (e) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(1);
        if (setresgid(1000, 2000, 3000) != 0) _exit(2);
        if (prctl(PR_GET_DUMPABLE) != 0) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (e) setresgid change resets dumpable");
}

static void check_setfsuid_resets_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (f) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(1);
        if (syscall(SYS_setfsuid, 2468) != 0) _exit(2);
        if (prctl(PR_GET_DUMPABLE) != 0) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (f) setfsuid change resets dumpable");
}

static void check_setfsgid_resets_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (g) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 1) != 0) _exit(1);
        if (syscall(SYS_setfsgid, 1357) != 0) _exit(2);
        if (prctl(PR_GET_DUMPABLE) != 0) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (g) setfsgid change resets dumpable");
}

static void check_fork_inherits_dumpable(void)
{
    if (getuid() != 0) {
        printf("  core_dump (h) skip\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (prctl(PR_SET_DUMPABLE, 0) != 0) _exit(1);
        pid_t child = fork();
        if (child == 0) {
            if (prctl(PR_GET_DUMPABLE) != 0) _exit(2);
            _exit(0);
        }
        int status = 0;
        waitpid(child, &status, 0);
        _exit(WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 3);
    }

    CHECK(wait_ok(pid) == 0, "core_dump (h) fork inherits dumpable");
}

int core_dump_inhibit_run(void)
{
    printf("\n----- core_dump_inhibit (man N2) -----\n");
    check_baseline_dumpable();
    check_dumpable_prctl_roundtrip();
    check_nochange_keeps_dumpable();
    check_uid_change_resets_dumpable();
    check_gid_change_resets_dumpable();
    check_setfsuid_resets_dumpable();
    check_setfsgid_resets_dumpable();
    check_fork_inherits_dumpable();
    printf("  ----- core_dump_inhibit: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
