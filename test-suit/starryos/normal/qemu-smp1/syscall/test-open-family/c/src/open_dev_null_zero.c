#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* open() 打开「字符设备节点」目标 —— /dev/null 与 /dev/zero。
 *
 * 现有 28 模块覆盖 open 的目标类型：普通文件 / 目录 / 有效 symlink /
 * 悬空 symlink / 目录 symlink，但未覆盖「打开字符设备节点」这一目标类型。
 * 本模块补该缺口：open 对设备节点解析成功后返回 fd，其 read/write 语义由
 * 设备驱动决定。
 *
 * man 2 open 原文（节选）：
 *   "open() ... return a file descriptor ... a small, nonnegative integer
 *    ... The file offset is set to the beginning of the file."
 * man 4 null（/dev/null、/dev/zero）原文（节选）：
 *   "Data written to a null ... special file is discarded."
 *   "Reads from a null special file always return end of file (i.e.,
 *    read(2) returns 0), whereas reads from /dev/zero always return
 *    bytes containing zero ('\0' characters)."
 *   "Writes to ... /dev/zero ... succeed ... the data is discarded."
 *
 * 仅验「打开成功 + 设备 read/write 语义 + honored flag 不被破坏」，
 * 不验设备驱动实现细节。 */

/* /dev/null：read 立即 EOF；write 接受并丢弃（返回写入字节数）。 */
static void devnull_semantics(void)
{
    int fd = open("/dev/null", O_RDONLY);
    CHECK(fd >= 0, "/dev/null: open O_RDONLY ok");
    if (fd >= 0) {
        char buf[16];
        CHECK(read(fd, buf, sizeof(buf)) == 0, "/dev/null: read returns 0 (EOF)");
        close(fd);
    }

    fd = open("/dev/null", O_WRONLY);
    CHECK(fd >= 0, "/dev/null: open O_WRONLY ok");
    if (fd >= 0) {
        CHECK(write(fd, "discard me", 10) == 10, "/dev/null: write accepts + discards (returns count)");
        close(fd);
    }

    fd = open("/dev/null", O_RDWR);
    CHECK(fd >= 0, "/dev/null: open O_RDWR ok");
    if (fd >= 0) {
        char buf[4];
        CHECK(write(fd, "x", 1) == 1, "/dev/null: O_RDWR write ok");
        CHECK(read(fd, buf, sizeof(buf)) == 0, "/dev/null: O_RDWR read EOF");
        close(fd);
    }
}

/* /dev/zero：read 总返回全 0 字节（无限源）；write 接受并丢弃。 */
static void devzero_semantics(void)
{
    int fd = open("/dev/zero", O_RDONLY);
    CHECK(fd >= 0, "/dev/zero: open O_RDONLY ok");
    if (fd >= 0) {
        char buf[64];
        memset(buf, 0xAB, sizeof(buf));
        ssize_t n = read(fd, buf, sizeof(buf));
        CHECK(n == (ssize_t)sizeof(buf), "/dev/zero: read fills the whole buffer");
        int all_zero = 1;
        for (size_t i = 0; i < sizeof(buf); i++)
            if (buf[i] != 0)
                all_zero = 0;
        CHECK(all_zero, "/dev/zero: read returns all-zero bytes");

        /* 第二次 read 仍是全 0（无限源，offset 推进不影响内容） */
        memset(buf, 0xAB, sizeof(buf));
        n = read(fd, buf, 8);
        int second_zero = (n == 8);
        for (int i = 0; i < 8 && second_zero; i++)
            if (buf[i] != 0)
                second_zero = 0;
        CHECK(second_zero, "/dev/zero: second read still all-zero");
        close(fd);
    }

    fd = open("/dev/zero", O_WRONLY);
    CHECK(fd >= 0, "/dev/zero: open O_WRONLY ok");
    if (fd >= 0) {
        CHECK(write(fd, "ignored", 7) == 7, "/dev/zero: write accepts + discards");
        close(fd);
    }
}

/* honored flag（O_CLOEXEC）在设备节点上同样生效。 */
static void device_honored_flags(void)
{
    int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    CHECK(fd >= 0, "/dev/null: open O_RDONLY|O_CLOEXEC ok");
    if (fd >= 0) {
        int fl = fcntl(fd, F_GETFD);
        CHECK(fl >= 0 && (fl & FD_CLOEXEC), "/dev/null: FD_CLOEXEC honored on a device fd");
        close(fd);
    }

    /* O_DIRECTORY 用在非目录的设备节点上 → ENOTDIR（与普通文件一致） */
    CHECK_ERR(open("/dev/null", O_RDONLY | O_DIRECTORY), ENOTDIR,
              "/dev/null: O_DIRECTORY on a char device → ENOTDIR");
}

int open_dev_null_zero_run(void)
{
    printf("\n----- open_dev_null_zero -----\n");
    devnull_semantics();
    devzero_semantics();
    device_honored_flags();
    printf("  ----- open_dev_null_zero: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
