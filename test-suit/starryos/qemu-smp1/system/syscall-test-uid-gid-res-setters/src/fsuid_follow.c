#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
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

static long query_fsuid(void)
{
    errno = 0;
    return syscall(SYS_setfsuid, (uid_t)-1);
}

static long query_fsgid(void)
{
    errno = 0;
    return syscall(SYS_setfsgid, (gid_t)-1);
}

static void check_procfs_uid_gid_roundtrip(void)
{
    if (getuid() != 0) {
        printf("  fsuid/procfs skip: requires root\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(0, 5678, 0) != 0) _exit(3);
        if (query_fsgid() != 5678) _exit(4);
        if (setresuid(0, 1234, 0) != 0) _exit(1);
        if (query_fsuid() != 1234) _exit(2);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "fsuid (a) setresuid/setresgid track fsuid/fsgid");
}

static void check_setfsuid_query_roundtrip(void)
{
    if (getuid() != 0) {
        printf("  fsuid (b) skip: requires root\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        long prev = syscall(SYS_setfsuid, 2468);
        if (prev != 0) _exit(1);
        long fs = query_fsuid();
        if (fs != 2468) _exit(2);
        prev = syscall(SYS_setfsuid, (uid_t)-1);
        if (prev != 2468) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "fsuid (b) setfsuid query and roundtrip");
}

static void check_setfsgid_query_roundtrip(void)
{
    if (getuid() != 0) {
        printf("  fsuid (c) skip: requires root\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        long prev = syscall(SYS_setfsgid, 1357);
        if (prev != 0) _exit(1);
        long fs = query_fsgid();
        if (fs != 1357) _exit(2);
        prev = syscall(SYS_setfsgid, (gid_t)-1);
        if (prev != 1357) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "fsuid (c) setfsgid query and roundtrip");
}

static void check_unprivileged_setfsuid_same_uid(void)
{
    if (getuid() != 0) {
        printf("  fsuid (d) skip: requires root\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(1);
        long prev = syscall(SYS_setfsuid, 1000);
        if (prev != 1000) _exit(2);
        if (query_fsuid() != 1000) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "fsuid (d) unprivileged setfsuid(self)");
}

static void check_unprivileged_setfsgid_same_gid(void)
{
    if (getuid() != 0) {
        printf("  fsuid (e) skip: requires root\n");
        return;
    }

    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(2000, 2000, 2000) != 0) _exit(1);
        long prev = syscall(SYS_setfsgid, 2000);
        if (prev != 2000) _exit(2);
        if (query_fsgid() != 2000) _exit(3);
        _exit(0);
    }

    CHECK(wait_ok(pid) == 0, "fsuid (e) unprivileged setfsgid(self)");
}

int fsuid_follow_run(void)
{
    printf("\n----- fsuid_follow -----\n");
    check_procfs_uid_gid_roundtrip();
    check_setfsuid_query_roundtrip();
    check_setfsgid_query_roundtrip();
    check_unprivileged_setfsuid_same_uid();
    check_unprivileged_setfsgid_same_gid();
    printf("  ----- fsuid_follow: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
