#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* openat 专属 errno 验证。
 *
 * man 2 open §"EBADF" (openat variant) 原文：
 *   "EBADF — (openat()) pathname is relative but dirfd is neither AT_FDCWD
 *    nor a valid file descriptor."
 *
 * man 2 open §"ENOTDIR" (openat variant) 原文：
 *   "ENOTDIR — A component used as a directory in pathname is not, in
 *    fact, a directory, or O_DIRECTORY was specified and pathname was
 *    not a directory."
 *   "ENOTDIR — (openat()) pathname is a relative pathname and dirfd is a
 *    file descriptor referring to a file other than a directory."
 *
 * 5 子场景：dirfd=-1 + rel → EBADF / dirfd=9999 + rel → EBADF /
 *           dirfd 是文件 + rel → ENOTDIR / closed dirfd → EBADF /
 *           empty path → ENOENT（已移 bug-openat-empty-path-no-enoent）。 */

#define M_DIR  OF_MOD("openat_err")
#define M_FILE M_DIR "/file"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "openat_err setup: mkdir");
    CHECK(write_file(M_FILE, "x", 1, 0644) == 0,                  "openat_err setup: file");
}

/* 相对路径 + 显式无效 dirfd (-1) → EBADF */
static void ebadf_invalid_dirfd_relative(void)
{
    errno = 0;
    int fd = openat(-1, "file", O_RDONLY);
    CHECK(fd == -1 && errno == EBADF,                             "openat err: -1 dirfd + relative -> EBADF");
    if (fd >= 0) close(fd);
}

/* 相对路径 + 不存在的 fd 数（如 9999） → EBADF */
static void ebadf_nonexistent_fd_relative(void)
{
    errno = 0;
    int fd = openat(9999, "file", O_RDONLY);
    CHECK(fd == -1 && errno == EBADF,                             "openat err: 9999 dirfd + relative -> EBADF");
    if (fd >= 0) close(fd);
}

/* 相对路径 + dirfd 是文件 → ENOTDIR */
static void enotdir_dirfd_is_file(void)
{
    int file_fd = open(M_FILE, O_RDONLY);
    CHECK(file_fd >= 0,                                           "openat err: open file as fd ok");
    if (file_fd < 0) return;

    errno = 0;
    int fd = openat(file_fd, "anything", O_RDONLY);
    CHECK(fd == -1 && errno == ENOTDIR,                           "openat err: file fd as dirfd + relative -> ENOTDIR");
    if (fd >= 0) close(fd);
    close(file_fd);
}

/* 关闭 dirfd 后再用 → EBADF */
static void ebadf_closed_dirfd(void)
{
    int dfd = open(M_DIR, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0,                                               "openat err closed: open dir ok");
    if (dfd < 0) return;
    close(dfd);

    errno = 0;
    int fd = openat(dfd, "file", O_RDONLY);
    CHECK(fd == -1 && errno == EBADF,                             "openat err: closed dirfd + relative -> EBADF");
    if (fd >= 0) close(fd);
}

/* enoent_empty_path(): 原为 "openat(AT_FDCWD, \"\") → ENOENT" 测试用例;
 * 因 starry 与 Linux 行为不一致(starry 把 "" 解析为 cwd 自身), 已移到 bugfix 复现:
 * test-suit/starryos/qemu/system/bugfix-bug-openat-empty-path-no-enoent/c/src/main.c */

int openat_err_run(void)
{
    printf("\n----- openat_err -----\n");
    mod_setup();
    ebadf_invalid_dirfd_relative();
    ebadf_nonexistent_fd_relative();
    enotdir_dirfd_is_file();
    ebadf_closed_dirfd();
    /* enoent_empty_path(): 见上, 已移到 bug-openat-empty-path-no-enoent */
    cleanup_tree(M_DIR);
    printf("  ----- openat_err: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
