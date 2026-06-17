#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define M_DIR   OF_MOD("nonblock")
#define M_FILE  M_DIR "/file"
#define M_FIFO  M_DIR "/fifo"

/* O_NONBLOCK 行为验证。
 *
 * man 2 open §"O_NONBLOCK or O_NDELAY" 原文：
 *   "O_NONBLOCK or O_NDELAY — When possible, the file is opened in
 *    nonblocking mode. Neither the open() nor any subsequent I/O
 *    operations on the file descriptor which is returned will cause the
 *    calling process to wait."
 *   "Note that this flag has no effect for regular files and block
 *    devices; that is, I/O operations will (briefly) block when device
 *    activity is required, regardless of whether O_NONBLOCK is set. ..."
 *
 * 4 场景：默认未设 / O_NONBLOCK 设上 F_GETFL 可见 / F_SETFL round-trip /
 *        普通文件 read 不 EAGAIN（man 明示无效果）。 */
static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "nonblock setup: mkdir");
    CHECK(write_file(M_FILE, "abc", 3, 0644) == 0,                "nonblock setup: regular file");
}

static void nonblock_default_off_on_regular(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0,                                                "nonblock default: open ok");
    if (fd < 0) return;

    int fl = fcntl(fd, F_GETFL);
    CHECK(fl >= 0,                                                "nonblock default: F_GETFL ok");
    /* 怎么测：F_GETFL 不应含 O_NONBLOCK
     * 期望：(fl & O_NONBLOCK) == 0
     * 为什么：默认 open 不设 O_NONBLOCK */
    CHECK((fl & O_NONBLOCK) == 0,                                 "nonblock default: O_NONBLOCK NOT in F_GETFL");
    close(fd);
}

static void nonblock_set_visible_in_getfl(void)
{
    int fd = open(M_FILE, O_RDONLY | O_NONBLOCK);
    CHECK(fd >= 0,                                                "nonblock set: open|O_NONBLOCK ok");
    if (fd < 0) return;

    int fl = fcntl(fd, F_GETFL);
    /* 怎么测：F_GETFL 应含 O_NONBLOCK
     * 期望：(fl & O_NONBLOCK) != 0
     * 为什么：man 明确「the flag is preserved and visible via F_GETFL」 */
    CHECK(fl >= 0 && (fl & O_NONBLOCK) != 0,                      "nonblock set: O_NONBLOCK in F_GETFL");
    close(fd);
}

static void nonblock_setfl_round_trip(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0,                                                "nonblock SETFL: open ok");
    if (fd < 0) return;

    int fl = fcntl(fd, F_GETFL);
    /* 怎么测：F_SETFL 设 O_NONBLOCK，再 F_GETFL 检查
     * 期望：可见；再 F_SETFL 清除，再 F_GETFL 检查 */
    CHECK(fcntl(fd, F_SETFL, fl | O_NONBLOCK) == 0,               "nonblock SETFL: set ok");
    CHECK((fcntl(fd, F_GETFL) & O_NONBLOCK) != 0,                 "nonblock SETFL: GET shows set");
    CHECK(fcntl(fd, F_SETFL, fl & ~O_NONBLOCK) == 0,              "nonblock SETFL: clear ok");
    CHECK((fcntl(fd, F_GETFL) & O_NONBLOCK) == 0,                 "nonblock SETFL: GET shows clear");
    close(fd);
}

static void nonblock_regular_read_not_eagain(void)
{
    /* 怎么测：O_NONBLOCK 打开普通文件，立刻 read
     * 期望：n>=0（普通文件 NONBLOCK 无效）
     * 为什么：man「has no effect for regular files」—— 普通文件读不会 EAGAIN */
    int fd = open(M_FILE, O_RDONLY | O_NONBLOCK);
    CHECK(fd >= 0,                                                "nonblock regular: open ok");
    if (fd < 0) return;
    char buf[8] = {0};
    ssize_t n = read(fd, buf, sizeof(buf));
    CHECK(n >= 0,                                                 "nonblock regular: read does not EAGAIN");
    CHECK(n == 3 && memcmp(buf, "abc", 3) == 0,                   "nonblock regular: read returns content");
    close(fd);
}

int open_nonblock_run(void)
{
    printf("\n----- open_nonblock -----\n");
    mod_setup();
    nonblock_default_off_on_regular();
    nonblock_set_visible_in_getfl();
    nonblock_setfl_round_trip();
    nonblock_regular_read_not_eagain();
    cleanup_tree(M_DIR);
    printf("  ----- open_nonblock: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
