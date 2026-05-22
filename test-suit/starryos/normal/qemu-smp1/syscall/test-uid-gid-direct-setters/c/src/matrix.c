/* matrix.c — 完备测试矩阵 setuid/setgid (man-first design)
 *
 * 参 notes/20-direct-setter-test-design.md
 *
 * man 2 setuid 关键子句：
 *   [D1] setuid sets effective UID
 *   [D2] If CAP_SETUID: also sets real & saved UIDs
 *   [D3] _POSIX_SAVED_IDS — drop & regain via {r, s}
 *   [D4] Root setuid → all 3 IDs set (irreversible after caps drop)
 *   [E3] EINVAL — uid not valid (Linux: uid == (uid_t)-1 → EINVAL via uid_valid)
 *   [E4] EPERM — !CAP + uid not in {r, s}
 *   [N1] fsuid follows euid
 *
 * man 2 setgid 关键子句：
 *   [DG1] sets egid
 *   [DG2] CAP_SETGID: also r/s gid
 *   [EG1] EINVAL — gid == (gid_t)-1
 *   [EG2] EPERM — !CAP_SETGID + gid not in {r, s}
 *   [NG2] CAP_SETGID independent of CAP_SETUID — setgid alone doesn't drop caps
 *
 * starry 实现摘要 (os/StarryOS/kernel/src/syscall/sys.rs + task/cred.rs):
 *   sys_setuid: if has_cap_setuid (=euid==0) then all 3 set; else input must be
 *               in {uid, suid} else EPERM. fsuid <- euid (always).
 *   sys_setgid: mirror, has_cap_setgid (=euid==0). fsgid <- egid.
 *
 * 矩阵：
 *   syscall  × state             × input         × mode
 *   ───────────────────────────────────────────────────
 *   setuid     S_ROOT_UNMOD        I1 (0)          M_LIBC
 *   setuid     S_ROOT_UNMOD        I1 (0)          M_RAW
 *   setuid     S_ROOT_UNMOD        I2 (1)          M_LIBC
 *   ...
 *   setgid     S_NONROOT_NATIVE    I16 (UINT32_MAX) M_RAW
 *
 *   2 × 7 × 16 × 2 = 448 base cases，每 case 含 ~3-5 assertion
 *   ≈ 1500–2000 PASS
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

/* ── 维度 A: uid_t 输入边界 (15 合法值；-1 单独测) ──────────────── */
static const uint32_t INPUT_VALUES[] = {
    0u,                                /* I1  root */
    1u,                                /* I2  first non-system */
    99u, 100u,                         /* I3-I4  system uid */
    999u, 1000u, 1001u,                /* I5-I7  regular user */
    65533u, 65534u, 65535u,            /* I8-I10  nobody / 16-bit max */
    65536u,                            /* I11  just over 16-bit */
    0x7FFFFFFEu, 0x7FFFFFFFu,          /* I12-I13  INT32_MAX-1, INT32_MAX */
    0x80000000u,                       /* I14  INT32 sign boundary */
    0xFFFFFFFEu,                       /* I15  UINT32_MAX-1 */
    /* I16 = 0xFFFFFFFF (uid_t)-1 → 抽出 invalid_uid_einval_run() 独立测，
     *   starry 不做 uid_valid 校验，对 -1 直接接受 → 详见
     *   bug-starry-setuid-setgid-no-uidvalid-check */
};
#define N_INPUTS (sizeof(INPUT_VALUES) / sizeof(INPUT_VALUES[0]))

/* ── 维度 B: cred 启动状态 (7 状态) ─────────────────────────────── */
typedef enum {
    S_ROOT_UNMOD = 0,                          /* r=e=s=0 (caps 在) */
    S_ROOT_AFTER_SETGID_1000,                  /* gid 改, euid=0 (caps 在) */
    S_ROOT_AFTER_SETUID_1000,                  /* uid=e=s=1000 (caps 清) */
    S_ROOT_AFTER_SETRESUID_1K_1K_0,            /* r=e=1000, s=0 (recover via saved) */
    S_NONROOT_NATIVE,                          /* r=e=s=1000 (从未 root) */
    S_ROOT_AFTER_SETRESGID,                    /* r/e/s gid=1000, euid=0 (caps 在) */
    S_NONROOT_AFTER_SETUID_SELF,               /* non-root setuid(self) idempotent */
    S_STATE_COUNT
} cred_state_t;

static const char *state_name(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return "root_unmod";
    case S_ROOT_AFTER_SETGID_1000:           return "root+setgid(1000)";
    case S_ROOT_AFTER_SETUID_1000:           return "root+setuid(1000)[caps_drop]";
    case S_ROOT_AFTER_SETRESUID_1K_1K_0:     return "root+setresuid(1k,1k,0)";
    case S_NONROOT_NATIVE:                   return "nonroot";
    case S_ROOT_AFTER_SETRESGID:             return "root+setresgid(1k)";
    case S_NONROOT_AFTER_SETUID_SELF:        return "nonroot+setuid(self)";
    default:                                 return "?";
    }
}

/* ── 维度 C: 调用模式 ───────────────────────────────────────────── */
typedef enum { M_LIBC = 0, M_RAW = 1 } call_mode_t;

/* ── 维度 D: syscall ────────────────────────────────────────────── */
typedef enum { SC_SETUID = 0, SC_SETGID = 1 } syscall_kind_t;

/* ── 工具：cred 三元 ────────────────────────────────────────────── */
typedef struct { uint32_t r, e, s; } cred_t;

static int read_uid_cred(cred_t *c)
{
    uid_t r, e, s;
    if (getresuid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}
static int read_gid_cred(cred_t *c)
{
    gid_t r, e, s;
    if (getresgid(&r, &e, &s) != 0) return -1;
    c->r = r; c->e = e; c->s = s; return 0;
}

/* setup_state: 将当前进程拉到指定 cred 状态 (在 fork 后 child 内调) */
static int setup_state(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:
        return (getuid() == 0) ? 0 : -100;

    case S_ROOT_AFTER_SETGID_1000:
        if (getuid() != 0) return -100;
        return setgid(1000) == 0 ? 0 : -101;

    case S_ROOT_AFTER_SETUID_1000:
        if (getuid() != 0) return -100;
        return setuid(1000) == 0 ? 0 : -102;

    case S_ROOT_AFTER_SETRESUID_1K_1K_0:
        if (getuid() != 0) return -100;
        return setresuid(1000, 1000, 0) == 0 ? 0 : -103;

    case S_NONROOT_NATIVE:
        if (getuid() == 0) {
            if (setresuid(1000, 1000, 1000) != 0) return -104;
        }
        return 0;

    case S_ROOT_AFTER_SETRESGID:
        if (getuid() != 0) return -100;
        return setresgid(1000, 1000, 1000) == 0 ? 0 : -105;

    case S_NONROOT_AFTER_SETUID_SELF:
        if (getuid() == 0) {
            if (setresuid(1000, 1000, 1000) != 0) return -106;
        }
        return setuid(getuid()) == 0 ? 0 : -107;

    default:
        return -200;
    }
}

/* ── 期望计算 ───────────────────────────────────────────────────── */
typedef struct {
    int      rc;            /* 0 / -1 */
    int      err;           /* errno (only meaningful when rc==-1) */
    bool     full_set;      /* r/e/s all set vs only e */
} expected_t;

/* derive — 推导期望
 *
 * 关键修正 (2026-05-16): CAP_SETUID / CAP_SETGID 由 **euid** 决定（per
 * starry has_cap_setuid/setgid = self.euid == 0），而非被改字段的 e。
 * setgid 时 pre 是 gid cred，pre->e 是 egid；cap 与 egid 无关。
 *
 * 所以 derive 需 额外 euid 参数。
 */
static expected_t derive(const cred_t *pre, uint32_t euid, uint32_t input)
{
    bool has_cap = (euid == 0);  /* CAP_SETUID/SETGID 等价于 euid==0 */

    if (has_cap) {
        /* [D2]/[DG2] 全设 */
        return (expected_t){ 0, 0, true };
    }
    /* [E4]/[EG2] 非特权：input 必须在 {r, s}（针对被改字段的 r/s）*/
    if (input == pre->r || input == pre->s) {
        return (expected_t){ 0, 0, false };
    }
    return (expected_t){ -1, EPERM, false };
}

/* ── 单 case 执行 ──────────────────────────────────────────────── */
static int waitpid_safely(pid_t pid, int *st)
{
    return (waitpid(pid, st, 0) == pid) ? 0 : -1;
}

static void matrix_one(syscall_kind_t sc, cred_state_t s,
                       uint32_t input, call_mode_t m)
{
    pid_t pid = fork();
    if (pid == 0) {
        int srv = setup_state(s);
        if (srv != 0) _exit(99);

        cred_t pre;
        int prv = (sc == SC_SETUID) ? read_uid_cred(&pre) : read_gid_cred(&pre);
        if (prv != 0) _exit(98);

        /* cap check 总是基于 euid（即 uid cred 的 e）*/
        cred_t uid_cred;
        if (read_uid_cred(&uid_cred) != 0) _exit(97);

        expected_t exp = derive(&pre, uid_cred.e, input);

        errno = 0;
        long rc;
        if (m == M_RAW) {
            rc = (sc == SC_SETUID) ? syscall(SYS_setuid, input)
                                    : syscall(SYS_setgid, input);
        } else {
            rc = (sc == SC_SETUID) ? setuid(input) : setgid(input);
        }
        int err = errno;

        if (rc != exp.rc) _exit(11);
        if (exp.rc == -1 && err != exp.err) _exit(12);

        if (exp.rc == 0) {
            cred_t post;
            int rv = (sc == SC_SETUID) ? read_uid_cred(&post)
                                        : read_gid_cred(&post);
            if (rv != 0) _exit(13);
            if (post.e != input) _exit(14);
            if (exp.full_set) {
                if (post.r != input || post.s != input) _exit(15);
            } else {
                if (post.r != pre.r || post.s != pre.s) _exit(16);
            }
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
             "matrix sc=%s state=%s input=0x%08x mode=%s",
             sc == SC_SETUID ? "setuid" : "setgid",
             state_name(s),
             input,
             m == M_RAW ? "raw" : "libc");
    if (ec == 0) {
        CHECK(1, msg);
    } else if (ec == 99) {
        printf("  SKIP (setup) | %s\n", msg);
    } else {
        char buf[400];
        snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

int matrix_run(void)
{
    printf("\n----- matrix (man-first 完备) -----\n");
    if (getuid() != 0) {
        printf("  matrix skip ALL: requires root\n");
        return 0;
    }
    int total = 0;
    for (int sc = 0; sc < 2; sc++) {
        for (int s = 0; s < S_STATE_COUNT; s++) {
            for (size_t i = 0; i < N_INPUTS; i++) {
                for (int m = 0; m < 2; m++) {
                    matrix_one((syscall_kind_t)sc, (cred_state_t)s,
                               INPUT_VALUES[i], (call_mode_t)m);
                    total++;
                }
            }
        }
    }
    printf("  ----- matrix: %d pass, %d fail (out of %d cases) -----\n",
           __pass, __fail, total);
    return __fail;
}
