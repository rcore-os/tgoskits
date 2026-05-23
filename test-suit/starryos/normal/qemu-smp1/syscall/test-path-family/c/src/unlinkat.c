#include "path_common.h"

/*
 * unlinkat(2) — delete a name and possibly the file it refers to.
 *
 * man 2 unlinkat:
 *   "unlinkat() operates in exactly the same way as either unlink(2) or
 *    rmdir(2) (depending on whether or not flags includes AT_REMOVEDIR),
 *    except for the differences described here."
 *   "If the pathname given in pathname is relative, then it is interpreted
 *    relative to the directory referred to by the file descriptor dirfd
 *    (rather than relative to the current working directory of the calling
 *    process, as is done by unlink(2) and rmdir(2))."
 *   "If pathname is relative and dirfd is AT_FDCWD, then pathname is
 *    interpreted relative to the current working directory of the calling
 *    process."
 *
 * 测试覆盖：
 *   (a) 删除普通文件成功 → 0
 *   (b) 删除不存在文件 → -1 ENOENT
 *   (c) dirfd=-1 → -1 EBADF
 *   (d) 非法 flags → -1 EINVAL（raw syscall，避免 libc 预处理）
 *   (e) 删除空目录（AT_REMOVEDIR）成功 → 0
 *   (f) 删除非空目录（AT_REMOVEDIR）→ -1 ENOTEMPTY
 *   (g) dirfd=普通文件fd + 相对路径 → -1 ENOTDIR
 *   (h) 对普通文件使用 AT_REMOVEDIR → -1 ENOTDIR
 *   (i) dirfd=AT_FDCWD + 相对路径删除成功 → 0
 *   (j) pathname 非法地址/NULL（raw syscall）→ -1 EFAULT
 */

void test_unlinkat(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    char old_cwd[512];
    CHECK(getcwd(old_cwd, sizeof(old_cwd)) != NULL, "unlinkat: capture old cwd");
    CHECK_RET(chdir(PATH_FAMILY_BASE), 0, "unlinkat: chdir(BASE)");

    char regfile[256];
    path_join(regfile, sizeof(regfile), "regfile");
    int filefd = open(regfile, O_RDONLY);
    CHECK(filefd >= 0, "unlinkat: open regfile");
    if (filefd < 0) {
        chdir(old_cwd);
        close(dfd);
        return;
    }

    struct test_case {
        int dirfd;
        const void *pathname;
        int flags;
        int exp_ret;
        int exp_errno;
        int use_raw;
    };

    struct test_case test_cases[] = {
        /* (a) 删除普通文件成功 */
        {dfd, "workfile", 0, 0, 0, 0},
        /* (b) 删除不存在文件 */
        {dfd, "no_such_file", 0, -1, ENOENT, 0},
        /* (c) dirfd=-1 */
        {-1, "x", 0, -1, EBADF, 0},
        /* (d) 非法 flags（raw） */
        {dfd, "x", 0x80000000u, -1, EINVAL, 1},
        /* (e) 删除空目录（AT_REMOVEDIR）成功 */
        {dfd, "emptydir", AT_REMOVEDIR, 0, 0, 0},
        /* (f) 删除非空目录（AT_REMOVEDIR） */
        {dfd, "nonempty", AT_REMOVEDIR, -1, ENOTEMPTY, 0},
        /* (g) dirfd=普通文件fd + 相对路径 */
        {filefd, "x", 0, -1, ENOTDIR, 0},
        /* (h) 对普通文件使用 AT_REMOVEDIR */
        {dfd, "workfile2", AT_REMOVEDIR, -1, ENOTDIR, 0},
        /* (i) dirfd=AT_FDCWD + 相对路径删除成功 */
        {AT_FDCWD, "atcwd_file", 0, 0, 0, 0},
        /* (j) pathname 非法地址/NULL（raw） */
        {dfd, (void *)-1, 0, -1, EFAULT, 1},
        {dfd, NULL, 0, -1, EFAULT, 1},
    };

    const char *case_msgs[] = {
        "unlinkat: delete file",
        "unlinkat: missing file -> ENOENT",
        "unlinkat: dirfd=-1 -> EBADF",
        "unlinkat: invalid flags -> EINVAL",
        "unlinkat: delete empty dir (AT_REMOVEDIR)",
        "unlinkat: non-empty dir -> ENOTEMPTY",
        "unlinkat: file dirfd -> ENOTDIR",
        "unlinkat: AT_REMOVEDIR on file -> ENOTDIR",
        "unlinkat: AT_FDCWD + relpath -> success",
        "unlinkat: bad address -> EFAULT",
        "unlinkat: NULL pathname -> EFAULT",
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];
        const char *msg = case_msgs[i];

        switch (i) {
        case 0: {
            int fd = openat(dfd, "workfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
            if (fd >= 0) {
                write(fd, "hello", 5);
                close(fd);
            }
            break;
        }
        case 4:
            mkdirat(dfd, "emptydir", 0755);
            break;
        case 5: {
            mkdirat(dfd, "nonempty", 0755);
            int child = openat(dfd, "nonempty/child", O_CREAT | O_WRONLY | O_TRUNC, 0644);
            if (child >= 0) {
                close(child);
            }
            break;
        }
        case 7: {
            int fd = openat(dfd, "workfile2", O_CREAT | O_WRONLY | O_TRUNC, 0644);
            if (fd >= 0) {
                close(fd);
            }
            break;
        }
        case 8: {
            int fd = open("atcwd_file", O_CREAT | O_WRONLY | O_TRUNC, 0644);
            if (fd >= 0) {
                close(fd);
            }
            break;
        }
        default:
            break;
        }

        errno = 0;
        long r;
        if (tc->use_raw) {
            r = syscall(SYS_unlinkat, tc->dirfd, tc->pathname, tc->flags);
        } else {
            r = unlinkat(tc->dirfd, (const char *)tc->pathname, tc->flags);
        }

        if (tc->exp_ret == 0) {
            CHECK_RET(r, 0, msg);
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, msg);
        }

        if (i == 0) {
            struct stat st;
            errno = 0;
            CHECK(fstatat(dfd, "workfile", &st, 0) == -1 && errno == ENOENT, "unlinkat: deleted file -> ENOENT");
        } else if (i == 4 && tc->exp_ret == 0) {
            struct stat st;
            errno = 0;
            CHECK(fstatat(dfd, "emptydir", &st, 0) == -1 && errno == ENOENT, "unlinkat: deleted dir -> ENOENT");
        } else if (i == 5) {
            unlinkat(dfd, "nonempty/child", 0);
            unlinkat(dfd, "nonempty", AT_REMOVEDIR);
        } else if (i == 7) {
            unlinkat(dfd, "workfile2", 0);
        } else if (i == 8) {
            unlink("atcwd_file");
        }
    }

    close(filefd);
    chdir(old_cwd);
    close(dfd);
}
