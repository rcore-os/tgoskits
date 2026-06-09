#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* boundary — setresuid/setresgid 边界 + raw syscall 直传 NOCHG sentinel. */

static int waitpid_safely(pid_t pid, int *status)
{
    pid_t r = waitpid(pid, status, 0);
    return r == pid ? 0 : -1;
}

static void boundary_raw_all_u32max(void)
{
    /* raw syscall 直传 u32::MAX × 3 — kernel 应识别 NOCHG */
    uid_t r0, e0, s0;
    getresuid(&r0, &e0, &s0);
    long rc = syscall(SYS_setresuid, (uid_t)-1, (uid_t)-1, (uid_t)-1);
    CHECK(rc == 0,                                                "boundary (a) raw setresuid(u32::MAX×3) -> 0");
    uid_t r1, e1, s1;
    getresuid(&r1, &e1, &s1);
    CHECK(r0 == r1 && e0 == e1 && s0 == s1,                       "boundary (a) cred unchanged after NOCHG×3");
}

static void boundary_root_extreme_uids(void)
{
    if (getuid() != 0) {
        printf("  boundary (b) skip: not root\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        /* 极大值 + root → 应接受 */
        if (setresuid(0xFFFFFFFE, 0xFFFFFFFD, 0xFFFFFFFC) != 0) _exit(1);
        uid_t r, e, s;
        if (getresuid(&r, &e, &s) != 0) _exit(2);
        if (r == 0xFFFFFFFE && e == 0xFFFFFFFD && s == 0xFFFFFFFC) _exit(0);
        _exit(3);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (b) root setresuid(u32::MAX-1, -2, -3) accepted");
        }
    }
}

static void boundary_unpriv_one_outside_eperm(void)
{
    /* 集合 {1000}；setresuid(1000, 2000, 1000) — euid 不在 → EPERM */
    if (getuid() != 0) {
        printf("  boundary (c) skip\n");
        return;
    }
    pid_t pid = fork();
    if (pid == 0) {
        if (setresuid(1000, 1000, 1000) != 0) _exit(99);
        errno = 0;
        int rc = setresuid(1000, 2000, 1000);  /* euid=2000 不在 {1000} */
        if (rc == -1 && errno == EPERM) _exit(0);
        _exit(1);
    }
    if (pid > 0) {
        int status;
        if (waitpid_safely(pid, &status) == 0) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
                  "boundary (c) unpriv: 任一 ID 不在集合内 → -1 EPERM");
        }
    }
}

int boundary_run(void)
{
    printf("\n----- boundary -----\n");
    boundary_raw_all_u32max();
    boundary_root_extreme_uids();
    boundary_unpriv_one_outside_eperm();
    printf("  ----- boundary: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
