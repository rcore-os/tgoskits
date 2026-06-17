#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR     OF_MOD("excl")
#define M_REG     M_DIR "/reg"
#define M_SUB     M_DIR "/sub"
#define M_SYM     M_DIR "/sym"
#define M_DANGLE  M_DIR "/dangle"

/* O_CREAT|O_EXCL 矩阵。
 *
 * man 2 open §"O_EXCL" 原文（节选）：
 *   "O_EXCL — Ensure that this call creates the file: if this flag is
 *    specified in conjunction with O_CREAT, and pathname already exists,
 *    then open() fails with the error EEXIST."
 *   "When these two flags are specified, symbolic links are not followed:
 *    if pathname is a symbolic link, then open() fails regardless of where
 *    the symbolic link points."
 *   "In general, the behavior of O_EXCL is undefined if it is used without
 *    O_CREAT. There is one exception: on Linux 2.6 and later, O_EXCL can
 *    be used without O_CREAT if pathname refers to a block device."
 *
 * 5 目标（absent/regfile/subdir/sym→file/dangling）× 3 flag 组合（CREAT only /
 * CREAT|EXCL / EXCL only）= 10 行矩阵。重点验证：
 *   (a) absent + CREAT|EXCL → 原子创建；
 *   (b) any-existing + CREAT|EXCL → EEXIST（symlink 不被跟随，sym 自身存在 → EEXIST）；
 *   (c) EXCL only（无 CREAT）→ 等同无 EXCL（starry flags_to_options 故意忽略）。 */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                "excl setup: mkdir");
    CHECK(ensure_dir(M_SUB) == 0,                                "excl setup: subdir");
    CHECK(write_file(M_REG, "x", 1, 0644) == 0,                  "excl setup: regular file");
    CHECK(symlink(M_REG, M_SYM) == 0,                            "excl setup: symlink to file");
    CHECK(symlink(M_DIR "/no", M_DANGLE) == 0,                   "excl setup: dangling symlink");
    umask(0);
}

/* 矩阵：(目标存在态) × (是否带 O_CREAT/O_EXCL) → 期望
 *   T1 不存在路径
 *   T2 存在的普通文件
 *   T3 存在的目录
 *   T4 指向文件的 symlink
 *   T5 悬空 symlink
 *
 * (没 O_CREAT，单独 O_EXCL) 的 case：man「一般而言，单独 O_EXCL 的行为是未定义的；
 * 唯一例外是块设备」。在 starry，flags_to_options 仅在 O_CREAT 同时存在时才设
 * create_new，所以单独 O_EXCL 等价于无 O_EXCL。本矩阵也覆盖这一点。 */
static void excl_target_matrix(void)
{
    struct row {
        const char *path;
        const char *target_name;
        int has_creat;
        int has_excl;
        int expect_errno;       /* 0 = 期望成功 */
        const char *desc;
    } rows[] = {
        /* T1 不存在 */
        { M_DIR "/new1", "absent",     1, 0, 0,       "absent + O_CREAT only" },
        { M_DIR "/new2", "absent",     1, 1, 0,       "absent + O_CREAT|O_EXCL (creates atomically)" },
        { M_DIR "/new3", "absent",     0, 1, ENOENT,  "absent + O_EXCL only (undefined; starry treats as no-CREAT)" },
        /* T2 存在普通文件 */
        { M_REG,         "regfile",    1, 0, 0,       "regfile + O_CREAT only (just opens)" },
        { M_REG,         "regfile",    1, 1, EEXIST,  "regfile + O_CREAT|O_EXCL -> EEXIST" },
        { M_REG,         "regfile",    0, 1, 0,       "regfile + O_EXCL only (no-CREAT, just opens)" },
        /* T3 存在目录 */
        { M_SUB,         "subdir",     1, 1, EEXIST,  "subdir + O_CREAT|O_EXCL -> EEXIST" },
        /* T4 symlink 到文件 */
        { M_SYM,         "sym->file",  1, 0, 0,       "sym->file + O_CREAT (follow symlink, opens target)" },
        { M_SYM,         "sym->file",  1, 1, EEXIST,  "sym->file + O_CREAT|O_EXCL -> EEXIST (man: symlinks not followed when both flags set)" },
        /* T5 悬空 symlink */
        { M_DANGLE,      "dangling",   1, 1, EEXIST,  "dangling + O_CREAT|O_EXCL -> EEXIST (symlink itself exists)" },
    };

    for (size_t i = 0; i < sizeof(rows)/sizeof(rows[0]); i++) {
        const struct row *r = &rows[i];
        char msg[200];

        int flags = O_RDWR;
        if (r->has_creat) flags |= O_CREAT;
        if (r->has_excl)  flags |= O_EXCL;

        errno = 0;
        int fd = open(r->path, flags, 0644);

        if (r->expect_errno == 0) {
            snprintf(msg, sizeof(msg), "excl[%s, %s]: open ok (fd>=0)", r->target_name, r->desc);
            CHECK(fd >= 0, msg);
        } else {
            snprintf(msg, sizeof(msg), "excl[%s, %s]: -> errno=%d", r->target_name, r->desc, r->expect_errno);
            CHECK(fd == -1 && errno == r->expect_errno, msg);
        }
        if (fd >= 0) close(fd);

        /* 清理本次循环创建的新文件，避免污染下一行 */
        if (r->has_creat && r->expect_errno == 0 && strncmp(r->path, M_DIR "/new", strlen(M_DIR "/new")) == 0)
            unlink(r->path);
    }
}

/* 子矩阵：O_CREAT|O_EXCL 与 O_TRUNC / O_DIRECTORY 等组合 */
static void excl_with_other_flags(void)
{
    char p[64];
    snprintf(p, sizeof(p), "%s/combo", M_DIR);
    unlink(p);

    /* 怎么测：O_CREAT|O_EXCL|O_TRUNC，文件不存在
     * 期望：成功（O_TRUNC 在新创建空文件上无副作用）*/
    int fd = open(p, O_RDWR | O_CREAT | O_EXCL | O_TRUNC, 0644);
    CHECK(fd >= 0,                                                "excl combo: O_CREAT|O_EXCL|O_TRUNC absent ok");
    if (fd >= 0) close(fd);

    /* 怎么测：再次 O_CREAT|O_EXCL，文件已存在
     * 期望：EEXIST */
    errno = 0;
    fd = open(p, O_RDWR | O_CREAT | O_EXCL, 0644);
    CHECK(fd == -1 && errno == EEXIST,                            "excl combo: O_CREAT|O_EXCL existing -> EEXIST");
    if (fd >= 0) close(fd);

    unlink(p);
}

int open_excl_run(void)
{
    printf("\n----- open_excl -----\n");
    mod_setup();
    excl_target_matrix();
    excl_with_other_flags();
    cleanup_tree(M_DIR);
    printf("  ----- open_excl: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
