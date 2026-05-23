#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* ELOOP — 解析 pathname 时遇到太多 symlink。
 *
 * man 2 open §"ELOOP" 原文（2 个 variant，本模块覆盖第 1 个）：
 *   "ELOOP — Too many symbolic links were encountered in resolving
 *    pathname."
 *   (第 2 个 variant: "ELOOP — pathname is a symbolic link, and flags
 *    specified O_NOFOLLOW but not O_PATH" — 移到 bug-open-nofollow-sym)
 *
 * Linux 默认 max-symlink-follow = 40；axfs-ng/highlevel/fs.rs:SYMLINKS_MAX
 * 也是 40。
 *
 * 3 子场景：A→B B→A 双节循环 / a→a 自循环 / 41 节深链（超 SYMLINKS_MAX）。 */

#define M_DIR  OF_MOD("err_eloop")

/* A → B → A 的两节循环 */
static void eloop_two_node_cycle(void)
{
    char a[64], b[64];
    snprintf(a, sizeof(a), "%s/a", M_DIR);
    snprintf(b, sizeof(b), "%s/b", M_DIR);
    unlink(a); unlink(b);

    /* 怎么测：a→b ; b→a 的循环，open(a)
     * 期望：-1 + ELOOP（解析 a → b → a → b → ... 超过 40 次）
     * 为什么：man ELOOP 第一变体 */
    CHECK(symlink(b, a) == 0,                                     "eloop setup: a -> b");
    CHECK(symlink(a, b) == 0,                                     "eloop setup: b -> a");

    errno = 0;
    int fd = open(a, O_RDONLY);
    CHECK(fd == -1 && errno == ELOOP,                             "eloop: a→b→a... cycle -> ELOOP");
    if (fd >= 0) close(fd);

    unlink(a); unlink(b);
}

/* 单节自循环：a → a */
static void eloop_self_cycle(void)
{
    char a[64];
    snprintf(a, sizeof(a), "%s/self", M_DIR);
    unlink(a);
    CHECK(symlink(a, a) == 0,                                     "eloop setup: self loop");

    errno = 0;
    int fd = open(a, O_RDONLY);
    CHECK(fd == -1 && errno == ELOOP,                             "eloop: a→a self -> ELOOP");
    if (fd >= 0) close(fd);

    unlink(a);
}

/* 41 节深链（超过 SYMLINKS_MAX=40） */
static void eloop_deep_chain(void)
{
    char real[64];
    snprintf(real, sizeof(real), "%s/end", M_DIR);
    write_file(real, "x", 1, 0644);

    char prev[64], cur[64];
    snprintf(prev, sizeof(prev), "%s", real);

    int chain_len = 41;
    for (int i = 0; i < chain_len; i++) {
        snprintf(cur, sizeof(cur), "%s/sym%d", M_DIR, i);
        unlink(cur);
        symlink(prev, cur);
        snprintf(prev, sizeof(prev), "%s/sym%d", M_DIR, i);
    }

    /* 现在 sym40 → sym39 → ... → sym0 → end，共 41 跳，超过 40 */
    errno = 0;
    int fd = open(prev, O_RDONLY);
    CHECK(fd == -1 && errno == ELOOP,                             "eloop: 41-deep chain -> ELOOP");
    if (fd >= 0) close(fd);

    /* 清理 */
    for (int i = 0; i < chain_len; i++) {
        char p[64];
        snprintf(p, sizeof(p), "%s/sym%d", M_DIR, i);
        unlink(p);
    }
    unlink(real);
}

int open_err_eloop_run(void)
{
    printf("\n----- open_err_eloop -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "eloop setup: mkdir");
    eloop_two_node_cycle();
    eloop_self_cycle();
    eloop_deep_chain();
    cleanup_tree(M_DIR);
    printf("  ----- open_err_eloop: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
