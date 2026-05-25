#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <sched.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setgroups(2) — set supplementary group IDs.
 *
 * man 2 setgroups:
 *   "setgroups() sets the supplementary group IDs for the calling process."
 *   "Appropriate privileges (Linux: the CAP_SETGID capability in the user
 *    namespace containing the process's effective user ID) are required."
 *
 * 测试覆盖：
 *   (a) root setgroups(0, NULL) — 清空所有 supp groups
 *   (b) root setgroups(N, list) — 设置 N 个 groups，getgroups 验内容
 *   (c) root setgroups(N, list) with duplicates — 应被接受（kernel 不强制去重）
 *   (d) unpriv setgroups(N, list) — -1 EPERM (cap-based)
 *   (e) raw vs libc
 *   (f) Linux 3.19+ user-namespace + /proc/[pid]/setgroups=deny → setgroups EPERM
 */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setgroups_root_clear_all(void)
{
    /* 测什么: man §DESCRIPTION — "A process can drop all of its supplementary
     *         groups with the call: setgroups(0, NULL)".
     * 怎么测: root fork → child setgroups(0, NULL) → getgroups(0,NULL) 应返 0.
     * 期望:   ngroups == 0.
     * 为什么: 验证 starry sys_setgroups(size=0) 路径正确清空 cred.groups. */
    if (getuid() != 0) {
        printf("  setgroups (a) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 清空 */
        if (setgroups(0, NULL) != 0) _exit(1);
        int n = getgroups(0, NULL);
        if (n == 0) _exit(0);
        _exit(2);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgroups (a) root setgroups(0, NULL) -> ngroups=0");
        }
    }
}

static void setgroups_root_set_n_and_verify(void)
{
    /* 测什么: man §DESCRIPTION — setgroups 设 sup groups; getgroups 读回.
     *         隐含 round-trip 一致性 (set 啥 → get 啥, 同序).
     * 怎么测: root fork → child setgroups(3, {100,200,300}) → getgroups → 验三值同序.
     * 期望:   n=3, got[0..2] = {100,200,300}.
     * 为什么: 验证 starry sys_setgroups vm_read_slice + cred.groups 写 +
     *         sys_getgroups vm_write_slice 读 全链 round-trip 正确. */
    if (getuid() != 0) {
        printf("  setgroups (b) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t set[] = {100, 200, 300};
        if (setgroups(3, set) != 0) _exit(1);
        gid_t got[8] = {0};
        int n = getgroups(8, got);
        if (n != 3) _exit(2);
        if (got[0] != 100 || got[1] != 200 || got[2] != 300) _exit(3);
        _exit(0);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgroups (b) root setgroups(3, {100,200,300}) → getgroups returns same");
        }
    }
}

static void setgroups_root_with_duplicates(void)
{
    /* 测什么: man 未明文 dedup; Linux 接受重复但 starry 行为 unspecified.
     * 怎么测: root fork → child setgroups(4, {100,100,200,100}) — 含重复.
     * 期望:   rc=0, 后续 ngroups >= 2 (容忍 dedup 或保留原始 4).
     * 为什么: 验证 starry sys_setgroups 不 crash / 不拒绝重复; 实际 ngroups
     *         由实现决定 (Linux 不 dedup → 4, starry 可能 dedup → 2). */
    if (getuid() != 0) {
        printf("  setgroups (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* man 不强制 unique；kernel 接受重复 */
        gid_t set[] = {100, 100, 200, 100};
        if (setgroups(4, set) != 0) _exit(1);
        int n = getgroups(0, NULL);
        /* 期望 4 或 dedup 后 2 — 不强求；只验非负 */
        if (n >= 2) _exit(0);
        _exit(2);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgroups (c) root setgroups with duplicates accepted (n>=2)");
        }
    }
}

static void setgroups_unpriv_eperm(void)
{
    /* 测什么: man §ERRORS — EPERM "calling process has insufficient privilege
     *         (does not have CAP_SETGID)".
     * 怎么测: root fork → child setresuid(1000,1000,1000) 清 caps → setgroups → EPERM.
     * 期望:   rc=-1 errno=EPERM.
     * 为什么: 验证 starry sys_setgroups 的 has_cap_setgid 拒非特权调用. */
    if (getuid() != 0) {
        printf("  setgroups (d) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        gid_t set[] = {100};
        errno = 0;
        int rc = setgroups(1, set);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgroups (d) unpriv setgroups -> -1 EPERM");
        }
    }
}

static void setgroups_raw_matches_libc(void)
{
    if (getuid() != 0) {
        printf("  setgroups (e) skip: requires root to call setgroups\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        gid_t set[] = {500};
        long rc = syscall(SYS_setgroups, 1, set);
        if (rc != 0) _exit(1);
        int n = getgroups(0, NULL);
        if (n == 1) _exit(0);
        _exit(2);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setgroups (e) raw syscall(1, {500}) -> 0 and getgroups returns 1");
        }
    }
}

static void setgroups_user_ns_deny_eperm(void)
{
    /* 测什么: man §ERRORS — "The calling process is in a user namespace and the
     *         /proc/[pid]/setgroups file has been set to 'deny'" (Linux 3.19+).
     *         user namespace 内写 deny 后, setgroups 必须返 EPERM, 与
     *         capability-based EPERM (case d) 是**独立的** EPERM 路径.
     * 怎么测: fork → child unshare(CLONE_NEWUSER) 创建 user-ns →
     *         write "deny" 到 /proc/self/setgroups → setgroups([7,8,9])
     *         → 期望 -1 EPERM.
     * 期望:   rc=-1 errno=EPERM (host gcc 实测验证 PASS).
     * 为什么: 验证 starry 是否支持 user-namespace + /proc/setgroups deny 语义.
     *         starry 若缺 user-ns (unshare ENOSYS) → child _exit(77) skip;
     *         若有 user-ns 但缺 /proc/setgroups → _exit(96/95) FAIL 显式暴露漏项;
     *         若全支持但 EPERM 路径未挂 → _exit(1) FAIL 暴露 deny 失效. */
    pid_t pid = fork();
    if (pid == 0) {
        if (unshare(CLONE_NEWUSER) != 0) {
            _exit(77); /* skip — user-ns not available */
        }
        int fd = open("/proc/self/setgroups", O_WRONLY);
        if (fd < 0) _exit(96);
        if (write(fd, "deny", 4) != 4) {
            close(fd);
            _exit(95);
        }
        close(fd);
        gid_t g[3] = {7, 8, 9};
        errno = 0;
        int rc = setgroups(3, g);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            int code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
            if (code == 77) {
                printf("  setgroups (f) skip: user namespace not supported\n");
                return;
            }
            CHECK(WIFEXITED(status) && code == 0,
                  "setgroups (f) user-ns + /proc/self/setgroups=deny → setgroups EPERM");
        }
    }
}

int setgroups_run(void)
{
    printf("\n----- setgroups -----\n");
    setgroups_root_clear_all();
    setgroups_root_set_n_and_verify();
    setgroups_root_with_duplicates();
    setgroups_unpriv_eperm();
    setgroups_raw_matches_libc();
    setgroups_user_ns_deny_eperm();
    printf("  ----- setgroups: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
