#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

/* ENXIO 三个 variant。
 *
 * man 2 open §"ENXIO" 原文：
 *   "ENXIO — O_NONBLOCK | O_WRONLY is set, the named file is a FIFO, and
 *    no process has the FIFO open for reading."
 *   "ENXIO — The file is a device special file and no corresponding
 *    device exists."
 *   "ENXIO — The file is a UNIX domain socket."
 *
 * 本模块原本覆盖 (1) FIFO 无 reader 和 (3) UNIX socket；starry 上两者都不报
 * ENXIO，已分别移到 bug-open-fifo-wronly-no-reader-no-enxio 和
 * bug-open-unix-socket-no-enxio。当前模块仅 mod_setup（占位）。
 * (2) 设备特殊文件需 mknod 特权，跳过。 */

#define M_DIR  OF_MOD("err_enxio")

/* enxio_unix_socket_file(): 原为 "open(UNIX socket 文件) → ENXIO" 测试用例;
 * 因 starry 与 Linux 行为不一致(starry 不区分), 已移到 bugfix 复现:
 * test-suit/starryos/qemu-smp1/system/bugfix-bug-open-unix-socket-no-enxio/c/src/main.c */

/* enxio_fifo_wronly_no_reader(): 原为 "FIFO O_WRONLY|O_NONBLOCK 无读者 → ENXIO" 测试用例;
 * 因 starry 与 Linux 行为不一致(starry 不检查), 已移到 bugfix 复现:
 * test-suit/starryos/qemu-smp1/system/bugfix-bug-open-fifo-wronly-no-reader-no-enxio/c/src/main.c */

/* codex P1 (PR #1, adopted in part): 主 suite 应有 ENXIO active 断言.
 * 正常文件 open 不误报 ENXIO — 反向断言验证错误路径 gate 正确. */
static void enxio_normal_open_does_not_false_positive(void)
{
    int fd = open(OF_REGULAR, O_RDONLY);
    CHECK(fd >= 0,                                                 "enxio positive: open normal file 不 ENXIO");
    if (fd >= 0) close(fd);
}

int open_err_enxio_run(void)
{
    printf("\n----- open_err_enxio -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                 "enxio setup: mkdir");
    enxio_normal_open_does_not_false_positive();
    /* 真 ENXIO 触发场景 (starry 不实现) 已移到 bug-*:
     * - bug-open-unix-socket-no-enxio
     * - bug-open-fifo-wronly-no-reader-no-enxio */
    cleanup_tree(M_DIR);
    printf("  ----- open_err_enxio: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
