/* matrix.c — 完备测试矩阵 setreuid/setregid (man-first design)
 *
 * 参 notes/21-re-setter-test-design.md
 *
 * man 2 setreuid 关键子句：
 *   [RE-D1] sets r and e
 *   [RE-D2] -1 = NOCHG
 *   [RE-D3] unpriv: euid in {r, e, s}
 *   [RE-D4] unpriv: ruid in {r, e}  (not s!)
 *   [RE-D5] saved rule: if ruid set OR (euid set != prev r) → s = new e
 *   [RE-E3] EPERM if violation
 *
 * starry 实现 (sys.rs:201-281) 完全贴合 Linux semantics 含 RE-D5。
 *
 * 矩阵：
 *   8 ruid × 8 euid × 6 state × 2 mode × 2 syscall = 1536 case
 *   每 case 4-5 assertion ≈ 6000+ PASS
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define NOCHG 0xFFFFFFFFu

/* ── 维度 A: ruid/rgid 输入 (6 值)
 *
 * 减少 8→6 输入 (drop INT32_MAX + UINT32_MAX-1) 以避免 aarch64 starry qemu
 * timeout (1536 case fork-heavy 在 emulated aarch64 上接近 5min 大概率超时).
 * 边界覆盖仍含 NOCHG / root / small / user / 16-bit max — 关键 boundary 保留.
 *
 * 大边界值由 Group D matrix (5 values) 以及 bug-* 复现独立覆盖.
 * ───────────────────────────────────────────────────────────── */
static const uint32_t INPUT_R[] = {
    NOCHG,           /* I-R1  NOCHG (-1) */
    0u,              /* I-R2  root */
    1u,              /* I-R3  small */
    1000u,           /* I-R4  user */
    2000u,           /* I-R5  another */
    65535u,          /* I-R6  16-bit max */
};
#define N_R (sizeof(INPUT_R) / sizeof(INPUT_R[0]))

/* ── 维度 B: euid/egid 输入 (同) ─────────────────────────────── */
#define INPUT_E INPUT_R
#define N_E N_R

/* ── 维度 C: cred 启动 state (6) ─────────────────────────────── */
typedef enum {
    S_ROOT_UNMOD = 0,
    S_ROOT_AFTER_SETUID_1000,           /* caps drop */
    S_ROOT_AFTER_SETRESUID_1K_2K_3K,
    S_NONROOT_FULL_1000,
    S_NONROOT_R0_E1000_S2000,           /* 混杂 (uid: root+setresuid(0,1000,2000)+setuid?) */
    S_AFTER_SETREUID_1K_2K,             /* 用 setreuid 设的初始态 */
    S_STATE_COUNT
} cred_state_t;

static const char *state_name(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return "root_unmod";
    case S_ROOT_AFTER_SETUID_1000:           return "setuid(1000)[caps_drop]";
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:    return "setresuid(1k,2k,3k)";
    case S_NONROOT_FULL_1000:                return "nonroot_full(1k)";
    case S_NONROOT_R0_E1000_S2000:           return "mixed(setresuid 0,1k,2k)";
    case S_AFTER_SETREUID_1K_2K:             return "setreuid(1k,2k)";
    default:                                 return "?";
    }
}

static int setup_state(cred_state_t s, bool for_gid)
{
    /* for_gid: 镜像应用到 gid 路径 (避免 gid 全零导致 derive 失效) */
    switch (s) {
    case S_ROOT_UNMOD:
        return (getuid() == 0) ? 0 : -100;

    case S_ROOT_AFTER_SETUID_1000:
        if (getuid() != 0) return -100;
        if (for_gid) {
            if (setresgid(1000, 1000, 1000) != 0) return -101;
        }
        return setuid(1000) == 0 ? 0 : -102;

    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:
        if (getuid() != 0) return -100;
        if (for_gid) {
            return setresgid(1000, 2000, 3000) == 0 ? 0 : -103;
        }
        return setresuid(1000, 2000, 3000) == 0 ? 0 : -104;

    case S_NONROOT_FULL_1000:
        if (getuid() == 0) {
            if (setresgid(1000, 1000, 1000) != 0) return -105;
            if (setresuid(1000, 1000, 1000) != 0) return -106;
        }
        return 0;

    case S_NONROOT_R0_E1000_S2000:
        if (getuid() != 0) return -100;
        /* 用 setresuid 设 r=0 e=1000 s=2000，但 e=1000 → caps drop
         * 所以此 state 只能用 setresuid 在 root 时 set，setresuid 进入
         * 时 cap 仍在 */
        if (for_gid) {
            return setresgid(0, 1000, 2000) == 0 ? 0 : -107;
        }
        return setresuid(0, 1000, 2000) == 0 ? 0 : -108;

    case S_AFTER_SETREUID_1K_2K:
        if (getuid() != 0) return -100;
        if (for_gid) {
            return setregid(1000, 2000) == 0 ? 0 : -109;
        }
        return setreuid(1000, 2000) == 0 ? 0 : -110;

    default:
        return -200;
    }
}

/* ── cred 读 ──────────────────────────────────────────────────── */
typedef struct { uint32_t r, e, s; } cred_t;

static int read_uid_cred(cred_t *c) {
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}
static int read_gid_cred(cred_t *c) {
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}

/* ── 期望 ─────────────────────────────────────────────────────── */
typedef struct {
    int rc;
    int err;
    uint32_t new_r, new_e, new_s;
} expected_t;

static expected_t derive(cred_t pre, uint32_t euid_caller,
                          uint32_t rin, uint32_t ein)
{
    expected_t exp = {0};
    uint32_t new_r = pre.r, new_e = pre.e, new_s = pre.s;
    bool has_cap = (euid_caller == 0);

    if (has_cap) {
        if (rin != NOCHG) new_r = rin;
        if (ein != NOCHG) new_e = ein;
    } else {
        if (rin != NOCHG) {
            if (rin != pre.r && rin != pre.e) {
                exp.rc = -1; exp.err = EPERM;
                return exp;
            }
            new_r = rin;
        }
        if (ein != NOCHG) {
            if (ein != pre.r && ein != pre.e && ein != pre.s) {
                exp.rc = -1; exp.err = EPERM;
                return exp;
            }
            new_e = ein;
        }
    }

    /* RE-D5 saved rule */
    if (rin != NOCHG || (ein != NOCHG && new_e != pre.r)) {
        new_s = new_e;
    }

    exp.rc = 0;
    exp.new_r = new_r;
    exp.new_e = new_e;
    exp.new_s = new_s;
    return exp;
}

/* ── 模式 / syscall ──────────────────────────────────────────── */
typedef enum { M_LIBC = 0, M_RAW = 1 } call_mode_t;
typedef enum { SC_SETREUID = 0, SC_SETREGID = 1 } syscall_kind_t;

static long do_call(syscall_kind_t sc, call_mode_t m,
                     uint32_t r, uint32_t e)
{
    if (m == M_LIBC) {
        return (sc == SC_SETREUID) ? setreuid((uid_t)r, (uid_t)e)
                                    : setregid((gid_t)r, (gid_t)e);
    } else {
        long sysn = (sc == SC_SETREUID) ? SYS_setreuid : SYS_setregid;
        return syscall(sysn, r, e);
    }
}

/* ── 单 case ──────────────────────────────────────────────────── */
static int waitpid_safely(pid_t pid, int *st)
{
    return waitpid(pid, st, 0) == pid ? 0 : -1;
}

static void matrix_one(syscall_kind_t sc, cred_state_t s,
                       uint32_t rin, uint32_t ein, call_mode_t m)
{
    pid_t pid = fork();
    if (pid == 0) {
        bool for_gid = (sc == SC_SETREGID);
        if (setup_state(s, for_gid) != 0) _exit(99);

        cred_t pre;
        if ((sc == SC_SETREUID ? read_uid_cred(&pre) : read_gid_cred(&pre)) != 0) _exit(98);

        cred_t uid_pre;
        if (read_uid_cred(&uid_pre) != 0) _exit(97);
        uint32_t euid_caller = uid_pre.e;

        expected_t exp = derive(pre, euid_caller, rin, ein);

        errno = 0;
        long rc = do_call(sc, m, rin, ein);
        int err = errno;

        if (rc != exp.rc) _exit(11);
        if (exp.rc == -1 && err != exp.err) _exit(12);

        if (exp.rc == 0) {
            cred_t post;
            int prv = (sc == SC_SETREUID ? read_uid_cred(&post)
                                          : read_gid_cred(&post));
            if (prv != 0) _exit(13);
            if (post.r != exp.new_r) _exit(14);
            if (post.e != exp.new_e) _exit(15);
            if (post.s != exp.new_s) _exit(16);
        }
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix: fork/waitpid failed");
        return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[300];
    snprintf(msg, sizeof msg,
             "matrix sc=%s s=%s r=0x%08x e=0x%08x m=%s",
             sc == SC_SETREUID ? "setreuid" : "setregid",
             state_name(s), rin, ein, m == M_RAW ? "raw" : "libc");
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
        for (size_t i = 0; i < N_R; i++)
          for (size_t j = 0; j < N_E; j++)
            for (int m = 0; m < 2; m++) {
                matrix_one((syscall_kind_t)sc, (cred_state_t)s,
                           INPUT_R[i], INPUT_E[j], (call_mode_t)m);
                total++;
            }
    printf("  ----- matrix: %d pass, %d fail (out of %d cases) -----\n",
           __pass, __fail, total);
    return __fail;
}
