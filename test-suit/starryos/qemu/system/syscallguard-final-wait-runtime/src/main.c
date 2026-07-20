#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int waitid_raw(int idtype, pid_t id, siginfo_t *info, int options)
{
    return (int)syscall(SYS_waitid, idtype, id, info, options, NULL);
}

static int waitid_event(pid_t pid, siginfo_t *info, int options)
{
    for (int attempt = 0; attempt < 500; attempt++) {
        memset(info, 0, sizeof(*info));
        int ret = waitid_raw(P_PID, pid, info, options | WNOHANG);
        if (ret != 0 || info->si_pid != 0)
            return ret;
        usleep(10000);
    }
    errno = ETIMEDOUT;
    return -1;
}

static pid_t spawn_exit(int code)
{
    pid_t pid = fork();
    if (pid == 0)
        _exit(code);
    return pid;
}

static void test_wait_wrappers(void)
{
    int status = 0;
    pid_t pid = spawn_exit(7);
    CHECK(pid > 0, "fork child for wait");
    CHECK_RET(wait(&status), pid, "wait returns exited child pid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 7,
          "wait publishes child exit status");
    CHECK_ERR(wait(NULL), ECHILD, "wait with no children returns ECHILD");

    pid = spawn_exit(9);
    CHECK(pid > 0, "fork child for waitpid");
    CHECK_RET(waitpid(pid, &status, 0), pid, "waitpid returns selected child pid");
    CHECK_ERR(waitpid(pid, NULL, 0), ECHILD,
              "waitpid on reaped child returns ECHILD");
    CHECK_ERR(waitpid(-1, NULL, 0), ECHILD,
              "waitpid any with no children returns ECHILD");
    CHECK_ERR(waitpid(1, NULL, 0), ECHILD,
              "waitpid non-child init returns ECHILD");

    struct rusage usage;
    memset(&usage, 0, sizeof(usage));
    CHECK_ERR(syscall(SYS_wait4, 4194305, &status, 0, &usage), ECHILD,
              "wait4 pid above pid_max returns ECHILD");
}

static void test_waitid_selectors(void)
{
    int ready[2];
    CHECK_RET(pipe(ready), 0, "create waitid readiness pipe");
    pid_t pid = fork();
    if (pid == 0) {
        close(ready[0]);
        char byte = 'R';
        write(ready[1], &byte, 1);
        pause();
        _exit(0);
    }
    CHECK(pid > 0, "fork running child for waitid selectors");
    close(ready[1]);
    char byte;
    CHECK_RET(read(ready[0], &byte, 1), 1, "wait for child readiness");
    close(ready[0]);

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid_raw(P_PID, pid + 1, &info, WEXITED), ECHILD,
              "waitid child+1 returns ECHILD");
    CHECK_ERR(waitid_raw(P_PGID, pid + 1, &info, WEXITED), ECHILD,
              "waitid absent process group returns ECHILD");
    memset(&info, 0x5a, sizeof(info));
    CHECK_RET(waitid_raw(P_ALL, pid, &info, WNOHANG | WEXITED), 0,
              "waitid P_ALL WNOHANG succeeds while child runs");
    CHECK(info.si_pid == 0, "waitid WNOHANG clears si_pid when no event is ready");

    kill(pid, SIGKILL);
    CHECK_RET(waitpid(pid, NULL, 0), pid, "reap selector child");
    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid_raw(P_ALL, 0, &info, WNOHANG | WEXITED), ECHILD,
              "waitid P_ALL with no children returns ECHILD");
}

static void test_waitid_job_control(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        for (;;)
            pause();
    }
    CHECK(pid > 0, "fork child for waitid job control");
    CHECK_RET(kill(pid, SIGSTOP), 0, "stop waitid child");

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid_event(pid, &info, WSTOPPED | WNOWAIT), 0,
              "waitid WSTOPPED|WNOWAIT observes stop");
    CHECK(info.si_pid == pid, "WNOWAIT stop reports selected child");
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid_event(pid, &info, WSTOPPED), 0,
              "waitid WSTOPPED consumes retained stop");

    CHECK_RET(kill(pid, SIGCONT), 0, "continue waitid child");
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid_event(pid, &info, WCONTINUED), 0,
              "waitid WCONTINUED observes continue event");

    kill(pid, SIGKILL);
    CHECK_RET(waitpid(pid, NULL, 0), pid, "reap job-control child");
}

int main(void)
{
    TEST_START("SyscallGuard final wait wait4 waitid waitpid behavior");
    test_wait_wrappers();
    test_waitid_selectors();
    test_waitid_job_control();
    TEST_DONE();
}
