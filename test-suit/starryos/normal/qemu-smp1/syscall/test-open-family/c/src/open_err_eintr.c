#define _GNU_SOURCE
#include "test_framework.h"
#include "helpers.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

/* EINTR: signal 中断阻塞中的 open() — Linux 行为返 -1 EINTR.
 *
 * man 2 open §"EINTR" 原文：
 *   "EINTR — While blocked waiting to complete an open of a slow device
 *    (e.g., a FIFO; see fifo(7)), the call was interrupted by a signal
 *    handler; see signal(7)."
 *
 * 触发条件：
 *   - "slow device"：FIFO 是最常见的；man 列举 FIFO 是首选
 *   - 当前进程在 open() 阻塞中（FIFO O_RDONLY 无 writer / O_WRONLY 无 reader 阻塞模式）
 *   - 收到没有 SA_RESTART 的 signal handler 处理后
 *
 * 测试方式（self-contained，全程在单进程内）：
 *   (a) FIFO O_RDONLY 无 writer + alarm(1) + SIGALRM handler 不带 SA_RESTART
 *       → open 阻塞 → 1s 后 SIGALRM → handler 返 → open 应 -1 EINTR
 *   (b) 同 (a) 但 handler 带 SA_RESTART
 *       → open 应自动重启（继续阻塞）— 用 timeout 短 alarm 验：
 *         alarm(1) 触发后 handler 退出，syscall 重启，再 alarm(1) 再退出循环。
 *       本测取消第二轮 alarm 让 open 永远阻塞 — 不实测，仅验 SA_RESTART 不
 *       触发 EINTR。改用 fork+writer 让 open 成功避开死锁。
 *   (c) FIFO O_WRONLY|O_NONBLOCK 无 reader → ENXIO（与 EINTR 互斥；NONBLOCK
 *       不会进入 slow path 阻塞 → 不会 EINTR）
 *
 * 已知差异：starry signal 子系统是否完整实现 SA_RESTART 与 EINTR 由 sigaction
 * 实现决定；若 starry 在 (a) 不返 EINTR → 走 bug-open-eintr 复现路径。本模块
 * 对 host 做 hard assert；should_skip 灰名单可挡 starry 主体。
 */

#define M_DIR  OF_MOD("err_eintr")

static volatile sig_atomic_t alrm_fired_count = 0;
static void alrm_handler(int sig)
{
    (void)sig;
    alrm_fired_count++;
}

/* (a) FIFO O_RDONLY 无 writer + signal 不带 SA_RESTART → -1 EINTR */
static void eintr_fifo_rdonly_no_writer(void)
{
    const char *fifo = M_DIR "/eintr_a";
    unlink(fifo);
    if (mkfifo(fifo, 0644) != 0) {
        CHECK(0, "EINTR (a) skip: mkfifo failed");
        return;
    }

    struct sigaction sa = {0};
    sa.sa_handler = alrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;  /* 不带 SA_RESTART → syscall 不自动重启 */
    sigaction(SIGALRM, &sa, NULL);

    alrm_fired_count = 0;
    alarm(1);  /* 1s 后 SIGALRM */
    errno = 0;
    int fd = open(fifo, O_RDONLY);  /* 无 writer 阻塞 */
    int err = errno;
    alarm(0);  /* 取消未触发的 alarm */

    int ok = (fd == -1 && err == EINTR && alrm_fired_count > 0);
    CHECK_OR_BUG(ok, "bug-open-eintr-not-implemented", "EINTR (a) FIFO RDONLY no writer + SIGALRM(no SA_RESTART) -> -1 EINTR");
    if (fd >= 0) close(fd);
    unlink(fifo);
}

/* (b) FIFO O_WRONLY|O_NONBLOCK 无 reader → ENXIO（基线对照，NONBLOCK 不阻塞 → 不 EINTR）*/
static void eintr_fifo_wronly_nonblock_baseline(void)
{
    const char *fifo = M_DIR "/eintr_b";
    unlink(fifo);
    if (mkfifo(fifo, 0644) != 0) {
        CHECK(0, "EINTR (b) skip: mkfifo failed");
        return;
    }

    /* 装 alarm 也无效 — NONBLOCK 立即返不会阻塞 */
    struct sigaction sa = {0};
    sa.sa_handler = alrm_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGALRM, &sa, NULL);
    alarm(1);

    errno = 0;
    int fd = open(fifo, O_WRONLY | O_NONBLOCK);
    int err = errno;
    alarm(0);

    /* 应是 -1 ENXIO（ENXIO 优先于 EINTR — 因为 NONBLOCK 立即返不进入 slow path）*/
    int ok = (fd == -1 && err == ENXIO);
    CHECK_OR_BUG(ok, "bug-open-fifo-wronly-no-reader-no-enxio", "EINTR (b) FIFO WRONLY|NONBLOCK no reader -> -1 ENXIO（基线，NONBLOCK 不进入 EINTR 路径）");
    if (fd >= 0) close(fd);
    unlink(fifo);
}

/* (c) FIFO O_RDONLY|O_NONBLOCK 无 writer → 0 （man: O_RDONLY|O_NONBLOCK FIFO 立即成功）*/
static void eintr_fifo_rdonly_nonblock_baseline(void)
{
    const char *fifo = M_DIR "/eintr_c";
    unlink(fifo);
    if (mkfifo(fifo, 0644) != 0) {
        CHECK(0, "EINTR (c) skip: mkfifo failed");
        return;
    }

    errno = 0;
    int fd = open(fifo, O_RDONLY | O_NONBLOCK);
    /* O_RDONLY|NONBLOCK FIFO 即使无 writer 也立即返 fd>=0（man fifo(7)）*/
    int ok = (fd >= 0);
    CHECK(ok,                                                    "EINTR (c) FIFO RDONLY|NONBLOCK no writer -> fd>=0（基线，man fifo(7)）");
    if (fd >= 0) close(fd);
    unlink(fifo);
}

int open_err_eintr_run(void)
{
    printf("\n----- open_err_eintr -----\n");
    cleanup_tree(M_DIR);
    CHECK(ensure_dir(M_DIR) == 0,                                "eintr setup: mkdir");
    eintr_fifo_rdonly_no_writer();
    eintr_fifo_wronly_nonblock_baseline();
    eintr_fifo_rdonly_nonblock_baseline();
    cleanup_tree(M_DIR);
    printf("  ----- open_err_eintr: %d pass, %d fail -----\n", __pass, __fail);
    return __fail;
}
