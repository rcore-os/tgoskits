#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define M_DIR   OF_MOD("pread_pwrite")
#define M_FILE  M_DIR "/ro"          /* 只读基线，内容 "0123456789"，测例不改 */
#define M_FILEW M_DIR "/rw"          /* pwrite 目标，会被改写 */

/* open() 返回的 fd 的「文件偏移 / 定位 I/O」语义。
 *
 * 现有模块 open_fd_semantics 覆盖 fd 表（dup / 最低可用 fd / O_CLOEXEC 表项），
 * 但未覆盖 open 返回的 open file description 的「文件偏移」这一维度。本模块补：
 * 初始 offset、read 推进 offset、pread/pwrite 不改 offset、lseek 定位。
 *
 * man 2 open 原文：
 *   "A call to open() creates a new open file description ... The file offset
 *    is set to the beginning of the file (see lseek(2))."
 * man 2 pread 原文：
 *   "pread() reads up to count bytes from file descriptor fd at offset offset
 *    (from the start of the file) into the buffer ... The file offset is not
 *    changed."
 * man 2 pwrite 原文：
 *   "pwrite() writes up to count bytes from the buffer ... to the file
 *    descriptor fd at offset offset ... The file offset is not changed." */

static void mod_setup(void)
{
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                          "pread_pwrite setup: mkdir");
    CHECK(write_file(M_FILE, "0123456789", 10, 0644) == 0, "pread_pwrite setup: ro 10-byte file");
    CHECK(write_file(M_FILEW, "0123456789", 10, 0644) == 0, "pread_pwrite setup: rw 10-byte file");
}

/* open 后初始 offset = 0；read 按读取字节数推进 offset */
static void initial_offset_and_read_advance(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0, "open RDONLY ok");
    if (fd < 0)
        return;
    CHECK(lseek(fd, 0, SEEK_CUR) == 0, "open: initial file offset is 0");
    char buf[4] = {0};
    CHECK(read(fd, buf, 3) == 3 && memcmp(buf, "012", 3) == 0, "read 3 bytes → '012'");
    CHECK(lseek(fd, 0, SEEK_CUR) == 3, "read advanced offset to 3");
    close(fd);
}

/* pread 在指定 offset 读，且不改变 fd 的 offset */
static void pread_no_offset_change(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0, "open RDONLY ok");
    if (fd < 0)
        return;
    char buf[4] = {0};
    CHECK(pread(fd, buf, 3, 5) == 3 && memcmp(buf, "567", 3) == 0, "pread at off=5 reads '567'");
    CHECK(lseek(fd, 0, SEEK_CUR) == 0, "pread did NOT change the fd offset (still 0)");
    /* 普通 read 仍从 offset 0 开始 */
    memset(buf, 0, sizeof(buf));
    CHECK(read(fd, buf, 3) == 3 && memcmp(buf, "012", 3) == 0, "read after pread still reads from 0");
    close(fd);
}

/* pwrite 写到指定 offset，且不改变 fd 的 offset */
static void pwrite_no_offset_change(void)
{
    int fd = open(M_FILEW, O_RDWR);
    CHECK(fd >= 0, "open RDWR ok");
    if (fd < 0)
        return;
    CHECK(pwrite(fd, "XY", 2, 4) == 2, "pwrite 2 bytes at off=4");
    CHECK(lseek(fd, 0, SEEK_CUR) == 0, "pwrite did NOT change the fd offset (still 0)");
    char buf[16] = {0};
    CHECK(pread(fd, buf, 10, 0) == 10 && memcmp(buf, "0123XY6789", 10) == 0,
          "pwrite landed exactly at off=4");
    close(fd);
}

/* lseek SEEK_END / SEEK_SET 定位（针对未改写的只读文件） */
static void lseek_positions(void)
{
    int fd = open(M_FILE, O_RDONLY);
    CHECK(fd >= 0, "open RDONLY ok");
    if (fd < 0)
        return;
    CHECK(lseek(fd, 0, SEEK_END) == 10, "SEEK_END == file size 10");
    CHECK(lseek(fd, 4, SEEK_SET) == 4, "SEEK_SET to 4");
    char c = 0;
    CHECK(read(fd, &c, 1) == 1 && c == '4', "read at SEEK_SET 4 gives '4'");
    close(fd);
}

int open_pread_pwrite_run(void)
{
    printf("\n----- open_pread_pwrite -----\n");
    mod_setup();
    initial_offset_and_read_advance();
    pread_no_offset_change();
    pwrite_no_offset_change();
    lseek_positions();
    cleanup_tree(M_DIR);
    printf("  ----- open_pread_pwrite: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
