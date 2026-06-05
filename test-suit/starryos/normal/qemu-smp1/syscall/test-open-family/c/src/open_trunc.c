#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR  OF_MOD("trunc")
#define M_FILE M_DIR "/file"
#define M_SUB  M_DIR "/sub"
#define M_SYM  M_DIR "/sym"

/* O_TRUNC 状态矩阵。
 *
 * man 2 open §"O_TRUNC" 原文：
 *   "O_TRUNC — If the file already exists and is a regular file and the
 *    access mode allows writing (i.e., is O_RDWR or O_WRONLY) it will be
 *    truncated to length 0. If the file is a FIFO or terminal device file,
 *    the O_TRUNC flag is ignored. Otherwise, the effect of O_TRUNC is
 *    unspecified."
 *
 * 配合 EISDIR：
 *   "EISDIR — pathname refers to a directory and the access requested
 *    involved writing"
 *
 * 注：O_RDONLY|O_TRUNC 是 POSIX-undefined（man VERSIONS：
 *   "The (undefined) effect of O_RDONLY | O_TRUNC varies among
 *    implementations. On many systems the file is actually truncated."）
 *   Linux 多数 fs 截断；starry OpenOptions::is_valid() 拒绝 → EINVAL。
 *   本测例不 assert 该场景，移到 bugfix/bug-open-rdonly-trunc-einval 复现。
 *
 * 4 init_size × 2 access 矩阵 + creat-new + dir-write 共 9 子断言组。 */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "trunc setup: mkdir");
    CHECK(ensure_dir(M_SUB) == 0,                                 "trunc setup: subdir");
}

/* 矩阵：(初始文件大小) × (access 模式) → 期望大小（仅可写模式才截断） */
static void trunc_size_matrix(void)
{
    struct row {
        size_t init_size;
        int access_mode;
        const char *am_name;
        int expect_truncate;       /* 1 = 期望截断到 0；0 = 期望大小不变 */
    } rows[] = {
        { 0,       O_WRONLY, "WRONLY", 1 },
        { 1,       O_WRONLY, "WRONLY", 1 },
        { 5,       O_WRONLY, "WRONLY", 1 },
        { 1024,    O_WRONLY, "WRONLY", 1 },
        { 0,       O_RDWR,   "RDWR",   1 },
        { 1,       O_RDWR,   "RDWR",   1 },
        { 5,       O_RDWR,   "RDWR",   1 },
        { 1024,    O_RDWR,   "RDWR",   1 },
        /* O_RDONLY|O_TRUNC: POSIX-undefined。在 Linux host 上多数文件系统会截断；
         * 在 starry 上 is_valid() 返 EINVAL。本测例不 assert 这种场景以保持
         * test-open-family 全绿；差异留给独立 bug-* 复现。 */
    };

    char buf[2048];
    memset(buf, 'X', sizeof(buf));

    for (size_t i = 0; i < sizeof(rows)/sizeof(rows[0]); i++) {
        const struct row *r = &rows[i];
        char msg[160];

        write_file(M_FILE, buf, r->init_size, 0644);
        CHECK((size_t)get_file_size(M_FILE) == r->init_size,      "trunc: pre-size correct");

        /* 怎么测：以 access|O_TRUNC 打开
         * 期望：fd>=0；文件大小被截断为 0
         * 为什么：man 写明可写模式必须截断到 0 */
        int fd = open(M_FILE, r->access_mode | O_TRUNC);
        snprintf(msg, sizeof(msg), "trunc[init=%zu, %s]: open|O_TRUNC ok", r->init_size, r->am_name);
        CHECK(fd >= 0, msg);
        if (fd < 0) continue;
        close(fd);

        snprintf(msg, sizeof(msg), "trunc[init=%zu, %s]: post-size==%d", r->init_size, r->am_name,
                 r->expect_truncate ? 0 : (int)r->init_size);
        CHECK(get_file_size(M_FILE) == (r->expect_truncate ? 0 : (off_t)r->init_size), msg);
    }
}

/* O_TRUNC + O_CREAT：新建空文件，O_TRUNC 无副作用 */
static void trunc_with_creat_new_file(void)
{
    unlink(M_FILE);
    int fd = open(M_FILE, O_CREAT | O_RDWR | O_TRUNC, 0644);
    CHECK(fd >= 0,                                                 "trunc+CREAT new: ok");
    if (fd >= 0) close(fd);
    CHECK(get_file_size(M_FILE) == 0,                              "trunc+CREAT new: size==0");
}

/* O_TRUNC 在目录上 + O_RDONLY：应 EISDIR 不到 _open() 的 truncate 路径
 * 注：O_DIRECTORY|O_RDONLY|O_TRUNC 在目录上 Linux 是 EISDIR；starry 也是。 */
static void trunc_on_directory(void)
{
    errno = 0;
    int fd = open(M_SUB, O_RDWR | O_TRUNC);
    CHECK(fd == -1 && errno == EISDIR,                             "trunc on directory + RDWR -> EISDIR");
    if (fd >= 0) close(fd);
}

int open_trunc_run(void)
{
    printf("\n----- open_trunc -----\n");
    mod_setup();
    trunc_size_matrix();
    trunc_with_creat_new_file();
    trunc_on_directory();
    cleanup_tree(M_DIR);
    printf("  ----- open_trunc: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
