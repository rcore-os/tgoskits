#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/*
 * openat × flag combo × dirfd 镜像矩阵。
 *
 * man 2 open §"openat()" 原文：
 *   "The openat() system call operates in exactly the same way as open(),
 *    except for the differences described here."
 *
 * 本模块验证：openat(dirfd, path, flags) 与 open(path', flags) 在所有 flag
 * combo 下行为等价（path' = dirfd 决定的解析起点）。
 *
 * open_flag_matrix.c 已覆盖 open(path, flags) 的全 combo × 5 target；
 * 本模块复用同一 combo 逻辑，但通过 openat 的 4 种 dirfd_kind：
 *   - AT_FDCWD + abs path
 *   - AT_FDCWD + rel path（先 chdir）
 *   - RDONLY-dir-fd + rel path
 *   - O_PATH-dir-fd + rel path
 *
 * 缩减：因 open_flag_matrix 已覆盖 1024 combo×5 target×3 access = 15360，
 * 本模块只跑核心 combo 子集（11 行 REG_COMBOS） × 4 dirfd_kind = 47 case，
 * 重点验证"openat 与 open 等价"而非重复全 flag combo 矩阵。
 */

#define M_DIR     OF_MOD("openat_flagmat")
#define M_REG     M_DIR "/reg"
#define M_SUB     M_DIR "/sub"

/* 核心 combo 子集（每个验证 openat 与 open 行为一致）
 * 不含 silent flags / DIRECT 等 */
struct combo_row {
    int access;
    int extra;
    int expect_ok;          /* 1 = 期望 fd>=0；0 = 期望失败 */
    int expect_errno;       /* 失败时期望 errno；0 = 不检查 */
    const char *name;
};

static const struct combo_row REG_COMBOS[] = {
    /* target = M_REG（已存在普通文件，相对路径 "reg"）*/
    { O_RDONLY, 0,                    1, 0,       "RDONLY" },
    { O_WRONLY, 0,                    1, 0,       "WRONLY" },
    { O_RDWR,   0,                    1, 0,       "RDWR" },
    { O_RDONLY, O_CLOEXEC,            1, 0,       "RDONLY|CLOEXEC" },
    { O_RDONLY, O_NONBLOCK,           1, 0,       "RDONLY|NONBLOCK" },
    { O_WRONLY, O_APPEND,             1, 0,       "WRONLY|APPEND" },
    { O_WRONLY, O_TRUNC,              1, 0,       "WRONLY|TRUNC" },
    { O_RDWR,   O_CREAT,              1, 0,       "RDWR|CREAT (file exists)" },
    { O_RDWR,   O_CREAT | O_EXCL,     0, EEXIST,  "RDWR|CREAT|EXCL on existing -> EEXIST" },
    { O_RDONLY, O_DIRECTORY,          0, ENOTDIR, "RDONLY|DIRECTORY on file -> ENOTDIR" },
    { O_RDONLY, O_PATH,               1, 0,       "RDONLY|PATH" },
};

#define N_REG_COMBOS (sizeof(REG_COMBOS)/sizeof(REG_COMBOS[0]))

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "openat_flagmat setup: mkdir top");
    CHECK(ensure_dir(M_SUB) == 0,                                 "openat_flagmat setup: subdir");
    CHECK(write_file(M_REG, "data", 4, 0644) == 0,                "openat_flagmat setup: regular");
    umask(0);
}

static void run_combos(int dirfd, const char *path, const char *dirfd_name)
{
    for (size_t i = 0; i < N_REG_COMBOS; i++) {
        const struct combo_row *r = &REG_COMBOS[i];
        char msg[200];

        /* 还原 file 大小（防 TRUNC 改坏）*/
        if (get_file_size(M_REG) != 4)
            write_file(M_REG, "data", 4, 0644);

        errno = 0;
        int fd = openat(dirfd, path, r->access | r->extra, 0644);

        int ok;
        if (r->expect_ok) {
            ok = (fd >= 0);
            snprintf(msg, sizeof(msg), "openat[%s, %s]: ok", dirfd_name, r->name);
        } else {
            ok = (fd == -1 && errno == r->expect_errno);
            snprintf(msg, sizeof(msg), "openat[%s, %s]: -> errno=%d",
                     dirfd_name, r->name, r->expect_errno);
        }
        CHECK_QUIET(ok, msg);
        if (fd >= 0) close(fd);
    }
}

int openat_flag_matrix_run(void)
{
    printf("\n----- openat_flag_matrix -----\n");
    mod_setup();

    /* 1) AT_FDCWD + 绝对路径 */
    run_combos(AT_FDCWD, M_REG, "AT_FDCWD,abs");

    /* 2) AT_FDCWD + 相对路径（先 chdir）*/
    char saved[256];
    if (getcwd(saved, sizeof(saved)) && chdir(M_DIR) == 0) {
        run_combos(AT_FDCWD, "reg", "AT_FDCWD,rel");
        chdir(saved);
    }

    /* 3) RDONLY|DIRECTORY 打开的 M_DIR 作为 dirfd + 相对路径 "reg" */
    int dfd = open(M_DIR, O_RDONLY | O_DIRECTORY);
    if (dfd >= 0) {
        run_combos(dfd, "reg", "RDONLY-dir-fd,rel");
        close(dfd);
    }

    /* 4) O_PATH 打开的 M_DIR 作为 dirfd + 相对路径 "reg" */
    dfd = open(M_DIR, O_PATH);
    if (dfd >= 0) {
        run_combos(dfd, "reg", "O_PATH-dir-fd,rel");
        close(dfd);
    }

    cleanup_tree(M_DIR);
    printf("  ----- openat_flag_matrix: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
