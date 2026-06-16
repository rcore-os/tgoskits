#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"
#include "open_access.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR  OF_MOD("access")
#define M_FILE M_DIR "/file"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "access setup: mkdir");
    CHECK(write_file(M_FILE, "abcdef", 6, 0644) == 0,             "access setup: file 'abcdef'");
}

/* 跨 3 个 access 模式（RDONLY/WRONLY/RDWR）穷举：
 * - 打开是否成功
 * - read 是否被允许（仅 RDONLY/RDWR 应允许）
 * - write 是否被允许（仅 WRONLY/RDWR 应允许）
 * - lseek 在三种模式下都允许
 *
 * man 2 open （DESCRIPTION 段，access mode 强制条款）原文：
 *   "The argument flags must include one of the following access modes:
 *    O_RDONLY, O_WRONLY, or O_RDWR. These request opening the file
 *    read-only, write-only, or read/write, respectively."
 *
 * 行为含义：access mode 决定 fd 上后续 I/O 的合法性 —— RDONLY 的 fd 不能 write、
 *           WRONLY 的 fd 不能 read（VFS 层在 read/write 入口检查 FMODE_READ/WRITE
 *           标志，违反返 EBADF）。lseek 不检查 access mode，三种都允许。 */
static void access_matrix(void)
{
    struct row {
        int access;
        const char *name;
        int read_ok;        /* 1 = 期望 read 成功，0 = 期望 EBADF */
        int write_ok;       /* 1 = 期望 write 成功，0 = 期望 EBADF */
    } rows[] = {
        { O_RDONLY, "O_RDONLY", 1, 0 },
        { O_WRONLY, "O_WRONLY", 0, 1 },
        { O_RDWR,   "O_RDWR",   1, 1 },
    };

    for (size_t i = 0; i < sizeof(rows)/sizeof(rows[0]); i++) {
        const struct row *r = &rows[i];
        char msg[128];

        /* 怎么测：以 access 模式打开已存在文件
         * 期望：fd>=0
         * 为什么：man 明确「以 X 方式打开文件」，仅 access 模式不附带其他副作用 */
        snprintf(msg, sizeof(msg), "access[%s]: open ok", r->name);
        int fd = open(M_FILE, r->access);
        CHECK(fd >= 0, msg);
        if (fd < 0) continue;

        /* 怎么测：尝试从 fd 读
         * 期望：read_ok ? 读到 'a' : -1+EBADF
         * 为什么：写打开的 fd 读 / 读打开的 fd 写应被 VFS 层拒绝（FMODE_READ/FMODE_WRITE 检查）*/
        char buf[4] = {0};
        snprintf(msg, sizeof(msg), "access[%s]: read behavior", r->name);
        if (r->read_ok) {
            ssize_t n = read(fd, buf, 1);
            CHECK(n == 1 && buf[0] == 'a', msg);
        } else {
            errno = 0;
            ssize_t n = read(fd, buf, 1);
            CHECK(n == -1 && errno == EBADF, msg);
        }

        /* 怎么测：把光标重定位到 1，再尝试 write
         * 期望：write_ok ? 写入 1 字节 : -1+EBADF
         * 为什么：见上 */
        lseek(fd, 1, SEEK_SET);
        snprintf(msg, sizeof(msg), "access[%s]: write behavior", r->name);
        if (r->write_ok) {
            ssize_t n = write(fd, "Z", 1);
            CHECK(n == 1, msg);
        } else {
            errno = 0;
            ssize_t n = write(fd, "Z", 1);
            CHECK(n == -1 && errno == EBADF, msg);
        }

        /* 怎么测：lseek 到末尾
         * 期望：返 6（文件大小）
         * 为什么：lseek 不依赖 access 模式，所有模式都允许 */
        snprintf(msg, sizeof(msg), "access[%s]: lseek SEEK_END allowed", r->name);
        off_t end = lseek(fd, 0, SEEK_END);
        CHECK(end == 6, msg);

        close(fd);

        /* 文件还原（以便下一次循环干净）*/
        write_file(M_FILE, "abcdef", 6, 0644);
    }
}

static void mod_teardown(void)
{
    cleanup_tree(M_DIR);
}

int open_access_run(void)
{
    printf("\n----- open_access -----\n");
    mod_setup();
    access_matrix();
    mod_teardown();
    printf("  ----- open_access: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
