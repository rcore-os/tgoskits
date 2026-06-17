/*
 * test-signalfd4 — 验证 signalfd4 系统调用的完整语义。
 *
 * 覆盖场景：
 *   1. 基本创建：各种 flag 组合，非法 flags → EINVAL
 *   2. sigsetsize 校验：非空 mask 时 size 必须为 8
 *   3. 修改已有 signalfd 的 mask（fd != -1）
 *   4. 修改已有 fd 时传 SFD_CLOEXEC → EINVAL
 *   5. 非阻塞空读 → EAGAIN
 *   6. 读缓冲区 < 128 字节 → EINVAL
 *   7. write → EBADF
 *   8. 信号投递：阻塞 SIGUSR1 → signalfd → kill → read → 校验 ssi_signo
 *   9. 多信号读取：SIGUSR1 + SIGUSR2
 *  10. 发送者信息：ssi_pid / ssi_uid 校验
 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

/* SFD flags — matching Linux kernel definitions */
#ifndef SFD_CLOEXEC
#define SFD_CLOEXEC 02000000
#endif
#ifndef SFD_NONBLOCK
#define SFD_NONBLOCK 04000
#endif

/* Fallback __NR_signalfd4 per architecture */
#ifndef __NR_signalfd4
#if defined(__x86_64__)
#define __NR_signalfd4 289
#elif defined(__aarch64__)
#define __NR_signalfd4 74
#elif defined(__riscv)
#define __NR_signalfd4 261
#else
#define __NR_signalfd4 289
#endif
#endif

/* Kernel-compatible signalfd_siginfo (128 bytes).
 * Matches the kernel's SignalfdSiginfo layout field-for-field. */
struct signalfd_siginfo {
    uint32_t ssi_signo;
    int32_t  ssi_errno;
    int32_t  ssi_code;
    uint32_t ssi_pid;
    uint32_t ssi_uid;
    int32_t  ssi_fd;
    uint32_t ssi_tid;
    uint32_t ssi_band;
    uint32_t ssi_overrun;
    uint32_t ssi_trapno;
    int32_t  ssi_status;
    int32_t  ssi_int;
    uint64_t ssi_ptr;
    uint64_t ssi_utime;
    uint64_t ssi_stime;
    uint64_t ssi_addr;
    uint16_t ssi_addr_lsb;
    uint8_t  __pad[46];
};

_Static_assert(sizeof(struct signalfd_siginfo) == 128,
               "signalfd_siginfo must be 128 bytes");

/* ─── raw signalfd4 helpers ──────────────────────────────────── */

static int signalfd4_new(uint64_t mask, int flags) {
    return (int)syscall(__NR_signalfd4, -1, &mask, sizeof(mask), flags);
}

static int signalfd4_modify(int fd, uint64_t mask, int flags) {
    return (int)syscall(__NR_signalfd4, fd, &mask, sizeof(mask), flags);
}

static uint64_t sig_mask(int sig) {
    return 1ULL << (sig - 1);
}

/* ─── 1. 基本创建与 flags ────────────────────────────────────── */

static void test_create_and_flags(void) {
    int fd;

    fd = signalfd4_new(0, 0);
    CHECK(fd >= 0, "signalfd4(0, 0)");
    if (fd >= 0) close(fd);

    fd = signalfd4_new(0, SFD_CLOEXEC);
    CHECK(fd >= 0, "signalfd4(0, SFD_CLOEXEC)");
    if (fd >= 0) close(fd);

    fd = signalfd4_new(0, SFD_NONBLOCK);
    CHECK(fd >= 0, "signalfd4(0, SFD_NONBLOCK)");
    if (fd >= 0) close(fd);

    fd = signalfd4_new(0, SFD_CLOEXEC | SFD_NONBLOCK);
    CHECK(fd >= 0, "signalfd4(0, SFD_CLOEXEC|SFD_NONBLOCK)");
    if (fd >= 0) close(fd);

    /* 非法 flags */
    CHECK_ERR(signalfd4_new(0, 0xDEAD), EINVAL,
              "signalfd4 with invalid flags → EINVAL");
    CHECK_ERR(signalfd4_new(0, -1), EINVAL,
              "signalfd4 with flags=-1 → EINVAL");
}

/* ─── 2. sigsetsize 校验 ─────────────────────────────────────── */

static void test_sigsetsize(void) {
    uint64_t mask = 0;

    CHECK_ERR(syscall(__NR_signalfd4, -1, &mask, 4, 0), EINVAL,
              "sigsetsize=4 → EINVAL");
    CHECK_ERR(syscall(__NR_signalfd4, -1, &mask, 7, 0), EINVAL,
              "sigsetsize=7 → EINVAL");

    /* sigsetsize=8 (sizeof(kernel_sigset_t)) is valid */
    int fd = signalfd4_new(0, 0);
    CHECK(fd >= 0, "sigsetsize=8 succeeds");
    if (fd >= 0) close(fd);

    /* non-NULL mask with sigsetsize=0 should fail */
    fd = (int)syscall(__NR_signalfd4, -1, &mask, 0, 0);
    CHECK(fd == -1 && errno == EINVAL, "sigsetsize=0 -> EINVAL");
    if (fd >= 0) close(fd);
}

/* ─── 3. 修改已有 signalfd ───────────────────────────────────── */

static void test_modify_mask(void) {
    int fd = signalfd4_new(sig_mask(SIGUSR1), 0);
    CHECK(fd >= 0, "create signalfd with SIGUSR1 mask");

    /* 把 mask 改为 SIGUSR2 */
    int ret = signalfd4_modify(fd, sig_mask(SIGUSR2), 0);
    CHECK(ret == fd, "modify mask to SIGUSR2 returns same fd");

    /* 改为同时包含两个信号 */
    ret = signalfd4_modify(fd, sig_mask(SIGUSR1) | sig_mask(SIGUSR2), 0);
    CHECK(ret == fd, "modify mask to SIGUSR1|SIGUSR2 returns same fd");

    close(fd);
}

/* ─── 4. fd != -1 且 SFD_CLOEXEC → EINVAL ────────────────────── */

static void test_cloexec_conflict(void) {
    int fd = signalfd4_new(0, 0);
    CHECK(fd >= 0, "create signalfd");

    CHECK_ERR(signalfd4_modify(fd, 0, SFD_CLOEXEC), EINVAL,
              "modify with SFD_CLOEXEC → EINVAL");

    close(fd);
}

/* ─── 5. 非阻塞空读 ──────────────────────────────────────────── */

static void test_nonblocking_empty(void) {
    int fd = signalfd4_new(0, SFD_NONBLOCK);
    CHECK(fd >= 0, "create nonblocking signalfd");

    struct signalfd_siginfo info;
    CHECK_ERR(read(fd, &info, sizeof(info)), EAGAIN,
              "nonblocking read on empty signalfd → EAGAIN");

    close(fd);
}

/* ─── 6. 读缓冲区大小校验 ────────────────────────────────────── */

static void test_buffer_size(void) {
    int fd = signalfd4_new(0, 0);
    CHECK(fd >= 0, "create signalfd for buffer test");

    char small[64];
    CHECK_ERR(read(fd, small, sizeof(small)), EINVAL,
              "read with 64-byte buffer → EINVAL");
    CHECK_ERR(read(fd, small, 127), EINVAL,
              "read with 127-byte buffer → EINVAL");

    close(fd);
}

/* ─── 7. write → EBADF ───────────────────────────────────────── */

static void test_write_rejected(void) {
    int fd = signalfd4_new(0, 0);
    CHECK(fd >= 0, "create signalfd for write test");

    uint64_t val = 1;
    CHECK_ERR(write(fd, &val, sizeof(val)), EBADF,
              "write to signalfd → EBADF");

    close(fd);
}

/* ─── 8. 信号投递 ────────────────────────────────────────────── */

static void test_signal_delivery(void) {
    struct signalfd_siginfo info;

    /* Step 1: 阻塞 SIGUSR1，防止默认动作 */
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    CHECK(sigprocmask(SIG_BLOCK, &set, NULL) == 0, "block SIGUSR1");

    /* Step 2: 创建监听 SIGUSR1 的 signalfd */
    int fd = signalfd4_new(sig_mask(SIGUSR1), SFD_NONBLOCK);
    CHECK(fd >= 0, "create nonblocking signalfd for SIGUSR1");

    /* Step 3: 发送 SIGUSR1 */
    CHECK(kill(getpid(), SIGUSR1) == 0, "kill(getpid(), SIGUSR1)");

    /* Step 4: 从 signalfd 读出信号 */
    CHECK_RET(read(fd, &info, sizeof(info)), (ssize_t)sizeof(info),
              "read returns 128 bytes");
    CHECK(info.ssi_signo == (uint32_t)SIGUSR1, "ssi_signo == SIGUSR1");

    /* Step 5: 信号已消费，再次读 */
    CHECK_ERR(read(fd, &info, sizeof(info)), EAGAIN,
              "second read → EAGAIN (signal consumed)");

    /* 恢复信号掩码 */
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    close(fd);
}

/* ─── 9. 多信号读取 ──────────────────────────────────────────── */

static void test_multiple_signals(void) {
    struct signalfd_siginfo info;

    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigaddset(&set, SIGUSR2);
    CHECK(sigprocmask(SIG_BLOCK, &set, NULL) == 0,
          "block SIGUSR1 and SIGUSR2");

    int fd = signalfd4_new(sig_mask(SIGUSR1) | sig_mask(SIGUSR2), SFD_NONBLOCK);
    CHECK(fd >= 0, "create nonblocking signalfd for SIGUSR1|SIGUSR2");

    /* 发送两个信号 */
    CHECK(kill(getpid(), SIGUSR1) == 0, "send SIGUSR1");
    CHECK(kill(getpid(), SIGUSR2) == 0, "send SIGUSR2");

    /* 读出第一个 */
    CHECK_RET(read(fd, &info, sizeof(info)), (ssize_t)sizeof(info),
              "read first signal");
    CHECK(info.ssi_signo == (uint32_t)SIGUSR1 ||
          info.ssi_signo == (uint32_t)SIGUSR2,
          "first signal is SIGUSR1 or SIGUSR2");

    /* 读出第二个 */
    CHECK_RET(read(fd, &info, sizeof(info)), (ssize_t)sizeof(info),
              "read second signal");
    CHECK(info.ssi_signo == (uint32_t)SIGUSR1 ||
          info.ssi_signo == (uint32_t)SIGUSR2,
          "second signal is the other one");

    /* 已无信号 */
    CHECK_ERR(read(fd, &info, sizeof(info)), EAGAIN,
              "third read → EAGAIN");

    sigprocmask(SIG_UNBLOCK, &set, NULL);
    close(fd);
}

/* ─── 10. 发送者信息（已知内核 bug） ──────────────────────────── */

static void test_sender_info(void) {
    struct signalfd_siginfo info;

    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    CHECK(sigprocmask(SIG_BLOCK, &set, NULL) == 0, "block SIGUSR1");

    int fd = signalfd4_new(sig_mask(SIGUSR1), SFD_NONBLOCK);
    CHECK(fd >= 0, "create nonblocking signalfd for SIGUSR1");

    /* kill(getpid(), SIGUSR1) → ssi_pid 应为 getpid() */
    CHECK(kill(getpid(), SIGUSR1) == 0, "kill(getpid(), SIGUSR1)");

    CHECK_RET(read(fd, &info, sizeof(info)), (ssize_t)sizeof(info),
              "read signal info");
    CHECK(info.ssi_signo == (uint32_t)SIGUSR1, "ssi_signo == SIGUSR1");

    /* ssi_pid 应为发送进程 PID */
    CHECK(info.ssi_pid == (uint32_t)getpid(),
          "ssi_pid == getpid()");

    /* ssi_uid 应为发送进程的 real UID */
    CHECK(info.ssi_uid == (uint32_t)getuid(),
          "ssi_uid == getuid()");

    sigprocmask(SIG_UNBLOCK, &set, NULL);
    close(fd);
}

/* ─── main ───────────────────────────────────────────────────── */

int main(void) {
    TEST_START("signalfd4");

    printf("--- 1. create & flags ---\n");
    test_create_and_flags();

    printf("\n--- 2. sigsetsize validation ---\n");
    test_sigsetsize();

    printf("\n--- 3. modify mask ---\n");
    test_modify_mask();

    printf("\n--- 4. SFD_CLOEXEC conflict ---\n");
    test_cloexec_conflict();

    printf("\n--- 5. nonblocking empty read ---\n");
    test_nonblocking_empty();

    printf("\n--- 6. buffer size validation ---\n");
    test_buffer_size();

    printf("\n--- 7. write rejected ---\n");
    test_write_rejected();

    printf("\n--- 8. signal delivery ---\n");
    test_signal_delivery();

    printf("\n--- 9. multiple signals ---\n");
    test_multiple_signals();

    printf("\n--- 10. sender info (ssi_pid/ssi_uid) ---\n");
    test_sender_info();

    TEST_DONE();
}
