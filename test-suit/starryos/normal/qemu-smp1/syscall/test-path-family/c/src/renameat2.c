#include "path_common.h"

#include <limits.h>

/*
 * renameat2(2) — rename a file relative to directory file descriptors, with flags.
 *
 * man 2 renameat2:
 *   "renameat2() is an extended version of renameat(2) that provides a
 *    superset of renameat(2)'s functionality."
 *   "A renameat2() call with a zero flags argument is equivalent to renameat()."
 *   "The flags argument is a bit mask consisting of zero or more of:
 *    RENAME_NOREPLACE, RENAME_EXCHANGE, RENAME_WHITEOUT."
 *
 * 测试覆盖（Linux 兼容最小集 + 针对当前实现的显式分支）：
 *   flags 语义：
 *     (a) RENAME_NOREPLACE 且 newpath 已存在 → -1 EEXIST
 *     (b) RENAME_NOREPLACE 且 newpath 不存在 → 0
 *     (c) RENAME_NOREPLACE 且 oldpath 不存在 → -1 ENOENT
 *     (d) 非法 flags（未知位）→ -1 EINVAL
 *     (e) RENAME_EXCHANGE → -1 EINVAL（当前内核未实现）
 *     (f) RENAME_NOREPLACE | RENAME_EXCHANGE → -1 EINVAL
 *     (g) RENAME_WHITEOUT | RENAME_EXCHANGE → -1 EINVAL
 *   flags=0 与 renameat(2) 等价的路径语义：
 *     (h) 普通文件 -> 已存在目录 → -1 EISDIR
 *     (i) 目录 -> 已存在普通文件 → -1 ENOTDIR
 *     (j) 目录 -> 已存在非空目录 → -1 ENOTEMPTY
 *     (k) 单路径分量超过 NAME_MAX → -1 ENAMETOOLONG
 *     (l) root + setuid 降权：父目录无写权限 → -1 EACCES
 *     (m) root + setuid 降权：父目录无执行权限 → -1 EACCES
 *     (n) root + setuid 降权：sticky 目录中重命名非本人文件 → -1 EPERM
 *     (o) rofs 路径对（环境提供）→ -1 EROFS
 *
 * 未覆盖/不便实现（语义复杂或依赖文件系统支持）：
 *   (p) RENAME_EXCHANGE 的成功语义、newpath 不存在时 ENOENT 等（需要内核/FS 实现该 flag）
 *   (q) RENAME_WHITEOUT 成功语义（需要 overlay/whiteout 语义与 FS 支持）
 */

#ifndef NAME_MAX
#define NAME_MAX 255
#endif

#ifndef RENAME_NOREPLACE
#define RENAME_NOREPLACE 1
#endif

#ifndef RENAME_EXCHANGE
#define RENAME_EXCHANGE 2
#endif

#ifndef RENAME_WHITEOUT
#define RENAME_WHITEOUT 4
#endif

struct renameat2_flag_case {
    unsigned int flags;
    int scenario;
    int exp_ret;
    int exp_errno;
    const char *desc;
};

struct renameat2_path_case {
    int scenario;
    int exp_errno;
    const char *desc;
};

struct renameat2_perm_case {
    const char *parent_dir;
    const char *dir_name;
    const char *src_name;
    const char *dst_name;
    mode_t dir_mode;
    int exp_errno;
    const char *desc;
};

struct renameat2_probe_args {
    int dirfd;
    const char *src_name;
    const char *dst_name;
};

enum {
    RENAMEAT2_SCN_DST_EXISTS = 0,
    RENAMEAT2_SCN_DST_MISSING,
    RENAMEAT2_SCN_SRC_MISSING,
    RENAMEAT2_SCN_INVALID_FLAGS,
    RENAMEAT2_SCN_FILE_TO_DIR,
    RENAMEAT2_SCN_DIR_TO_FILE,
    RENAMEAT2_SCN_DIR_TO_NONEMPTY_DIR,
    RENAMEAT2_SCN_NAME_TOO_LONG,
};

static void cleanup_name(int dirfd, const char *name)
{
    unlinkat(dirfd, name, 0);
    unlinkat(dirfd, name, AT_REMOVEDIR);
}

static int join_two_paths(char *out, size_t out_size, const char *lhs, const char *rhs)
{
    size_t lhs_len = strlen(lhs);
    size_t rhs_len = strlen(rhs);
    if (lhs_len + 1 + rhs_len + 1 > out_size) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(out, lhs, lhs_len);
    out[lhs_len] = '/';
    memcpy(out + lhs_len + 1, rhs, rhs_len);
    out[lhs_len + 1 + rhs_len] = '\0';
    return 0;
}

static void write_file_at_checked(int dirfd, const char *name, const char *data, size_t len, const char *msg)
{
    int fd = openat(dirfd, name, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, msg);
    if (fd >= 0) {
        if (len > 0) {
            write(fd, data, len);
        }
        close(fd);
    }
}

static void mkdirat_checked(int dirfd, const char *name, mode_t mode, const char *msg)
{
    CHECK(mkdirat(dirfd, name, mode) == 0 || errno == EEXIST, msg);
}

static void reset_cases(int dfd)
{
    cleanup_name(dfd, "srcfile");
    cleanup_name(dfd, "dstfile");
    cleanup_name(dfd, "dstfile2");
}

static void prepare_flag_case(int dfd, int scenario)
{
    reset_cases(dfd);

    switch (scenario) {
    case RENAMEAT2_SCN_DST_EXISTS:
        write_file_at_checked(dfd, "srcfile", "renameat2", 9, "renameat2: prepare src");
        write_file_at_checked(dfd, "dstfile", "", 0, "renameat2: prepare existing dst");
        break;
    case RENAMEAT2_SCN_DST_MISSING:
        write_file_at_checked(dfd, "srcfile", "renameat2", 9, "renameat2: prepare src");
        break;
    case RENAMEAT2_SCN_SRC_MISSING:
        write_file_at_checked(dfd, "dstfile2", "", 0, "renameat2: prepare dstfile2");
        break;
    case RENAMEAT2_SCN_INVALID_FLAGS:
        write_file_at_checked(dfd, "srcfile", "renameat2", 9, "renameat2: prepare src");
        write_file_at_checked(dfd, "dstfile2", "", 0, "renameat2: prepare dstfile2");
        break;
    default:
        CHECK(0, "renameat2: unknown flag-case scenario");
        break;
    }
}

static void prepare_path_case(int dfd, int scenario)
{
    reset_cases(dfd);
    cleanup_name(dfd, "dir_src");
    cleanup_name(dfd, "dir_dst");

    switch (scenario) {
    case RENAMEAT2_SCN_FILE_TO_DIR:
        write_file_at_checked(dfd, "srcfile", "x", 1, "renameat2: prepare file->dir src");
        mkdirat_checked(dfd, "dstfile", 0755, "renameat2: prepare existing dir");
        break;
    case RENAMEAT2_SCN_DIR_TO_FILE:
        mkdirat_checked(dfd, "srcfile", 0755, "renameat2: prepare dir->file src");
        write_file_at_checked(dfd, "dstfile", "x", 1, "renameat2: prepare dir->file dst");
        break;
    case RENAMEAT2_SCN_DIR_TO_NONEMPTY_DIR:
        mkdirat_checked(dfd, "srcfile", 0755, "renameat2: prepare dir->nonempty src");
        mkdirat_checked(dfd, "dstfile", 0755, "renameat2: prepare dir->nonempty dst");
        write_file_at_checked(dfd, "dstfile/x", "x", 1, "renameat2: prepare nonempty dst");
        break;
    case RENAMEAT2_SCN_NAME_TOO_LONG:
        write_file_at_checked(dfd, "srcfile", "x", 1, "renameat2: prepare long-name src");
        break;
    default:
        CHECK(0, "renameat2: unknown path-case scenario");
        break;
    }
}

static int probe_renameat2_perm(void *arg)
{
    struct renameat2_probe_args *probe = (struct renameat2_probe_args *)arg;
    return syscall(SYS_renameat2, probe->dirfd, probe->src_name, probe->dirfd, probe->dst_name, 0) == -1
               ? errno
               : 0;
}

static void run_perm_case(const struct renameat2_perm_case *tc)
{
    char dir_path[1024];
    if (join_two_paths(dir_path, sizeof(dir_path), tc->parent_dir, tc->dir_name) != 0) {
        CHECK(0, "renameat2: build permission-test path");
        return;
    }

    mkdirat_checked(AT_FDCWD, dir_path, 0777, "renameat2: prepare permission dir");
    int dirfd = open(dir_path, O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "renameat2: open permission dir");
    if (dirfd < 0) {
        cleanup_name(AT_FDCWD, dir_path);
        return;
    }

    cleanup_name(dirfd, tc->src_name);
    cleanup_name(dirfd, tc->dst_name);
    write_file_at_checked(dirfd, tc->src_name, "perm", 4, "renameat2: prepare permission src");
    CHECK_RET(fchmod(dirfd, tc->dir_mode), 0, "renameat2: chmod permission dir");

    struct renameat2_probe_args probe = {
        .dirfd = dirfd,
        .src_name = tc->src_name,
        .dst_name = tc->dst_name,
    };
    int probe_value = 0;
    int probe_status = path_run_as_dropped_user(&probe_value, probe_renameat2_perm, &probe);
    CHECK(probe_status >= 0, "renameat2: launch dropped-user permission probe");
    if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
        CHECK(0, "renameat2: setuid failed in child for permission probe");
    } else if (probe_status == PATH_DROP_PROBE_OK) {
        if (probe_value != tc->exp_errno) {
            errno = probe_value > 0 ? probe_value : -probe_value;
        }
        CHECK(probe_value == tc->exp_errno, tc->desc);
    }

    CHECK_RET(fchmod(dirfd, 0777), 0, "renameat2: restore permission dir");
    cleanup_name(dirfd, tc->src_name);
    cleanup_name(dirfd, tc->dst_name);
    close(dirfd);
    cleanup_name(AT_FDCWD, dir_path);
}

static void run_rofs_case(void)
{
    const char *oldpath = getenv("PATH_FAMILY_RENAMEAT2_ROFS_OLD");
    const char *newpath = getenv("PATH_FAMILY_RENAMEAT2_ROFS_NEW");
    if (oldpath == NULL || newpath == NULL || oldpath[0] != '/' || newpath[0] != '/') {
        printf("  SKIP | %s:%d | renameat2: no readonly path pair provided for EROFS coverage\n",
               __FILE__,
               __LINE__);
        return;
    }
    CHECK_ERR(syscall(SYS_renameat2, AT_FDCWD, oldpath, AT_FDCWD, newpath, 0),
              EROFS,
              "renameat2(flags=0): readonly fs -> EROFS");
}

void test_renameat2(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    char too_long_name[NAME_MAX + 32];
    memset(too_long_name, 'a', sizeof(too_long_name) - 1);
    too_long_name[sizeof(too_long_name) - 1] = '\0';

    struct renameat2_flag_case flag_cases[] = {
        {(unsigned int)RENAME_NOREPLACE, RENAMEAT2_SCN_DST_EXISTS, -1, EEXIST,
         "renameat2(RENAME_NOREPLACE): dst exists -> EEXIST"},
        {(unsigned int)RENAME_NOREPLACE, RENAMEAT2_SCN_DST_MISSING, 0, 0,
         "renameat2(RENAME_NOREPLACE): rename success when dst missing"},
        {(unsigned int)RENAME_NOREPLACE, RENAMEAT2_SCN_SRC_MISSING, -1, ENOENT,
         "renameat2(RENAME_NOREPLACE): missing source -> ENOENT"},
        {0x80000000u, RENAMEAT2_SCN_INVALID_FLAGS, -1, EINVAL, "renameat2: invalid flags -> EINVAL"},
        {(unsigned int)RENAME_EXCHANGE, RENAMEAT2_SCN_INVALID_FLAGS, -1, EINVAL,
         "renameat2(RENAME_EXCHANGE): unsupported -> EINVAL"},
        {(unsigned int)(RENAME_NOREPLACE | RENAME_EXCHANGE), RENAMEAT2_SCN_INVALID_FLAGS, -1, EINVAL,
         "renameat2(NOREPLACE|EXCHANGE): invalid -> EINVAL"},
        {(unsigned int)(RENAME_WHITEOUT | RENAME_EXCHANGE), RENAMEAT2_SCN_INVALID_FLAGS, -1, EINVAL,
         "renameat2(WHITEOUT|EXCHANGE): invalid -> EINVAL"},
    };

    for (size_t i = 0; i < sizeof(flag_cases) / sizeof(flag_cases[0]); i++) {
        const struct renameat2_flag_case *tc = &flag_cases[i];
        prepare_flag_case(dfd, tc->scenario);

        errno = 0;
        long r = syscall(SYS_renameat2, dfd, tc->scenario == RENAMEAT2_SCN_SRC_MISSING ? "no_src" : "srcfile", dfd,
                         tc->scenario == RENAMEAT2_SCN_DST_EXISTS ? "dstfile" : "dstfile2", tc->flags);
        if (tc->exp_ret == 0) {
            struct stat st;
            CHECK_RET(r, 0, tc->desc);
            errno = 0;
            CHECK(fstatat(dfd, "srcfile", &st, 0) == -1 && errno == ENOENT,
                  "renameat2: source missing after rename");
            CHECK_RET(fstatat(dfd, "dstfile2", &st, 0), 0, "renameat2: dest exists after rename");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }
    }

    struct renameat2_path_case path_cases[] = {
        {RENAMEAT2_SCN_FILE_TO_DIR, EISDIR, "renameat2(flags=0): file -> existing dir -> EISDIR"},
        {RENAMEAT2_SCN_DIR_TO_FILE, ENOTDIR, "renameat2(flags=0): dir -> existing file -> ENOTDIR"},
        {RENAMEAT2_SCN_DIR_TO_NONEMPTY_DIR, ENOTEMPTY, "renameat2(flags=0): dir -> nonempty dir -> ENOTEMPTY"},
        {RENAMEAT2_SCN_NAME_TOO_LONG, ENAMETOOLONG, "renameat2(flags=0): component too long -> ENAMETOOLONG"},
    };

    for (size_t i = 0; i < sizeof(path_cases) / sizeof(path_cases[0]); i++) {
        const struct renameat2_path_case *tc = &path_cases[i];
        prepare_path_case(dfd, tc->scenario);

        errno = 0;
        long r;
        if (tc->scenario == RENAMEAT2_SCN_NAME_TOO_LONG) {
            r = syscall(SYS_renameat2, dfd, "srcfile", dfd, too_long_name, 0);
        } else {
            r = syscall(SYS_renameat2, dfd, "srcfile", dfd, "dstfile", 0);
        }
        CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
    }

    if (geteuid() == 0) {
        struct path_perm_matrix_entry perm_entries[] = {
            {"renameat2_drop_parent", 0777, PATH_PERM_DIR},
        };
        path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
        CHECK_RET(path_setup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
                  0,
                  "renameat2: setup permission matrix");

        char perm_parent[512];
        path_join(perm_parent, sizeof(perm_parent), "renameat2_drop_parent");
        struct renameat2_perm_case perm_cases[] = {
            {
                .parent_dir = perm_parent,
                .dir_name = "renameat2_nowrite_parent",
                .src_name = "src",
                .dst_name = "dst",
                .dir_mode = 0555,
                .exp_errno = EACCES,
                .desc = "renameat2(flags=0): parent dir without write permission -> EACCES",
            },
            {
                .parent_dir = perm_parent,
                .dir_name = "renameat2_noexec_parent",
                .src_name = "src",
                .dst_name = "dst",
                .dir_mode = 0666,
                .exp_errno = EACCES,
                .desc = "renameat2(flags=0): parent dir without execute permission -> EACCES",
            },
            {
                .parent_dir = perm_parent,
                .dir_name = "renameat2_sticky_parent",
                .src_name = "src",
                .dst_name = "dst",
                .dir_mode = 01777,
                .exp_errno = EPERM,
                .desc = "renameat2(flags=0): sticky dir non-owner rename -> EPERM",
            },
        };
        for (size_t i = 0; i < sizeof(perm_cases) / sizeof(perm_cases[0]); i++) {
            run_perm_case(&perm_cases[i]);
        }
        path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    } else {
        printf("  SKIP | %s:%d | renameat2: needs_root=1 for permission coverage\n",
               __FILE__,
               __LINE__);
    }

    run_rofs_case();

    reset_cases(dfd);
    cleanup_name(dfd, "dir_src");
    cleanup_name(dfd, "dir_dst");
    close(dfd);
}
