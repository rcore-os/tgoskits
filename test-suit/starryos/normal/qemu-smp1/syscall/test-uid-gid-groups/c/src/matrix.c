/* matrix.c — 完备测试矩阵 getgroups/setgroups (man-first design)
 *
 * 参 notes/23-groups-test-design.md
 *
 * man 2 getgroups/setgroups 关键子句：
 *   [GG-D1] getgroups returns sup group IDs
 *   [GG-D3] size==0: returns count only, list not modified
 *   [GG-D5] setgroups needs CAP_SETGID
 *   [GG-D6] setgroups(0, NULL) drops all
 *   [GG-E2] getgroups EINVAL: size < count
 *   [GG-E4] setgroups EINVAL: size > NGROUPS_MAX (65536)
 *   [GG-E6] setgroups EPERM: !CAP_SETGID
 *
 * starry 实现 (sys.rs:283-340)：
 *   getgroups: size==0 → count; size<ngroups → EINVAL; vm_write_slice
 *   setgroups: !cap → EPERM; size > NGROUPS_MAX → EINVAL; vm_read_slice
 *
 * 矩阵：
 *   getgroups: 7 size × 3 ptr × 3 state × 2 mode ≈ 126 case
 *   setgroups: 7 size × 3 ptr × 3 state × 2 mode ≈ 126 case
 *   round-trip: 5 size × 3 state ≈ 15 case
 *   ~270 case total
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <grp.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#define NGROUPS_MAX_C 65536

/* ── 维度 A: 大小 (含边界) ─────────────────────────────────── */
static const int GET_SIZES[] = { 0, 1, 2, 16, 256, 65535, 65536 };
#define N_GET_SZ (sizeof(GET_SIZES)/sizeof(GET_SIZES[0]))

static const long SET_SIZES[] = { 0, 1, 16, 256, 65535, 65536, 65537 };
#define N_SET_SZ (sizeof(SET_SIZES)/sizeof(SET_SIZES[0]))

/* ── 维度 B: 指针 ──────────────────────────────────────────── */
typedef enum { PV_VALID, PV_NULL, PV_KERNEL } ptr_kind_t;
static const char *ptr_name(ptr_kind_t p)
{
    switch (p) {
    case PV_VALID:  return "valid";
    case PV_NULL:   return "null";
    case PV_KERNEL: return "kernel";
    default:        return "?";
    }
}

/* ── 维度 C: state ─────────────────────────────────────────── */
typedef enum {
    GS_ROOT_NO_GROUPS = 0,
    GS_ROOT_WITH_GROUPS,   /* 预设 3 个 groups */
    GS_NONROOT,
    GS_STATE_COUNT
} g_state_t;

static const char *g_state_name(g_state_t s)
{
    switch (s) {
    case GS_ROOT_NO_GROUPS:    return "root_no_groups";
    case GS_ROOT_WITH_GROUPS:  return "root_with_groups(3)";
    case GS_NONROOT:           return "nonroot";
    default:                   return "?";
    }
}

static int setup_state(g_state_t s)
{
    switch (s) {
    case GS_ROOT_NO_GROUPS:
        if (getuid() != 0) return -100;
        setgroups(0, NULL);
        return 0;
    case GS_ROOT_WITH_GROUPS: {
        if (getuid() != 0) return -100;
        gid_t preset[] = {100, 200, 300};
        if (setgroups(3, preset) != 0) return -101;
        return 0;
    }
    case GS_NONROOT:
        if (getuid() == 0) {
            setgroups(0, NULL);
            if (setresgid(1000, 1000, 1000) != 0) return -102;
            if (setresuid(1000, 1000, 1000) != 0) return -103;
        }
        return 0;
    default:
        return -200;
    }
}

/* ── 维度 D: mode ─────────────────────────────────────────── */
typedef enum { M_LIBC = 0, M_RAW = 1 } call_mode_t;

static long do_getgroups(call_mode_t m, int size, gid_t *list)
{
    if (m == M_LIBC) return getgroups(size, list);
    return syscall(SYS_getgroups, size, list);
}

static long do_setgroups(call_mode_t m, size_t size, const gid_t *list)
{
    if (m == M_LIBC) return setgroups(size, list);
    return syscall(SYS_setgroups, size, list);
}

/* ── waitpid ──────────────────────────────────────────────── */
static int waitpid_safely(pid_t pid, int *st)
{
    return waitpid(pid, st, 0) == pid ? 0 : -1;
}

/* ── matrix 1: getgroups (size × ptr × state × mode) ────────── */
static void matrix_get_one(int size, ptr_kind_t pp, g_state_t s, call_mode_t m,
                            void *kern_addr)
{
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);

        int pre_count = getgroups(0, NULL);
        if (pre_count < 0) _exit(98);

        gid_t buf[64] = {0};
        gid_t *list_ptr;
        switch (pp) {
        case PV_VALID:  list_ptr = (size <= 64) ? buf : malloc(sizeof(gid_t) * size); break;
        case PV_NULL:   list_ptr = NULL; break;
        case PV_KERNEL: list_ptr = (gid_t *)kern_addr; break;
        default:        list_ptr = NULL;
        }

        /* expected:
         * - size==0: always success, return count (PV_NULL OK too 因为 list 不被用)
         * - size>0 & PV_NULL → EFAULT (Linux), starry 也是
         * - size>0 & PV_KERNEL → EFAULT
         * - size>0 & valid:
         *     - size < count: EINVAL
         *     - size >= count: success, return count
         */
        /* Linux kernel sys_getgroups + starry sys_getgroups 检查顺序：
         *   1) size==0 → return count (list 不被访问)
         *   2) size < ngroups → EINVAL
         *   3) ngroups == 0 → return 0 (无 copy, 不查 addr)
         *   4) ptr 无效 → EFAULT
         *   5) else → success */
        int exp_rc, exp_err = 0;
        if (size == 0) {
            exp_rc = pre_count;
        } else if (size < pre_count) {
            exp_rc = -1; exp_err = EINVAL;
        } else if (pre_count == 0) {
            exp_rc = 0;             /* 无 copy，bad ptr 不触发 EFAULT */
        } else if (pp != PV_VALID) {
            exp_rc = -1; exp_err = EFAULT;
        } else {
            exp_rc = pre_count;
        }

        errno = 0;
        long rc = do_getgroups(m, size, list_ptr);
        int err = errno;

        /* 显式 free malloc 分配 (PV_VALID && size > 64 路径); _exit 会自动回收,
         * 但显式 free 避免被误读为漏掉的 leak (maicode 静态分析建议). */
        if (pp == PV_VALID && list_ptr != buf && list_ptr != NULL) {
            free(list_ptr);
        }

        if (rc != exp_rc) _exit(11);
        if (exp_rc == -1 && err != exp_err) _exit(12);

        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix_get: fork/waitpid"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[260];
    snprintf(msg, sizeof msg, "getgroups size=%d ptr=%s s=%s m=%s",
             size, ptr_name(pp), g_state_name(s), m == M_RAW ? "raw" : "libc");
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf[320]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

/* ── matrix 2: setgroups ──────────────────────────────────── */
static void matrix_set_one(long size, ptr_kind_t pp, g_state_t s, call_mode_t m,
                            void *kern_addr)
{
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);

        /* 准备 buf */
        gid_t *buf = NULL;
        if (pp == PV_VALID && size > 0 && size <= 65536) {
            buf = malloc(sizeof(gid_t) * (size_t)size);
            if (!buf) _exit(97);
            for (long i = 0; i < size; i++) buf[i] = 100 + (gid_t)(i % 256);
        }

        const gid_t *list_ptr;
        switch (pp) {
        case PV_VALID:  list_ptr = (size == 0) ? NULL : buf; break;
        case PV_NULL:   list_ptr = NULL; break;
        case PV_KERNEL: list_ptr = (const gid_t *)kern_addr; break;
        default:        list_ptr = NULL;
        }

        /* expected per starry sys_setgroups */
        int exp_rc, exp_err = 0;
        bool is_root = (getuid() == 0);  /* cap = euid==0 simplification */
        if (!is_root && size != 0) {
            exp_rc = -1; exp_err = EPERM;
        } else if (!is_root && size == 0) {
            /* unpriv setgroups(0, NULL) — starry EPERM 因 !cap; Linux 同 */
            exp_rc = -1; exp_err = EPERM;
        } else if ((size_t)size > (size_t)NGROUPS_MAX_C) {
            exp_rc = -1; exp_err = EINVAL;
        } else if (size > 0 && pp != PV_VALID) {
            exp_rc = -1; exp_err = EFAULT;
        } else {
            exp_rc = 0;
        }

        errno = 0;
        long rc = do_setgroups(m, (size_t)size, list_ptr);
        int err = errno;

        if (buf) free(buf);

        if (rc != exp_rc) _exit(11);
        if (exp_rc == -1 && err != exp_err) _exit(12);
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "matrix_set: fork/waitpid"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[260];
    snprintf(msg, sizeof msg, "setgroups size=%ld ptr=%s s=%s m=%s",
             size, ptr_name(pp), g_state_name(s), m == M_RAW ? "raw" : "libc");
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup) | %s\n", msg);
    else {
        char buf2[320]; snprintf(buf2, sizeof buf2, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf2);
    }
}

/* ── matrix 3: round-trip set + get ──────────────────────── */
static int gid_cmp(const void *a, const void *b)
{
    gid_t ga = *(const gid_t *)a, gb = *(const gid_t *)b;
    return (ga > gb) - (ga < gb);
}

static void matrix_roundtrip_one(size_t size, g_state_t s)
{
    /* Linux kernel groups_sort() 会对 setgroups 输入排序 — back[] 是 sorted.
     * 用 unique 值避免 dedup 问题 (Linux 不 dedup 但保险), 比较前两边都 sort. */
    pid_t pid = fork();
    if (pid == 0) {
        if (setup_state(s) != 0) _exit(99);
        if (getuid() != 0) _exit(99);  /* roundtrip needs root */
        gid_t *src = (size > 0) ? malloc(sizeof(gid_t) * size) : NULL;
        if (size > 0 && !src) _exit(97);
        /* 用 unique 值: 200, 201, ..., 200+size-1
         * (NGROUPS_MAX=65536, size<=1024 安全) */
        for (size_t i = 0; i < size; i++) src[i] = 200 + (gid_t)i;

        if (setgroups(size, src) != 0) { if (src) free(src); _exit(11); }
        int count = getgroups(0, NULL);
        if (count != (int)size) { if (src) free(src); _exit(12); }

        if (size > 0) {
            gid_t *back = malloc(sizeof(gid_t) * size);
            if (!back) { free(src); _exit(13); }
            int n = getgroups((int)size, back);
            if (n != (int)size) { free(src); free(back); _exit(14); }
            /* Linux groups_sort() 排序 — 两边都 sort 后 element-wise 比 */
            qsort(src, size, sizeof(gid_t), gid_cmp);
            qsort(back, size, sizeof(gid_t), gid_cmp);
            for (size_t i = 0; i < size; i++) {
                if (back[i] != src[i]) { free(src); free(back); _exit(15); }
            }
            free(back);
        }
        if (src) free(src);
        _exit(0);
    }
    int status;
    if (pid < 0 || waitpid_safely(pid, &status) != 0) {
        CHECK(0, "roundtrip: fork/waitpid"); return;
    }
    int ec = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    char msg[200];
    snprintf(msg, sizeof msg, "roundtrip size=%zu s=%s", size, g_state_name(s));
    if (ec == 0)        CHECK(1, msg);
    else if (ec == 99)  printf("  SKIP (setup/nonroot) | %s\n", msg);
    else {
        char buf[260]; snprintf(buf, sizeof buf, "FAIL ec=%d | %s", ec, msg);
        CHECK(0, buf);
    }
}

int matrix_run(void)
{
    printf("\n----- matrix (man-first 完备) -----\n");
    if (getuid() != 0) {
        printf("  matrix: many cases need root, expect skip\n");
    }
    void *kern = (void *)0xffffffffff000000ULL;

    int total = 0;

    printf("  [1/3] getgroups matrix\n");
    for (size_t i = 0; i < N_GET_SZ; i++)
      for (int pp = 0; pp < 3; pp++)
        for (int s = 0; s < GS_STATE_COUNT; s++)
          for (int m = 0; m < 2; m++) {
              matrix_get_one(GET_SIZES[i], (ptr_kind_t)pp, (g_state_t)s,
                             (call_mode_t)m, kern);
              total++;
          }

    printf("  [2/3] setgroups matrix\n");
    for (size_t i = 0; i < N_SET_SZ; i++)
      for (int pp = 0; pp < 3; pp++)
        for (int s = 0; s < GS_STATE_COUNT; s++)
          for (int m = 0; m < 2; m++) {
              matrix_set_one(SET_SIZES[i], (ptr_kind_t)pp, (g_state_t)s,
                             (call_mode_t)m, kern);
              total++;
          }

    printf("  [3/3] round-trip\n");
    static const size_t RT_SIZES[] = {0, 1, 16, 256, 1024};
    for (size_t i = 0; i < 5; i++)
      for (int s = 0; s < GS_STATE_COUNT; s++) {
          matrix_roundtrip_one(RT_SIZES[i], (g_state_t)s);
          total++;
      }

    printf("  ----- matrix: %d pass, %d fail (out of %d cases) -----\n",
           __pass, __fail, total);
    return __fail;
}
