#define _GNU_SOURCE
/*
 * bug-setns-requires-cap-sys-admin — setns(2) must require CAP_SYS_ADMIN.
 *
 * ground truth: Linux install hooks (kernel/utsname.c:132, ipc/namespace.c:237,
 * net/core/net_namespace.c:1534, kernel/user_namespace.c:1361) each require
 * ns_capable(..., CAP_SYS_ADMIN) to join a namespace; without it setns returns
 * EPERM. StarryOS only gated CLONE_NEWCGROUP, so an unprivileged process could
 * join uts/ipc/mnt/pid/net/user namespaces (CWE-269/862).
 */
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <unistd.h>

static int failed;
static void check(int cond, const char *msg)
{
    if (cond) {
        printf("  PASS | %s\n", msg);
    } else {
        printf("  FAIL | %s | errno=%d (%s)\n", msg, errno, strerror(errno));
        failed = 1;
    }
}

int main(void)
{
    check(getuid() == 0, "测试前置: 以 root 启动 (才能验掉权后的拒绝路径)");

    int nsfd = open("/proc/self/ns/uts", O_RDONLY);
    check(nsfd >= 0, "open /proc/self/ns/uts");
    if (nsfd < 0) {
        printf("=== bug-setns-requires-cap-sys-admin: FAIL ===\n");
        return 1;
    }

    /* root holds CAP_SYS_ADMIN: joining the (own) uts namespace is allowed. */
    errno = 0;
    check(setns(nsfd, CLONE_NEWUTS) == 0, "root(有 CAP_SYS_ADMIN) setns(UTS) 允许");

    /* drop to an unprivileged uid; StarryOS clears the effective cap set. */
    check(setuid(1000) == 0, "setuid(1000) 掉特权");
    check(geteuid() == 1000, "euid 变为 1000");

    /* without CAP_SYS_ADMIN, setns must be refused with EPERM (was allowed). */
    errno = 0;
    int r = setns(nsfd, CLONE_NEWUTS);
    check(r == -1 && errno == EPERM,
          "非特权 setns(UTS) 被拒 EPERM (缺 CAP_SYS_ADMIN)");

    close(nsfd);
    printf("=== bug-setns-requires-cap-sys-admin: %s ===\n", failed ? "FAIL" : "PASS");
    return failed;
}
