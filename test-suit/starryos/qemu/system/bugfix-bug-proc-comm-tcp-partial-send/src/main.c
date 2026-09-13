/*
 * proc-comm-tcp-partial-send.c - Regression for the two kernel gaps Envoy
 * (higress) exposed, fixed against Linux fs/proc/base.c (comm_show) and the
 * non-blocking TCP send path.
 *
 *   1. /proc/<pid>/comm read format: Linux comm_show() prints exactly the
 *      thread name followed by a single '\n' with no NUL padding. musl's
 *      pthread_getname_np() reads the file and strips only the LAST byte, so a
 *      16-byte NUL-padded buffer would leave the '\n' embedded inside the
 *      read-back name and break Envoy's thread setName() round-trip. This test
 *      asserts the exact byte layout via /proc/self/comm and via the real
 *      pthread_setname_np/pthread_getname_np round-trip Envoy uses.
 *
 *   2. Non-blocking TCP partial send: a partial send on an O_NONBLOCK socket
 *      must report the bytes already enqueued, not WouldBlock. StarryOS
 *      finish_tcp_send_step now honors the effective non-blocking flag
 *      (O_NONBLOCK || MSG_DONTWAIT); a regressed build only looked at
 *      MSG_DONTWAIT and returned EAGAIN after src was already consumed, so the
 *      caller retransmitted those bytes and corrupted the stream. This test
 *      fills the pipe with O_NONBLOCK (never MSG_DONTWAIT), drains a bounded
 *      amount, and asserts the next large send returns a partial count > 0.
 *
 * Loopback only, single process; deterministic.
 */

#define _GNU_SOURCE

#include "test_framework.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <unistd.h>

/* 1a. /proc/self/comm returns exactly "<name>\n" with no NUL padding. */
static void check_comm_read(const char *set_name, const char *expect)
{
    CHECK_RET(prctl(PR_SET_NAME, (unsigned long)set_name, 0, 0, 0), 0,
              "prctl(PR_SET_NAME) sets the thread name");

    int fd = open("/proc/self/comm", O_RDONLY);
    CHECK(fd >= 0, "open /proc/self/comm");
    if (fd < 0)
        return;

    char buf[64];
    memset(buf, 0xAA, sizeof(buf));
    ssize_t n = read(fd, buf, sizeof(buf));
    close(fd);

    size_t elen = strlen(expect);
    CHECK(n == (ssize_t)(elen + 1),
          "/proc/self/comm returns name + one newline with no NUL padding");
    if (n == (ssize_t)(elen + 1))
    {
        CHECK(memcmp(buf, expect, elen) == 0, "/proc/self/comm name bytes match");
        CHECK(buf[elen] == '\n', "/proc/self/comm ends with a single '\\n'");
    }
}

static void test_comm_read_format(void)
{
    check_comm_read("envoy", "envoy");
    check_comm_read("worker_0", "worker_0");
    /* TASK_COMM_LEN-1 = 15 characters is the exact boundary. */
    check_comm_read("abcdefghijklmno", "abcdefghijklmno");
    /* A longer name is truncated to 15 characters by the read path. */
    check_comm_read("abcdefghijklmnopqrst", "abcdefghijklmno");
}

/* Linux reads at most TASK_COMM_LEN - 1 bytes for PR_SET_NAME. Put exactly
 * that many non-NUL bytes at a page boundary to ensure the next byte is not
 * accessed. */
static void test_pr_set_name_bounded_user_read(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    CHECK(page_size > 0, "get page size for PR_SET_NAME boundary test");
    if (page_size <= 0)
        return;

    size_t page = (size_t)page_size;
    char *mapping = mmap(NULL, page * 2, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mapping != MAP_FAILED, "map pages for PR_SET_NAME boundary test");
    if (mapping == MAP_FAILED)
        return;

    char *name = mapping + page - 15;
    memset(name, 'x', 15);
    if (mprotect(mapping + page, page, PROT_NONE) != 0) {
        CHECK(0, "protect byte after PR_SET_NAME input");
        munmap(mapping, page * 2);
        return;
    }
    CHECK(1, "protect byte after PR_SET_NAME input");

    int set_name_ret = prctl(PR_SET_NAME, (unsigned long)name, 0, 0, 0);
    CHECK_RET(set_name_ret, 0, "PR_SET_NAME accepts 15 non-NUL bytes at page boundary");

    char got[16];
    char expected[16];
    memset(expected, 'x', 15);
    expected[15] = '\0';
    if (set_name_ret == 0) {
        int get_name_ret = prctl(PR_GET_NAME, (unsigned long)got, 0, 0, 0);
        CHECK_RET(get_name_ret, 0, "PR_GET_NAME returns bounded name");
        if (get_name_ret == 0) {
            CHECK(memcmp(got, expected, sizeof(expected)) == 0,
                  "PR_SET_NAME appends a NUL without reading the next page");
        }
    }

    CHECK_RET(munmap(mapping, page * 2), 0, "unmap PR_SET_NAME boundary test pages");
}

/* A NUL-terminated name may end at a page boundary: Linux stops at the NUL
 * and must not require the remaining bytes of the 15-byte input window. */
static void test_pr_set_name_stops_at_nul(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    CHECK(page_size > 0, "get page size for NUL-terminated PR_SET_NAME test");
    if (page_size <= 0)
        return;

    size_t page = (size_t)page_size;
    char *mapping = mmap(NULL, page * 2, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mapping != MAP_FAILED, "map pages for NUL-terminated PR_SET_NAME test");
    if (mapping == MAP_FAILED)
        return;

    char *name = mapping + page - 2;
    name[0] = 'x';
    name[1] = '\0';
    if (mprotect(mapping + page, page, PROT_NONE) != 0) {
        CHECK(0, "protect byte after NUL-terminated PR_SET_NAME input");
        munmap(mapping, page * 2);
        return;
    }
    CHECK(1, "protect byte after NUL-terminated PR_SET_NAME input");

    int set_name_ret = prctl(PR_SET_NAME, (unsigned long)name, 0, 0, 0);
    CHECK_RET(set_name_ret, 0, "PR_SET_NAME stops reading at a page-boundary NUL");

    char got[16];
    if (set_name_ret == 0) {
        int get_name_ret = prctl(PR_GET_NAME, (unsigned long)got, 0, 0, 0);
        CHECK_RET(get_name_ret, 0, "PR_GET_NAME returns NUL-terminated boundary name");
        if (get_name_ret == 0) {
            CHECK(got[0] == 'x' && got[1] == '\0',
                  "PR_SET_NAME preserves the short name without reading the next page");
        }
    }

    CHECK_RET(munmap(mapping, page * 2), 0,
              "unmap NUL-terminated PR_SET_NAME boundary test pages");
}

/* 1b. Envoy's real path: pthread_setname_np then pthread_getname_np must
 * round-trip byte-for-byte. The bug left a '\n' embedded in the read-back name
 * because musl strips only the trailing byte from a padded buffer. */
static void check_setname_roundtrip(const char *name)
{
    int rc = pthread_setname_np(pthread_self(), name);
    CHECK(rc == 0, "pthread_setname_np succeeds");
    if (rc != 0)
        return;

    char got[32];
    memset(got, 0x5A, sizeof(got));
    rc = pthread_getname_np(pthread_self(), got, sizeof(got));
    CHECK(rc == 0, "pthread_getname_np succeeds");
    if (rc != 0)
        return;

    CHECK(strcmp(got, name) == 0,
          "thread name round-trips with no embedded newline (Envoy setName)");
}

static void test_pthread_setname_roundtrip(void)
{
    check_setname_roundtrip("envoy");
    check_setname_roundtrip("worker_1");
    check_setname_roundtrip("abcdefghijklmno"); /* 15 chars, musl max */
}

/* 2. A non-blocking (O_NONBLOCK) partial send reports the queued byte count. */
static void test_nonblocking_partial_send(void)
{
    int ln = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(ln >= 0, "open TCP listener");
    if (ln < 0)
        return;

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0; /* ephemeral */
    CHECK_RET(bind(ln, (struct sockaddr *)&addr, sizeof(addr)), 0, "bind listener to loopback");
    CHECK_RET(listen(ln, 1), 0, "listen");

    socklen_t alen = sizeof(addr);
    CHECK_RET(getsockname(ln, (struct sockaddr *)&addr, &alen), 0, "getsockname listener port");

    int tx = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(tx >= 0, "open TCP sender");
    if (tx < 0)
    {
        close(ln);
        return;
    }

    /* Request small buffers on Linux. StarryOS currently uses fixed buffers,
     * so filling the pipe must also work efficiently with its larger capacity. */
    int small = 4096;
    setsockopt(tx, SOL_SOCKET, SO_SNDBUF, &small, sizeof(small));

    CHECK_RET(connect(tx, (struct sockaddr *)&addr, sizeof(addr)), 0, "connect to listener");

    int rx = accept(ln, NULL, NULL);
    CHECK(rx >= 0, "accept the connection");
    if (rx < 0)
    {
        close(tx);
        close(ln);
        return;
    }
    setsockopt(rx, SOL_SOCKET, SO_RCVBUF, &small, sizeof(small));

    /* Non-blocking via O_NONBLOCK (fcntl), never MSG_DONTWAIT: this is exactly
     * the path the fix repaired. */
    int fl = fcntl(tx, F_GETFL, 0);
    CHECK(fl >= 0, "fcntl F_GETFL on sender");
    CHECK_RET(fcntl(tx, F_SETFL, fl | O_NONBLOCK), 0, "set O_NONBLOCK on sender");

    /* Fill in chunks: one-byte sends spend most of the test in syscalls on
     * emulated CPUs. After EAGAIN, wait for outstanding ACKs before declaring
     * the pipe full; otherwise the receiver may still have unused capacity. */
    char fill[4096];
    memset(fill, 'x', sizeof(fill));
    long filled = 0;
    int hit_eagain = 0;
    unsigned int attempts = 0;
    while (attempts < 1024)
    {
        attempts++;
        ssize_t r = send(tx, fill, sizeof(fill), 0); /* rely on O_NONBLOCK */
        if (r > 0)
        {
            filled += r;
            continue;
        }
        if (r < 0 && errno == EAGAIN)
        {
            struct pollfd fill_ready = {.fd = tx, .events = POLLOUT};
            int ready = poll(&fill_ready, 1, 2000);
            if (ready == 0)
            {
                hit_eagain = 1;
                break;
            }
            CHECK(ready == 1 && (fill_ready.revents & POLLOUT),
                  "pending TCP acknowledgments reopen the sender while filling");
            if (ready != 1 || !(fill_ready.revents & POLLOUT))
                break;
            continue;
        }
        CHECK(0, "unexpected send result while filling the buffer");
        break;
    }
    printf("TCP buffer fill: %ld bytes, %u send calls\n", filled, attempts);
    CHECK(filled > 0, "non-blocking send enqueues bytes into the socket buffer");
    CHECK(hit_eagain, "non-blocking send reports EAGAIN once the buffer is full");

    /* A send against the now-full buffer returns EAGAIN and enqueues nothing. */
    ssize_t full = send(tx, "AAAA", 4, 0);
    CHECK(full < 0 && errno == EAGAIN, "send on a full non-blocking socket returns EAGAIN");

    /* Drain a bounded chunk large enough to acknowledge a complete segment
     * even with Linux's large loopback MSS. Partial ACKs of a large segment
     * need not release the sender's allocation or make POLLOUT ready. */
    char drain[64 * 1024];
    size_t drained = 0;
    struct pollfd progress[] = {
        {.fd = tx, .events = POLLOUT},
        {.fd = rx, .events = POLLIN},
    };
    while (drained < sizeof(drain))
    {
        int ready = poll(progress, 2, 2000);
        CHECK(ready > 0, "TCP window update or receiver data makes progress");
        if (ready <= 0 || (progress[0].revents & POLLOUT))
            break;
        if (!(progress[1].revents & POLLIN))
            break;
        ssize_t r = recv(rx, drain + drained, sizeof(drain) - drained, 0);
        CHECK(r > 0, "receive queued data while reopening the sender");
        if (r <= 0)
            break;
        drained += r;
    }
    CHECK(drained > 0, "receiver drains some bytes to reopen the window");

    /* Wait for the window update to make the sender writable again. */
    struct pollfd pfd = {.fd = tx, .events = POLLOUT, .revents = 0};
    int pr = poll(&pfd, 1, 2000);
    CHECK(pr == 1 && (pfd.revents & POLLOUT),
          "sender becomes writable after the receiver drains");

    /* Send one megabyte, far more than the bounded drain reopened: the fix
     * reports the partial count of queued bytes (> 0)
     * instead of EAGAIN-after-consuming-src, and the count is strictly less than
     * requested because the window is bounded. */
    static char big[1 << 20];
    memset(big, 'B', sizeof(big));
    ssize_t n = send(tx, big, sizeof(big), 0);
    CHECK(n > 0, "partial send on a non-blocking socket returns queued bytes, not EAGAIN");
    CHECK(n < (ssize_t)sizeof(big),
          "partial send reports fewer bytes than requested (window is bounded)");

    close(tx);
    close(rx);
    close(ln);
}

int main(void)
{
    TEST_START("/proc/comm format + non-blocking TCP partial send");

    test_comm_read_format();
    test_pr_set_name_bounded_user_read();
    test_pr_set_name_stops_at_nul();
    test_pthread_setname_roundtrip();
    test_nonblocking_partial_send();

    TEST_DONE();
}
