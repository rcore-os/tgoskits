#include "path_common.h"

/*
 * linkat(2) — make a new name for a file.
 *
 * man 2 linkat:
 *   "linkat() makes a new link (also known as a hard link) to an existing file."
 *   "If oldpath is relative, it is interpreted relative to olddirfd."
 *   "If newpath is relative, it is interpreted relative to newdirfd."
 *   "If oldpath is absolute, olddirfd is ignored."
 *   "If newpath is absolute, newdirfd is ignored."
 *   "If flags is 0, linkat() does not dereference oldpath if it is a symbolic link
 *    (like link(2)); only AT_SYMLINK_FOLLOW causes dereference of the final
 *    symlink."
 *   "AT_EMPTY_PATH allows an empty oldpath, in which case olddirfd refers to
 *    the file."
 *
 * 测试覆盖（Linux 兼容最小集 + 针对当前实现的显式分支）：
 *   (a) 相对路径 hardlink 创建成功 → 0（inode 相同，nlink=2）
 *   (b) oldpath 绝对路径（忽略 olddirfd）→ 0
 *   (c) newpath 绝对路径（忽略 newdirfd）→ 0
 *   (d) AT_EMPTY_PATH + oldpath="" → -1 ENOENT
 *   (e) 目标已存在 → -1 EEXIST
 *   (f) 源不存在 → -1 ENOENT
 *   (g) 非法 flags → -1 EINVAL（raw syscall）
 *   (h) old 是目录 → -1 EPERM
 *   (i) olddirfd=-1 → -1 EBADF
 *   (j) newdirfd=-1 → -1 EBADF
 *   (k) olddirfd=普通文件fd + 相对 oldpath → -1 ENOTDIR
 *   (l) newdirfd=普通文件fd + 相对 newpath → -1 ENOTDIR
 *   (m) AT_SYMLINK_FOLLOW + 符号链接循环/过深 → -1 ELOOP
 *
 * 未覆盖/不便实现（环境/资源依赖）：
 *   (n) 跨设备硬链接 → -1 EXDEV（需要可控多挂载点/多设备环境）
 *   (o) EMLINK（达到链接数上限）/ ENOSPC / EROFS / EACCES / ENAMETOOLONG 等
 */

void test_linkat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    char abs_old[256];
    path_join(abs_old, sizeof(abs_old), "origfile");
    char abs_new[256];
    path_join(abs_new, sizeof(abs_new), "abslink");

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    int filefd = open(regfile, O_RDONLY);
    CHECK(filefd >= 0, "linkat: open regfile");
    if (filefd < 0) {
        close(dfd);
        return;
    }

    int orig_fd = openat(dfd, "origfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(orig_fd >= 0, "linkat: create origfile");
    if (orig_fd >= 0) {
        write(orig_fd, "hardlink", 8);
        close(orig_fd);
    }
    orig_fd = openat(dfd, "origfile", O_RDONLY);
    CHECK(orig_fd >= 0, "linkat: open origfile");
    if (orig_fd < 0) {
        close(filefd);
        close(dfd);
        return;
    }

    mkdirat(dfd, "dirlink_src", 0755);

    symlinkat("eloop2", dfd, "eloop1");
    symlinkat("eloop1", dfd, "eloop2");

    struct test_case {
        int olddirfd;
        const char *oldpath;
        int newdirfd;
        const char *newpath;
        unsigned int flags;
        int exp_ret;
        int exp_errno;
        int use_raw;
        const char *desc;
    };

    struct test_case test_cases[] = {
        /* (a) 相对路径 hardlink 创建成功 */
        {dfd, "origfile", dfd, "hardlink", 0, 0, 0, 0, "linkat: create hardlink"},
        /* (b) oldpath 绝对路径（忽略 olddirfd） */
        {filefd, abs_old, dfd, "abs_old_link", 0, 0, 0, 0, "linkat: abs oldpath -> success"},
        /* (c) newpath 绝对路径（忽略 newdirfd） */
        {dfd, "origfile", filefd, abs_new, 0, 0, 0, 0, "linkat: abs newpath -> success"},
        /* (d) AT_EMPTY_PATH + oldpath=\"\"（raw） */
        {orig_fd, "", dfd, "empty_path_link", (unsigned int)AT_EMPTY_PATH, -1, ENOENT, 1, "linkat: AT_EMPTY_PATH + empty oldpath -> ENOENT"},
        /* (e) 目标已存在 */
        {dfd, "origfile", dfd, "hardlink", 0, -1, EEXIST, 1, "linkat: existing target -> EEXIST"},
        /* (f) 源不存在 */
        {dfd, "no_such_orig", dfd, "missing_link", 0, -1, ENOENT, 0, "linkat: missing source -> ENOENT"},
        /* (g) 非法 flags（raw） */
        {dfd, "origfile", dfd, "flag_link", 0x80000000u, -1, EINVAL, 1, "linkat: invalid flags -> EINVAL"},
        /* (h) old 是目录 */
        {dfd, "dirlink_src", dfd, "dirlink_dst", 0, -1, EPERM, 0, "linkat: directory hardlink -> EPERM"},
        /* (i) olddirfd=-1 */
        {-1, "origfile", dfd, "x", 0, -1, EBADF, 0, "linkat: olddirfd=-1 -> EBADF"},
        /* (j) newdirfd=-1 */
        {dfd, "origfile", -1, "x", 0, -1, EBADF, 0, "linkat: newdirfd=-1 -> EBADF"},
        /* (k) olddirfd=普通文件fd + 相对 oldpath */
        {filefd, "origfile", dfd, "x", 0, -1, ENOTDIR, 0, "linkat: olddirfd is file -> ENOTDIR"},
        /* (l) newdirfd=普通文件fd + 相对 newpath */
        {dfd, "origfile", filefd, "x", 0, -1, ENOTDIR, 0, "linkat: newdirfd is file -> ENOTDIR"},
        /* (m) AT_SYMLINK_FOLLOW + 符号链接循环/过深（raw） */
        {dfd, "eloop1", dfd, "eloop_link", (unsigned int)AT_SYMLINK_FOLLOW, -1, ELOOP, 1, "linkat: symlink loop + AT_SYMLINK_FOLLOW -> ELOOP"},
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];

        if (tc->exp_ret == 0) {
            unlinkat(dfd, tc->newpath, 0);
        } else if (tc->exp_errno == EEXIST) {
            unlinkat(dfd, tc->newpath, 0);
            CHECK_RET(syscall(SYS_linkat,
                              tc->olddirfd,
                              tc->oldpath,
                              tc->newdirfd,
                              tc->newpath,
                              tc->flags),
                      0,
                      "linkat: precreate existing target");
        }

        errno = 0;
        long r;
        if (tc->use_raw) {
            r = syscall(SYS_linkat, tc->olddirfd, tc->oldpath, tc->newdirfd, tc->newpath, tc->flags);
        } else {
            r = linkat(tc->olddirfd, tc->oldpath, tc->newdirfd, tc->newpath, (int)tc->flags);
        }

        if (tc->exp_ret == 0) {
            CHECK_RET(r, 0, tc->desc);
            struct stat st_old, st_new;
            CHECK_RET(fstatat(dfd, "origfile", &st_old, 0), 0, "linkat: stat origfile");
            if (tc->newpath == abs_new) {
                CHECK_RET(stat(tc->newpath, &st_new), 0, "linkat: stat abs newpath");
            } else {
                CHECK_RET(fstatat(dfd, tc->newpath, &st_new, 0), 0, "linkat: stat new link");
            }
            CHECK(st_old.st_ino == st_new.st_ino, "linkat: st_ino match");
            CHECK(st_old.st_dev == st_new.st_dev, "linkat: st_dev match");
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }

        if (tc->exp_ret == 0) {
            if (tc->newpath == abs_new) {
                unlink(tc->newpath);
            } else {
                unlinkat(dfd, tc->newpath, 0);
            }
        } else if (tc->exp_errno == EEXIST) {
            unlinkat(dfd, tc->newpath, 0);
        }
    }

    unlinkat(dfd, "eloop1", 0);
    unlinkat(dfd, "eloop2", 0);
    unlinkat(dfd, "missing_link", 0);
    unlinkat(dfd, "flag_link", 0);
    unlinkat(dfd, "dirlink_dst", 0);

    unlinkat(dfd, "dirlink_src", AT_REMOVEDIR);
    unlinkat(dfd, "origfile", 0);

    close(orig_fd);
    close(filefd);
    close(dfd);
}
