#include "path_common.h"

#include <limits.h>

#ifndef NAME_MAX
#define NAME_MAX 255
#endif

/*
 * mkdirat(2) — create a directory relative to a directory file descriptor.
 *
 * man 2 mkdirat:
 *   "mkdirat() attempts to create a directory named pathname."
 *   "If pathname is relative, then it is interpreted relative to the directory
 *    referred to by the file descriptor dirfd (rather than relative to the
 *    current working directory of the calling process)."
 *   "If pathname is absolute, then dirfd is ignored."
 *   "If dirfd is AT_FDCWD, then pathname is interpreted relative to the current
 *    working directory of the calling process."
 *
 * 测试覆盖：
 *   (a) dirfd=目录fd + 相对路径创建成功
 *   (b) dirfd=目录fd + 绝对路径创建成功（验证 absolute-path ignores dirfd）
 *   (c) dirfd=AT_FDCWD + 相对路径创建成功
 *   (d) 同一路径重复创建 → -1 EEXIST
 *   (e) dirfd=普通文件fd → -1 ENOTDIR
 *   (f) dirfd=-1 → -1 EBADF
 *   (g) 父目录不存在 → -1 ENOENT
 *   (h) 过深/循环符号链接解析 → -1 ELOOP（路径解析层语义）
 *   (j) 父目录无写权限/无执行权限 → -1 EACCES（root 场景下降权验证；非 root 直接验证）
 *   (k) 单路径分量超过 NAME_MAX → -1 ENAMETOOLONG
 *   (m) pathname 非法地址 → -1 EFAULT（raw syscall，避免 libc 参数预处理）
 *
 * 未覆盖/不便实现（环境/资源依赖或需要额外设施）：
 *   (i) 只读文件系统 → -1 EROFS（需要可控 rofs 挂载点或测试框架支持 needs_rofs）
 *   (l) 资源耗尽 → -1 ENOSPC/EDQUOT/ENOMEM（依赖磁盘配额/镜像容量/内存压力环境）
 */

struct mkdirat_eacces_case {
    const char *parent_dir;
    const char *dir_name;
    const char *child_name;
    mode_t parent_mode;
    int exp_errno;
    const char *desc;
};

static int probe_mkdirat_eacces(void *arg)
{
    struct mkdirat_eacces_case *tc = (struct mkdirat_eacces_case *)arg;
    char dir_path[512];
    snprintf(dir_path, sizeof(dir_path), "%s/%s", tc->parent_dir, tc->dir_name);

    if (mkdir(dir_path, 0755) != 0) {
        return -errno;
    }

    int dirfd = open(dir_path, O_RDONLY | O_DIRECTORY);
    if (dirfd < 0) {
        int saved_errno = errno;
        rmdir(dir_path);
        return -saved_errno;
    }

    if (fchmod(dirfd, tc->parent_mode) != 0) {
        int saved_errno = errno;
        close(dirfd);
        rmdir(dir_path);
        return -saved_errno;
    }

    errno = 0;
    int result = mkdirat(dirfd, tc->child_name, 0755) == -1 ? errno : 0;
    int saved_errno = 0;
    if (fchmod(dirfd, 0755) != 0) {
        saved_errno = errno;
    } else if (result == 0 && unlinkat(dirfd, tc->child_name, AT_REMOVEDIR) != 0) {
        saved_errno = errno;
    }

    close(dirfd);
    if (saved_errno != 0) {
        errno = saved_errno;
        return -saved_errno;
    }
    if (rmdir(dir_path) != 0) {
        return -errno;
    }
    return result;
}

static void run_mkdirat_eacces_case(struct mkdirat_eacces_case *tc)
{
    if (geteuid() == 0) {
        int probe_value = 0;
        int probe_status = path_run_as_dropped_user(&probe_value, probe_mkdirat_eacces, tc);
        CHECK(probe_status >= 0, "mkdirat: launch dropped-user EACCES probe");
        if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
            CHECK(0, "mkdirat: setuid failed in child for EACCES probe");
        } else if (probe_status == PATH_DROP_PROBE_OK) {
            if (probe_value != tc->exp_errno) {
                errno = probe_value > 0 ? probe_value : -probe_value;
            }
            CHECK(probe_value == tc->exp_errno, tc->desc);
        }
        return;
    }

    int probe_value = probe_mkdirat_eacces(tc);
    if (probe_value != tc->exp_errno) {
        errno = probe_value > 0 ? probe_value : -probe_value;
    }
    CHECK(probe_value == tc->exp_errno, tc->desc);
}

static void run_mkdirat_eacces_cases(struct mkdirat_eacces_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        run_mkdirat_eacces_case(&test_cases[i]);
    }
}

void test_mkdirat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    char abs_dir[256];
    path_join(abs_dir, sizeof(abs_dir), "d_abs");

    char path[256];
    path_join(path, sizeof(path), "regfile");

    mkdirat(dfd, "test_eloop", 0755);
    symlinkat("../test_eloop", dfd, "test_eloop/test_eloop");
    char loop_path[1024];
    strcpy(loop_path, "test_eloop");
    for (int i = 0; i < 45; i++) {
        strcat(loop_path, "/test_eloop");
    }

    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "mkdirat: capture old cwd");
    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "mkdirat: chdir(BASE)");

    char too_long_name[NAME_MAX + 32];
    memset(too_long_name, 'a', sizeof(too_long_name) - 1);
    too_long_name[sizeof(too_long_name) - 1] = '\0';

    struct path_perm_matrix_entry perm_entries[] = {
        {"mkdirat_drop_parent", 0777, PATH_PERM_DIR},
    };
    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(path_setup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
              0,
              "mkdirat: setup permission matrix");

    char eacces_parent[512];
    snprintf(eacces_parent, sizeof(eacces_parent), "%s/%s", PATH_FAMILY_BASE, "mkdirat_drop_parent");

    int dir_fd = dfd;
    int fd_atcwd = AT_FDCWD;
    int fd_invalid = -1;
    int file_fd = open(path, O_RDONLY);
    CHECK(file_fd >= 0, "mkdirat(ENOTDIR): open regfile");
    if (file_fd < 0) {
        chdir(old_cwd);
        close(dfd);
        return;
    }

    struct test_case {
        int *dir_fd;
        const char *name;
        int exp_ret;
        int exp_errno;
        int use_raw;
        int use_stat;
        int enabled;
        const char *desc;
    };

    struct test_case test_cases[] = {
        /* (a) dirfd=目录fd + 相对路径：创建成功 */
        {&dir_fd, "d1", 0, 0, 0, 0, 1, "mkdirat: create d1"},
        /* (b) dirfd=目录fd + 绝对路径：创建成功（dirfd 应被忽略） */
        {&dir_fd, abs_dir, 0, 0, 0, 1, 1, "mkdirat: absolute path -> success"},
        /* (c) dirfd=AT_FDCWD + 相对路径：创建成功 */
        {&fd_atcwd, "d_atcwd", 0, 0, 0, 0, 1, "mkdirat: AT_FDCWD + relpath -> success"},
        /* (d) 重复创建同一路径：EEXIST */
        {&dir_fd, "d1", -1, EEXIST, 0, 0, 1, "mkdirat: existing dir -> EEXIST"},
        /* (e) dirfd 不是目录：ENOTDIR */
        {&file_fd, "x", -1, ENOTDIR, 0, 0, 1, "mkdirat: dirfd is file -> ENOTDIR"},
        /* (f) dirfd 非法：EBADF */
        {&fd_invalid, "x", -1, EBADF, 0, 0, 1, "mkdirat: dirfd=-1 -> EBADF"},
        /* (g) 父目录不存在：ENOENT */
        {&dir_fd, "no_such_parent/x", -1, ENOENT, 0, 0, 1, "mkdirat: missing parent -> ENOENT"},
        /* (h) 符号链接循环/过深：ELOOP */
        {&dir_fd, loop_path, -1, ELOOP, 0, 0, 1, "mkdirat: symlink loop -> ELOOP"},
        /* (k) 单路径分量超长：ENAMETOOLONG */
        {&dir_fd, too_long_name, -1, ENAMETOOLONG, 0, 0, 1, "mkdirat: component too long -> ENAMETOOLONG"},
        /* (m) pathname 非法地址：EFAULT（raw syscall） */
        {&dir_fd, (const char *)-1, -1, EFAULT, 1, 0, 1, "mkdirat: bad pathname -> EFAULT"},
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];
        if (!tc->enabled) {
            printf("  SKIP | %s:%d | %s\n", __FILE__, __LINE__, tc->desc);
            continue;
        }

        if (tc->exp_ret == 0) {
            CHECK_RET(mkdirat(*tc->dir_fd, tc->name, 0755), 0, tc->desc);

            struct stat st;
            if (tc->use_stat) {
                CHECK_RET(stat(tc->name, &st), 0, "mkdirat: stat(abs)");
            } else {
                CHECK_RET(fstatat(*tc->dir_fd, tc->name, &st, 0), 0, "mkdirat: fstatat");
            }
            CHECK(S_ISDIR(st.st_mode), "mkdirat: created object is directory");
        } else if (tc->use_raw) {
            CHECK_ERR(syscall(SYS_mkdirat, *tc->dir_fd, tc->name, 0755), tc->exp_errno, tc->desc);
        } else {
            CHECK_ERR(mkdirat(*tc->dir_fd, tc->name, 0755), tc->exp_errno, tc->desc);
        }
    }

    struct mkdirat_eacces_case eacces_cases[] = {
        // 测试用例：父目录无写权限
        {
            .parent_dir = eacces_parent,
            .dir_name = "mkdirat_eacces_parent",
            .child_name = "mkdirat_eacces_child",
            .parent_mode = 0555,
            .exp_errno = EACCES,
            .desc = "mkdirat: parent dir without write permission -> EACCES",
        },
        // 测试用例：父目录无执行权限
        {
            .parent_dir = eacces_parent,
            .dir_name = "mkdirat_noexec_parent",
            .child_name = "mkdirat_noexec_child",
            .parent_mode = 0666,
            .exp_errno = EACCES,
            .desc = "mkdirat: parent dir without execute permission -> EACCES",
        },
    };
    run_mkdirat_eacces_cases(eacces_cases, sizeof(eacces_cases) / sizeof(eacces_cases[0]));

    if (file_fd >= 0) {
        close(file_fd);
    }
    path_cleanup_perm_matrix_at(AT_FDCWD, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    chdir(old_cwd);
    close(dfd);
}
