/* matrix.c — 完备测试矩阵 setresuid/setresgid (man-first design)
 *
 * 参 notes/22-res-setter-test-design.md
 *
 * man 2 setresuid 关键子句：
 *   [RS-D1] sets r/e/s 三 IDs
 *   [RS-D2] unpriv: each non-NOCHG value must ∈ {old.r, old.e, old.s}
 *   [RS-D3] priv: arbitrary
 *   [RS-D4] -1 = NOCHG
 *   [RS-D5] fsuid 无条件跟 new euid
 *   [RS-E3] EPERM if violation
 *
 * starry 实现 (sys.rs:58-148) 完全贴合 Linux 含 RS-D5。
 *
 * 矩阵：
 *   5 ruid × 5 euid × 5 suid × 6 state × 2 mode × 2 syscall = 3000 case
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define NOCHG 0xFFFFFFFFu

static const uint32_t INPUT[] = {
    NOCHG, 0u, 1000u, 2000u, 0x7FFFFFFFu,
};
#define N_IN (sizeof(INPUT)/sizeof(INPUT[0]))

typedef enum {
    S_ROOT_UNMOD = 0,
    S_ROOT_AFTER_SETUID_1000,
    S_ROOT_AFTER_SETRESUID_1K_2K_3K,
    S_NONROOT_FULL_1000,
    S_NONROOT_R0_E1000_S2000,
    S_AFTER_SETREUID_1K_2K,
    S_STATE_COUNT
} cred_state_t;

static const char *state_name(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return "root_unmod";
    case S_ROOT_AFTER_SETUID_1000:           return "setuid(1k)[caps_drop]";
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:    return "setresuid(1k,2k,3k)";
    case S_NONROOT_FULL_1000:                return "nonroot(1k)";
    case S_NONROOT_R0_E1000_S2000:           return "setresuid(0,1k,2k)";
    case S_AFTER_SETREUID_1K_2K:             return "setreuid(1k,2k)";
    default:                                 return "?";
    }
}

static int setup_state(cred_state_t s, bool for_gid)
{
    switch (s) {
    case S_ROOT_UNMOD:
        return (getuid() == 0) ? 0 : -100;
    case S_ROOT_AFTER_SETUID_1000:
        if (getuid() != 0) return -100;
        if (for_gid) { if (setresgid(1000, 1000, 1000) != 0) return -101; }
        return setuid(1000) == 0 ? 0 : -102;
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:
        if (getuid() != 0) return -100;
        if (for_gid) return setresgid(1000, 2000, 3000) == 0 ? 0 : -103;
        return setresuid(1000, 2000, 3000) == 0 ? 0 : -104;
    case S_NONROOT_FULL_1000:
        if (getuid() == 0) {
            if (setresgid(1000, 1000, 1000) != 0) return -105;
            if (setresuid(1000, 1000, 1000) != 0) return -106;
        }
        return 0;
    case S_NONROOT_R0_E1000_S2000:
        if (getuid() != 0) return -100;
        if (for_gid) return setresgid(0, 1000, 2000) == 0 ? 0 : -107;
        return setresuid(0, 1000, 2000) == 0 ? 0 : -108;
    case S_AFTER_SETREUID_1K_2K:
        if (getuid() != 0) return -100;
        if (for_gid) return setregid(1000, 2000) == 0 ? 0 : -109;
        return setreuid(1000, 2000) == 0 ? 0 : -110;
    default:
        return -200;
    }
}

typedef struct { uint32_t r, e, s; } cred_t;
static int read_uid(cred_t *c) {
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}
static int read_gid(cred_t *c) {
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}

typedef struct { int rc; int err; uint32_t nr, ne, ns; } expected_t;

static bool in_set(uint32_t v, uint32_t a, uint32_t b, uint32_t c)
{
    return v == a || v == b || v == c;
}

static expected_t derive(cred_t pre, uint32_t euid_caller,
                          uint32_t rin, uint32_t ein, uint32_t sin)
{
    bool has_cap = (euid_caller == 0);
    uint32_t nr = pre.r, ne = pre.e, ns = pre.s;
    if (has_cap) {
        if (rin != NOCHG) nr = rin;
        if (ein != NOCHG) ne = ein;
        if (sin != NOCHG) ns = sin;
    } else {
        if (rin != NOCHG) {
            if (!in_set(rin, pre.r, pre.e, pre.s))
                return (expected_t){-1, EPERM, 0, 0, 0};
            nr = rin;
        }
        if (ein != NOCHG) {
            if (!in_set(ein, pre.r, pre.e, pre.s))
                return (expected_t){-1, EPERM, 0, 0, 0};
            ne = ein;
        }
        if (sin != NOCHG) {
            if (!in_set(sin, pre.r, pre.e, pre.s))
                return (expected_t){-1, EPERM, 0, 0, 0};
            ns = sin;
        }
    }
    return (expected_t){0, 0, nr, ne, ns};
}

typedef enum { M_LIBC = 0, M_RAW = 1 } call_mode_t;
typedef enum { SC_SETRESUID = 0, SC_SETRESGID = 1 } syscall_kind_t;

static long do_call(syscall_kind_t sc, call_mode_t m,
                     uint32_t r, uint32_t e, uint32_t s)
{
    if (m == M_LIBC) {
        return (sc == SC_SETRESUID) ? setresuid((uid_t)r, (uid_t)e, (uid_t)s)
                                     : setresgid((gid_t)r, (gid_t)e, (gid_t)s);
    } else {
        long sysn = (sc == SC_SETRESUID) ? SYS_setresuid : SYS_setresgid;
        return syscall(sysn, r, e, s);
    }
}

static int waitpid_safely(pid_t pid, int *st)
{
    return waitpid(pid, st, 0) == pid ? 0 : -1;
}

static void matrix_one(syscall_kind_t sc, cred_state_t s,
                       uint32_t r, uint32_t e, uint32_t sv,
                       call_mode_t m)
{
    pid_t pid = fork();
    if (pid == 0) {
        bool for_gid = (sc == SC_SETRESGID);
        if (setup_state(s, for_gid) != 0) _exit(99);

        cred_t pre;
        if ((sc == SC_SETRESUID ? read_uid(&pre) : read_gid(&pre)) != 0) _exit(98);
        cred_t uc;
        if (read_uid(&uc) != 0) _exit(97);
        uint32_t euid_caller = uc.e;

        expected_t exp = derive(pre, euid_caller, r, e, sv);

        errno = 0;
        long rc = do_call(sc, m, r, e, sv);
        int err = errno;

        if (rc != exp.rc) _exit(11);
        if (exp.rc == -1 && err != exp.err) _exit(12);

        if (exp.rc == 0) {
            cred_t post;
            int prv = (sc == SC_SETRESUID ? read_uid(&post) : read_gid(&post));
            if (prv != 0) _exit(13);
            if (post.r != exp.nr) _exit(14);
            if (post.e != exp.ne) _exit(15);
            if (post.s != exp.ns) _exit(16);
        }
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix: fork/waitpid failed"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[300];
    snprintf(msg, sizeof msg,
             "matrix sc=%s s=%s r=0x%x e=0x%x s=0x%x m=%s",
             sc == SC_SETRESUID ? "setresuid" : "setresgid",
             state_name(s), r, e, sv, m == M_RAW ? "raw" : "libc");
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf[400]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

int matrix_run(void)
{
    printf("\n----- matrix (man-first 完备) -----\n");
    if (getuid() != 0) {
        printf("  matrix: needs root; non-root state subset only\n");
    }
    int total = 0;
    for (int sc = 0; sc < 2; sc++)
      for (int s = 0; s < S_STATE_COUNT; s++)
        for (size_t i = 0; i < N_IN; i++)
          for (size_t j = 0; j < N_IN; j++)
            for (size_t k = 0; k < N_IN; k++)
              for (int m = 0; m < 2; m++) {
                  matrix_one((syscall_kind_t)sc, (cred_state_t)s,
                             INPUT[i], INPUT[j], INPUT[k],
                             (call_mode_t)m);
                  total++;
              }
    printf("  ----- matrix: %d pass, %d fail (out of %d cases) -----\n",
           __pass, __fail, total);
    return __fail;
}
