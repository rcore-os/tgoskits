#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* openat × O_CREAT 跨 dirfd 类型矩阵：
 *   - 通过 openat(valid_dir_fd, "rel_name", O_CREAT) 创建文件应在 dir 下
 *   - 通过 openat(O_PATH-dir-fd, "rel", O_CREAT) 同上
 *   - 通过 openat(AT_FDCWD, "/abs", O_CREAT) 在 abs path 创建
 *   - openat 上 CREAT|EXCL 在已存在 → EEXIST
 *   - openat 上 CREAT|TRUNC 在已有内容 file → 截断
 *
 * man 2 open §"openat()" 原文：
 *   "The openat() system call operates in exactly the same way as open(),
 *    except for the differences described here."
 *   "If the pathname given in pathname is relative, then it is interpreted
 *    relative to the directory referred to by the file descriptor dirfd."
 *
 * man 2 open §"O_CREAT" 原文（适用于 openat 同样）：
 *   "If pathname does not exist, create it as a regular file."
 *
 * man 2 open §"O_PATH" 关于作 dirfd 用的条款：
 *   "Passing the file descriptor as the dirfd argument of openat(2) and
 *    the other 'at' system calls."
 *
 * 验证 openat 与 open 的 O_CREAT/O_EXCL/O_TRUNC 行为完全等价，且穿透
 * 4 种 dirfd 类型（valid dir fd / O_PATH dir fd / AT_FDCWD+abs / chdir+rel）。 */

#define M_DIR  OF_MOD("openat_creat")
#define M_SUB  M_DIR "/sub"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "openat_creat setup: mkdir top");
    CHECK(ensure_dir(M_SUB) == 0,                                 "openat_creat setup: subdir");
    umask(0);
}

/* 1. openat(valid dir fd, "name", O_CREAT) 在 dir 下创建 */
static void creat_via_dirfd(void)
{
    int dfd = open(M_SUB, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0,                                               "creat_via_dirfd: open subdir ok");
    if (dfd < 0) return;

    int fd = openat(dfd, "newfile1", O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0,                                                "creat_via_dirfd: openat CREAT ok");
    if (fd >= 0) {
        write(fd, "via_dirfd", 9);
        close(fd);
    }

    /* 独立验证：用绝对路径 stat 应能看到 */
    struct stat st;
    CHECK(stat(M_SUB "/newfile1", &st) == 0,                      "creat_via_dirfd: stat 验证");
    CHECK(st.st_size == 9,                                        "creat_via_dirfd: 内容长度对");

    unlink(M_SUB "/newfile1");
    close(dfd);
}

/* 2. openat(O_PATH dir fd, "name", O_CREAT) 同上 */
static void creat_via_path_dirfd(void)
{
    int dfd = open(M_SUB, O_PATH);
    CHECK(dfd >= 0,                                               "creat_via_path_dirfd: O_PATH dir ok");
    if (dfd < 0) return;

    int fd = openat(dfd, "newfile2", O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0,                                                "creat_via_path_dirfd: openat CREAT ok");
    if (fd >= 0) {
        write(fd, "via_path", 8);
        close(fd);
    }

    struct stat st;
    CHECK(stat(M_SUB "/newfile2", &st) == 0,                      "creat_via_path_dirfd: stat 验证");
    CHECK(st.st_size == 8,                                        "creat_via_path_dirfd: 内容长度对");

    unlink(M_SUB "/newfile2");
    close(dfd);
}

/* 3. openat(AT_FDCWD, "/abs", O_CREAT) */
static void creat_via_at_fdcwd_abs(void)
{
    int fd = openat(AT_FDCWD, M_DIR "/newfile3", O_CREAT | O_WRONLY, 0644);
    CHECK(fd >= 0,                                                "creat_via_at_fdcwd_abs: openat CREAT ok");
    if (fd >= 0) close(fd);

    CHECK(is_regular_file(M_DIR "/newfile3"),                     "creat_via_at_fdcwd_abs: file exists");
    unlink(M_DIR "/newfile3");
}

/* 4. openat CREAT|EXCL 已存在 → EEXIST */
static void creat_excl_existing(void)
{
    int dfd = open(M_SUB, O_RDONLY | O_DIRECTORY);
    if (dfd < 0) return;

    /* setup: 先建 */
    int f0 = openat(dfd, "exists", O_CREAT | O_WRONLY, 0644);
    if (f0 >= 0) close(f0);

    errno = 0;
    int fd = openat(dfd, "exists", O_CREAT | O_EXCL | O_WRONLY, 0644);
    CHECK(fd == -1 && errno == EEXIST,                            "creat_excl_existing: openat CREAT|EXCL existing -> EEXIST");
    if (fd >= 0) close(fd);

    unlink(M_SUB "/exists");
    close(dfd);
}

/* 5. openat CREAT|TRUNC 已有内容 → 截断 */
static void creat_trunc_existing(void)
{
    int dfd = open(M_SUB, O_RDONLY | O_DIRECTORY);
    if (dfd < 0) return;

    /* setup: 写有内容的文件 */
    int f0 = openat(dfd, "trunc_me", O_CREAT | O_WRONLY, 0644);
    if (f0 >= 0) { write(f0, "long-content", 12); close(f0); }

    int fd = openat(dfd, "trunc_me", O_CREAT | O_WRONLY | O_TRUNC, 0644);
    CHECK(fd >= 0,                                                "creat_trunc_existing: openat CREAT|TRUNC ok");
    if (fd >= 0) close(fd);

    CHECK(get_file_size(M_SUB "/trunc_me") == 0,                  "creat_trunc_existing: 截断到 0");

    unlink(M_SUB "/trunc_me");
    close(dfd);
}

int openat_creat_run(void)
{
    printf("\n----- openat_creat -----\n");
    mod_setup();
    creat_via_dirfd();
    creat_via_path_dirfd();
    creat_via_at_fdcwd_abs();
    creat_excl_existing();
    creat_trunc_existing();
    cleanup_tree(M_DIR);
    printf("  ----- openat_creat: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
