#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"
#include "open_append.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR  OF_MOD("append")
#define M_FILE M_DIR "/file"

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0, "append setup: mkdir");
}

/* O_APPEND 必须保证 write 总在文件末尾（原子）。
 * 验证场景：
 *   1) 单 fd 多次 write 累加
 *   2) lseek 回头部后写仍在末尾（不被 lseek 覆盖）
 *   3) 多 fd 同时 O_APPEND 写不互相覆盖
 *   4) RDWR|APPEND 与 WRONLY|APPEND 等价
 *
 * man 2 open §"O_APPEND" 原文：
 *   "O_APPEND — The file is opened in append mode. Before each write(2),
 *    the file offset is positioned at the end of the file, as if with
 *    lseek(2). The modification of the file offset and the write operation
 *    are performed as a single atomic step."
 *
 * 关键：原子性意味着 (a) lseek 改的 offset 在 write 时被忽略 / 重置到末尾；
 *       (b) 多 fd 并发 append 不会互相覆盖（每次写前重读 inode 末尾）。 */
static void append_basic_concat(void)
{
    write_file(M_FILE, "AA", 2, 0644);
    int fd = open(M_FILE, O_WRONLY | O_APPEND);
    /* 期望：fd>=0；写入累加到末尾 */
    CHECK(fd >= 0, "append: open WRONLY|APPEND ok");
    if (fd < 0) return;

    /* 怎么测：连续两次 write "B" "C"；不 lseek
     * 期望：文件最终 = "AABC"
     * 为什么：append 每次写前 offset 已被定位到末尾 */
    CHECK(write(fd, "B", 1) == 1, "append: write 'B'");
    CHECK(write(fd, "C", 1) == 1, "append: write 'C'");
    close(fd);

    char buf[8] = {0};
    ssize_t n = read_file(M_FILE, buf, sizeof(buf));
    CHECK(n == 4 && memcmp(buf, "AABC", 4) == 0, "append: file content == 'AABC'");
}

static void append_lseek_does_not_override(void)
{
    write_file(M_FILE, "DD", 2, 0644);
    int fd = open(M_FILE, O_WRONLY | O_APPEND);
    CHECK(fd >= 0, "append: open for lseek-override test");
    if (fd < 0) return;

    /* 怎么测：lseek 到 0，再 write "E"
     * 期望：'E' 仍写在末尾，不覆盖位置 0
     * 为什么：man 明确 "Each write..., the file offset is positioned to the
     *         end of the file, as if with lseek(2)" —— append 模式下 lseek
     *         对写位置无效（atomic offset move + write）*/
    lseek(fd, 0, SEEK_SET);
    CHECK(write(fd, "E", 1) == 1, "append: write 'E' after lseek-to-0");
    close(fd);

    char buf[8] = {0};
    ssize_t n = read_file(M_FILE, buf, sizeof(buf));
    CHECK(n == 3 && memcmp(buf, "DDE", 3) == 0, "append: lseek-to-0 then write -> 'DDE' (not 'EDD')");
}

static void append_two_fds_no_overwrite(void)
{
    write_file(M_FILE, "F", 1, 0644);
    int fd1 = open(M_FILE, O_WRONLY | O_APPEND);
    int fd2 = open(M_FILE, O_WRONLY | O_APPEND);
    CHECK(fd1 >= 0 && fd2 >= 0, "append: two fds open ok");

    /* 怎么测：fd1 写 'G'，fd2 写 'H'
     * 期望：内容 == "FGH"（不丢字节）
     * 为什么：两个 fd 各自的 file description 都用 append 语义，
     *         每次 write 都重新读 inode 末尾 → 不会互相覆盖 */
    CHECK(write(fd1, "G", 1) == 1, "append: fd1 write 'G'");
    CHECK(write(fd2, "H", 1) == 1, "append: fd2 write 'H'");
    close(fd1);
    close(fd2);

    char buf[8] = {0};
    ssize_t n = read_file(M_FILE, buf, sizeof(buf));
    CHECK(n == 3 && memcmp(buf, "FGH", 3) == 0, "append: two fds -> 'FGH'");
}

static void append_rdwr_equiv_wronly(void)
{
    write_file(M_FILE, "I", 1, 0644);
    int fd = open(M_FILE, O_RDWR | O_APPEND);
    /* 期望：fd>=0；write 仍 append，read 仍读得回 */
    CHECK(fd >= 0, "append: open RDWR|APPEND ok");
    if (fd < 0) return;

    /* 怎么测：write "J"，再 lseek 0 + read 全部
     * 期望：write 把 'J' 加到末尾，read 读到 "IJ"
     * 为什么：RDWR|APPEND 与 WRONLY|APPEND 在 write 行为上等价；read 仍正常 */
    CHECK(write(fd, "J", 1) == 1, "append RDWR: write 'J'");
    lseek(fd, 0, SEEK_SET);
    char buf[8] = {0};
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    CHECK(n == 2 && memcmp(buf, "IJ", 2) == 0, "append RDWR: read sees 'IJ'");
    close(fd);
}

static void mod_teardown(void)
{
    cleanup_tree(M_DIR);
}

int open_append_run(void)
{
    printf("\n----- open_append -----\n");
    mod_setup();
    append_basic_concat();
    append_lseek_does_not_override();
    append_two_fds_no_overwrite();
    append_rdwr_equiv_wronly();
    mod_teardown();
    printf("  ----- open_append: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
