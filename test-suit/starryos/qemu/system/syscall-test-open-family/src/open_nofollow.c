#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR        OF_MOD("nofollow")
#define M_TARGET     M_DIR "/target"
#define M_SYM2REG    M_DIR "/sym_to_reg"
#define M_SUB        M_DIR "/sub"
#define M_SYM2DIR    M_DIR "/sym_to_dir"
#define M_MID_SYM    M_DIR "/mid_sym"             /* 中间组件是 symlink 到 sub */

/* O_NOFOLLOW 行为矩阵。
 *
 * man 2 open §"O_NOFOLLOW" 原文：
 *   "O_NOFOLLOW — If the trailing component (i.e., basename) of pathname
 *    is a symbolic link, then the open fails, with the error ELOOP.
 *    Symbolic links in earlier components of the pathname will still be
 *    followed. (Note that the ELOOP error that can occur in this case is
 *    indistinguishable from the kind of ELOOP error that occurs when an
 *    open fails because there are too many symbolic links found while
 *    resolving components in the prefix part of the pathname.)"
 *
 * 配合 O_PATH 与 O_NOFOLLOW 共用条款：
 *   "If pathname is a symbolic link and the O_NOFOLLOW flag is also
 *    specified, then the call returns a file descriptor referring to the
 *    symbolic link." (in §"O_PATH")
 *
 * 5 子场景：basename-真文件 OK / basename-symlink → ELOOP（移到 bug-*）
 *           / 中间组件是 symlink 仍跟随 / basename-sym→dir → ELOOP（移到 bug-*）
 *           / O_PATH|O_NOFOLLOW 在 sym 上返回 symlink 自身 fd. */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                "nofollow setup: mkdir");
    CHECK(ensure_dir(M_SUB) == 0,                                "nofollow setup: subdir");
    CHECK(write_file(M_TARGET, "target!", 7, 0644) == 0,         "nofollow setup: target file");
    CHECK(write_file(M_SUB "/inner", "inner!", 6, 0644) == 0,    "nofollow setup: inner file under subdir");
    CHECK(symlink(M_TARGET, M_SYM2REG) == 0,                     "nofollow setup: sym to reg");
    CHECK(symlink(M_SUB,    M_SYM2DIR) == 0,                     "nofollow setup: sym to dir");
    CHECK(symlink(M_SUB,    M_MID_SYM) == 0,                     "nofollow setup: middle symlink to subdir");
}

/* nofollow_basename_symlink(): 原为 "basename 是 symlink + O_NOFOLLOW → ELOOP"
 * 测试用例; 因 starry 与 Linux 行为不一致(starry 不返 ELOOP), 已移到 bugfix 复现:
 * test-suit/starryos/qemu/system/bugfix-bug-open-nofollow-sym/c/src/main.c */

/* basename 不是 symlink → 正常打开 */
static void nofollow_basename_regular(void)
{
    /* 怎么测：open(target, O_RDONLY|O_NOFOLLOW)
     * 期望：fd>=0
     * 为什么：basename 是真文件，O_NOFOLLOW 无影响 */
    int fd = open(M_TARGET, O_RDONLY | O_NOFOLLOW);
    CHECK(fd >= 0,                                               "nofollow basename regular -> ok");
    if (fd >= 0) close(fd);
}

/* 中间组件是 symlink，basename 是真名 → 仍跟随中间 symlink */
static void nofollow_middle_symlink_followed(void)
{
    /* 怎么测：open("middle_sym/inner", O_NOFOLLOW)
     * 期望：fd>=0；读到 "inner!"
     * 为什么：man 明确 "Symbolic links in earlier components ... will still be followed" */
    int fd = open(M_MID_SYM "/inner", O_RDONLY | O_NOFOLLOW);
    CHECK(fd >= 0,                                               "nofollow middle sym + real basename -> ok");
    if (fd >= 0) {
        char buf[16] = {0};
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        CHECK(n == 6 && memcmp(buf, "inner!", 6) == 0,           "nofollow middle sym: content correct");
        close(fd);
    }
}

/* nofollow_basename_symlink_to_dir(): 原为 "basename 是指向目录的 symlink + O_NOFOLLOW
 * → ELOOP" 测试用例(上面 bug 的另一变体); 因 starry 与 Linux 行为不一致, 已移到 bugfix 复现:
 * test-suit/starryos/qemu/system/bugfix-bug-open-nofollow-sym/c/src/main.c */

/* O_PATH | O_NOFOLLOW：man「If pathname is a symbolic link and the O_NOFOLLOW
 * flag is also specified, then the call returns a file descriptor referring
 * to the symbolic link.」—— 此组合不应 ELOOP，应返回指向 symlink 自身的 fd */
static void nofollow_with_path_returns_symlink_fd(void)
{
    int fd = open(M_SYM2REG, O_PATH | O_NOFOLLOW);
    /* 期望：fd>=0；fstat 后 S_ISLNK */
    CHECK(fd >= 0,                                               "nofollow|O_PATH on sym -> ok (returns symlink fd)");
    if (fd >= 0) {
        struct stat st;
        if (fstat(fd, &st) == 0)
            CHECK(S_ISLNK(st.st_mode),                           "nofollow|O_PATH: fstat says symlink");
        close(fd);
    }
}

int open_nofollow_run(void)
{
    printf("\n----- open_nofollow -----\n");
    mod_setup();
    /* nofollow_basename_symlink()        — 已移到 bugfix/bug-open-nofollow-sym
     * nofollow_basename_symlink_to_dir() — 同上 bug 的另一变体 */
    nofollow_basename_regular();
    nofollow_middle_symlink_followed();
    nofollow_with_path_returns_symlink_fd();
    cleanup_tree(M_DIR);
    printf("  ----- open_nofollow: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
