#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <unistd.h>

#define M_DIR    OF_MOD("path")
#define M_FILE   M_DIR "/file"
#define M_SUB    M_DIR "/sub"
#define M_INNER  M_SUB "/inner"
#define M_SYM    M_DIR "/sym"

/* O_PATH 完整行为验证。
 *
 * man 2 open §"O_PATH" 原文（节选）：
 *   "O_PATH (since Linux 2.6.39) — Obtain a file descriptor that can be
 *    used for two purposes: to indicate a location in the filesystem tree
 *    and to perform operations that act purely at the file descriptor
 *    level. The file itself is not opened, and other file operations
 *    (e.g., read(2), write(2), fchmod(2), fchown(2), fgetxattr(2),
 *    ioctl(2), mmap(2)) fail with the error EBADF."
 *
 *   "The following operations can be performed on the resulting file
 *    descriptor: close(2). fchdir(2) (since Linux 3.5). fstat(2) (since
 *    Linux 3.6). Duplicating the file descriptor (dup(2), fcntl(2)
 *    F_DUPFD, etc.). Getting and setting file descriptor flags (fcntl(2)
 *    F_GETFD and F_SETFD). Retrieving open file status flags using the
 *    fcntl(2) F_GETFL operation: the returned flags will include the
 *    bit O_PATH. Passing the file descriptor as the dirfd argument of
 *    openat(2) and the other 'at' system calls."
 *
 *   "When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
 *    O_DIRECTORY, and O_NOFOLLOW are ignored."
 *
 *   "If pathname is a symbolic link and the O_NOFOLLOW flag is also
 *    specified, then the call returns a file descriptor referring to the
 *    symbolic link."
 *
 * 10 子函数：basic-on-file / basic-on-dir / io-EBADF（fchmod 已移 bug-*） /
 *           fstat（已移 bug-*） / close-OK / dup-OK / on-dir-as-dirfd /
 *           F_GETFL 含 O_PATH / NOFOLLOW 返 symlink fd / O_PATH|CLOEXEC. */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "path setup: mkdir top");
    CHECK(ensure_dir(M_SUB) == 0,                                 "path setup: subdir");
    CHECK(write_file(M_FILE, "Hi", 2, 0644) == 0,                 "path setup: file");
    CHECK(write_file(M_INNER, "in", 2, 0644) == 0,                "path setup: inner file");
    CHECK(symlink(M_FILE, M_SYM) == 0,                            "path setup: symlink");
}

static void path_basic_open_on_file(void)
{
    int fd = open(M_FILE, O_PATH);
    CHECK(fd >= 0,                                                "path basic file: open ok");
    if (fd >= 0) close(fd);
}

static void path_basic_open_on_dir(void)
{
    int fd = open(M_SUB, O_PATH);
    CHECK(fd >= 0,                                                "path basic dir: open ok");
    if (fd >= 0) close(fd);
}

static void path_io_must_ebadf(void)
{
    int fd = open(M_FILE, O_PATH);
    CHECK(fd >= 0,                                                "path io-EBADF: open ok");
    if (fd < 0) return;

    char buf[4] = {0};
    errno = 0;
    CHECK(read(fd, buf, 1) == -1 && errno == EBADF,               "path io: read -> EBADF");
    errno = 0;
    CHECK(write(fd, "x", 1) == -1 && errno == EBADF,              "path io: write -> EBADF");
    /* fchmod EBADF 已移到 bugfix/bug-open-path-fchmod-bypass */
    errno = 0;
    CHECK(lseek(fd, 0, SEEK_SET) == -1 && errno == EBADF,         "path io: lseek -> EBADF");

    /* readv/writev 也应 EBADF */
    char rb[4]; struct iovec iov = { .iov_base = rb, .iov_len = 1 };
    errno = 0;
    CHECK(readv(fd, &iov, 1) == -1 && errno == EBADF,             "path io: readv -> EBADF");
    errno = 0;
    CHECK(writev(fd, &iov, 1) == -1 && errno == EBADF,            "path io: writev -> EBADF");

    close(fd);
}

/* path_fstat_works(): 原为 "O_PATH fd 上 fstat" 测试用例; 因 starry 与 Linux
 * 行为不一致, 已移到 bugfix 复现:
 * test-suit/starryos/qemu-smp1/system/bugfix-bug-open-path-fstat-ebadf/c/src/main.c */

static void path_close_ok(void)
{
    int fd = open(M_FILE, O_PATH);
    CHECK(fd >= 0,                                                "path close: open ok");
    if (fd < 0) return;
    CHECK(close(fd) == 0,                                         "path close: close ok");
}

static void path_dup_ok(void)
{
    int fd = open(M_FILE, O_PATH);
    CHECK(fd >= 0,                                                "path dup: open ok");
    if (fd < 0) return;
    int fd2 = dup(fd);
    CHECK(fd2 >= 0,                                               "path dup: dup ok");
    if (fd2 >= 0) {
        /* duplicated fd 仍是 O_PATH */
        char buf[2];
        errno = 0;
        CHECK(read(fd2, buf, 1) == -1 && errno == EBADF,          "path dup: dup'd fd read -> EBADF");
        close(fd2);
    }
    close(fd);
}

static void path_on_dir_as_dirfd(void)
{
    int fd = open(M_SUB, O_PATH);
    CHECK(fd >= 0,                                                "path dirfd: open dir ok");
    if (fd < 0) return;

    int sub_fd = openat(fd, "inner", O_RDONLY);
    CHECK(sub_fd >= 0,                                            "path dirfd: openat with O_PATH dirfd ok");
    if (sub_fd >= 0) {
        char buf[4] = {0};
        ssize_t n = read(sub_fd, buf, sizeof(buf) - 1);
        CHECK(n == 2 && memcmp(buf, "in", 2) == 0,                "path dirfd: openat fd reads inner");
        close(sub_fd);
    }
    close(fd);
}

static void path_getfl_includes_o_path(void)
{
    int fd = open(M_FILE, O_PATH);
    CHECK(fd >= 0,                                                "path getfl: open ok");
    if (fd < 0) return;
    int fl = fcntl(fd, F_GETFL);
    CHECK(fl >= 0 && (fl & O_PATH) != 0,                          "path getfl: F_GETFL includes O_PATH");
    close(fd);
}

static void path_with_nofollow_returns_symlink_fd(void)
{
    int fd = open(M_SYM, O_PATH | O_NOFOLLOW);
    CHECK(fd >= 0,                                                "path|NOFOLLOW: returns sym fd ok");
    if (fd >= 0) {
        struct stat st;
        if (fstat(fd, &st) == 0)
            CHECK(S_ISLNK(st.st_mode),                            "path|NOFOLLOW: fstat sees symlink");
        close(fd);
    }
}

static void path_setting_cloexec_works(void)
{
    /* O_PATH + O_CLOEXEC 一起设；F_GETFD 应见 FD_CLOEXEC */
    int fd = open(M_FILE, O_PATH | O_CLOEXEC);
    CHECK(fd >= 0,                                                "path|CLOEXEC: open ok");
    if (fd < 0) return;
    int fl = fcntl(fd, F_GETFD);
    CHECK(fl >= 0 && (fl & FD_CLOEXEC),                           "path|CLOEXEC: FD_CLOEXEC set");
    close(fd);
}

int open_path_run(void)
{
    printf("\n----- open_path -----\n");
    mod_setup();
    path_basic_open_on_file();
    path_basic_open_on_dir();
    path_io_must_ebadf();
    path_close_ok();
    path_dup_ok();
    path_on_dir_as_dirfd();
    path_getfl_includes_o_path();
    path_with_nofollow_returns_symlink_fd();
    path_setting_cloexec_works();
    cleanup_tree(M_DIR);
    printf("  ----- open_path: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
