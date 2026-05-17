#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_pidfd_open
#error "__NR_pidfd_open required from <sys/syscall.h> for this arch/toolchain"
#endif

static int x_pidfd_open(pid_t pid, unsigned int flags)
{
    return (int)syscall(__NR_pidfd_open, pid, flags);
}

static void test_pidfd_open_self(void)
{
    printf("--- pidfd_open 正常路径 ---\n");

    errno = 0;
    int pfd = x_pidfd_open(getpid(), 0);
    CHECK(pfd >= 0, "pidfd_open(getpid(), 0) 返回 fd");
    if (pfd >= 0) {
        CHECK_RET(close(pfd), 0, "close pidfd");
    }
}

static void test_pidfd_open_errors(void)
{
    printf("--- pidfd_open 错误路径 ---\n");

    errno = 0;
    pid_t stale = (pid_t)999999001;
    if (stale <= 0) {
        stale = (pid_t)2147483644;
    }
    int r = x_pidfd_open(stale, 0);
    CHECK(r == -1 && (errno == ESRCH || errno == EINVAL),
          "不存在 pid -> ESRCH 或 EINVAL");

    CHECK_ERR(x_pidfd_open(getpid(), 0xFFFFFFFFu), EINVAL, "非法 flags -> EINVAL");
}

int main(void)
{
    TEST_START("pidfd_open");

    signal(SIGPIPE, SIG_IGN);

    test_pidfd_open_self();
    test_pidfd_open_errors();

    TEST_DONE();
}
