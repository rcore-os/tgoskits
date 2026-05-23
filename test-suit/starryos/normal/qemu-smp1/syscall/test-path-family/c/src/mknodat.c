#include "path_common.h"

/*
 * mknodat(2) — create a filesystem node.
 *
 * man 2 mknod (mknodat):
 *   "mknodat() is identical to mknod(), except that if the pathname given in
 *    pathname is relative, then it is interpreted relative to the directory
 *    referred to by the file descriptor dirfd (rather than relative to the
 *    current working directory of the calling process)."
 *   "On success, 0 is returned. On error, -1 is returned, and errno is set to
 *    indicate the error."
 *
 * 测试覆盖：
 *   (a) 创建普通文件（type bits=0 / 仅 mode）→ 0，且 fstatat 显示为 REG
 *   (b) 创建 FIFO（S_IFIFO）→ 0，且 fstatat 显示为 FIFO
 *   (c) AT_FDCWD + 相对路径创建普通文件 → 0
 *   (d) 绝对路径创建本地套接字节点（S_IFSOCK）→ 0，且 stat 显示为 SOCK
 *   (e) 目标已存在（FIFO）→ -1 EEXIST
 *   (f) dirfd=-1 → -1 EBADF
 *   (g) dirfd 指向普通文件 → -1 ENOTDIR
 *   (h) 父目录不存在 → -1 ENOENT
 *   (i) 请求创建目录（S_IFDIR）→ -1 EPERM
 *   (j) 非法 type bits → -1 EINVAL
 *   (k) 符号链接循环路径 → -1 ELOOP
 *
 * 未覆盖/不便实现（权限/设备/资源依赖）：
 *   (l) S_IFCHR/S_IFBLK 设备节点与 rdev 相关语义（需要设备支持/权限）
 */

static long raw_mknodat(int dirfd, const char *pathname, mode_t mode, dev_t dev)
{
    return syscall(SYS_mknodat, dirfd, pathname, mode, dev);
}

void test_mknodat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    unlinkat(dfd, "mknod_reg", 0);
    unlinkat(dfd, "mknod_fifo", 0);
    unlinkat(dfd, "mknod_atcwd", 0);
    unlinkat(dfd, "mknod_sock_abs", 0);
    unlinkat(dfd, "mknod_loop_a", 0);
    unlinkat(dfd, "mknod_loop_b", 0);

    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "mknodat: capture old cwd");
    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "mknodat: chdir(BASE) for AT_FDCWD");

    char abs_sock[256];
    path_join(abs_sock, sizeof(abs_sock), "mknod_sock_abs");
    char abs_atcwd[256];
    path_join(abs_atcwd, sizeof(abs_atcwd), "mknod_atcwd");

    int tfd = openat(dfd, "regfile", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (tfd >= 0) {
        close(tfd);
    }
    int filefd = openat(dfd, "regfile", O_RDONLY);
    CHECK(filefd >= 0, "mknodat: open regfile");

    enum {
        VERIFY_NONE = 0,
        VERIFY_REG,
        VERIFY_FIFO,
        VERIFY_SOCK,
    };

    struct test_case {
        int dirfd;
        const char *pathname;
        mode_t mode;
        dev_t dev;
        int exp_ret;
        int exp_errno;
        int verify_kind;
        const char *verify_path;
        const char *desc;
    };

    CHECK_RET(symlinkat("mknod_loop_b", dfd, "mknod_loop_a"), 0, "mknodat: create loop_a symlink");
    CHECK_RET(symlinkat("mknod_loop_a", dfd, "mknod_loop_b"), 0, "mknodat: create loop_b symlink");

    struct test_case test_cases[] = {
        /* (a) */
        {dfd, "mknod_reg", 0644, 0, 0, 0, VERIFY_REG, "mknod_reg", "mknodat: create regular file with mode only"},
        /* (b) */
        {dfd, "mknod_fifo", S_IFIFO | 0644, 0, 0, 0, VERIFY_FIFO, "mknod_fifo", "mknodat: create FIFO"},
        /* (c) */
        {AT_FDCWD, "mknod_atcwd", 0644, 0, 0, 0, VERIFY_REG, abs_atcwd, "mknodat: AT_FDCWD create regular file"},
        /* (d) */
        {dfd, abs_sock, S_IFSOCK | 0600, 0, 0, 0, VERIFY_SOCK, abs_sock, "mknodat: absolute path create socket node"},
        /* (e) */
        {dfd, "mknod_fifo", S_IFIFO | 0644, 0, -1, EEXIST, VERIFY_NONE, NULL, "mknodat: existing FIFO -> EEXIST"},
        /* (f) */
        {-1, "badfd_fifo", S_IFIFO | 0644, 0, -1, EBADF, VERIFY_NONE, NULL, "mknodat: dirfd=-1 -> EBADF"},
        /* (g) */
        {filefd, "notdir_fifo", S_IFIFO | 0644, 0, -1, ENOTDIR, VERIFY_NONE, NULL, "mknodat: file dirfd -> ENOTDIR"},
        /* (h) */
        {dfd,
         "no_such_parent/fifo",
         S_IFIFO | 0644,
         0,
         -1,
         ENOENT,
         VERIFY_NONE,
         NULL,
         "mknodat: missing parent -> ENOENT"},
        /* (i) */
        {dfd, "mknod_dir", S_IFDIR | 0755, 0, -1, EPERM, VERIFY_NONE, NULL, "mknodat: S_IFDIR -> EPERM"},
        /* (j) */
        {dfd, "mknod_invalid", S_IFMT | 0644, 0, -1, EINVAL, VERIFY_NONE, NULL, "mknodat: invalid type bits -> EINVAL"},
        /* (k) */
        {dfd, "mknod_loop_a/leaf", S_IFIFO | 0644, 0, -1, ELOOP, VERIFY_NONE, NULL, "mknodat: symlink loop -> ELOOP"},
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];
        errno = 0;
        long r = raw_mknodat(tc->dirfd, tc->pathname, tc->mode, tc->dev);
        if (tc->exp_ret == 0) {
            CHECK_RET(r, 0, tc->desc);
            if (tc->verify_kind != VERIFY_NONE) {
                struct stat st;
                const char *verify_path = tc->verify_path != NULL ? tc->verify_path : tc->pathname;
                CHECK_RET(stat(verify_path, &st), 0, "mknodat: stat created node");
                if (tc->verify_kind == VERIFY_REG) {
                    CHECK(S_ISREG(st.st_mode), "mknodat: created node is regular file");
                } else if (tc->verify_kind == VERIFY_FIFO) {
                    CHECK(S_ISFIFO(st.st_mode), "mknodat: created node is FIFO");
                } else if (tc->verify_kind == VERIFY_SOCK) {
                    CHECK(S_ISSOCK(st.st_mode), "mknodat: created node is socket");
                }
            }
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }
    }

    if (filefd >= 0) {
        close(filefd);
    }
    CHECK_RET(chdir(old_cwd), 0, "mknodat: restore old cwd");
    unlinkat(dfd, "mknod_reg", 0);
    unlinkat(dfd, "mknod_fifo", 0);
    unlinkat(dfd, "mknod_atcwd", 0);
    unlinkat(dfd, "mknod_sock_abs", 0);
    unlinkat(dfd, "mknod_loop_a", 0);
    unlinkat(dfd, "mknod_loop_b", 0);
    unlinkat(dfd, "regfile", 0);
    close(dfd);
}
