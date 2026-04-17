/*
 * test_credentials.c
 *
 * 测试进程凭证子系统: getuid/geteuid/getgid/getegid, getresuid/getresgid,
 * setresuid/setresgid, setuid/setgid, setreuid/setregid, fork继承,
 * getgroups/setgroups.
 *
 * 所有修改凭证的测试在 fork 子进程中进行，避免影响后续测试。
 */

#include "test_framework.h"
#include <unistd.h>
#include <grp.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <sys/wait.h>

/* Helper: fork, run func in child, wait, check exit code 0 */
static int run_in_child(void (*func)(void))
{
    pid_t pid = fork();
    if (pid == 0) {
        func();
        _exit(__fail > 0 ? 1 : 0);
    }
    int status;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) && WEXITSTATUS(status) == 0;
}

/* ---- child test functions ---- */

static void child_setresuid_all(void)
{
    long rc = syscall(SYS_setresuid, 1000, 1000, 1000);
    CHECK(rc == 0, "setresuid(1000,1000,1000)");
    uid_t ruid, euid, suid;
    getresuid(&ruid, &euid, &suid);
    CHECK(ruid == 1000, "ruid == 1000");
    CHECK(euid == 1000, "euid == 1000");
    CHECK(suid == 1000, "suid == 1000");
    CHECK(getuid() == 1000, "getuid() == 1000");
    CHECK(geteuid() == 1000, "geteuid() == 1000");
}

static void child_setresuid_nochg(void)
{
    syscall(SYS_setresuid, 1000, -1, -1);
    CHECK(getuid() == 1000, "setresuid(-1): uid changed");
    CHECK(geteuid() == 0, "setresuid(-1): euid unchanged");
}

static void child_setresuid_eperm(void)
{
    syscall(SYS_setresuid, 1000, 1000, 1000);
    long rc = syscall(SYS_setresuid, 0, -1, -1);
    CHECK(rc == -1 && errno == EPERM, "unprivileged setresuid(0) returns EPERM");
    /* Same value should still work */
    rc = syscall(SYS_setresuid, 1000, 1000, 1000);
    CHECK(rc == 0, "setresuid back to current values succeeds");
}

static void child_setresgid_all(void)
{
    long rc = syscall(SYS_setresgid, 1000, 1000, 1000);
    CHECK(rc == 0, "setresgid(1000,1000,1000)");
    gid_t rgid, egid, sgid;
    getresgid(&rgid, &egid, &sgid);
    CHECK(rgid == 1000, "rgid == 1000");
    CHECK(egid == 1000, "egid == 1000");
    CHECK(sgid == 1000, "sgid == 1000");
}

static void child_setuid_root(void)
{
    int rc = setuid(1000);
    CHECK(rc == 0, "root setuid(1000)");
    uid_t ruid, euid, suid;
    getresuid(&ruid, &euid, &suid);
    CHECK(ruid == 1000, "setuid root: ruid changed");
    CHECK(euid == 1000, "setuid root: euid changed");
    CHECK(suid == 1000, "setuid root: suid changed");
}

static void child_setuid_unpriv(void)
{
    syscall(SYS_setresuid, 1000, 1000, 1000);
    int rc = setuid(1000);
    CHECK(rc == 0, "unpriv setuid(1000) matches uid/suid");
    rc = setuid(999);
    CHECK(rc == -1 && errno == EPERM, "unpriv setuid(999) returns EPERM");
}

static void child_fork_inherit(void)
{
    syscall(SYS_setresgid, 500, 500, 500);
    syscall(SYS_setresuid, 500, 500, 500);
    pid_t grandchild = fork();
    if (grandchild == 0) {
        CHECK(getuid() == 500, "grandchild uid inherited");
        CHECK(geteuid() == 500, "grandchild euid inherited");
        CHECK(getgid() == 500, "grandchild gid inherited");
        CHECK(getegid() == 500, "grandchild egid inherited");
        _exit(__fail > 0 ? 1 : 0);
    }
    int status;
    waitpid(grandchild, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "grandchild exited ok");
}

static void child_setgroups(void)
{
    gid_t groups[] = {100, 200, 300};
    int rc = setgroups(3, groups);
    CHECK(rc == 0, "setgroups(3, [100,200,300])");
    gid_t buf[10];
    int n = getgroups(10, buf);
    CHECK(n == 3, "getgroups returns 3");
    CHECK(buf[0] == 100, "group[0] == 100");
    CHECK(buf[1] == 200, "group[1] == 200");
    CHECK(buf[2] == 300, "group[2] == 300");
    n = getgroups(0, NULL);
    CHECK(n == 3, "getgroups(0) returns count=3");
}

static void child_setgroups_eperm(void)
{
    syscall(SYS_setresuid, 1000, 1000, 1000);
    gid_t groups[] = {100};
    int rc = setgroups(1, groups);
    CHECK(rc == -1 && errno == EPERM, "unpriv setgroups returns EPERM");
}

int main(void)
{
    TEST_START("credentials: 进程凭证子系统测试");

    /* Initial state: root */
    CHECK_RET(getuid(), 0, "initial uid == 0");
    CHECK_RET(geteuid(), 0, "initial euid == 0");
    CHECK_RET(getgid(), 0, "initial gid == 0");
    CHECK_RET(getegid(), 0, "initial egid == 0");

    uid_t ruid, euid, suid;
    int rc = getresuid(&ruid, &euid, &suid);
    CHECK(rc == 0 && ruid == 0 && euid == 0 && suid == 0,
          "getresuid returns all zero");

    gid_t rgid, egid, sgid;
    rc = getresgid(&rgid, &egid, &sgid);
    CHECK(rc == 0 && rgid == 0 && egid == 0 && sgid == 0,
          "getresgid returns all zero");

    CHECK(run_in_child(child_setresuid_all),  "setresuid changes all ids");
    CHECK(run_in_child(child_setresuid_nochg), "setresuid -1 no change");
    CHECK(run_in_child(child_setresuid_eperm), "setresuid EPERM unprivileged");
    CHECK(run_in_child(child_setresgid_all),  "setresgid changes all gids");
    CHECK(run_in_child(child_setuid_root),    "setuid root sets all three");
    CHECK(run_in_child(child_setuid_unpriv),  "setuid unprivileged");
    CHECK(run_in_child(child_fork_inherit),   "fork inherits credentials");
    CHECK(run_in_child(child_setgroups),      "setgroups/getgroups");
    CHECK(run_in_child(child_setgroups_eperm), "setgroups EPERM unprivileged");

    TEST_DONE();
}
