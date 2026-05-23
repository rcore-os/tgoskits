#include "path_common.h"

/*
 * getdents64(2) — get directory entries.
 *
 * man 2 getdents64:
 *   "getdents64() reads several linux_dirent64 structures from the directory
 *    referred to by the open file descriptor fd into the buffer pointed to by
 *    dirp."
 *   "On success, the number of bytes read is returned. On end of directory, 0
 *    is returned. On error, -1 is returned, and errno is set to indicate the
 *    error."
 *
 * 测试覆盖（Linux 兼容最小集 + 针对当前实现的显式分支）：
 *   (a) 正常读取 → 返回 >0，且条目包含 "."、".."、测试文件
 *   (b) buf 过小且仍有剩余条目 → -1 EINVAL
 *   (c) 目录末尾再次读取 → 0（EOF）
 *   (d) fd=-1 → -1 EBADF
 *   (e) 非法 dirp 指针 → -1 EFAULT（raw syscall）
 *   (f) fd 指向普通文件 → -1 ENOTDIR
 *
 * 未覆盖/不便实现（地址/权限/资源依赖）：
 *   (g) 目录内容变化/并发读取与 offset 竞争（需要多线程/并发控制）
 */

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static long raw_getdents64(int fd, void *dirp, size_t count)
{
    return syscall(SYS_getdents64, fd, dirp, count);
}

void test_getdents64(void)
{
    int dfd = open_base_dir();
    if (dfd < 0) {
        return;
    }

    mkdirat(dfd, "testdir", 0755);
    int tfd = openat(dfd, "testdir/testfile", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (tfd >= 0) {
        close(tfd);
    }

    int dirfd = openat(dfd, "testdir", O_RDONLY | O_DIRECTORY);
    CHECK(dirfd >= 0, "getdents64: open testdir");
    if (dirfd < 0) {
        unlinkat(dfd, "testdir/testfile", 0);
        unlinkat(dfd, "testdir", AT_REMOVEDIR);
        close(dfd);
        return;
    }

    char small_buf[1];
    char buf[4096];
    int filefd = openat(dfd, "testdir/testfile", O_RDONLY);
    CHECK(filefd >= 0, "getdents64: open testfile");

    int found_self = 0;
    int found_parent = 0;
    int found_file = 0;
    long nread = 0;

    struct test_case {
        int action;
        int fd;
        void *buf;
        size_t size;
        int exp_kind;
        int exp_errno;
        int *flag;
        const char *desc;
    };

    enum {
        ACTION_GETDENTS = 0,
        ACTION_CHECK_FLAG = 1,
    };

    enum {
        EXPECT_POSITIVE = 1,
        EXPECT_ZERO = 0,
        EXPECT_NEGATIVE = -1,
    };

    struct test_case test_cases[] = {
        /* (b) buf 过小且仍有剩余条目 → -1 EINVAL */
        {ACTION_GETDENTS, dirfd, small_buf, sizeof(small_buf), EXPECT_NEGATIVE, EINVAL, NULL, "getdents64: tiny buffer -> EINVAL"},
        /* (a) 正常读取：应返回 >0 */
        {ACTION_GETDENTS, dirfd, buf, sizeof(buf), EXPECT_POSITIVE, 0, NULL, "getdents64: returns positive length"},
        /* (a) 正常读取：条目包含 "." */
        {ACTION_CHECK_FLAG, 0, NULL, 0, 0, 0, &found_self, "getdents64: found '.'"},
        /* (a) 正常读取：条目包含 ".." */
        {ACTION_CHECK_FLAG, 0, NULL, 0, 0, 0, &found_parent, "getdents64: found '..'"},
        /* (a) 正常读取：条目包含测试文件 */
        {ACTION_CHECK_FLAG, 0, NULL, 0, 0, 0, &found_file, "getdents64: found 'testfile'"},
        /* (c) EOF：再次读取应返回 0 */
        {ACTION_GETDENTS, dirfd, buf, sizeof(buf), EXPECT_ZERO, 0, NULL, "getdents64: EOF -> 0"},
        /* (d) fd=-1 */
        {ACTION_GETDENTS, -1, buf, sizeof(buf), EXPECT_NEGATIVE, EBADF, NULL, "getdents64: fd=-1 -> EBADF"},
        /* (e) 非法 dirp 指针 */
        {ACTION_GETDENTS, dirfd, (void *)-1, 32, EXPECT_NEGATIVE, EFAULT, NULL, "getdents64: bad dirp -> EFAULT"},
        /* (f) fd 指向普通文件 */
        {ACTION_GETDENTS, filefd, buf, sizeof(buf), EXPECT_NEGATIVE, ENOTDIR, NULL, "getdents64: file fd -> ENOTDIR"},
    };

    for (size_t i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        struct test_case *tc = &test_cases[i];
        if (tc->action == ACTION_CHECK_FLAG) {
            errno = 0;
            CHECK(tc->flag != NULL && *tc->flag, tc->desc);
            continue;
        }

        errno = 0;
        long r = raw_getdents64(tc->fd, tc->buf, tc->size);
        if (tc->exp_kind == EXPECT_POSITIVE) {
            CHECK(r > 0, tc->desc);
            nread = r;
            long pos = 0;
            while (pos + (long)sizeof(struct linux_dirent64) <= nread) {
                struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + pos);
                if (d->d_reclen == 0) {
                    break;
                }
                if (strcmp(d->d_name, ".") == 0) {
                    found_self = 1;
                } else if (strcmp(d->d_name, "..") == 0) {
                    found_parent = 1;
                } else if (strcmp(d->d_name, "testfile") == 0) {
                    found_file = 1;
                }
                pos += d->d_reclen;
            }
        } else if (tc->exp_kind == EXPECT_ZERO) {
            CHECK_RET(r, 0, tc->desc);
        } else {
            CHECK(r == -1 && errno == tc->exp_errno, tc->desc);
        }
    }

    if (filefd >= 0) {
        close(filefd);
    }
    close(dirfd);
    unlinkat(dfd, "testdir/testfile", 0);
    unlinkat(dfd, "testdir", AT_REMOVEDIR);
    close(dfd);
}
