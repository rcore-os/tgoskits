#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"
#include "open_directory.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* O_DIRECTORY 在不同目标 + access mode 组合下的行为。
 *
 * man 2 open §"O_DIRECTORY" 原文：
 *   "O_DIRECTORY — If pathname is not a directory, cause the open to fail.
 *    This flag was added in Linux 2.1.126, to avoid denial-of-service
 *    problems if opendir(3) is called on a FIFO or tape device."
 *
 * 配合 EISDIR 条款：
 *   "EISDIR — pathname refers to a directory and the access requested
 *    involved writing (that is, O_WRONLY or O_RDWR is set)."
 *
 * 配合 ENOTDIR 条款：
 *   "ENOTDIR — A component used as a directory in pathname is not, in
 *    fact, a directory, or O_DIRECTORY was specified and pathname was not
 *    a directory."
 *
 * 6 目标（file/dir/sym→dir/sym→file/dangling/absent）× 2 flag (with/without
 * O_DIRECTORY) + 3 access mode 子矩阵（dir+RDONLY OK / dir+WRONLY/RDWR EISDIR）。 */

#define M_DIR     OF_MOD("directory")
#define M_FILE    M_DIR "/file"
#define M_SUBDIR  M_DIR "/sub"
#define M_SYM2DIR M_DIR "/sym_to_sub"
#define M_SYM2FIL M_DIR "/sym_to_file"
#define M_DANGLE  M_DIR "/dangle"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "directory setup: mkdir top");
    CHECK(ensure_dir(M_SUBDIR) == 0,                              "directory setup: mkdir sub");
    CHECK(write_file(M_FILE, "x", 1, 0644) == 0,                  "directory setup: file");
    CHECK(symlink(M_SUBDIR, M_SYM2DIR) == 0,                      "directory setup: symlink to dir");
    CHECK(symlink(M_FILE,   M_SYM2FIL) == 0,                      "directory setup: symlink to file");
    CHECK(symlink(M_DIR "/nope", M_DANGLE) == 0,                  "directory setup: dangling symlink");
}

/* 矩阵：（O_DIRECTORY 是否设置）×（路径目标类型）→ 期望
 *
 * 目标类型：
 *   T1: 普通文件（M_FILE）
 *   T2: 目录（M_SUBDIR）
 *   T3: 指向目录的 symlink（M_SYM2DIR）
 *   T4: 指向文件的 symlink（M_SYM2FIL）
 *   T5: 悬空 symlink（M_DANGLE）
 *   T6: 不存在路径
 *
 * 期望（man + Linux POSIX 实测）：
 *   without O_DIRECTORY: T1 OK / T2 OK(O_RDONLY) / T3 OK / T4 OK / T5 ENOENT / T6 ENOENT
 *   with O_DIRECTORY:    T1 ENOTDIR / T2 OK / T3 OK / T4 ENOTDIR / T5 ENOENT / T6 ENOENT */
static void directory_target_matrix(void)
{
    struct row {
        const char *path;
        int with_dir_flag_should;     /* 0 = 期望成功（fd>=0）, 否则 = errno 值 */
        int without_dir_flag_should;
        const char *name;
    } rows[] = {
        { M_FILE,    ENOTDIR, 0,       "T1 regular file" },
        { M_SUBDIR,  0,       0,       "T2 directory" },
        { M_SYM2DIR, 0,       0,       "T3 sym -> dir" },
        { M_SYM2FIL, ENOTDIR, 0,       "T4 sym -> file" },
        { M_DANGLE,  ENOENT,  ENOENT,  "T5 dangling symlink" },
        { M_DIR "/nonexistent_xx", ENOENT, ENOENT, "T6 nonexistent" },
    };

    for (size_t i = 0; i < sizeof(rows)/sizeof(rows[0]); i++) {
        const struct row *r = &rows[i];
        char msg[160];

        /* without O_DIRECTORY */
        errno = 0;
        int fd = open(r->path, O_RDONLY);
        if (r->without_dir_flag_should == 0) {
            snprintf(msg, sizeof(msg), "directory[%s]: open RDONLY ok", r->name);
            CHECK(fd >= 0, msg);
            if (fd >= 0) close(fd);
        } else {
            snprintf(msg, sizeof(msg), "directory[%s]: open RDONLY -> errno=%d", r->name, r->without_dir_flag_should);
            CHECK(fd == -1 && errno == r->without_dir_flag_should, msg);
            if (fd >= 0) close(fd);
        }

        /* with O_DIRECTORY */
        errno = 0;
        fd = open(r->path, O_RDONLY | O_DIRECTORY);
        if (r->with_dir_flag_should == 0) {
            snprintf(msg, sizeof(msg), "directory[%s]: open RDONLY|O_DIRECTORY ok", r->name);
            CHECK(fd >= 0, msg);
            if (fd >= 0) close(fd);
        } else {
            snprintf(msg, sizeof(msg), "directory[%s]: open RDONLY|O_DIRECTORY -> errno=%d", r->name, r->with_dir_flag_should);
            CHECK(fd == -1 && errno == r->with_dir_flag_should, msg);
            if (fd >= 0) close(fd);
        }
    }
}

/* 子矩阵：O_DIRECTORY 与 access 模式组合
 * man 隐含：打开目录必须用 O_RDONLY；O_WRONLY/O_RDWR 应 EISDIR */
static void directory_with_access_modes(void)
{
    int modes[] = { O_RDONLY, O_WRONLY, O_RDWR };
    int expected[] = { 0, EISDIR, EISDIR };
    const char *names[] = { "O_RDONLY", "O_WRONLY", "O_RDWR" };

    for (size_t i = 0; i < 3; i++) {
        char msg[128];
        errno = 0;
        int fd = open(M_SUBDIR, modes[i] | O_DIRECTORY);
        if (expected[i] == 0) {
            snprintf(msg, sizeof(msg), "directory access[%s+O_DIRECTORY]: ok", names[i]);
            CHECK(fd >= 0, msg);
        } else {
            snprintf(msg, sizeof(msg), "directory access[%s+O_DIRECTORY]: errno=%d", names[i], expected[i]);
            CHECK(fd == -1 && errno == expected[i], msg);
        }
        if (fd >= 0) close(fd);
    }
}

static void mod_teardown(void)
{
    cleanup_tree(M_DIR);
}

int open_directory_run(void)
{
    printf("\n----- open_directory -----\n");
    mod_setup();
    directory_target_matrix();
    directory_with_access_modes();
    mod_teardown();
    printf("  ----- open_directory: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
