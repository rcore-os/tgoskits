/* matrix.c — 完备测试矩阵 6 个 getter (man-first design)
 *
 * 参 notes/19-getter-test-design.md
 *
 * man 2 getuid 关键子句：
 *   [G-D1] getuid returns real UID
 *   [G-D2] geteuid returns effective UID
 *   [G-E1] always successful, never modify errno
 *
 * man 2 getresuid 关键子句：
 *   [GR-D1] r/e/s 三 out-args 被填写
 *   [GR-R1] success → 0
 *   [GR-E1] EFAULT — arg outside address space
 *
 * starry 实现 (sys.rs:20-54)：thin wrapper 直接读 cred + vm_write
 *
 * 矩阵：
 *   0-arg getter (4): syscall × state × mode = 4 × 8 × 2 = 64 case
 *   3-arg getres (2): syscall × state × ptr_pattern × mode = 2 × 8 × 10 × 2 = 320 case
 *   errno 不动: 4 syscall × 6 preset errno = 24 case
 *   cross consistency: 8 state × 4 check = 32 case
 *   ─────
 *   ≈ 440 case
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/* ── 维度 A: cred 状态 (8) ─────────────────────────────────────── */
typedef enum {
    S_ROOT_UNMOD = 0,                          /* r=e=s=0 (uid+gid) */
    S_ROOT_AFTER_SETUID_1000,                  /* uid 全 1000, gid 不动 */
    S_ROOT_AFTER_SETUID_BIG,                   /* uid 全 0x7FFFFFFE */
    S_ROOT_AFTER_SETRESUID_1K_2K_3K,           /* r=1k, e=2k, s=3k */
    S_NONROOT_NATIVE,                          /* r=e=s=1000 (uid+gid) */
    S_ROOT_AFTER_SETGID_1000,                  /* gid 全 1000, uid=0 */
    S_ROOT_AFTER_SETRESGID_1K_2K_3K,           /* gid r=1k e=2k s=3k */
    S_MIXED_UID_GID,                           /* 同时设 uid 1k+gid 2k */
    S_STATE_COUNT
} cred_state_t;

static const char *state_name(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return "root_unmod";
    case S_ROOT_AFTER_SETUID_1000:           return "after_setuid(1k)";
    case S_ROOT_AFTER_SETUID_BIG:            return "after_setuid(0x7FFFFFFE)";
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:    return "after_setresuid(1k,2k,3k)";
    case S_NONROOT_NATIVE:                   return "nonroot";
    case S_ROOT_AFTER_SETGID_1000:           return "after_setgid(1k)";
    case S_ROOT_AFTER_SETRESGID_1K_2K_3K:    return "after_setresgid(1k,2k,3k)";
    case S_MIXED_UID_GID:                    return "mixed(setuid+setgid)";
    default:                                 return "?";
    }
}

/* setup_state: 在 fork 后 child 内调 */
static int setup_state(cred_state_t s)
{
    switch (s) {
    case S_ROOT_UNMOD:
        return (getuid() == 0) ? 0 : -100;

    case S_ROOT_AFTER_SETUID_1000:
        if (getuid() != 0) return -100;
        return setuid(1000) == 0 ? 0 : -101;

    case S_ROOT_AFTER_SETUID_BIG:
        if (getuid() != 0) return -100;
        return setuid(0x7FFFFFFEu) == 0 ? 0 : -102;

    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:
        if (getuid() != 0) return -100;
        return setresuid(1000, 2000, 3000) == 0 ? 0 : -103;

    case S_NONROOT_NATIVE:
        if (getuid() == 0) {
            /* 必须先 setresgid（root 时 cap 在）再 setresuid（清 caps）
             * 否则 cap drop 后 setresgid 会 EPERM
             * expected_gid(S_NONROOT_NATIVE) = (1000,1000,1000) 假设 gid 也降了 */
            if (setresgid(1000, 1000, 1000) != 0) return -1041;
            if (setresuid(1000, 1000, 1000) != 0) return -1042;
        }
        return 0;

    case S_ROOT_AFTER_SETGID_1000:
        if (getuid() != 0) return -100;
        return setgid(1000) == 0 ? 0 : -105;

    case S_ROOT_AFTER_SETRESGID_1K_2K_3K:
        if (getuid() != 0) return -100;
        return setresgid(1000, 2000, 3000) == 0 ? 0 : -106;

    case S_MIXED_UID_GID:
        if (getuid() != 0) return -100;
        if (setgid(2000) != 0) return -107;
        if (setuid(1000) != 0) return -108;
        return 0;

    default:
        return -200;
    }
}

/* 预期 cred 三元 (per state)
 *
 * codex P1 (adopted): nonroot caller (uid != 0) 时 setup_state(S_NONROOT_NATIVE)
 * 无法 setresuid 切到 1000 (无 cap), 应保持 caller 原 uid/gid.
 * matrix_run 先 capture caller 实际 uid/gid 传入 expected_*().
 */
typedef struct { uint32_t r, e, s; } cred_t;

static cred_t expected_uid(cred_state_t s, uint32_t caller_uid)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return (cred_t){0, 0, 0};
    case S_ROOT_AFTER_SETUID_1000:           return (cred_t){1000, 1000, 1000};
    case S_ROOT_AFTER_SETUID_BIG:            return (cred_t){0x7FFFFFFEu, 0x7FFFFFFEu, 0x7FFFFFFEu};
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:    return (cred_t){1000, 2000, 3000};
    case S_NONROOT_NATIVE:
        /* 根据 caller 是否 root 决定 */
        return (caller_uid == 0)
                 ? (cred_t){1000, 1000, 1000}
                 : (cred_t){caller_uid, caller_uid, caller_uid};
    case S_ROOT_AFTER_SETGID_1000:           return (cred_t){0, 0, 0};
    case S_ROOT_AFTER_SETRESGID_1K_2K_3K:    return (cred_t){0, 0, 0};
    case S_MIXED_UID_GID:                    return (cred_t){1000, 1000, 1000};
    default:                                 return (cred_t){0, 0, 0};
    }
}

static cred_t expected_gid(cred_state_t s, uint32_t caller_uid, uint32_t caller_gid)
{
    switch (s) {
    case S_ROOT_UNMOD:                       return (cred_t){0, 0, 0};
    case S_ROOT_AFTER_SETUID_1000:           return (cred_t){0, 0, 0};
    case S_ROOT_AFTER_SETUID_BIG:            return (cred_t){0, 0, 0};
    case S_ROOT_AFTER_SETRESUID_1K_2K_3K:    return (cred_t){0, 0, 0};
    case S_NONROOT_NATIVE:
        return (caller_uid == 0)
                 ? (cred_t){1000, 1000, 1000}
                 : (cred_t){caller_gid, caller_gid, caller_gid};
    case S_ROOT_AFTER_SETGID_1000:           return (cred_t){1000, 1000, 1000};
    case S_ROOT_AFTER_SETRESGID_1K_2K_3K:    return (cred_t){1000, 2000, 3000};
    case S_MIXED_UID_GID:                    return (cred_t){2000, 2000, 2000};
    default:                                 return (cred_t){0, 0, 0};
    }
}

/* ── 维度 B: getter syscall (6) ─────────────────────────────────── */
typedef enum {
    GET_UID = 0, GET_EUID, GET_GID, GET_EGID, GET_RESUID, GET_RESGID
} getter_kind_t;

static const char *getter_name(getter_kind_t g)
{
    switch (g) {
    case GET_UID:    return "getuid";
    case GET_EUID:   return "geteuid";
    case GET_GID:    return "getgid";
    case GET_EGID:   return "getegid";
    case GET_RESUID: return "getresuid";
    case GET_RESGID: return "getresgid";
    default:         return "?";
    }
}

/* ── 维度 C: 模式 ───────────────────────────────────────────────── */
typedef enum { M_LIBC = 0, M_RAW = 1 } call_mode_t;

/* ── 0-arg getter 实际 call ────────────────────────────────────── */
static long call_0arg(getter_kind_t g, call_mode_t m)
{
    if (m == M_LIBC) {
        switch (g) {
        case GET_UID:  return (long)getuid();
        case GET_EUID: return (long)geteuid();
        case GET_GID:  return (long)getgid();
        case GET_EGID: return (long)getegid();
        default:       return -1;
        }
    } else {
        switch (g) {
        case GET_UID:  return syscall(SYS_getuid);
        case GET_EUID: return syscall(SYS_geteuid);
        case GET_GID:  return syscall(SYS_getgid);
        case GET_EGID: return syscall(SYS_getegid);
        default:       return -1;
        }
    }
}

/* expected for 0-arg — 接受 caller_uid/gid (codex P1 修) */
static uint32_t expected_0arg(getter_kind_t g, cred_state_t s,
                               uint32_t caller_uid, uint32_t caller_gid)
{
    cred_t uid_c = expected_uid(s, caller_uid);
    cred_t gid_c = expected_gid(s, caller_uid, caller_gid);
    switch (g) {
    case GET_UID:  return uid_c.r;
    case GET_EUID: return uid_c.e;
    case GET_GID:  return gid_c.r;
    case GET_EGID: return gid_c.e;
    default:       return 0;
    }
}

static int waitpid_safely(pid_t pid, int *st)
{
    return waitpid(pid, st, 0) == pid ? 0 : -1;
}

/* ── 矩阵 1: 0-arg getter (4 syscall × 8 state × 2 mode = 64 case) ── */
static void matrix_0arg_one(getter_kind_t g, cred_state_t s, call_mode_t m,
                             uint32_t caller_uid, uint32_t caller_gid)
{
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);
        uint32_t exp = expected_0arg(g, s, caller_uid, caller_gid);
        long val = call_0arg(g, m);
        if (val < 0) _exit(10);
        if ((uint32_t)val != exp) _exit(11);
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix_0arg: fork/waitpid"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[200];
    snprintf(msg, sizeof msg, "0arg %s state=%s mode=%s",
             getter_name(g), state_name(s), m == M_RAW ? "raw" : "libc");
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf[260]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

/* ── 维度 D: getres 指针 pattern (10) ──────────────────────────── */
typedef enum {
    PP_ALL_VALID_DISJOINT = 0,
    PP_ALL_VALID_SAME,
    PP_NULL_FIRST,
    PP_NULL_SECOND,
    PP_NULL_THIRD,
    PP_KERNEL_FIRST,
    PP_KERNEL_SECOND,
    PP_KERNEL_THIRD,
    PP_UNMAPPED_FIRST,
    PP_ALL_NULL,
    PP_COUNT
} ptr_pattern_t;

static const char *pp_name(ptr_pattern_t p)
{
    switch (p) {
    case PP_ALL_VALID_DISJOINT: return "valid_disjoint";
    case PP_ALL_VALID_SAME:     return "valid_same";
    case PP_NULL_FIRST:         return "null_1st";
    case PP_NULL_SECOND:        return "null_2nd";
    case PP_NULL_THIRD:         return "null_3rd";
    case PP_KERNEL_FIRST:       return "kern_1st";
    case PP_KERNEL_SECOND:      return "kern_2nd";
    case PP_KERNEL_THIRD:       return "kern_3rd";
    case PP_UNMAPPED_FIRST:     return "unmapped_1st";
    case PP_ALL_NULL:           return "all_null";
    default:                    return "?";
    }
}

/* 设三指针 + 预期 rc/err */
typedef struct {
    uint32_t *p1, *p2, *p3;
    int      expected_rc;        /* 0 or -1 */
    int      expected_err;       /* 0 or EFAULT */
    bool     check_values;       /* 成功时是否验值（valid_same 时 last-write 才检） */
} ptr_setup_t;

static ptr_setup_t setup_pointers(ptr_pattern_t p,
                                   uint32_t *a, uint32_t *b, uint32_t *c,
                                   void *kern_addr, void *unmapped_addr)
{
    ptr_setup_t ps = {0};
    switch (p) {
    case PP_ALL_VALID_DISJOINT:
        ps.p1 = a; ps.p2 = b; ps.p3 = c;
        ps.expected_rc = 0; ps.expected_err = 0; ps.check_values = true;
        break;
    case PP_ALL_VALID_SAME:
        ps.p1 = a; ps.p2 = a; ps.p3 = a;  /* alias: last write hits a */
        ps.expected_rc = 0; ps.expected_err = 0; ps.check_values = true;
        break;
    case PP_NULL_FIRST:
        ps.p1 = NULL; ps.p2 = b; ps.p3 = c;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_NULL_SECOND:
        ps.p1 = a; ps.p2 = NULL; ps.p3 = c;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_NULL_THIRD:
        ps.p1 = a; ps.p2 = b; ps.p3 = NULL;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_KERNEL_FIRST:
        ps.p1 = (uint32_t *)kern_addr; ps.p2 = b; ps.p3 = c;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_KERNEL_SECOND:
        ps.p1 = a; ps.p2 = (uint32_t *)kern_addr; ps.p3 = c;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_KERNEL_THIRD:
        ps.p1 = a; ps.p2 = b; ps.p3 = (uint32_t *)kern_addr;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_UNMAPPED_FIRST:
        ps.p1 = (uint32_t *)unmapped_addr; ps.p2 = b; ps.p3 = c;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    case PP_ALL_NULL:
        ps.p1 = NULL; ps.p2 = NULL; ps.p3 = NULL;
        ps.expected_rc = -1; ps.expected_err = EFAULT;
        break;
    default:
        ps.expected_rc = -1; ps.expected_err = EFAULT;
    }
    return ps;
}

/* call_3arg */
static long call_3arg(getter_kind_t g, call_mode_t m,
                      uint32_t *p1, uint32_t *p2, uint32_t *p3)
{
    if (m == M_LIBC) {
        if (g == GET_RESUID) return getresuid((uid_t*)p1, (uid_t*)p2, (uid_t*)p3);
        else                  return getresgid((gid_t*)p1, (gid_t*)p2, (gid_t*)p3);
    } else {
        long sysn = (g == GET_RESUID) ? SYS_getresuid : SYS_getresgid;
        return syscall(sysn, p1, p2, p3);
    }
}

/* ── 矩阵 2: 3-arg getres (2 × 8 × 10 × 2 = 320 case) ──────────── */
static void matrix_3arg_one(getter_kind_t g, cred_state_t s,
                             ptr_pattern_t pp, call_mode_t m,
                             void *kern_addr, void *unmapped_addr,
                             uint32_t caller_uid, uint32_t caller_gid)
{
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);

        uint32_t r = 0xAAAAAAAA, e = 0xBBBBBBBB, sval = 0xCCCCCCCC;
        ptr_setup_t ps = setup_pointers(pp, &r, &e, &sval, kern_addr, unmapped_addr);

        errno = 0;
        long rc = call_3arg(g, m, ps.p1, ps.p2, ps.p3);
        int err = errno;

        if (rc != ps.expected_rc) _exit(11);
        if (ps.expected_rc == -1 && err != ps.expected_err) _exit(12);

        if (ps.expected_rc == 0 && ps.check_values) {
            cred_t exp = (g == GET_RESUID)
                          ? expected_uid(s, caller_uid)
                          : expected_gid(s, caller_uid, caller_gid);
            if (pp == PP_ALL_VALID_DISJOINT) {
                if (r != exp.r || e != exp.e || sval != exp.s) _exit(13);
            } else if (pp == PP_ALL_VALID_SAME) {
                if (r != exp.s) _exit(14);
            }
        }
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix_3arg: fork/waitpid"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[260];
    snprintf(msg, sizeof msg, "3arg %s state=%s pp=%s mode=%s",
             getter_name(g), state_name(s), pp_name(pp),
             m == M_RAW ? "raw" : "libc");
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf[320]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

/* ── 矩阵 3: errno 不动 (4 0-arg syscall × 6 preset errno) ────── */
static const int ERRNO_PRESETS[] = { 1, 5, 22, 4242, 9999, 88888 };
#define N_ERRNO_PRESETS (sizeof(ERRNO_PRESETS)/sizeof(ERRNO_PRESETS[0]))

static void errno_preservation_one(getter_kind_t g, int preset)
{
    pid_t pid = fork();
    if (pid == 0) {
        errno = preset;
        (void)call_0arg(g, M_LIBC);
        if (errno != preset) _exit(11);
        _exit(0);
    }
    int status;
    waitpid_safely(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[200];
    snprintf(msg, sizeof msg, "errno preserve %s preset=%d", getter_name(g), preset);
    CHECK(ec == 0, msg);
}

/* ── 矩阵 4: cross-syscall consistency (8 state × 4 check) ─────── */
static void cross_consistency_one(cred_state_t s)
{
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);
        uid_t u = getuid(), e = geteuid();
        uid_t r2, e2, s2;
        if (getresuid(&r2, &e2, &s2) != 0) _exit(11);
        if (u != r2) _exit(12);
        if (e != e2) _exit(13);
        gid_t g = getgid(), eg = getegid();
        gid_t gr2, ge2, gs2;
        if (getresgid(&gr2, &ge2, &gs2) != 0) _exit(14);
        if (g != gr2) _exit(15);
        if (eg != ge2) _exit(16);
        _exit(0);
    }
    int status;
    waitpid_safely(pid, &status);
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[200];
    snprintf(msg, sizeof msg, "consistency state=%s", state_name(s));
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf[260]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

/* ── 入口 ──────────────────────────────────────────────────────── */
int matrix_run(void)
{
    printf("\n----- matrix (man-first 完备) -----\n");
    /* codex P1 (adopted): capture caller uid/gid 用于 derive
     * S_NONROOT_NATIVE 在 non-root caller 时的真实期望 */
    uint32_t caller_uid = getuid();
    uint32_t caller_gid = getgid();
    if (caller_uid != 0) {
        printf("  matrix: caller uid=%u gid=%u (nonroot; root-only states will skip)\n",
               caller_uid, caller_gid);
    }

    /* 准备 unmapped 用户态地址 (mmap+munmap) */
    void *unmapped = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (unmapped != MAP_FAILED) {
        munmap(unmapped, 4096);
    } else {
        unmapped = (void *)0x1000;  /* fallback unlikely-mapped */
    }
    void *kern_addr = (void *)0xffffffffff000000ULL;  /* 内核段 */

    int total = 0;

    /* matrix 1: 0-arg */
    printf("  [matrix 1/4] 0-arg getter (4 × 8 × 2 = 64 case)\n");
    for (int g = 0; g < 4; g++)
      for (int s = 0; s < S_STATE_COUNT; s++)
        for (int m = 0; m < 2; m++) {
            matrix_0arg_one((getter_kind_t)g, (cred_state_t)s, (call_mode_t)m,
                            caller_uid, caller_gid);
            total++;
        }

    /* matrix 2: 3-arg */
    printf("  [matrix 2/4] 3-arg getres (2 × 8 × 10 × 2 = 320 case)\n");
    for (int g = GET_RESUID; g <= GET_RESGID; g++)
      for (int s = 0; s < S_STATE_COUNT; s++)
        for (int pp = 0; pp < PP_COUNT; pp++)
          for (int m = 0; m < 2; m++) {
              matrix_3arg_one((getter_kind_t)g, (cred_state_t)s,
                              (ptr_pattern_t)pp, (call_mode_t)m,
                              kern_addr, unmapped,
                              caller_uid, caller_gid);
              total++;
          }

    /* matrix 3: errno preservation */
    printf("  [matrix 3/4] errno preservation (4 × 6 = 24 case)\n");
    for (int g = 0; g < 4; g++)
      for (size_t i = 0; i < N_ERRNO_PRESETS; i++) {
          errno_preservation_one((getter_kind_t)g, ERRNO_PRESETS[i]);
          total++;
      }

    /* matrix 4: cross consistency */
    printf("  [matrix 4/4] cross-syscall consistency (8 case)\n");
    for (int s = 0; s < S_STATE_COUNT; s++) {
        cross_consistency_one((cred_state_t)s);
        total++;
    }

    printf("  ----- matrix: %d pass, %d fail (out of %d cases) -----\n",
           __pass, __fail, total);
    return __fail;
}
