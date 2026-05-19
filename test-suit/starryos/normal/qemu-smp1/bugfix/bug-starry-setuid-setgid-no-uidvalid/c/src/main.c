/*
 * bug-starry-setuid-setgid-no-uidvalid
 *
 * 现象: starry kernel 在 setuid((uid_t)-1) / setgid((gid_t)-1) 时
 *       直接接受并设置 r/e/s = 0xFFFFFFFF；Linux 应返 -1 EINVAL。
 *
 * Linux man 2 setuid §"ERRORS":
 *   "EINVAL — The user ID specified in uid is not valid in this user namespace."
 *
 * Linux kernel/cred.c sys_setuid:
 *   kuid = make_kuid(ns, uid);
 *   if (!uid_valid(kuid))
 *       return -EINVAL;
 *
 * uid_valid 拒绝 __kuid_val(uid) == (uid_t)-1。
 *
 * starry: os/StarryOS/kernel/src/syscall/sys.rs::sys_setuid 无 uid_valid 检查；
 *         若 has_cap_setuid → 直接 new.uid = new.euid = new.suid = uid (含 -1)。
 *         这违反 Linux/POSIX 约定，且使 (uid_t)-1 在后续 setresuid 中无法作
 *         NOCHG sentinel 区分（NOCHG 也是 -1）。
 *
 * 影响: 与 setresuid/setresgid 的 -1 NOCHG 语义冲突；用户态可能因 -1 被
 *       cred 化而出现安全错配（rare 但严重）。
 *
 * 触发条件: starry kernel 任何版本；以 root 调用 setuid(0xFFFFFFFF)
 *           或 setgid(0xFFFFFFFF)。
 *
 * 期望修复方向: 在 sys_setuid/setgid/setresuid/setresgid 入口校验
 *               (uid_t)-1 → 返 EINVAL（除 setres* 的 NOCHG 路径）。
 */

#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int run_setuid_minus_one(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        errno = 0;
        int rc = setuid((uid_t)-1);
        int err = errno;
        printf("  setuid((uid_t)-1) returned rc=%d errno=%d (%s)\n",
               rc, err, strerror(err));
        printf("  expected: rc=-1, errno=EINVAL (%d)\n", EINVAL);
        if (rc == -1 && err == EINVAL) {
            printf("  → behavior matches Linux: PASS\n");
            _exit(0);
        }
        if (rc == 0) {
            uid_t r, e, s;
            getresuid(&r, &e, &s);
            printf("  → starry accepted -1, now r=%u e=%u s=%u (cred poisoned)\n",
                   r, e, s);
            _exit(1);
        }
        printf("  → unexpected rc/errno combo\n");
        _exit(2);
    }
    int status;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

static int run_setgid_minus_one(void)
{
    pid_t pid = fork();
    if (pid == 0) {
        errno = 0;
        int rc = setgid((gid_t)-1);
        int err = errno;
        printf("  setgid((gid_t)-1) returned rc=%d errno=%d (%s)\n",
               rc, err, strerror(err));
        printf("  expected: rc=-1, errno=EINVAL (%d)\n", EINVAL);
        if (rc == -1 && err == EINVAL) {
            printf("  → behavior matches Linux: PASS\n");
            _exit(0);
        }
        if (rc == 0) {
            gid_t r, e, s;
            getresgid(&r, &e, &s);
            printf("  → starry accepted -1, now r=%u e=%u s=%u (cred poisoned)\n",
                   r, e, s);
            _exit(1);
        }
        printf("  → unexpected rc/errno combo\n");
        _exit(2);
    }
    int status;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

int main(void)
{
    printf("=== bug-starry-setuid-setgid-no-uidvalid ===\n");
    printf("Linux 拒绝 setuid/setgid 接受 (uid_t)-1 = 0xFFFFFFFF, 应返 EINVAL\n");
    printf("starry 缺 uid_valid 检查 → 直接接受 (cred 被污染)\n\n");

    if (getuid() != 0) {
        printf("SKIP: needs root\n");
        return 0;
    }

    int u = run_setuid_minus_one();
    int g = run_setgid_minus_one();

    if (u == 0 && g == 0) {
        printf("\nTEST PASSED (bug fixed)\n");
        return 0;
    }
    printf("\nTEST FAILED — starry behavior diverges from Linux on uid_valid check\n");
    return 1;
}
