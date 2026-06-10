#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* setresgid(2) — set real, effective, saved group IDs. Analogous to setresuid. */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void setresgid_all_nochg(void)
{
    gid_t r0, e0, s0;
    getresgid(&r0, &e0, &s0);
    int rc = setresgid((gid_t)-1, (gid_t)-1, (gid_t)-1);
    CHECK(rc == 0, "setresgid (a) (-1,-1,-1) -> 0");
    gid_t r1, e1, s1;
    getresgid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1, "setresgid (a) cred unchanged");
}

static void setresgid_self_self_self(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 setresuid (b) idempotent —
     * 三参数 = 当前 cred 总是允许. 验 starry !has_cap_setgid in_set 正向. */
    gid_t r, e, s;
    getresgid(&r, &e, &s);
    int rc = setresgid(r, e, s);
    CHECK(rc == 0, "setresgid (b) (r,e,s) idempotent");
}

static void setresgid_root_arbitrary_three_values(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 setresuid (c) — root (CAP_SETGID) 三参数
     * 独立 set 任意值. 验 starry has_cap_setgid 路径 r/e/s 各自精确 set. */
    if (getuid() != 0) {
        printf("  setresgid (c) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(2000, 3000, 4000) != 0) _exit(1);
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 3000 && s == 4000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresgid (c) root setresgid(2000,3000,4000) → r=2000 e=3000 s=4000");
        }
    }
}

static void setresgid_unpriv_within_set_ok(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 setresuid (d) — unpriv 三参数在
     * {old.r, old.e, old.s} 集合内 → 允许. 验 starry in_set 正向 case. */
    if (getuid() != 0) {
        printf("  setresgid (d) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 切 gid 集 + uid（避免后续权限问题）*/
        if (setresgid(1000, 2000, 3000) != 0) _exit(99);
        if (setresuid(1000, 1000, 1000) != 0) _exit(98);
        /* 现在尝试切到集合内不同序：r=2000 e=1000 s=3000 */
        if (setresgid(2000, 1000, 3000) != 0) _exit(1);
        gid_t r, e, s;
        if (getresgid(&r, &e, &s) != 0) _exit(2);
        if (r == 2000 && e == 1000 && s == 3000) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresgid (d) unpriv setresgid within {r,e,s} set → ok");
        }
    }
}

static void setresgid_unpriv_outside_set_eperm(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 setresuid (e) — unpriv 任一参数越界
     * → EPERM. 验 starry in_set 负向 case. */
    if (getuid() != 0) {
        printf("  setresgid (e) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresgid(1000, 1000, 1000) != 0) _exit(99);
        if (setresuid(1000, 1000, 1000) != 0) _exit(98);
        errno = 0;
        int rc = setresgid(2000, (gid_t)-1, (gid_t)-1);
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "setresgid (e) unpriv setresgid(outside_set,-1,-1) → -1 EPERM");
        }
    }
}

static void setresgid_raw_matches_libc(void)
{
    /* 测什么/怎么测/期望/为什么: 镜像 setresuid (f) — libc 应直接转发 syscall.
     * raw NOCHG×3 success. 验 ABI 契约. */
    long rc = syscall(SYS_setresgid, (gid_t)-1, (gid_t)-1, (gid_t)-1);
    CHECK(rc == 0, "setresgid (f) raw syscall(NOCHG×3) -> 0");
}

int setresgid_run(void)
{
    printf("\n----- setresgid -----\n");
    setresgid_all_nochg();
    setresgid_self_self_self();
    setresgid_root_arbitrary_three_values();
    setresgid_unpriv_within_set_ok();
    setresgid_unpriv_outside_set_eperm();
    setresgid_raw_matches_libc();
    printf("  ----- setresgid: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
