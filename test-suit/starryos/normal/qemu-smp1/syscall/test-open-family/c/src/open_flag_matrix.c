#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR     OF_MOD("flag_mat")
#define M_REG     M_DIR "/reg"
#define M_SUB     M_DIR "/sub"
#define M_SYM     M_DIR "/sym"           /* symlink → reg */
#define M_DANGLE  M_DIR "/dangle"        /* symlink → /tmp/topen_flag_mat/no */
#define M_ABSENT  M_DIR "/absent"        /* never exists; per-case unlinked */

/*
 * 13 honored flag 全组合矩阵 — 多目标地毯式：
 *
 *   目标   × access × 1024 combo
 *   5      × 3      × 1024     = 15360 case
 *
 * + 与 silent flag 子矩阵 + access==invalid(3) 占位（明确 SKIP；确认差异在 bug-*）
 *
 * 整 case 用 CHECK_QUIET 静默 PASS（避免 QEMU 串口被淹），只报 FAIL。
 *
 * man 2 open 的相关条款（predict_linux 9 优先级规则全部基于这些原文）：
 *   §"O_CREAT" + §"O_DIRECTORY"：man 隐含 + Linux 实测 — open 不能创建目录，
 *      O_CREAT|O_DIRECTORY 任何 target 都 EINVAL 早返
 *   §"O_PATH": "When O_PATH is specified in flags, flag bits other than
 *      O_CLOEXEC, O_DIRECTORY, and O_NOFOLLOW are ignored."
 *   §"O_EXCL": "When these two flags are specified, symbolic links are not
 *      followed: if pathname is a symbolic link, then open() fails
 *      regardless of where the symbolic link points." → 已存在任何 target +
 *      CREAT|EXCL → EEXIST
 *   §"O_NOFOLLOW": "If the trailing component (i.e., basename) of pathname
 *      is a symbolic link, then the open fails, with the error ELOOP."
 *      （含 DIRECTORY 时 ENOTDIR 优先）
 *   §"O_DIRECTORY": "If pathname is not a directory, cause the open to
 *      fail."
 *   §"EISDIR": "pathname refers to a directory and the access requested
 *      involved writing"
 *   §"ENOENT": 含 ABSENT 非 CREAT / DANGLE 非 CREAT 两个 variant
 *   §"O_CREAT" + 已存在 dir → Linux 实测 EISDIR（CREAT 隐含创建普通文件意图）
 *
 * 已知 starry vs Linux 差异 → 针对性 SKIP（每个 SKIP 都对应 bug-* 复现）：
 *   skip_A: access==RDONLY && O_TRUNC（任何场景）→ bug-open-rdonly-trunc-einval
 *   skip_B: access==RDONLY && O_APPEND → bug-open-rdonly-append-promotes-rw
 *   skip_C: O_APPEND && O_TRUNC && !newly_creating → bug-open-append-trunc-einval
 *   skip_D: O_DIRECT（fs 依赖，不稳定）
 *   skip_E: O_NOFOLLOW + sym/dangling → bug-open-nofollow-sym
 *   skip_F: O_CREAT|O_DIRECTORY → bug-open-creat-directory-einval
 *   skip_G: O_PATH + (O_CREAT 或 O_EXCL) → bug-open-path-honors-excl + bug-open-path-creat-creates
 *   skip_H: O_PATH + 写访问 → bug-open-path-dir-write-eisdir + bug-open-path-sym-write-enoent
 *   skip_I: O_CREAT + 已存在 dir → bug-open-creat-on-existing-dir-no-eisdir
 *   skip_J: O_CREAT + DANGLE + !O_EXCL → bug-open-creat-dangling-no-create
 */

/* 10 binary flag 索引 */
static const int  FLAG_BITS[10]  = {
    O_APPEND, O_TRUNC, O_CREAT, O_EXCL, O_DIRECTORY, O_NOFOLLOW,
    O_DIRECT, O_PATH, O_CLOEXEC, O_NONBLOCK
};
static const char *FLAG_NAMES[10] = {
    "APPEND","TRUNC","CREAT","EXCL","DIRECTORY","NOFOLLOW",
    "DIRECT","PATH","CLOEXEC","NONBLOCK"
};
#define HAS(c, i)        (((c) >> (i)) & 1)
#define HAS_APPEND(c)    HAS(c, 0)
#define HAS_TRUNC(c)     HAS(c, 1)
#define HAS_CREAT(c)     HAS(c, 2)
#define HAS_EXCL(c)      HAS(c, 3)
#define HAS_DIRECTORY(c) HAS(c, 4)
#define HAS_NOFOLLOW(c)  HAS(c, 5)
#define HAS_DIRECT(c)    HAS(c, 6)
#define HAS_PATH(c)      HAS(c, 7)
#define HAS_CLOEXEC(c)   HAS(c, 8)
#define HAS_NONBLOCK(c)  HAS(c, 9)

typedef enum { TGT_REG, TGT_DIR, TGT_SYM, TGT_DANGLE, TGT_ABSENT } TargetKind;
static const char *TGT_NAMES[] = { "REG", "DIR", "SYM", "DANGLE", "ABSENT" };
static const char *TGT_PATHS[] = { M_REG, M_SUB, M_SYM, M_DANGLE, M_ABSENT };

static int build_flags(int combo, int access)
{
    int f = access;
    for (int i = 0; i < 10; i++)
        if (HAS(combo, i)) f |= FLAG_BITS[i];
    return f;
}

static void combo_str(int combo, char *out, size_t sz)
{
    out[0] = '\0';
    int first = 1;
    for (int i = 0; i < 10; i++) {
        if (HAS(combo, i)) {
            if (!first) strncat(out, "|", sz - strlen(out) - 1);
            strncat(out, FLAG_NAMES[i], sz - strlen(out) - 1);
            first = 0;
        }
    }
    if (first) strncat(out, "<none>", sz - strlen(out) - 1);
}

/* 是否跳过该 (combo, access) 组合 — 针对已知 starry 差异 */
static int should_skip(int combo, int access, TargetKind tgt)
{
    /* TGT_ABSENT 时 file_doesnt_exist=1；其他 target 都 file_exists=1
     * （symlink/dangling 在 starry 也算"存在的 entry"；DIR 同理）*/
    int file_present = (tgt != TGT_ABSENT);
    int newly_creating = HAS_CREAT(combo) && HAS_EXCL(combo) && !file_present;

    /* skip_A: RDONLY|TRUNC（任何场景）— starry is_valid 总拒绝
     *   bugfix/bug-open-rdonly-trunc-einval */
    if (access == O_RDONLY && HAS_TRUNC(combo)) return 1;
    /* skip_B: RDONLY|APPEND（任何场景）*/
    if (access == O_RDONLY && HAS_APPEND(combo)) return 1;
    /* skip_C: APPEND|TRUNC && 没新建 */
    if (HAS_APPEND(combo) && HAS_TRUNC(combo) && !newly_creating) return 1;
    /* skip_D: O_DIRECT — fs 依赖（host /tmp 可能 ext4 支持，可能 tmpfs 不支持），
     * 矩阵无法稳定预测；O_DIRECT 单独在 silent_flags / 专门测例覆盖 */
    if (HAS_DIRECT(combo)) return 1;
    /* skip_E: O_NOFOLLOW + symlink/dangling target → starry 不返 ELOOP
     *   bugfix/bug-open-nofollow-sym */
    if (HAS_NOFOLLOW(combo) && !HAS_PATH(combo) &&
        (tgt == TGT_SYM || tgt == TGT_DANGLE)) return 1;
    /* skip_F: O_CREAT|O_DIRECTORY → starry 不返 EINVAL
     *   bugfix/bug-open-creat-directory-einval */
    if (HAS_CREAT(combo) && HAS_DIRECTORY(combo) && !HAS_PATH(combo)) return 1;
    /* skip_G: O_PATH + (CREAT 或 EXCL) → starry 错误地仍处理 CREAT/EXCL
     *   bugfix/bug-open-path-honors-excl + bug-open-path-creat-creates */
    if (HAS_PATH(combo) && (HAS_CREAT(combo) || HAS_EXCL(combo))) return 1;
    /* skip_H: O_PATH + 写访问 (WRONLY/RDWR) → starry 在 dir/sym 上行为异常
     *   bugfix/bug-open-path-dir-write-eisdir + bug-open-path-sym-write-enoent */
    if (HAS_PATH(combo) && access != O_RDONLY) return 1;
    /* skip_I: O_CREAT 在已存在 dir 上 → starry 不返 EISDIR
     *   bugfix/bug-open-creat-on-existing-dir-no-eisdir */
    if (HAS_CREAT(combo) && tgt == TGT_DIR) return 1;
    /* skip_J: O_CREAT 跟随 dangling symlink 创建 → starry 不创建（ENOENT）
     *   bugfix/bug-open-creat-dangling-no-create */
    if (HAS_CREAT(combo) && !HAS_EXCL(combo) && tgt == TGT_DANGLE) return 1;
    return 0;
}

/* 预测 Linux 期望：返回 0 = 期望成功（fd>=0），否则 = 期望 errno
 *
 * Linux open(2) 行为（基于 host gcc 实测验证）的核心优先级：
 *   1. O_CREAT|O_DIRECTORY → 立即 EINVAL（open 不能创建目录）
 *   2. O_PATH 路径独立：仅 O_DIRECTORY/O_NOFOLLOW/O_CLOEXEC 仍生效；
 *      其余被忽略；不创建；access mode 被忽略
 *   3. O_CREAT|O_EXCL + 已存在路径（含 symlink/dangling/dir）→ EEXIST
 *      （man：with O_EXCL, symlinks not followed）
 *   4. O_NOFOLLOW + basename 是 symlink/dangling + 非 O_PATH：
 *        若同时 O_DIRECTORY：ENOTDIR（kernel 先判 dir 再返 ELOOP）
 *        否则：ELOOP
 *   5. 路径不存在 + 非 CREAT：ENOENT
 *   6. dangling symlink + CREAT + 无 EXCL：跟随到 target，parent 存在 → 创建（成功）
 *   7. O_CREAT 看到已有 dir → EISDIR（不论 access mode）
 *   8. O_DIRECTORY + 非 dir → ENOTDIR；DIR + WRONLY/RDWR → EISDIR
 *   9. effective target 是 dir + 写访问 → EISDIR
 *   否则 → 成功 */
static int predict_linux(int combo, int access, TargetKind tgt)
{
    /* 1. O_CREAT|O_DIRECTORY → EINVAL（仅在非 PATH 时；PATH 忽略 CREAT）*/
    if (HAS_CREAT(combo) && HAS_DIRECTORY(combo) && !HAS_PATH(combo))
        return EINVAL;

    /* 2. O_PATH：独立路径 */
    if (HAS_PATH(combo)) {
        if (tgt == TGT_ABSENT) return ENOENT;
        if (HAS_NOFOLLOW(combo)) {
            /* 返回 pathname 自身（不解析 symlink）的 fd */
            if (tgt == TGT_SYM || tgt == TGT_DANGLE) {
                if (HAS_DIRECTORY(combo)) return ENOTDIR;     /* symlink 不是 dir */
                return 0;
            }
        } else {
            /* 跟随 symlink */
            if (tgt == TGT_DANGLE) return ENOENT;
        }
        if (HAS_DIRECTORY(combo)) {
            if (tgt == TGT_DIR) return 0;
            return ENOTDIR;        /* SYM(→REG)/REG */
        }
        return 0;
    }

    /* 3. O_CREAT|O_EXCL + 已存在 path（含 symlink） → EEXIST
     *    man: "with O_EXCL, symbolic links are not followed" */
    if (HAS_CREAT(combo) && HAS_EXCL(combo)) {
        if (tgt != TGT_ABSENT) return EEXIST;
    }

    /* 4. O_NOFOLLOW + basename 是 symlink/dangling */
    if (HAS_NOFOLLOW(combo) && (tgt == TGT_SYM || tgt == TGT_DANGLE)) {
        if (HAS_DIRECTORY(combo)) return ENOTDIR;            /* DIRECTORY 优先于 ELOOP */
        return ELOOP;
    }

    /* 5. 路径不存在 + 非 CREAT → ENOENT */
    if (tgt == TGT_ABSENT && !HAS_CREAT(combo)) return ENOENT;
    if (tgt == TGT_DANGLE && !HAS_CREAT(combo)) return ENOENT;

    /* 6. dangling + CREAT (无 EXCL)：跟随 symlink，parent 存在 → 创建成功
     *    (CREAT|EXCL 已在 #3 拦下) */

    /* 7. CREAT + 已有 dir → EISDIR */
    if (HAS_CREAT(combo) && tgt == TGT_DIR) return EISDIR;

    /* 8. O_DIRECTORY 在非目录上 → ENOTDIR */
    if (HAS_DIRECTORY(combo)) {
        if (tgt == TGT_REG)    return ENOTDIR;
        if (tgt == TGT_SYM)    return ENOTDIR;     /* 跟随到 REG */
        if (tgt == TGT_DIR) {
            if (access != O_RDONLY) return EISDIR;
            return 0;
        }
        /* DANGLE/ABSENT 不可达此分支（已在 #5 处理）*/
    }

    /* 9. effective target 是 dir + 写打开 → EISDIR */
    if (tgt == TGT_DIR && (access == O_WRONLY || access == O_RDWR))
        return EISDIR;

    return 0;
}

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "flag_mat setup: mkdir top");
    CHECK(ensure_dir(M_SUB) == 0,                                 "flag_mat setup: subdir");
    CHECK(write_file(M_REG, "data", 4, 0644) == 0,                "flag_mat setup: regular");
    CHECK(symlink(M_REG, M_SYM) == 0,                             "flag_mat setup: symlink");
    CHECK(symlink(M_DIR "/no", M_DANGLE) == 0,                    "flag_mat setup: dangling symlink");
    umask(0);
}

/* 每 case 之间还原环境（避免 CREAT 创建出污染下一行）*/
static void per_case_reset(TargetKind tgt)
{
    switch (tgt) {
        case TGT_REG:
            /* 若被 TRUNC 改大小，重写 */
            if (get_file_size(M_REG) != 4)
                write_file(M_REG, "data", 4, 0644);
            break;
        case TGT_ABSENT:
            unlink(M_ABSENT);
            break;
        case TGT_DANGLE:
            /* DANGLE+CREAT 可能会在 /tmp/topen_flag_mat/no 创建真文件；删之 */
            unlink(M_DIR "/no");
            break;
        case TGT_DIR:
        case TGT_SYM:
            break;
    }
}

int open_flag_matrix_run(void)
{
    printf("\n----- open_flag_matrix -----\n");
    mod_setup();

    int run_count = 0, skip_count = 0, fail_local = 0;
    int access_modes[]    = { O_RDONLY,  O_WRONLY,  O_RDWR  };
    const char *am_names[]= { "RDONLY",  "WRONLY",  "RDWR"  };

    for (int ti = 0; ti < 5; ti++) {
        TargetKind tgt = (TargetKind)ti;
        for (int ai = 0; ai < 3; ai++) {
            for (int combo = 0; combo < 1024; combo++) {
                if (should_skip(combo, access_modes[ai], tgt)) {
                    skip_count++;
                    continue;
                }

                int flags  = build_flags(combo, access_modes[ai]);
                int expect = predict_linux(combo, access_modes[ai], tgt);

                per_case_reset(tgt);
                errno = 0;
                int fd = open(TGT_PATHS[tgt], flags, 0644);

                int ok;
                if (expect == 0) ok = (fd >= 0);
                else             ok = (fd == -1 && errno == expect);

                if (fd >= 0) close(fd);

                if (!ok) {
                    char cstr[256];
                    combo_str(combo, cstr, sizeof(cstr));
                    printf("  FAIL | flag_matrix | tgt=%s am=%s combo=%s | "
                           "predict %s, got fd=%d errno=%d (%s)\n",
                           TGT_NAMES[ti], am_names[ai], cstr,
                           expect == 0 ? "OK" : strerror(expect),
                           fd, errno, strerror(errno));
                    fail_local++;
                    __fail++;
                } else {
                    __pass++;
                }
                run_count++;

                /* 清理可能创建的文件 */
                if (HAS_CREAT(combo) && expect == 0 && tgt == TGT_ABSENT)
                    unlink(M_ABSENT);
            }
        }
    }

    printf("  ----- open_flag_matrix: ran=%d skipped=%d pass=%d fail=%d -----\n",
           run_count, skip_count, run_count - fail_local, fail_local);
    cleanup_tree(M_DIR);
    return fail_local;
}
