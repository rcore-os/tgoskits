/*
 * !test-cmsg-cloexec — AF_UNIX SCM_RIGHTS fd 传递 + MSG_CMSG_CLOEXEC 穷尽测试
 *
 * ground truth: man 3 cmsg / man 2 recvmsg,sendmsg / man 7 unix
 * + Linux v7.2 源码 net/core/scm.c。逐条覆盖 SCM_RIGHTS fd 传递、
 * MSG_CMSG_CLOEXEC 的 O_CLOEXEC 设置、SCM_MAX_FD 上限、MSG_CTRUNC 控制截断、
 * 多 fd、errno 路径。
 *
 * =====================================================================
 * 语义 (man 3 cmsg, man 2 recvmsg, man 7 unix)
 * =====================================================================
 *   SCM_RIGHTS: 通过 AF_UNIX socket 的 cmsg 传递打开文件描述符; 收端得到指向
 *   同一 open file description 的新 fd (共享 offset/flags)。
 *   MSG_CMSG_CLOEXEC (recvmsg): 收到的 fd 设 close-on-exec (FD_CLOEXEC); 不带
 *   则不设。
 *   SCM_MAX_FD = 253: 单次 sendmsg SCM_RIGHTS 超过 253 个 fd -> EINVAL。
 *   MSG_CTRUNC (recvmsg 输出): 控制缓冲不足以容纳全部 fd 时置位, 超出的 fd 被
 *   内核关闭 (不泄漏)。
 *
 * =====================================================================
 * Linux v7.2 源码对齐 (net/core/scm.c)
 * =====================================================================
 *   - scm_detach_fds(:354): o_flags = (msg_flags & MSG_CMSG_CLOEXEC) ? O_CLOEXEC : 0
 *     (:358); scm_recv_one_fd 用 o_flags 装 fd (:373)。
 *   - scm_fp_copy(:70): num > SCM_MAX_FD -> EINVAL (:84); fd<0 或 fget 失败 -> EBADF (:113)。
 *   - 控制缓冲不足: fdmax 计算后超出的 fd 关闭 + msg_flags |= MSG_CTRUNC (:396-397)。
 *
 *   浏览器关联: Chromium Mojo / Firefox IPDL 经 SCM_RIGHTS 传共享内存与管道 fd,
 *   收端用 MSG_CMSG_CLOEXEC 避免 fd 泄漏到子进程。
 * =====================================================================
 */

#include "test_framework.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <fcntl.h>
#include <signal.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>

static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死(疑内核gap)\n"
                    "==== test-cmsg-cloexec 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

/* 发送 n 个 fd 经 SCM_RIGHTS, 附带 1 字节数据。返回 sendmsg 结果。 */
static ssize_t send_fds(int sock, const int *fds, int n)
{
    char iobuf[1] = { 'x' };
    struct iovec iov = { iobuf, 1 };
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;

    char cbuf[CMSG_SPACE(sizeof(int) * 253)];
    memset(cbuf, 0, sizeof(cbuf));
    msg.msg_control = cbuf;
    msg.msg_controllen = CMSG_SPACE(sizeof(int) * (size_t)n);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int) * (size_t)n);
    memcpy(CMSG_DATA(cm), fds, sizeof(int) * (size_t)n);
    return sendmsg(sock, &msg, 0);
}

/* 接收 fd: 返回收到的 fd 数; out_fds 填入; msg_flags 回传于 *out_flags。
 * ctrl_fds = 控制缓冲按几个 fd 预留(可小于发送数以测 CTRUNC)。 */
static int recv_fds(int sock, int flags, int *out_fds, int ctrl_fds, int *out_flags)
{
    char iobuf[1];
    struct iovec iov = { iobuf, 1 };
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    char cbuf[CMSG_SPACE(sizeof(int) * 253)];
    memset(cbuf, 0, sizeof(cbuf));
    msg.msg_control = cbuf;
    /* CMSG_SPACE-sized control buffer, matching how the kernel accounts for
     * fd delivery. To force truncation, senders pass more fds than fit. */
    msg.msg_controllen = (ctrl_fds > 0) ? CMSG_SPACE(sizeof(int) * (size_t)ctrl_fds) : 0;

    ssize_t r = recvmsg(sock, &msg, flags);
    if (out_flags) *out_flags = msg.msg_flags;
    if (r < 0) return -1;

    int got = 0;
    /* musl's CMSG_NXTHDR macro trips clang -Wsign-compare (unsigned vs signed
     * length arithmetic inside the macro); suppress just around the walk. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wsign-compare"
    for (struct cmsghdr *cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm)) {
        if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
            int cnt = (int)((cm->cmsg_len - CMSG_LEN(0)) / sizeof(int));
            for (int i = 0; i < cnt; i++) {
                int fd;
                memcpy(&fd, CMSG_DATA(cm) + i * sizeof(int), sizeof(int));
                out_fds[got++] = fd;
            }
        }
    }
#pragma GCC diagnostic pop
    return got;
}

/* 建一个可读的 fd: pipe 写端写入 marker, 返回读端(传给对端读回验证) */
static int make_readable_fd(const char *marker, int len)
{
    int pfd[2];
    if (pipe(pfd) != 0) return -1;
    ssize_t w = write(pfd[1], marker, (size_t)len);
    (void)w;
    close(pfd[1]);
    return pfd[0];
}

/* ===== G. MSG_CMSG_CLOEXEC 跨 socket 类型(DGRAM/SEQPACKET) ===== */
static void cloexec_on_type(int type, const char *tname)
{
    int sv[2];
    if (socketpair(AF_UNIX, type, 0, sv) != 0) {
        CHECK(0, "socketpair(该类型)");
        return;
    }
    int f = make_readable_fd("Z", 1);
    if (f < 0) { CHECK(0, "make_readable_fd"); close(sv[0]); close(sv[1]); return; }

    CHECK(send_fds(sv[0], &f, 1) == 1, tname);
    int got[4];
    int n = recv_fds(sv[1], MSG_CMSG_CLOEXEC, got, 1, NULL);
    CHECK(n == 1, tname);
    if (n == 1) {
        int fl = fcntl(got[0], F_GETFD);
        CHECK(fl != -1 && (fl & FD_CLOEXEC), tname);
        char buf[4] = { 0 };
        CHECK(read(got[0], buf, 1) == 1 && buf[0] == 'Z', tname);
        close(got[0]);
    }

    CHECK(send_fds(sv[0], &f, 1) == 1, tname);
    n = recv_fds(sv[1], 0, got, 1, NULL);
    CHECK(n == 1, tname);
    if (n == 1) {
        int fl = fcntl(got[0], F_GETFD);
        CHECK(fl != -1 && !(fl & FD_CLOEXEC), tname);
        close(got[0]);
    }
    close(f);
    close(sv[0]);
    close(sv[1]);
}

static int test_cloexec_across_types(void)
{
    TEST_START("G. MSG_CMSG_CLOEXEC on DGRAM/SEQPACKET");
    cloexec_on_type(SOCK_DGRAM,
                    "DGRAM: SCM_RIGHTS + MSG_CMSG_CLOEXEC 语义");
    cloexec_on_type(SOCK_SEQPACKET,
                    "SEQPACKET: SCM_RIGHTS + MSG_CMSG_CLOEXEC 语义");
    TEST_DONE();
}

/* 建一个可 lseek 的临时文件 fd, 写入 content, offset 归零。 */
static int make_seekable_fd(const char *content, int len)
{
    char path[] = "/tmp/cmsg_ofd_XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) return -1;
    unlink(path);
    if (write(fd, content, (size_t)len) != len) { close(fd); return -1; }
    lseek(fd, 0, SEEK_SET);
    return fd;
}

/* ===== H. 收端 fd 与发端共享 open file description(offset + status flags) ===== */
static int test_shared_ofd(void)
{
    TEST_START("H. SCM_RIGHTS 收端与发端共享同一 OFD");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    /* --- H1: 共享 file offset --- */
    int sfd = make_seekable_fd("ABCDEFGH", 8);
    CHECK(sfd >= 0, "建可 lseek 临时文件 fd");
    if (sfd >= 0) {
        CHECK(send_fds(sv[0], &sfd, 1) == 1, "send seekable fd");
        int got[4];
        int n = recv_fds(sv[1], 0, got, 1, NULL);
        CHECK(n == 1, "收到共享 OFD 的 fd");
        if (n == 1) {
            off_t p = lseek(got[0], 3, SEEK_SET);
            CHECK(p == 3, "收端 lseek(recv_fd, 3) 成功");
            char c = 0;
            ssize_t rd = read(sfd, &c, 1);
            CHECK(rd == 1 && c == 'D', "发端 read 从收端设置的 offset 3 起(共享 offset -> 'D')");
            char c2 = 0;
            ssize_t rd2 = read(got[0], &c2, 1);
            CHECK(rd2 == 1 && c2 == 'E', "收端 read 接续发端推进的 offset(共享 offset -> 'E')");
            close(got[0]);
        }
        close(sfd);
    }

    /* --- H2: 共享 file status flags(F_SETFL 一端可见于另一端) --- */
    int sfd2 = make_seekable_fd("xyz", 3);
    if (sfd2 >= 0) {
        CHECK(send_fds(sv[0], &sfd2, 1) == 1, "send fd (for status-flag share)");
        int got[4];
        int n = recv_fds(sv[1], 0, got, 1, NULL);
        CHECK(n == 1, "收到 fd (for status-flag share)");
        if (n == 1) {
            int fl0 = fcntl(sfd2, F_GETFL);
            CHECK(fl0 != -1 && fcntl(sfd2, F_SETFL, fl0 | O_APPEND) == 0,
                  "发端 F_SETFL O_APPEND");
            int fl_recv = fcntl(got[0], F_GETFL);
            CHECK(fl_recv != -1 && (fl_recv & O_APPEND),
                  "收端 F_GETFL 见 O_APPEND(共享 status flags)");
            CHECK(fcntl(got[0], F_SETFL, fl_recv & ~O_APPEND) == 0,
                  "收端 F_SETFL 清 O_APPEND");
            int fl_send = fcntl(sfd2, F_GETFL);
            CHECK(fl_send != -1 && !(fl_send & O_APPEND),
                  "发端 F_GETFL 见 O_APPEND 已清(双向共享 status flags)");
            close(got[0]);
        }
        close(sfd2);
    }
    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== A. 基础 SCM_RIGHTS fd 传递 ===== */
static int test_basic_fd_passing(void)
{
    TEST_START("A. SCM_RIGHTS 基础 fd 传递");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    int rfd = make_readable_fd("HELLO", 5);
    CHECK(rfd >= 0, "建可读 pipe fd");
    if (rfd >= 0) {
        int one = rfd;
        CHECK(send_fds(sv[0], &one, 1) == 1, "sendmsg SCM_RIGHTS 单 fd");
        int got[4];
        int flags = 0;
        int n = recv_fds(sv[1], 0, got, 1, &flags);
        CHECK(n == 1, "recvmsg 收到 1 个 fd");
        if (n == 1) {
            CHECK(got[0] >= 0 && got[0] != rfd, "收到的 fd 有效且是新 fd(非同号)");
            char buf[8] = { 0 };
            ssize_t rd = read(got[0], buf, sizeof(buf));
            CHECK(rd == 5 && memcmp(buf, "HELLO", 5) == 0, "收到的 fd 真连通(读回原内容)");
            close(got[0]);
        }
        close(rfd);
    }
    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== B. MSG_CMSG_CLOEXEC(核心) ===== */
static int test_cmsg_cloexec(void)
{
    TEST_START("B. MSG_CMSG_CLOEXEC 设置 FD_CLOEXEC");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    /* 带 MSG_CMSG_CLOEXEC -> 收到的 fd 应有 FD_CLOEXEC */
    int f1 = make_readable_fd("A", 1);
    if (f1 >= 0) {
        CHECK(send_fds(sv[0], &f1, 1) == 1, "send fd (for cloexec)");
        int got[4];
        int n = recv_fds(sv[1], MSG_CMSG_CLOEXEC, got, 1, NULL);
        CHECK(n == 1, "recvmsg(MSG_CMSG_CLOEXEC) 收到 fd");
        if (n == 1) {
            int fl = fcntl(got[0], F_GETFD);
            CHECK(fl != -1 && (fl & FD_CLOEXEC), "MSG_CMSG_CLOEXEC -> 收到 fd 有 FD_CLOEXEC");
            close(got[0]);
        }
        close(f1);
    }

    /* 不带 -> 无 FD_CLOEXEC */
    int f2 = make_readable_fd("B", 1);
    if (f2 >= 0) {
        CHECK(send_fds(sv[0], &f2, 1) == 1, "send fd (no cloexec)");
        int got[4];
        int n = recv_fds(sv[1], 0, got, 1, NULL);
        CHECK(n == 1, "recvmsg(flags=0) 收到 fd");
        if (n == 1) {
            int fl = fcntl(got[0], F_GETFD);
            CHECK(fl != -1 && !(fl & FD_CLOEXEC), "无 MSG_CMSG_CLOEXEC -> 收到 fd 无 FD_CLOEXEC");
            close(got[0]);
        }
        close(f2);
    }
    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== C. 多 fd + SCM_MAX_FD 上限 ===== */
static int test_multiple_and_limit(void)
{
    TEST_START("C. 多 fd + SCM_MAX_FD(253) 上限");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    /* 一个 SCM_RIGHTS 携 3 个 fd */
    int fds[3];
    int ok = 1;
    for (int i = 0; i < 3; i++) { fds[i] = make_readable_fd("m", 1); if (fds[i] < 0) ok = 0; }
    if (ok) {
        CHECK(send_fds(sv[0], fds, 3) == 1, "sendmsg 3 fd in one SCM_RIGHTS");
        int got[8];
        int n = recv_fds(sv[1], 0, got, 3, NULL);
        CHECK(n == 3, "recvmsg 收到 3 个 fd");
        for (int i = 0; i < n; i++) close(got[i]);
    }
    for (int i = 0; i < 3; i++) if (fds[i] >= 0) close(fds[i]);

    /* 超过 SCM_MAX_FD(253): 发 254 个 -> EINVAL。用同一个 fd 重复填充。 */
    int dup = make_readable_fd("x", 1);
    if (dup >= 0) {
        int big[254];
        for (int i = 0; i < 254; i++) big[i] = dup;
        errno = 0;
        ssize_t r = send_fds(sv[0], big, 254);
        CHECK(r == -1 && errno == EINVAL, "sendmsg 254 fd(>SCM_MAX_FD) -> EINVAL");
        close(dup);
    }
    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== D. MSG_CTRUNC 控制截断 ===== */
static int test_ctrunc(void)
{
    TEST_START("D. MSG_CTRUNC 控制缓冲不足");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    int fds[5];
    int ok = 1;
    for (int i = 0; i < 5; i++) { fds[i] = make_readable_fd("c", 1); if (fds[i] < 0) ok = 0; }
    if (ok) {
        CHECK(send_fds(sv[0], fds, 5) == 1, "send 5 fd");
        int got[8];
        int flags = 0;
        /* 控制缓冲只够 1 个 fd 的空间(远少于 5)-> 部分交付 + MSG_CTRUNC,
         * 超出的 fd 内核关闭。收到数不依赖精确对齐, 只需 truncated。 */
        int n = recv_fds(sv[1], 0, got, 1, &flags);
        CHECK(n >= 1 && n < 5, "小控制缓冲截断: 收到部分 fd(<5)");
        CHECK((flags & MSG_CTRUNC) != 0, "msg_flags 置 MSG_CTRUNC");
        for (int i = 0; i < n; i++) close(got[i]);
    }
    for (int i = 0; i < 5; i++) if (fds[i] >= 0) close(fds[i]);
    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== E. errno 路径 ===== */
static int test_errno(void)
{
    TEST_START("E. SCM_RIGHTS errno(EBADF/负fd/ENOTSOCK)");
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { CHECK(0, "socketpair"); TEST_DONE(); }

    /* 无效 fd(999) -> EBADF */
    int bad = 999;
    errno = 0;
    CHECK(send_fds(sv[0], &bad, 1) == -1 && errno == EBADF, "SCM_RIGHTS 无效 fd -> EBADF");

    /* 负 fd(-1) -> EBADF */
    int neg = -1;
    errno = 0;
    CHECK(send_fds(sv[0], &neg, 1) == -1 && errno == EBADF, "SCM_RIGHTS 负 fd -> EBADF");

    /* recvmsg 于非 socket fd -> ENOTSOCK */
    int rfd = open("/", O_RDONLY);
    if (rfd >= 0) {
        int got[2];
        errno = 0;
        int n = recv_fds(rfd, 0, got, 1, NULL);
        CHECK(n == -1 && errno == ENOTSOCK, "recvmsg 非 socket -> ENOTSOCK");
        close(rfd);
    }

    /* 空 socket recvmsg(MSG_DONTWAIT) -> EAGAIN */
    int got[2];
    errno = 0;
    int n = recv_fds(sv[1], MSG_DONTWAIT, got, 1, NULL);
    CHECK(n == -1 && (errno == EAGAIN || errno == EWOULDBLOCK), "空 socket MSG_DONTWAIT -> EAGAIN");

    close(sv[0]);
    close(sv[1]);
    TEST_DONE();
}

/* ===== F. MSG_PEEK 保留 fd ===== */
/* MSG_PEEK 交付 SCM_RIGHTS 的复制 fd 且不消费记录: Linux net/unix/af_unix.c
 * unix_peek_fds() -> scm_fp_dup() 在 peek 时复制 fd 列表交付, skb 保留原 fd, 后续
 * 真 recv 再次交付。peek/消费/原 fd 共享同一 OFD, 用 lseek(0)+read 验证可用而不因
 * 共享 offset 互相干扰。覆盖 STREAM/DGRAM/SEQPACKET 三种连接式/数据报路径。 */
static void peek_delivers_rights(int type, const char *label)
{
    int sv[2];
    if (socketpair(AF_UNIX, type, 0, sv) != 0) { CHECK(0, label); return; }
    int sfd = make_seekable_fd("PEEKOK", 6);
    if (sfd < 0) { CHECK(0, label); close(sv[0]); close(sv[1]); return; }

    CHECK(send_fds(sv[0], &sfd, 1) == 1, label);
    close(sfd);

    /* peek 必须交付一个可用的复制 fd(此前 gap: peek 交付 0 个 fd) */
    int pfd[4];
    int np = recv_fds(sv[1], MSG_PEEK, pfd, 1, NULL);
    CHECK(np == 1, label);
    if (np == 1) {
        char b = 0;
        CHECK(lseek(pfd[0], 0, SEEK_SET) == 0 && read(pfd[0], &b, 1) == 1 && b == 'P', label);
        close(pfd[0]);
    }

    /* peek 未消费记录: 后续真 recv 再次交付可用 fd */
    int rfd[4];
    int nr = recv_fds(sv[1], MSG_DONTWAIT, rfd, 1, NULL);
    CHECK(nr == 1, label);
    if (nr == 1) {
        char b = 0;
        CHECK(lseek(rfd[0], 0, SEEK_SET) == 0 && read(rfd[0], &b, 1) == 1 && b == 'P', label);
        close(rfd[0]);
    }
    close(sv[0]);
    close(sv[1]);
}

static int test_peek_keeps_fd(void)
{
    TEST_START("F. MSG_PEEK 交付 SCM_RIGHTS 复制 fd + 后续真 recv 再交付");
    peek_delivers_rights(SOCK_STREAM, "STREAM: peek 与消费各得可用 fd");
    peek_delivers_rights(SOCK_DGRAM, "DGRAM: peek 与消费各得可用 fd");
    peek_delivers_rights(SOCK_SEQPACKET, "SEQPACKET: peek 与消费各得可用 fd");
    TEST_DONE();
}

int main(void)
{
    signal(SIGPIPE, SIG_IGN);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_basic_fd_passing();
    fail |= test_cmsg_cloexec();
    fail |= test_multiple_and_limit();
    fail |= test_ctrunc();
    fail |= test_errno();
    fail |= test_peek_keeps_fd();
    fail |= test_cloexec_across_types();
    fail |= test_shared_ofd();
    printf("\n==== test-cmsg-cloexec 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
