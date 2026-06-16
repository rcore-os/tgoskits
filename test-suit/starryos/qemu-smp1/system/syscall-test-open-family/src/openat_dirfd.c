#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* openat() dirfd 语义验证。
 *
 * man 2 open §"openat()" 原文：
 *   "The openat() system call operates in exactly the same way as open(),
 *    except for the differences described here."
 *   "If the pathname given in pathname is relative, then it is interpreted
 *    relative to the directory referred to by the file descriptor dirfd
 *    (rather than relative to the current working directory of the calling
 *    process, as is done by open() for a relative pathname)."
 *   "If pathname is relative and dirfd is the special value AT_FDCWD,
 *    then pathname is interpreted relative to the current working
 *    directory of the calling process (like open())."
 *   "If pathname is absolute, then dirfd is ignored."
 *
 * 5 子场景：AT_FDCWD === open / 相对+dirfd / 绝对忽略 valid dirfd /
 * O_PATH dirfd / chdir+rel == abs.
 *
 * 绝对忽略 invalid dirfd 的强形式断言（man "If pathname is absolute, then
 * dirfd is ignored" 不带"必须有效"前提）由独立的
 * bug-openat-abs-path-honors-invalid-dirfd 复现承担 — 因为在 starry 上
 * 该 case 触发 fd-table 异常路径有 SIGSEGV 风险，不适合放在 test 主体里。 */

#define M_DIR     OF_MOD("openat_dirfd")
#define M_FILE    M_DIR "/file"
#define M_SUB     M_DIR "/sub"
#define M_INNER   M_SUB "/inner"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "openat_dirfd setup: mkdir top");
    CHECK(ensure_dir(M_SUB) == 0,                                 "openat_dirfd setup: subdir");
    CHECK(write_file(M_FILE, "top", 3, 0644) == 0,                "openat_dirfd setup: top file");
    CHECK(write_file(M_INNER, "in", 2, 0644) == 0,                "openat_dirfd setup: inner file");
}

/* 1. AT_FDCWD === open() */
static void at_fdcwd_equiv_open(void)
{
    int a = openat(AT_FDCWD, M_FILE, O_RDONLY);
    int b = open(M_FILE, O_RDONLY);
    CHECK(a >= 0 && b >= 0,                                       "openat AT_FDCWD: both opens ok");
    char ba[4] = {0}, bb[4] = {0};
    if (a >= 0) read(a, ba, 3);
    if (b >= 0) read(b, bb, 3);
    CHECK(memcmp(ba, "top", 3) == 0 && memcmp(bb, "top", 3) == 0, "openat AT_FDCWD: same content");
    if (a >= 0) close(a);
    if (b >= 0) close(b);
}

/* 2. 相对路径 + 有效 dirfd → 相对该目录解析 */
static void relative_with_dirfd(void)
{
    int dfd = open(M_SUB, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0,                                               "openat relative dirfd: open subdir ok");
    if (dfd < 0) return;

    int fd = openat(dfd, "inner", O_RDONLY);
    CHECK(fd >= 0,                                                "openat relative dirfd: openat \"inner\" ok");
    if (fd >= 0) {
        char buf[4] = {0};
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        CHECK(n == 2 && memcmp(buf, "in", 2) == 0,                "openat relative dirfd: content correct");
        close(fd);
    }
    close(dfd);
}

/* 3. 绝对路径 → dirfd 被忽略
 *    使用一个有效的、与目标无关的 dirfd（应不影响绝对路径解析） */
static void absolute_path_ignores_dirfd(void)
{
    int dfd = open(M_SUB, O_RDONLY | O_DIRECTORY);
    CHECK(dfd >= 0,                                               "openat abs ignores dirfd: open unrelated dir ok");
    if (dfd < 0) return;

    /* 绝对路径 M_FILE，dirfd 是 sub —— 应忽略 dirfd 直接打开 M_FILE */
    int fd = openat(dfd, M_FILE, O_RDONLY);
    CHECK(fd >= 0,                                                "openat abs ignores dirfd: openat(sub_fd, /abs/path) ok");
    if (fd >= 0) {
        char buf[4] = {0};
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        CHECK(n == 3 && memcmp(buf, "top", 3) == 0,               "openat abs ignores dirfd: read top file content");
        close(fd);
    }
    close(dfd);
}

/* 4. O_PATH 打开的目录可作 dirfd */
static void o_path_dirfd_works(void)
{
    int dfd = open(M_SUB, O_PATH);
    CHECK(dfd >= 0,                                               "openat O_PATH dirfd: open ok");
    if (dfd < 0) return;

    int fd = openat(dfd, "inner", O_RDONLY);
    CHECK(fd >= 0,                                                "openat O_PATH dirfd: openat ok");
    if (fd >= 0) {
        char buf[4] = {0};
        read(fd, buf, sizeof(buf) - 1);
        CHECK(memcmp(buf, "in", 2) == 0,                          "openat O_PATH dirfd: content correct");
        close(fd);
    }
    close(dfd);
}

/* 4b. 绝对路径 + invalid dirfd → 必须忽略 dirfd 仍打开成功
 *
 * 该正向断言由独立的 bug-openat-abs-path-honors-invalid-dirfd 复现承担 —
 * 因为在 PR #2 fix-open-openat-bugs 加入「abs path → dirfd = AT_FDCWD」
 * 校正前，starry 对 openat(-1, "/abs") 直接 EBADF，与 Linux man 不符。
 * 把正向断言放在 test-open-family 内部需要带 fork+wait 包裹（避免 starry
 * 内核 fd-table 异常情况下 SIGSEGV 杀掉整个 runner），开销大；不如直接
 * 用 bug-* 单独验证。已删除本函数；bug-* 仍然覆盖该行为。
 */

/* 5. 相对路径 + AT_FDCWD vs 绝对路径效果一致 */
static void at_fdcwd_relative_eq_absolute(void)
{
    /* 把 cwd 改到 M_DIR，相对路径 "file" 应等价于绝对路径 M_FILE */
    char saved[256];
    if (!getcwd(saved, sizeof(saved))) { CHECK(0, "getcwd"); return; }
    if (chdir(M_DIR) != 0) { CHECK(0, "chdir M_DIR"); return; }

    int a = openat(AT_FDCWD, "file", O_RDONLY);
    int b = openat(AT_FDCWD, M_FILE, O_RDONLY);
    CHECK(a >= 0 && b >= 0,                                       "openat AT_FDCWD rel==abs: both ok");
    if (a >= 0) close(a);
    if (b >= 0) close(b);

    chdir(saved);
}

int openat_dirfd_run(void)
{
    printf("\n----- openat_dirfd -----\n");
    mod_setup();
    at_fdcwd_equiv_open();
    relative_with_dirfd();
    absolute_path_ignores_dirfd();
    /* absolute_path_ignores_invalid_dirfd 移到 bug-openat-abs-path-honors-invalid-dirfd
     * 单独验证，避免 starry 上 fd-table 异常导致 SIGSEGV 拖垮整个 runner */
    o_path_dirfd_works();
    at_fdcwd_relative_eq_absolute();
    cleanup_tree(M_DIR);
    printf("  ----- openat_dirfd: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
