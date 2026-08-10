/*
 * test-eventfd2 — 验证 eventfd2 系统调用的完整语义。
 *
 * 覆盖场景：
 *   1. 基本创建：各种 flag 组合，包括非法 flags → EINVAL
 *   2. initval：零值和非零值初始计数器
 *   3. 普通模式读写：write 累加，read 返回并清零
 *   4. 信号量模式：read 每次返回 1 并递减
 *   5. 多次写入累积
 *   6. 写 UINT64_MAX → EINVAL
 *   7. 读写缓冲区大小校验（eventfd 写入必须恰好为 8 字节）
 *   8. 非阻塞模式：空 eventfd 读 → EAGAIN，满 eventfd 写 → EAGAIN
 *   9. 写 0 边界情况
 *  10. 计数器溢出保护：写会使计数超过 UINT64_MAX-1 → EAGAIN
 *  11. 阻塞读：子进程写入后父进程阻塞读被唤醒
 *  12. fork 继承：子进程可以读写父进程创建的 eventfd
 *  13. poll 返回精确的 Linux readiness mask，并清空负 fd 的 revents
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* 标准 eventfd 读写辅助函数 */
static int do_write(int fd, uint64_t val) {
    return (int)write(fd, &val, sizeof(val));
}

static int do_read(int fd, uint64_t *val) {
    return (int)read(fd, val, sizeof(*val));
}

static long raw_ppoll(struct pollfd *fds, nfds_t nfds) {
    const struct timespec timeout = {
        .tv_sec = 0,
        .tv_nsec = 0,
    };

    return syscall(SYS_ppoll, fds, nfds, &timeout, NULL, 0);
}

/* ─── 1. 基本创建与 flags ─────────────────────────────────── */

static void test_create_with_flags(void) {
    int fd;

    /* 合法 flags */
    fd = eventfd(0, 0);
    CHECK(fd >= 0, "eventfd(0, 0) succeeds");
    if (fd >= 0) close(fd);

    fd = eventfd(0, EFD_CLOEXEC);
    CHECK(fd >= 0, "eventfd(0, EFD_CLOEXEC) succeeds");
    if (fd >= 0) close(fd);

    fd = eventfd(0, EFD_NONBLOCK);
    CHECK(fd >= 0, "eventfd(0, EFD_NONBLOCK) succeeds");
    if (fd >= 0) close(fd);

    fd = eventfd(0, EFD_SEMAPHORE);
    CHECK(fd >= 0, "eventfd(0, EFD_SEMAPHORE) succeeds");
    if (fd >= 0) close(fd);

    fd = eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK);
    CHECK(fd >= 0, "eventfd(0, EFD_SEMAPHORE|EFD_NONBLOCK) succeeds");
    if (fd >= 0) close(fd);

    fd = eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK | EFD_CLOEXEC);
    CHECK(fd >= 0, "eventfd(0, EFD_SEMAPHORE|EFD_NONBLOCK|EFD_CLOEXEC) succeeds");
    if (fd >= 0) close(fd);

    /* 非法 flags */
    CHECK_ERR(eventfd(0, 0x80000000U), EINVAL, "eventfd(0, unknown flag) returns EINVAL");
}

/* ─── 2. initval ──────────────────────────────────────────── */

static void test_initval(void) {
    uint64_t val;

    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "eventfd(0, EFD_NONBLOCK) initval=0");
        CHECK_ERR(do_read(fd, &val), EAGAIN, "initval=0 nonblocking read returns EAGAIN");
        close(fd);
    }

    {
        int fd = eventfd(42, 0);
        CHECK(fd >= 0, "eventfd(42, 0) initval=42");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read returns 8 bytes");
        CHECK(val == 42, "read returns initval 42");
        close(fd);
    }

    {
        int fd = eventfd(UINT32_MAX, 0);
        CHECK(fd >= 0, "eventfd(UINT32_MAX, 0) initval=UINT32_MAX");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read max initval");
        CHECK(val == UINT32_MAX, "read returns UINT32_MAX");
        close(fd);
    }
}

/* ─── 3. 普通模式读写 ──────────────────────────────────────── */

static void test_normal_read_write(void) {
    uint64_t val;

    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "normal mode fd nonblocking");
        CHECK_RET(do_write(fd, 5), (ssize_t)sizeof(val), "write 5");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read after write");
        CHECK(val == 5, "read returns 5");
        /* 读后计数器清零，再次读应返回 EAGAIN */
        CHECK_ERR(do_read(fd, &val), EAGAIN, "second read on empty fd returns EAGAIN");
        close(fd);
    }
}

/* ─── 4. 多次写入累积 ─────────────────────────────────────── */

static void test_multiple_writes_accumulate(void) {
    uint64_t val;

    {
        int fd = eventfd(0, 0);
        CHECK(fd >= 0, "fd for accumulation test");
        CHECK_RET(do_write(fd, 3), (ssize_t)sizeof(val), "write 3");
        CHECK_RET(do_write(fd, 4), (ssize_t)sizeof(val), "write 4");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read accumulated");
        CHECK(val == 7, "read returns 3+4=7");
        close(fd);
    }

    /* 大量写入累积 */
    {
        int fd = eventfd(0, 0);
        CHECK(fd >= 0, "fd for large accumulation");
        for (int i = 0; i < 10; i++) {
            CHECK_RET(do_write(fd, 100), (ssize_t)sizeof(val), "write 100");
        }
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read after 10 writes");
        CHECK(val == 1000, "read returns 10*100=1000");
        close(fd);
    }
}

/* ─── 5. 信号量模式 ───────────────────────────────────────── */

static void test_semaphore_mode(void) {
    uint64_t val;

    {
        int fd = eventfd(0, EFD_SEMAPHORE | EFD_NONBLOCK);
        CHECK(fd >= 0, "semaphore+nonblocking mode fd");
        CHECK_RET(do_write(fd, 3), (ssize_t)sizeof(val), "write 3");
        /* 每次 read 返回 1 并递减 */
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "first semaphore read");
        CHECK(val == 1, "semaphore read returns 1");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "second semaphore read");
        CHECK(val == 1, "semaphore read returns 1 again");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "third semaphore read");
        CHECK(val == 1, "semaphore read returns 1 again");
        /* 计数器已耗尽 */
        CHECK_ERR(do_read(fd, &val), EAGAIN, "fourth semaphore read on empty returns EAGAIN");
        close(fd);
    }
}

/* ─── 6. 写 UINT64_MAX ────────────────────────────────────── */

static void test_write_uint64_max(void) {
    uint64_t val = UINT64_MAX;

    {
        int fd = eventfd(0, 0);
        CHECK(fd >= 0, "fd for UINT64_MAX write test");
        errno = 0;
        ssize_t ret = write(fd, &val, sizeof(val));
        CHECK(ret == -1 && errno == EINVAL, "write UINT64_MAX returns EINVAL");
        close(fd);
    }
}

/* ─── 7. 读写缓冲区大小校验 ────────────────────────────────── */

static void test_buffer_size_validation(void) {
    uint64_t val64;
    uint32_t val32;

    {
        int fd = eventfd(1, 0);
        CHECK(fd >= 0, "fd for buffer size test");

        /* read 缓冲区 < 8 → EINVAL */
        errno = 0;
        ssize_t ret = read(fd, &val32, sizeof(val32));
        CHECK(ret == -1 && errno == EINVAL, "read with 4-byte buffer returns EINVAL");

        /* write 缓冲区 < 8 → EINVAL */
        errno = 0;
        ret = write(fd, &val32, sizeof(val32));
        CHECK(ret == -1 && errno == EINVAL, "write with 4-byte buffer returns EINVAL");

        /* read 缓冲区 == 8 → 成功 */
        CHECK_RET(do_read(fd, &val64), (ssize_t)sizeof(val64), "read with 8-byte buffer succeeds");
        CHECK(val64 == 1, "read with 8-byte buffer returns initval 1");

        /* eventfd checks count before touching the user pointer. */
        errno = 0;
        ret = syscall(SYS_write, fd, (const void *)(uintptr_t)1, 7);
        CHECK(ret == -1 && errno == EINVAL,
              "short raw write with invalid pointer returns EINVAL before EFAULT");

        errno = 0;
        ret = syscall(SYS_write, fd, (const void *)(uintptr_t)1, 9);
        CHECK(ret == -1 && errno == EINVAL,
              "oversized raw write with invalid pointer returns EINVAL before EFAULT");

        close(fd);
    }

    {
        int fd = eventfd(0, EFD_NONBLOCK);
        uint8_t oversized[sizeof(uint64_t) + 1];
        uint64_t written = 17;
        uint64_t observed = 0;

        CHECK(fd >= 0, "fd for oversized raw write test");
        memcpy(oversized, &written, sizeof(written));
        oversized[sizeof(written)] = 0xa5;

        errno = 0;
        long ret = syscall(SYS_write, fd, oversized, sizeof(oversized));
        CHECK(ret == -1 && errno == EINVAL,
              "raw write with 9-byte buffer returns EINVAL");
        CHECK_ERR(do_read(fd, &observed), EAGAIN,
                  "rejected oversized raw write leaves the counter unchanged");

        struct iovec oversized_iov = {
            .iov_base = oversized,
            .iov_len = sizeof(oversized),
        };
        errno = 0;
        ret = syscall(SYS_writev, fd, &oversized_iov, 1);
        CHECK(ret == -1 && errno == EINVAL,
              "raw writev with one 9-byte segment returns EINVAL");

        struct iovec invalid_oversized_iov = {
            .iov_base = (void *)(uintptr_t)1,
            .iov_len = sizeof(oversized),
        };
        errno = 0;
        ret = syscall(SYS_writev, fd, &invalid_oversized_iov, 1);
        CHECK(ret == -1 && errno == EINVAL,
              "oversized raw writev validates total length before iov_base");

        close(fd);
    }
}

/* ─── 8. 非阻塞模式 ───────────────────────────────────────── */

static void test_nonblocking(void) {
    uint64_t val;

    /* 空 eventfd 非阻塞读 → EAGAIN */
    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "nonblocking fd for empty read");
        CHECK_ERR(do_read(fd, &val), EAGAIN, "nonblocking read from empty returns EAGAIN");
        close(fd);
    }

    /* 满 eventfd 非阻塞写 → EAGAIN */
    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "nonblocking fd for full write test");
        /* 先写满到 UINT64_MAX-1 */
        CHECK_RET(do_write(fd, UINT64_MAX - 1), (ssize_t)sizeof(uint64_t),
                  "fill counter to max");
        /* 再写 1 会超过上限 */
        CHECK_ERR(do_write(fd, 1), EAGAIN, "nonblocking write to full returns EAGAIN");
        close(fd);
    }

    /* 接近满时写超大值 → EAGAIN */
    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "nonblocking fd for near-full write test");
        CHECK_RET(do_write(fd, UINT64_MAX - 2), (ssize_t)sizeof(uint64_t),
                  "fill to UINT64_MAX-2");
        /* 写 2 会超过上限 */
        CHECK_ERR(do_write(fd, 2), EAGAIN, "write 2 to near-full returns EAGAIN");
        close(fd);
    }
}

/* ─── 9. 写 0 边界情况 ─────────────────────────────────────── */

static void test_write_zero(void) {
    uint64_t val;

    /* 写 0 到非空 eventfd：无操作但成功 */
    {
        int fd = eventfd(5, 0);
        CHECK(fd >= 0, "fd initval=5 for write zero test");
        CHECK_RET(do_write(fd, 0), (ssize_t)sizeof(val), "write 0 succeeds");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read after write 0");
        CHECK(val == 5, "counter unchanged after write 0");
        close(fd);
    }

    /* 写 0 到空 eventfd：仍然成功，计数器保持 0 */
    {
        int fd = eventfd(0, EFD_NONBLOCK);
        CHECK(fd >= 0, "nonblocking fd for write zero on empty");
        CHECK_RET(do_write(fd, 0), (ssize_t)sizeof(val), "write 0 to empty succeeds");
        CHECK_ERR(do_read(fd, &val), EAGAIN, "read after write 0 to empty returns EAGAIN");
        close(fd);
    }
}

/* ─── 10. 计数器溢出保护 ───────────────────────────────────── */

static void test_overflow_protection(void) {
    uint64_t val;

    /* 写 UINT64_MAX-1 到空 eventfd → 计数达到最大值 */
    {
        int fd = eventfd(0, 0);
        CHECK(fd >= 0, "fd for near-max write");
        CHECK_RET(do_write(fd, UINT64_MAX - 1), (ssize_t)sizeof(val),
                  "write UINT64_MAX-1 succeeds");
        CHECK_RET(do_read(fd, &val), (ssize_t)sizeof(val), "read returns max");
        CHECK(val == UINT64_MAX - 1, "read returns UINT64_MAX-1");
        close(fd);
    }

    /* 从接近满写小值 → 成功（未溢出） */
    {
        int fd = eventfd(0, 0);
        CHECK(fd >= 0, "fd for near-overflow test");
        CHECK_RET(do_write(fd, UINT64_MAX - 100), (ssize_t)sizeof(val),
                  "write UINT64_MAX-100");
        CHECK_RET(do_write(fd, 50), (ssize_t)sizeof(val), "write 50 (total now UINT64_MAX-50)");
        close(fd);
    }
}

/* ─── 11. 阻塞读写与 fork 继承 ─────────────────────────────── */

static void test_blocking_read(void) {
    int fd = eventfd(0, 0);
    CHECK(fd >= 0, "fd for blocking read test");

    pid_t pid = fork();
    if (pid == -1) {
        printf("  FAIL | %s:%d | fork failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        close(fd);
        return;
    }

    if (pid == 0) {
        /* 子进程：短暂等待后写入 */
        usleep(50000); /* 50ms */
        uint64_t val = 42;
        if (write(fd, &val, sizeof(val)) != sizeof(val)) {
            _exit(1);
        }
        close(fd);
        _exit(0);
    }

    /* 父进程：阻塞读 */
    uint64_t result = 0;
    ssize_t n = read(fd, &result, sizeof(result));
    CHECK_RET(n, (ssize_t)sizeof(result), "blocking read wakes up after child write");
    CHECK(result == 42, "blocking read returns child's value 42");

    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child exited cleanly");

    close(fd);
}

static void test_fork_inheritance(void) {
    int fd = eventfd(7, EFD_NONBLOCK);
    CHECK(fd >= 0, "fd=7 nonblocking for fork inheritance test");

    pid_t pid = fork();
    if (pid == -1) {
        printf("  FAIL | %s:%d | fork failed | errno=%d (%s)\n",
               __FILE__, __LINE__, errno, strerror(errno));
        __fail++;
        close(fd);
        return;
    }

    if (pid == 0) {
        /* 子进程：读取父进程创建的 eventfd */
        uint64_t val;
        if (read(fd, &val, sizeof(val)) != sizeof(val)) {
            _exit(1);
        }
        if (val != 7) {
            _exit(2);
        }
        close(fd);
        _exit(0);
    }

    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child reads inherited eventfd and gets initval=7");

    /* 父进程 fd 应该已被子进程的读操作消耗 */
    uint64_t val;
    CHECK_ERR(do_read(fd, &val), EAGAIN, "parent fd is now empty after child read");
    close(fd);
}

/* ─── 13. poll readiness mask ───────────────────────────── */

static void test_poll_readiness_mask(void) {
    int fd = eventfd(1, EFD_NONBLOCK);
    struct pollfd fds[] = {
        {.fd = fd, .events = INT16_MAX, .revents = (short)0x5a5a},
        {.fd = -2, .events = INT16_MAX, .revents = (short)0x5a5a},
        {.fd = -2, .events = POLLIN | POLLOUT, .revents = (short)0x5a5a},
        {.fd = -2, .events = POLLERR, .revents = (short)0x5a5a},
    };

    CHECK(fd >= 0, "fd for poll readiness mask test");
    CHECK_RET(raw_ppoll(fds, sizeof(fds) / sizeof(fds[0])), 1,
              "poll reports exactly one ready eventfd");
    CHECK(fds[0].revents == (POLLIN | POLLOUT),
          "eventfd poll reports only Linux POLLIN|POLLOUT readiness");
    CHECK(fds[1].revents == 0 && fds[2].revents == 0 && fds[3].revents == 0,
          "poll ignores negative fds and clears their revents");
    close(fd);
}

/* ─── main ────────────────────────────────────────────────── */

int main(void) {
    TEST_START("eventfd2");

    printf("--- 1. create & flags ---\n");
    test_create_with_flags();

    printf("\n--- 2. initval ---\n");
    test_initval();

    printf("\n--- 3. normal read/write ---\n");
    test_normal_read_write();

    printf("\n--- 4. multiple writes accumulate ---\n");
    test_multiple_writes_accumulate();

    printf("\n--- 5. semaphore mode ---\n");
    test_semaphore_mode();

    printf("\n--- 6. write UINT64_MAX ---\n");
    test_write_uint64_max();

    printf("\n--- 7. buffer size validation ---\n");
    test_buffer_size_validation();

    printf("\n--- 8. nonblocking ---\n");
    test_nonblocking();

    printf("\n--- 9. write zero ---\n");
    test_write_zero();

    printf("\n--- 10. overflow protection ---\n");
    test_overflow_protection();

    printf("\n--- 11. blocking read ---\n");
    test_blocking_read();

    printf("\n--- 12. fork inheritance ---\n");
    test_fork_inheritance();

    printf("\n--- 13. poll readiness mask ---\n");
    test_poll_readiness_mask();

    TEST_DONE();
}
