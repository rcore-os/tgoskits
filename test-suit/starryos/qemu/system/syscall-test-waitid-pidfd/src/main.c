#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h>"
#endif

#ifndef P_PIDFD
#define P_PIDFD 3
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static void expect_waitpid_echild(pid_t pid, const char *msg)
{
    int status = 0;
    errno = 0;
    pid_t waited = waitpid(pid, &status, WNOHANG);
    CHECK(waited == -1 && errno == ECHILD, msg);
}

static void expect_sigchld_exit(const siginfo_t *info, pid_t pid, int status,
                                const char *msg)
{
    CHECK(info->si_pid == pid, msg);
    CHECK(info->si_code == CLD_EXITED, "waitid reports CLD_EXITED");
    CHECK(info->si_status == status, "waitid reports child exit status");
}

static pid_t fork_exit_child(int status)
{
    pid_t pid = fork();
    CHECK(pid >= 0, "fork child");
    if (pid == 0) {
        _exit(status);
    }
    return pid;
}

static void test_waitid_pidfd_reaps_child(void)
{
    printf("--- waitid(P_PIDFD) reaps exited child ---\n");

    pid_t child = fork_exit_child(42);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) succeeds");
    if (pfd < 0) {
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED), 0,
              "waitid(P_PIDFD, WEXITED) succeeds");
    expect_sigchld_exit(&info, child, 42, "waitid reports the pidfd child");
    expect_waitpid_echild(child, "pidfd waitid consumes the child zombie");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_waitid_pidfd_wnowait_keeps_child_waitable(void)
{
    printf("--- waitid(P_PIDFD) WNOWAIT keeps zombie waitable ---\n");

    pid_t child = fork_exit_child(7);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(child) succeeds");
    if (pfd < 0) {
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOWAIT), 0,
              "waitid(P_PIDFD, WNOWAIT) observes child");
    expect_sigchld_exit(&info, child, 7, "WNOWAIT reports the pidfd child");

    int status = 0;
    CHECK_RET(waitpid(child, &status, 0), child,
              "waitpid can still reap after WNOWAIT");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 7,
          "waitpid sees original exit status");

    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOHANG), ECHILD,
              "pidfd waitid after reap returns ECHILD");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_waitid_pidfd_nohang_alive_child(void)
{
    printf("--- waitid(P_PIDFD) WNOHANG for live child ---\n");

    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "create child sync pipe");

    pid_t child = fork();
    CHECK(child >= 0, "fork blocking child");
    if (child == 0) {
        close(pipefd[1]);
        char byte = 0;
        (void)read(pipefd[0], &byte, 1);
        close(pipefd[0]);
        _exit(5);
    }

    close(pipefd[0]);
    int pfd = x_pidfd_open(child, 0);
    CHECK(pfd >= 0, "pidfd_open(live child) succeeds");
    if (pfd < 0) {
        close(pipefd[1]);
        (void)waitpid(child, NULL, 0);
        return;
    }

    siginfo_t info;
    memset(&info, 0xff, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED | WNOHANG), 0,
              "waitid(P_PIDFD, WNOHANG) succeeds for live child");
    CHECK(info.si_pid == 0, "WNOHANG clears siginfo when child is not waitable");

    CHECK_RET(write(pipefd[1], "x", 1), 1, "release child");
    close(pipefd[1]);

    memset(&info, 0, sizeof(info));
    CHECK_RET(waitid(P_PIDFD, (id_t)pfd, &info, WEXITED), 0,
              "waitid(P_PIDFD) reaps released child");
    expect_sigchld_exit(&info, child, 5, "waitid reports released child");

    CHECK_RET(close(pfd), 0, "close pidfd");
}

static void test_waitid_pidfd_errors(void)
{
    printf("--- waitid(P_PIDFD) error paths ---\n");

    siginfo_t info;
    memset(&info, 0, sizeof(info));
    CHECK_ERR(waitid(P_PIDFD, -1, &info, WEXITED | WNOHANG), EINVAL,
              "negative pidfd returns EINVAL");

    int pipefd[2];
    CHECK_RET(pipe(pipefd), 0, "create non-pidfd pipe");
    CHECK_ERR(waitid(P_PIDFD, (id_t)pipefd[0], &info, WEXITED | WNOHANG),
              EBADF, "non-pidfd file descriptor returns EBADF");
    close(pipefd[0]);
    close(pipefd[1]);

    int self_pfd = x_pidfd_open(getpid(), 0);
    CHECK(self_pfd >= 0, "pidfd_open(self) succeeds");
    if (self_pfd >= 0) {
        CHECK_ERR(waitid(P_PIDFD, (id_t)self_pfd, &info, WEXITED | WNOHANG),
                  ECHILD, "pidfd for non-child process returns ECHILD");
        close(self_pfd);
    }
}

int main(void)
{
    TEST_START("waitid P_PIDFD");
    test_waitid_pidfd_reaps_child();
    test_waitid_pidfd_wnowait_keeps_child_waitable();
    test_waitid_pidfd_nohang_alive_child();
    test_waitid_pidfd_errors();
    TEST_DONE();
}
