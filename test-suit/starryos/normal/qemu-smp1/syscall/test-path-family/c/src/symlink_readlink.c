#include "path_common.h"

/*
 * symlinkat(2), readlinkat(2) — symbolic links.
 *
 * man 2 symlinkat:
 *   "symlinkat() creates a symbolic link named linkpath which contains the
 *    string target."
 *   "If linkpath is relative, it is interpreted relative to newdirfd."
 *   "If linkpath is absolute, then newdirfd is ignored."
 *
 * man 2 readlinkat:
 *   "readlinkat() places the contents of the symbolic link pathname in the
 *    buffer buf, which has size bufsiz."
 *   "readlinkat() does not append a null byte to buf."
 *   "The caller must allocate a buffer large enough to hold the contents."
 *   "If bufsiz is not positive, readlinkat() fails with EINVAL."
 *   "If pathname does not refer to a symbolic link, readlinkat() fails with EINVAL."
 *
 * 测试覆盖（Linux 兼容最小集 + 针对内核显式分支）：
 *   symlinkat(2):
 *     (a) 创建符号链接成功 → 0
 *     (b) 目标已存在 → -1 EEXIST
 *     (c) dirfd=-1 → -1 EBADF
 *     (d) root + setuid 降权：父目录无写权限/无执行权限 → -1 EACCES
 *   readlinkat(2):
 *     (e) 正常读取成功 → 返回正长度（不带 '\0'），内容匹配 target
 *     (f) 截断读取 → 返回 bufsiz，且内容为 target 前缀
 *     (g) bufsiz=0 → -1 EINVAL（raw syscall，避免 libc 行为差异）
 *     (h) 非符号链接 → -1 EINVAL
 *     (i) 不存在路径 → -1 ENOENT
 *     (j) fstatat 跟随/不跟随符号链接：S_IFREG vs S_IFLNK
 *     (k) root + setuid 降权：父目录无执行权限 → -1 EACCES
 *
 * 未覆盖/不便实现（环境/语义依赖）：
 *   (l) 超长 target / ENAMETOOLONG（依赖路径/FS 限制）
 *   (m) 只读文件系统（EROFS）
 */

struct symlink_basic_test_case {
    int dirfd;
    const char *target;
    const char *linkpath;
    int exp_ret;
    int exp_errno;
    const char *desc;
};

struct symlink_perm_test_case {
    const char *parent_dir;
    const char *dir_name;
    const char *link_name;
    mode_t mode;
    int exp_errno;
    const char *desc;
};

struct readlink_perm_test_case {
    const char *parent_dir;
    const char *dir_name;
    const char *link_name;
    const char *target;
    mode_t mode;
    int exp_errno;
    const char *desc;
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

static int probe_symlinkat_eacces(void *arg)
{
    struct symlink_perm_test_case *tc = (struct symlink_perm_test_case *)arg;
    char dir_path[512];
    if (join_two_paths(dir_path, sizeof(dir_path), tc->parent_dir, tc->dir_name) != 0) {
        return -errno;
    }

    if (mkdir(dir_path, 0755) != 0) {
        return -errno;
    }

    int dirfd = open(dir_path, O_RDONLY | O_DIRECTORY);
    if (dirfd < 0) {
        int saved_errno = errno;
        rmdir(dir_path);
        return -saved_errno;
    }
    if (fchmod(dirfd, tc->mode) != 0) {
        int saved_errno = errno;
        close(dirfd);
        rmdir(dir_path);
        return -saved_errno;
    }

    errno = 0;
    int result = symlinkat("target", dirfd, tc->link_name) == -1 ? errno : 0;

    int saved_errno = 0;
    if (fchmod(dirfd, 0755) != 0) {
        saved_errno = errno;
    } else if (result == 0 && unlinkat(dirfd, tc->link_name, 0) != 0) {
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

static int probe_readlinkat_eacces(void *arg)
{
    struct readlink_perm_test_case *tc = (struct readlink_perm_test_case *)arg;
    char dir_path[1024];
    char link_path[1024];
    char read_path[1024];
    char buf[256];
    if (join_two_paths(dir_path, sizeof(dir_path), tc->parent_dir, tc->dir_name) != 0) {
        return -errno;
    }
    if (join_two_paths(link_path, sizeof(link_path), dir_path, tc->link_name) != 0) {
        return -errno;
    }
    if (join_two_paths(read_path, sizeof(read_path), dir_path, tc->link_name) != 0) {
        return -errno;
    }

    if (mkdir(dir_path, 0755) != 0) {
        return -errno;
    }
    if (symlink(tc->target, link_path) != 0) {
        int saved_errno = errno;
        rmdir(dir_path);
        return -saved_errno;
    }
    if (chmod(dir_path, tc->mode) != 0) {
        int saved_errno = errno;
        unlink(link_path);
        rmdir(dir_path);
        return -saved_errno;
    }

    errno = 0;
    int result = readlink(read_path, buf, sizeof(buf)) == -1 ? errno : 0;

    int saved_errno = 0;
    if (chmod(dir_path, 0755) != 0) {
        saved_errno = errno;
    } else if (unlink(link_path) != 0) {
        saved_errno = errno;
    } else if (rmdir(dir_path) != 0) {
        saved_errno = errno;
    }
    if (saved_errno != 0) {
        errno = saved_errno;
        return -saved_errno;
    }
    return result;
}

static void run_symlink_perm_test_cases(const struct symlink_perm_test_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        const struct symlink_perm_test_case *tc = &test_cases[i];
        int probe_value = 0;
        int probe_status = path_run_as_dropped_user(&probe_value, probe_symlinkat_eacces, (void *)tc);
        CHECK(probe_status >= 0, "symlinkat: launch dropped-user EACCES probe");
        if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
            CHECK(0, "symlinkat: setuid failed in child for EACCES probe");
        } else if (probe_status == PATH_DROP_PROBE_OK) {
            if (probe_value != tc->exp_errno) {
                errno = probe_value > 0 ? probe_value : -probe_value;
            }
            CHECK(probe_value == tc->exp_errno, tc->desc);
        }
    }
}

static void run_readlink_perm_test_cases(const struct readlink_perm_test_case *test_cases, size_t count)
{
    for (size_t i = 0; i < count; i++) {
        const struct readlink_perm_test_case *tc = &test_cases[i];
        int probe_value = 0;
        int probe_status = path_run_as_dropped_user(&probe_value, probe_readlinkat_eacces, (void *)tc);
        CHECK(probe_status >= 0, "readlinkat: launch dropped-user EACCES probe");
        if (probe_status == PATH_DROP_PROBE_SETUID_FAILED) {
            CHECK(0, "readlinkat: setuid failed in child for EACCES probe");
        } else if (probe_status == PATH_DROP_PROBE_OK) {
            if (probe_value != tc->exp_errno) {
                errno = probe_value > 0 ? probe_value : -probe_value;
            }
            CHECK(probe_value == tc->exp_errno, tc->desc);
        }
    }
}

void test_symlinkat_readlinkat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    int fd = openat(dfd, "realfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0, "symlinkat: create realfile");
    if (fd >= 0) {
        write(fd, "symlink_data", 12);
        close(fd);
    }

    struct path_perm_matrix_entry perm_entries[] = {
        {"symlink_drop_parent", 0777, PATH_PERM_DIR},
        {"readlink_drop_parent", 0777, PATH_PERM_DIR},
    };
    path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    CHECK_RET(path_setup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0])),
              0,
              "symlink/readlink: setup permission matrix");

    char symlink_parent[512];
    char readlink_parent[512];
    path_join(symlink_parent, sizeof(symlink_parent), "symlink_drop_parent");
    path_join(readlink_parent, sizeof(readlink_parent), "readlink_drop_parent");

    struct symlink_basic_test_case symlink_cases[] = {
        /* (a) 创建符号链接成功 */
        {dfd, "realfile", "symlink", 0, 0, "symlinkat: create symlink"},
        /* (b) 目标已存在 */
        {dfd, "realfile", "symlink", -1, EEXIST, "symlinkat: existing symlink -> EEXIST"},
        /* (c) dirfd=-1 */
        {-1, "realfile", "bad_symlink", -1, EBADF, "symlinkat: dirfd=-1 -> EBADF"},
    };

    for (size_t i = 0; i < sizeof(symlink_cases) / sizeof(symlink_cases[0]); i++) {
        struct symlink_basic_test_case *tc = &symlink_cases[i];
        if (i == 0) {
            unlinkat(dfd, "symlink", 0);
        }
        if (tc->exp_ret == 0) {
            CHECK_RET(symlinkat(tc->target, tc->dirfd, tc->linkpath), 0, tc->desc);
        } else {
            CHECK_ERR(symlinkat(tc->target, tc->dirfd, tc->linkpath), tc->exp_errno, tc->desc);
        }
    }

    struct stat st;
    CHECK_RET(fstatat(dfd, "symlink", &st, 0), 0, "symlinkat: fstatat follow");
    CHECK(S_ISREG(st.st_mode), "symlinkat: follow sees regular file");
    CHECK_RET(fstatat(dfd, "symlink", &st, AT_SYMLINK_NOFOLLOW), 0, "symlinkat: fstatat nofollow");
    CHECK(S_ISLNK(st.st_mode), "symlinkat: nofollow sees symlink");

    char linkbuf[256];
    ssize_t linklen = readlinkat(dfd, "symlink", linkbuf, sizeof(linkbuf) - 1);
    CHECK(linklen > 0, "readlinkat: returns positive length");
    if (linklen > 0) {
        CHECK((size_t)linklen == strlen("realfile"), "readlinkat: length equals target length");
        linkbuf[linklen] = '\0';
        CHECK(strcmp(linkbuf, "realfile") == 0, "readlinkat: target path equals realfile");
    }

    char small[4];
    linklen = readlinkat(dfd, "symlink", small, sizeof(small));
    CHECK_RET(linklen, (ssize_t)sizeof(small), "readlinkat: truncation returns buffer size");
    CHECK(memcmp(small, "real", sizeof(small)) == 0, "readlinkat: truncation keeps prefix");

    memset(linkbuf, 0x5a, sizeof(linkbuf));
    CHECK_ERR(syscall(SYS_readlinkat, dfd, "symlink", linkbuf, 0), EINVAL, "readlinkat: size==0 -> EINVAL");
    CHECK(linkbuf[0] == (char)0x5a, "readlinkat: size==0 keeps buffer unchanged");

    CHECK_ERR(readlinkat(dfd, "realfile", linkbuf, sizeof(linkbuf)), EINVAL, "readlinkat: non-symlink -> EINVAL");
    CHECK_ERR(readlinkat(dfd, "no_such_link", linkbuf, sizeof(linkbuf)), ENOENT, "readlinkat: missing -> ENOENT");

    if (geteuid() == 0) {
        struct symlink_perm_test_case symlink_perm_cases[] = {
            {
                .parent_dir = symlink_parent,
                .dir_name = "symlink_nowrite_parent",
                .link_name = "link_nowrite",
                .mode = 0555,
                .exp_errno = EACCES,
                .desc = "symlinkat: parent dir without write permission -> EACCES",
            },
            {
                .parent_dir = symlink_parent,
                .dir_name = "symlink_noexec_parent",
                .link_name = "link_noexec",
                .mode = 0666,
                .exp_errno = EACCES,
                .desc = "symlinkat: parent dir without execute permission -> EACCES",
            },
        };
        run_symlink_perm_test_cases(symlink_perm_cases,
                                    sizeof(symlink_perm_cases) / sizeof(symlink_perm_cases[0]));

        struct readlink_perm_test_case readlink_perm_cases[] = {
            {
                .parent_dir = readlink_parent,
                .dir_name = "readlink_noexec_parent",
                .link_name = "link_noexec",
                .target = "realfile",
                .mode = 0666,
                .exp_errno = EACCES,
                .desc = "readlinkat: parent dir without execute permission -> EACCES",
            },
        };
        run_readlink_perm_test_cases(readlink_perm_cases,
                                     sizeof(readlink_perm_cases) / sizeof(readlink_perm_cases[0]));
    } else {
        printf("  SKIP | %s:%d | symlink/readlink: needs_root=1 for permission coverage\n",
               __FILE__,
               __LINE__);
    }

    unlinkat(dfd, "symlink", 0);
    unlinkat(dfd, "realfile", 0);
    path_cleanup_perm_matrix_at(dfd, perm_entries, sizeof(perm_entries) / sizeof(perm_entries[0]));
    close(dfd);
}
