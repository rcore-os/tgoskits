/*
 * !test-epoll-et — epoll(7) EPOLLET 边缘触发语义穷尽测试(浏览器/nginx 事件循环命脉)
 *
 * ground truth: man 7 epoll "Level-triggered and edge-triggered" + Linux v7.2
 * fs/eventpoll.c ep_send_events。浏览器(Chromium/Firefox)与 nginx 事件循环重度依赖
 * EPOLLET: 只在状态"边缘"(不可读->可读)通知一次, 消费后不再重复通知直到新事件。
 *
 * =====================================================================
 * 语义 (man 7 epoll)
 * =====================================================================
 *   EPOLLET(边缘): fd 就绪只报一次; 即使数据未读完/未读, 后续 epoll_wait 不再报,
 *     直到有新事件(新写入)产生新边缘。
 *   默认 LT(水平): 只要 fd 仍可读, 每次 epoll_wait 都报。
 *   Linux ep_send_events: LT 报完把 epi 重新加回 ready list; ET 不加回(靠新 wakeup)。
 *
 * =====================================================================
 * StarryOS 对齐 (file/epoll.rs EDGE_TRIGGER=EPOLLET)
 * =====================================================================
 *   epoll.rs:45 EDGE_TRIGGER=EPOLLET; :730 ET 装新 waker 等下次边缘。
 * =====================================================================
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死\n==== test-epoll-et 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

/* ===== A. eventfd + EPOLLET: 就绪只报一次, 新写才再报 ===== */
static int test_et_eventfd(void)
{
    TEST_START("A. eventfd EPOLLET 边缘: 只报一次 + 新写再报");
    int ep = epoll_create1(0);
    int ev = eventfd(0, EFD_NONBLOCK);
    CHECK(ep >= 0 && ev >= 0, "epoll_create1 + eventfd");
    if (ep < 0 || ev < 0) { TEST_DONE(); }

    struct epoll_event e = { .events = EPOLLIN | EPOLLET, .data.fd = ev };
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, ev, &e) == 0, "EPOLL_CTL_ADD EPOLLIN|EPOLLET");

    uint64_t one = 1;
    CHECK(write(ev, &one, 8) == 8, "write eventfd(产生可读边缘)");

    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "首次 epoll_wait 报 EPOLLIN");

    /* ★ET 关键: 不消费(不读 eventfd), 再 epoll_wait -> 边缘已报过, 无新写 -> 超时 0 */
    n = epoll_wait(ep, out, 4, 100);
    CHECK(n == 0, "EPOLLET 不重复报(未读也不再报, 边缘已消费)");

    /* 新写产生新边缘 -> 再报 */
    CHECK(write(ev, &one, 8) == 8, "再 write eventfd(新边缘)");
    n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "新写后 epoll_wait 再报(新边缘)");

    close(ev);
    close(ep);
    TEST_DONE();
}

/* ===== B. pipe + EPOLLET: 部分读后不重报(second-chunk 语义) ===== */
static int test_et_pipe_partial(void)
{
    TEST_START("B. pipe EPOLLET 部分读后不重报");
    int pfd[2];
    if (pipe(pfd) != 0) { CHECK(0, "pipe"); TEST_DONE(); }
    fcntl(pfd[0], F_SETFL, O_NONBLOCK);

    int ep = epoll_create1(0);
    struct epoll_event e = { .events = EPOLLIN | EPOLLET, .data.fd = pfd[0] };
    epoll_ctl(ep, EPOLL_CTL_ADD, pfd[0], &e);

    char buf[100];
    memset(buf, 'x', sizeof(buf));
    CHECK(write(pfd[1], buf, sizeof(buf)) == (ssize_t)sizeof(buf), "write 100B 到 pipe");

    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "首次 epoll_wait 报可读");

    /* 只读 50B, 管道还剩 50B 可读, 但 ET 边缘已报 -> 再 epoll_wait 超时 */
    char rb[50];
    CHECK(read(pfd[0], rb, 50) == 50, "部分读 50B(还剩 50B)");
    n = epoll_wait(ep, out, 4, 100);
    CHECK(n == 0, "EPOLLET 部分读后不重报(剩 50B 也不报, 无新边缘)");

    /* 再写产生新边缘 -> 报 */
    CHECK(write(pfd[1], buf, 10) == 10, "再 write 10B(新边缘)");
    n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1, "新写后再报(新边缘)");

    close(pfd[0]);
    close(pfd[1]);
    close(ep);
    TEST_DONE();
}

/* ===== C. LT 对照: 不消费则每次都报(与 ET 区分) ===== */
static int test_lt_contrast(void)
{
    TEST_START("C. LT(默认)对照: 未读则每次都报");
    int ep = epoll_create1(0);
    int ev = eventfd(0, EFD_NONBLOCK);
    struct epoll_event e = { .events = EPOLLIN, .data.fd = ev }; /* 无 EPOLLET = LT */
    epoll_ctl(ep, EPOLL_CTL_ADD, ev, &e);

    uint64_t one = 1;
    write(ev, &one, 8);

    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "LT 首次报 EPOLLIN");
    /* ★LT 关键: 不读, 再 epoll_wait -> 仍可读 -> 继续报(与 ET 相反) */
    n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "LT 未读也重复报(水平触发)");

    close(ev);
    close(ep);
    TEST_DONE();
}

/* ===== D. pipe EPOLLET read-until-EAGAIN 显式契约(ET 应用核心) ===== */
static int test_et_read_until_eagain(void)
{
    TEST_START("D. pipe EPOLLET: read 循环读到 EAGAIN 判 I/O 空间耗尽");
    int pfd[2];
    if (pipe(pfd) != 0) { CHECK(0, "pipe"); TEST_DONE(); }
    CHECK(fcntl(pfd[0], F_SETFL, O_NONBLOCK) == 0, "读端设 O_NONBLOCK");

    int ep = epoll_create1(0);
    struct epoll_event e = { .events = EPOLLIN | EPOLLET, .data.fd = pfd[0] };
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, pfd[0], &e) == 0, "EPOLL_CTL_ADD EPOLLIN|EPOLLET");

    char wbuf[100];
    memset(wbuf, 'z', sizeof(wbuf));
    CHECK(write(pfd[1], wbuf, sizeof(wbuf)) == (ssize_t)sizeof(wbuf), "write 100B(产生边缘)");

    struct epoll_event out[4];
    int n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "首次 epoll_wait 报 EPOLLIN");

    /* man 7 epoll: ET 就绪后必须循环 read 直到 EAGAIN 才判 I/O 空间耗尽。 */
    char rbuf[40];
    ssize_t total = 0;
    int got_short = 0;
    int loops = 0;
    for (;;) {
        errno = 0;
        ssize_t r = read(pfd[0], rbuf, sizeof(rbuf));
        if (r > 0) {
            total += r;
            if (r < (ssize_t)sizeof(rbuf)) got_short = 1;
            if (++loops > 100) break;
            continue;
        }
        break;
    }
    CHECK(total == 100, "循环 read 累计读回全部 100B");
    CHECK(got_short == 1, "末次 read 短读(<40B) = 流耗尽信号(man stream-oriented)");

    CHECK_ERR(read(pfd[0], rbuf, sizeof(rbuf)), EAGAIN,
              "排空后 read 返回 EAGAIN(ET 显式 I/O 耗尽契约)");

    n = epoll_wait(ep, out, 4, 100);
    CHECK(n == 0, "读到 EAGAIN 后无新边缘, epoll_wait 不再报");

    CHECK(write(pfd[1], wbuf, 10) == 10, "再 write 10B(新边缘)");
    n = epoll_wait(ep, out, 4, 200);
    CHECK(n == 1 && (out[0].events & EPOLLIN), "新写后 epoll_wait 再报(新边缘)");

    close(pfd[0]);
    close(pfd[1]);
    close(ep);
    TEST_DONE();
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_et_eventfd();
    fail |= test_et_pipe_partial();
    fail |= test_lt_contrast();
    fail |= test_et_read_until_eagain();
    printf("\n==== test-epoll-et 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
