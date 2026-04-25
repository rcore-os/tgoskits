#define _GNU_SOURCE
#include "test_framework.h"
#include <unistd.h>
#include <sys/wait.h>
#include <errno.h>
#include <signal.h>

static void test_getsid_getpgid_basic(void)
{
    printf("--- getsid/getpgid 基础 ---\n");

    {
        pid_t sid = getsid(0);
        CHECK(sid > 0, "getsid(0) 返回正值");
    }

    {
        pid_t pgid = getpgid(0);
        CHECK(pgid > 0, "getpgid(0) 返回正值");
    }

    {
        pid_t sid0 = getsid(0);
        pid_t sid_self = getsid(getpid());
        CHECK(sid0 == sid_self, "getsid(0) == getsid(getpid())");
    }

    {
        pid_t pgid0 = getpgid(0);
        pid_t pgid_self = getpgid(getpid());
        CHECK(pgid0 == pgid_self, "getpgid(0) == getpgid(getpid())");
    }
}

static void test_setpgid(void)
{
    printf("--- setpgid ---\n");

    {
        pid_t orig_pgid = getpgid(0);
        CHECK_RET(setpgid(0, 0), 0, "setpgid(0, 0) 成功");
        pid_t new_pgid = getpgid(0);
        CHECK(new_pgid == getpid(), "setpgid(0,0) 后 pgid == pid");
        if (orig_pgid != getpid()) {
            setpgid(0, orig_pgid);
        }
    }

    {
        CHECK_RET(setpgid(getpid(), getpid()), 0, "setpgid(pid, pid) 成功");
        CHECK(getpgid(0) == getpid(), "setpgid(pid,pid) 后 pgid == pid");
    }

    {
        pid_t pid = fork();
        if (pid == 0) {
            usleep(100000);
            _exit(0);
        }
        CHECK_RET(setpgid(pid, pid), 0, "父进程 setpgid(子pid, 子pid) 成功");
        CHECK(getpgid(pid) == pid, "子进程 pgid == 子进程 pid");
        waitpid(pid, NULL, 0);
    }

    CHECK_ERR(setpgid(999999, 0), ESRCH, "setpgid 不存在 PID -> ESRCH");
    CHECK_ERR(setpgid(0, 999999), ESRCH, "setpgid 不存在 pgid -> ESRCH");

    /* setpgid 将子进程移入已有进程组 */
    {
        pid_t child1 = fork();
        if (child1 == 0) {
            usleep(200000);
            _exit(0);
        }
        pid_t child2 = fork();
        if (child2 == 0) {
            usleep(200000);
            _exit(0);
        }
        /* child1 创建自己的组 */
        setpgid(child1, child1);
        /* child2 移入 child1 的组 */
        CHECK_RET(setpgid(child2, child1), 0, "setpgid 将进程移入已有组成功");
        CHECK(getpgid(child2) == child1, "移入后 pgid == child1 的 pgid");
        waitpid(child1, NULL, 0);
        waitpid(child2, NULL, 0);
    }
}

static void test_setsid(void)
{
    printf("--- setsid ---\n");

    {
        pid_t pid = fork();
        if (pid == 0) {
            pid_t old_sid = getsid(0);
            pid_t new_sid = setsid();
            if (new_sid == (pid_t)-1) {
                printf("  FAIL | setsid 在子进程失败 errno=%d\n", errno);
                _exit(1);
            }
            if (new_sid != getpid()) {
                printf("  FAIL | setsid 返回值 != pid\n");
                _exit(1);
            }
            if (new_sid == old_sid) {
                printf("  FAIL | 新 sid == 旧 sid\n");
                _exit(1);
            }
            if (getsid(0) != new_sid) {
                printf("  FAIL | getsid(0) != new_sid\n");
                _exit(1);
            }
            if (getpgid(0) != getpid()) {
                printf("  FAIL | setsid 后 pgid != pid\n");
                _exit(1);
            }
            _exit(0);
        }
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "setsid 子进程全部检查通过");
    }

    {
        pid_t pid = fork();
        if (pid == 0) {
            setsid();
            errno = 0;
            pid_t r = setsid();
            if (r == -1 && errno == EPERM) {
                _exit(0);
            }
            _exit(1);
        }
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "setsid 重复调用 -> EPERM");
    }

    /* init 进程(进程组组长)调用 setsid -> EPERM */
    {
        errno = 0;
        pid_t r = setsid();
        CHECK(r == -1 && errno == EPERM, "进程组组长 setsid -> EPERM");
    }
}

static void test_cross_session(void)
{
    printf("--- 跨 session 操作 ---\n");

    {
        pid_t pid = fork();
        if (pid == 0) {
            setsid();
            usleep(200000);
            _exit(0);
        }
        usleep(50000);
        errno = 0;
        int r = setpgid(pid, getpgid(0));
        CHECK(r == -1 && errno == EPERM, "跨 session setpgid -> EPERM");
        waitpid(pid, NULL, 0);
    }

    CHECK_ERR(getsid(999999), ESRCH, "getsid 不存在 PID -> ESRCH");
    CHECK_ERR(getpgid(999999), ESRCH, "getpgid 不存在 PID -> ESRCH");
}

int main(void)
{
    TEST_START("session-syscalls");

    test_getsid_getpgid_basic();
    test_setpgid();
    test_setsid();
    test_cross_session();

    TEST_DONE();
}
