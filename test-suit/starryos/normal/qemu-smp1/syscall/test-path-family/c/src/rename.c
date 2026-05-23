#include "path_common.h"

#include <limits.h>

#ifndef NAME_MAX
#define NAME_MAX 255
#endif

/*
 * rename(2) — rename a file.
 *
 * man 2 rename:
 *   "rename() renames a file, moving it between directories if required."
 *   "If newpath already exists, it will be atomically replaced."
 *   "oldpath can specify a directory. In this case, newpath must either not
 *    exist, or it must specify an empty directory."
 * 测试覆盖（Linux 兼容最小集 + 低成本增强）：
 *   (a) 绝对路径重命名成功 → 0，源消失、目标存在
 *   (b) 目标已存在（普通文件）→ 0，目标被原子替换
 *   (c) 源路径不存在 → -1 ENOENT
 *   (d) 目标父目录不存在 → -1 ENOENT
 *   (e) 普通文件 -> 已存在目录 → -1 EISDIR
 *   (f) 目录 -> 已存在普通文件 → -1 ENOTDIR
 *   (g) 目录 -> 已存在非空目录 → -1 ENOTEMPTY
 *   (h) 单路径分量超过 NAME_MAX → -1 ENAMETOOLONG
 *   (i) rofs 路径对（环境提供）→ -1 EROFS
 *
 * 未覆盖/不便实现（环境/挂载依赖或语义更复杂）：
 *   (j) 跨设备重命名 → -1 EXDEV（需要可控多挂载点/多设备环境）
 *   (k) oldpath 为目录且自身无写权限导致的 EACCES（需要额外区分 ".." 更新语义）
 */

struct rename_case {
    int kind;
    int exp_errno;
    const char *desc;
};

enum {
    RENAME_ABS_SUCCESS = 0,
    RENAME_OVERWRITE_FILE,
    RENAME_MISSING_SOURCE,
    RENAME_MISSING_PARENT,
    RENAME_FILE_TO_EXISTING_DIR,
    RENAME_DIR_TO_EXISTING_FILE,
    RENAME_DIR_TO_NONEMPTY_DIR,
    RENAME_NAME_TOO_LONG,
};

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

static void cleanup_name(int dirfd, const char *name)
{
    unlinkat(dirfd, name, 0);
    unlinkat(dirfd, name, AT_REMOVEDIR);
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

static void prepare_rename_case(
    int dfd,
    int kind,
    const char *src_name,
    const char *dst_name,
    const char *long_name
)
{
    cleanup_name(dfd, src_name);
    cleanup_name(dfd, dst_name);
    cleanup_name(dfd, "midfile");
    cleanup_name(dfd, "nonempty_src");
    cleanup_name(dfd, "nonempty_dst");

    switch (kind) {
    case RENAME_ABS_SUCCESS:
        write_file_at_checked(dfd, src_name, "rename", 6, "rename: prepare src");
        break;
    case RENAME_OVERWRITE_FILE:
        write_file_at_checked(dfd, src_name, "new", 3, "rename: prepare overwrite src");
        write_file_at_checked(dfd, dst_name, "oldold", 6, "rename: prepare overwrite dst");
        break;
    case RENAME_MISSING_SOURCE:
        break;
    case RENAME_MISSING_PARENT:
        write_file_at_checked(dfd, src_name, "x", 1, "rename: prepare missing-parent src");
        break;
    case RENAME_FILE_TO_EXISTING_DIR:
        write_file_at_checked(dfd, src_name, "x", 1, "rename: prepare file->dir src");
        mkdirat_checked(dfd, dst_name, 0755, "rename: prepare existing dir");
        break;
    case RENAME_DIR_TO_EXISTING_FILE:
        mkdirat_checked(dfd, src_name, 0755, "rename: prepare dir->file src");
        write_file_at_checked(dfd, dst_name, "x", 1, "rename: prepare dir->file dst");
        break;
    case RENAME_DIR_TO_NONEMPTY_DIR: {
        char dst_child[256];
        mkdirat_checked(dfd, src_name, 0755, "rename: prepare dir->nonempty src");
        mkdirat_checked(dfd, dst_name, 0755, "rename: prepare dir->nonempty dst");
        CHECK(join_two_paths(dst_child, sizeof(dst_child), dst_name, "x") == 0,
              "rename: build nonempty-dst child path");
        write_file_at_checked(dfd, dst_child, "x", 1, "rename: prepare nonempty dst file");
        break;
    }
    case RENAME_NAME_TOO_LONG:
        write_file_at_checked(dfd, src_name, "x", 1, "rename: prepare long-name src");
        (void)long_name;
        break;
    default:
        CHECK(0, "rename: unknown case kind");
        break;
    }
}

static void run_rename_rofs_case(void)
{
    const char *oldpath = getenv("PATH_FAMILY_RENAME_ROFS_OLD");
    const char *newpath = getenv("PATH_FAMILY_RENAME_ROFS_NEW");
    if (oldpath == NULL || newpath == NULL || oldpath[0] != '/' || newpath[0] != '/') {
        printf("  SKIP | %s:%d | rename: no readonly path pair provided for EROFS coverage\n",
               __FILE__,
               __LINE__);
        return;
    }
    CHECK_ERR(rename(oldpath, newpath), EROFS, "rename: readonly fs -> EROFS");
}

void test_rename(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    char src_path[256];
    char dst_path[256];
    char src2_path[256];
    char dst2_path[256];
    char dst_child_path[256];
    char no_parent_dst[256];
    path_join(src_path, sizeof(src_path), "rename_src");
    path_join(dst_path, sizeof(dst_path), "rename_dst");
    path_join(src2_path, sizeof(src2_path), "rename_src2");
    path_join(dst2_path, sizeof(dst2_path), "rename_dst2");
    path_join(dst_child_path, sizeof(dst_child_path), "rename_dst/x");
    path_join(no_parent_dst, sizeof(no_parent_dst), "no_such_parent/rename_dst");

    char too_long_name[NAME_MAX + 32];
    memset(too_long_name, 'a', sizeof(too_long_name) - 1);
    too_long_name[sizeof(too_long_name) - 1] = '\0';

    struct rename_case rename_cases[] = {
        {RENAME_ABS_SUCCESS, 0, "rename: abs success"},
        {RENAME_OVERWRITE_FILE, 0, "rename: overwrite existing file"},
        {RENAME_MISSING_SOURCE, ENOENT, "rename: missing source -> ENOENT"},
        {RENAME_MISSING_PARENT, ENOENT, "rename: missing parent -> ENOENT"},
        {RENAME_FILE_TO_EXISTING_DIR, EISDIR, "rename: file -> existing dir -> EISDIR"},
        {RENAME_DIR_TO_EXISTING_FILE, ENOTDIR, "rename: dir -> existing file -> ENOTDIR"},
        {RENAME_DIR_TO_NONEMPTY_DIR, ENOTEMPTY, "rename: dir -> nonempty dir -> ENOTEMPTY"},
        {RENAME_NAME_TOO_LONG, ENAMETOOLONG, "rename: component too long -> ENAMETOOLONG"},
    };

    for (size_t i = 0; i < sizeof(rename_cases) / sizeof(rename_cases[0]); i++) {
        const struct rename_case *tc = &rename_cases[i];
        prepare_rename_case(dfd, tc->kind, "rename_src", "rename_dst", too_long_name);

        errno = 0;
        int r;
        if (tc->kind == RENAME_MISSING_PARENT) {
            r = rename(src2_path, no_parent_dst);
        } else if (tc->kind == RENAME_NAME_TOO_LONG) {
            char long_dst[1024];
            CHECK(join_two_paths(long_dst, sizeof(long_dst), PATH_FAMILY_BASE, too_long_name) == 0,
                  "rename: build long destination path");
            r = rename(src_path, long_dst);
        } else {
            r = rename(src_path, dst_path);
        }

        if (tc->exp_errno == 0) {
            struct stat st;
            CHECK_RET(r, 0, tc->desc);
            errno = 0;
            CHECK(stat(src_path, &st) == -1 && errno == ENOENT, "rename: source missing after rename");
            CHECK_RET(stat(dst_path, &st), 0, "rename: target exists after rename");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }

        cleanup_name(dfd, "rename_src");
        cleanup_name(dfd, "rename_dst");
        cleanup_name(dfd, "rename_src2");
        cleanup_name(dfd, "rename_dst2");
        unlink(dst_child_path);
        cleanup_name(dfd, "no_such_parent");
        cleanup_name(dfd, "midfile");
        cleanup_name(dfd, "nonempty_src");
        cleanup_name(dfd, "nonempty_dst");
    }

    run_rename_rofs_case();
    close(dfd);
}
