/*
 * !test-seqpacket — AF_UNIX SOCK_SEQPACKET 记录式 IPC 穷尽测试
 *
 * ground truth: man 7 unix / man 2 socketpair,socket,send,recv,sendmsg,recvmsg
 * + Linux v7.2 源码 net/unix/af_unix.c。逐条覆盖 socketpair 全 errno、SEQPACKET
 * 消息边界、MSG_TRUNC、recv/send flag 全家、errno 路径、SO_TYPE/SO_PEERCRED。
 *
 * =====================================================================
 * SOCK_SEQPACKET 语义 (unix(7))
 * =====================================================================
 *   面向连接、可靠、有序、**保留消息边界**的双向流。与 SOCK_STREAM 的判别点:
 *   1. 消息边界: 每次 recv 恰好一个 send 记录, 不合并 (dgram 语义)。
 *   2. 截断丢弃: recv 缓冲 < 记录长 -> msg_flags |= MSG_TRUNC, 余量丢弃, 下条 recv
 *      读下一记录; recv 带 MSG_TRUNC flag -> 返回真实记录长。
 *   3. SO_TYPE 回读应为 SOCK_SEQPACKET (非 SOCK_STREAM)。
 *   4. MSG_OOB 不支持 -> EOPNOTSUPP (send/recv 两侧)。
 *   5. 对端关闭: recv 返 0 (EOF); send 触发 EPIPE。
 *
 * =====================================================================
 * Linux v7.2 源码对齐 (net/unix/af_unix.c)
 * =====================================================================
 *   - unix_seqpacket_recvmsg(:2540) -> __unix_dgram_recvmsg(:2561): 一 skb 一记录。
 *   - 截断(:2619-2622) msg_flags|=MSG_TRUNC; 返回值(:2659) MSG_TRUNC flag 时为真实长。
 *   - send: MSG_OOB->EOPNOTSUPP(:2098); len>sndbuf-32->EMSGSIZE(:2122); peer 死->EPIPE(:2196)。
 *   - EOF: SEQPACKET+EAGAIN+RCV_SHUTDOWN->err=0(:2600)。全文无 MSG_EOR (不断言)。
 *
 *   浏览器关联: Firefox IPDL 进程间通道以 SOCK_SEQPACKET 承载, 依赖记录边界分帧。
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
#include <sys/mman.h>

static volatile sig_atomic_t g_sigpipe = 0;
static void sigpipe_handler(int s) { (void)s; g_sigpipe = 1; }

/* 兜底: 若某个 recv 因内核 gap 阻塞挂死, SIGALRM 干净退出而非耗尽整个 qemu run */
static void alarm_handler(int s)
{
    (void)s;
    const char *m = "\n  FAIL | TIMEOUT | 测试挂死(某阻塞调用未按预期返回, 疑内核gap)\n"
                    "==== test-seqpacket 汇总: FAIL ====\n";
    ssize_t r = write(2, m, strlen(m));
    (void)r;
    _exit(1);
}

static int sp_seqpacket(int sv[2])
{
    return socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv);
}

/* ===== A. 创建 / 参数 / 全 errno ===== */
static int test_create_and_params(void)
{
    TEST_START("A. socketpair SEQPACKET 创建/参数/errno");
    int sv[2] = { -1, -1 };
    CHECK_RET(sp_seqpacket(sv), 0, "socketpair(AF_UNIX,SOCK_SEQPACKET,0)");
    CHECK(sv[0] >= 0 && sv[1] >= 0 && sv[0] != sv[1], "两端 fd 有效且不同");
    if (sv[0] >= 0) { close(sv[0]); close(sv[1]); }

    int svc[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0, svc), 0, "SEQPACKET|CLOEXEC");
    int fl = fcntl(svc[0], F_GETFD);
    CHECK(fl != -1 && (fl & FD_CLOEXEC), "SOCK_CLOEXEC -> FD_CLOEXEC 置位");
    close(svc[0]); close(svc[1]);

    int svn[2];
    CHECK_RET(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK, 0, svn), 0, "SEQPACKET|NONBLOCK");
    int sfl = fcntl(svn[0], F_GETFL);
    CHECK(sfl != -1 && (sfl & O_NONBLOCK), "SOCK_NONBLOCK -> O_NONBLOCK 置位");
    close(svn[0]); close(svn[1]);

    int svd[2];
    CHECK_RET(sp_seqpacket(svd), 0, "默认无 CLOEXEC 前置");
    int dfl = fcntl(svd[0], F_GETFD);
    CHECK(dfl != -1 && !(dfl & FD_CLOEXEC), "默认不置 FD_CLOEXEC");
    close(svd[0]); close(svd[1]);

    /* errno: 错误 domain。AF_INET 不支持 socketpair -> EOPNOTSUPP/EAFNOSUPPORT */
    int sb[2];
    errno = 0;
    int r = socketpair(AF_INET, SOCK_SEQPACKET, 0, sb);
    CHECK(r == -1 && (errno == EOPNOTSUPP || errno == EAFNOSUPPORT || errno == ESOCKTNOSUPPORT || errno == EPROTONOSUPPORT),
          "错误 domain AF_INET -> 失败(EOPNOTSUPP/EAFNOSUPPORT类)");

    /* errno: EFAULT, sv 非法指针(volatile 隐藏地址骗过编译期静态分析) */
    {
        volatile uintptr_t badp = 0x1;
        errno = 0;
        r = socketpair(AF_UNIX, SOCK_SEQPACKET, 0, (int *)badp);
        CHECK(r == -1 && errno == EFAULT, "sv 非法指针 -> EFAULT");
    }
    TEST_DONE();
}

/* ===== B. 消息边界保留 ===== */
static int test_message_boundaries(void)
{
    TEST_START("B. SEQPACKET 消息边界保留");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置失败"); TEST_DONE(); }

    CHECK_RET(send(sv[0], "alpha", 5, 0), 5, "send 记录1(5B)");
    CHECK_RET(send(sv[0], "bravocharlie", 12, 0), 12, "send 记录2(12B)");
    CHECK_RET(send(sv[0], "d", 1, 0), 1, "send 记录3(1B)");

    char buf[256];
    CHECK_RET(recv(sv[1], buf, sizeof(buf), 0), 5, "recv 只读记录1(非合并18)");
    CHECK_RET(recv(sv[1], buf, sizeof(buf), 0), 12, "recv 只读记录2");
    CHECK_RET(recv(sv[1], buf, sizeof(buf), 0), 1, "recv 只读记录3");

    /* 零长记录: send 0 返回 0, recv 返回 0 但非 EOF(后续仍可收) */
    CHECK_RET(send(sv[0], "", 0, 0), 0, "send 零长记录");
    CHECK_RET(send(sv[0], "after", 5, 0), 5, "send 后续记录");
    ssize_t n0 = recv(sv[1], buf, sizeof(buf), 0);
    CHECK(n0 == 0, "recv 零长记录返回 0(非 EOF)");
    CHECK_RET(recv(sv[1], buf, sizeof(buf), 0), 5, "零长后仍能收后续记录(证非 EOF)");

    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

/* ===== C. 截断 + MSG_TRUNC ===== */
static int test_truncation(void)
{
    TEST_START("C. SEQPACKET 截断 + MSG_TRUNC");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置失败"); TEST_DONE(); }

    CHECK_RET(send(sv[0], "0123456789ABCDEF", 16, 0), 16, "send 大记录(16B)");
    CHECK_RET(send(sv[0], "next", 4, 0), 4, "send 下条记录(4B)");

    char small[8];
    struct iovec iov = { small, sizeof(small) };
    struct msghdr mh;
    memset(&mh, 0, sizeof(mh));
    mh.msg_iov = &iov; mh.msg_iovlen = 1;
    ssize_t n = recvmsg(sv[1], &mh, 0);
    CHECK(n == 8, "recvmsg 小缓冲返回 8(截断)");
    CHECK((mh.msg_flags & MSG_TRUNC) != 0, "msg_flags 置 MSG_TRUNC");

    char buf[64];
    CHECK_RET(recv(sv[1], buf, sizeof(buf), 0), 4, "下条 recv=下条记录(大记录余量丢弃)");
    CHECK(memcmp(buf, "next", 4) == 0, "下条内容=next");

    /* recv 带 MSG_TRUNC flag -> 返回真实记录长 */
    CHECK_RET(send(sv[0], "0123456789", 10, 0), 10, "send 10B 记录");
    char tb[4];
    struct iovec tiov = { tb, sizeof(tb) };
    struct msghdr tmh;
    memset(&tmh, 0, sizeof(tmh));
    tmh.msg_iov = &tiov; tmh.msg_iovlen = 1;
    ssize_t tn = recvmsg(sv[1], &tmh, MSG_TRUNC);
    CHECK(tn == 10, "recvmsg(MSG_TRUNC) 返回真实记录长 10(缓冲仅4)");

    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

/* ===== D. recv flags ===== */
static int test_recv_flags(void)
{
    TEST_START("D. SEQPACKET recv flags(PEEK/DONTWAIT/OOB)");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置失败"); TEST_DONE(); }

    /* MSG_PEEK: 不移除, 下次仍可读 */
    CHECK_RET(send(sv[0], "HELLO", 5, 0), 5, "send for PEEK");
    char pb[16];
    ssize_t pn = recv(sv[1], pb, sizeof(pb), MSG_PEEK);
    CHECK(pn == 5 && memcmp(pb, "HELLO", 5) == 0, "MSG_PEEK 读到 HELLO");
    /* PEEK 后数据应仍在队列; 用 DONTWAIT 防内核不 honor PEEK(消费了数据)时挂死 */
    ssize_t pn2 = recv(sv[1], pb, sizeof(pb), MSG_DONTWAIT);
    CHECK(pn2 == 5 && memcmp(pb, "HELLO", 5) == 0, "PEEK 后 recv 仍得 HELLO(未移除)");

    /* MSG_DONTWAIT 空队列 -> EAGAIN */
    errno = 0;
    ssize_t dn = recv(sv[1], pb, sizeof(pb), MSG_DONTWAIT);
    CHECK(dn == -1 && (errno == EAGAIN || errno == EWOULDBLOCK), "空队列 MSG_DONTWAIT -> EAGAIN");

    /* MSG_OOB 不支持 -> EOPNOTSUPP */
    CHECK_RET(send(sv[0], "x", 1, 0), 1, "send 普通记录");
    errno = 0;
    ssize_t on = recv(sv[1], pb, sizeof(pb), MSG_OOB);
    CHECK(on == -1 && errno == EOPNOTSUPP, "recv MSG_OOB -> EOPNOTSUPP");

    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

/* ===== E. send flags ===== */
static int test_send_flags(void)
{
    TEST_START("E. SEQPACKET send flags(OOB/NOSIGNAL/EMSGSIZE)");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置失败"); TEST_DONE(); }

    /* MSG_OOB 发送不支持 -> EOPNOTSUPP */
    errno = 0;
    ssize_t on = send(sv[0], "x", 1, MSG_OOB);
    CHECK(on == -1 && errno == EOPNOTSUPP, "send MSG_OOB -> EOPNOTSUPP");

    /* EMSGSIZE: 超过 sndbuf 的原子记录 */
    int sndbuf = 0;
    socklen_t bl = sizeof(sndbuf);
    getsockopt(sv[0], SOL_SOCKET, SO_SNDBUF, &sndbuf, &bl);
    if (sndbuf > 0) {
        size_t big = (size_t)sndbuf + 65536;
        char *bigbuf = malloc(big);
        if (bigbuf) {
            memset(bigbuf, 'Z', big);
            errno = 0;
            ssize_t bn = send(sv[0], bigbuf, big, MSG_DONTWAIT);
            CHECK(bn == -1 && errno == EMSGSIZE, "send 超 sndbuf 记录 -> EMSGSIZE");
            free(bigbuf);
        }
    }

    /* 对端关闭 + MSG_NOSIGNAL -> EPIPE 无信号 */
    close(sv[1]);
    g_sigpipe = 0;
    errno = 0;
    ssize_t en = send(sv[0], "y", 1, MSG_NOSIGNAL);
    CHECK(en == -1 && (errno == EPIPE || errno == ECONNRESET), "对端关闭 send(NOSIGNAL) -> EPIPE");
    close(sv[0]);
    TEST_DONE();
}

/* ===== F. errno 路径 + EOF ===== */
static int test_errno_paths(void)
{
    TEST_START("F. SEQPACKET errno(EBADF/ENOTSOCK/EOF)");
    char buf[16];

    errno = 0;
    CHECK(recv(999, buf, sizeof(buf), 0) == -1 && errno == EBADF, "recv 无效 fd -> EBADF");
    errno = 0;
    CHECK(send(999, "x", 1, 0) == -1 && errno == EBADF, "send 无效 fd -> EBADF");

    int rfd = open("/", O_RDONLY);
    if (rfd >= 0) {
        errno = 0;
        CHECK(recv(rfd, buf, sizeof(buf), 0) == -1 && errno == ENOTSOCK, "recv 非 socket fd -> ENOTSOCK");
        close(rfd);
    }

    /* EOF: 对端关闭后 recv 返回 0, 且再次 recv 仍 0 */
    int sv[2];
    if (sp_seqpacket(sv) == 0) {
        close(sv[1]);
        CHECK_RET(recv(sv[0], buf, sizeof(buf), 0), 0, "对端关闭 recv -> 0(EOF)");
        CHECK_RET(recv(sv[0], buf, sizeof(buf), 0), 0, "EOF 后再 recv 仍 0");
        close(sv[0]);
    }
    TEST_DONE();
}

/* ===== G. sockopt 回读: SO_TYPE / SO_PEERCRED / getsockname ===== */
static int test_sockopt(void)
{
    TEST_START("G. SEQPACKET sockopt(SO_TYPE/PEERCRED/getsockname)");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置失败"); TEST_DONE(); }

    int type = 0;
    socklen_t tl = sizeof(type);
    CHECK_RET(getsockopt(sv[0], SOL_SOCKET, SO_TYPE, &type, &tl), 0, "getsockopt SO_TYPE");
    CHECK(type == SOCK_SEQPACKET, "SO_TYPE 回读为 SOCK_SEQPACKET(非 SOCK_STREAM)");

    struct ucred cr;
    socklen_t cl = sizeof(cr);
    if (getsockopt(sv[0], SOL_SOCKET, SO_PEERCRED, &cr, &cl) == 0) {
        CHECK(cr.pid == getpid(), "SO_PEERCRED.pid == 本进程(socketpair 对端同进程)");
    } else {
        CHECK(0, "getsockopt SO_PEERCRED 应可用");
    }

    struct sockaddr_un sa;
    socklen_t sl = sizeof(sa);
    memset(&sa, 0, sizeof(sa));
    if (getsockname(sv[0], (struct sockaddr *)&sa, &sl) == 0) {
        CHECK(sa.sun_family == AF_UNIX, "getsockname family=AF_UNIX(unnamed)");
    } else {
        CHECK(0, "getsockname 应可用");
    }

    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

/* ===== H. 连接式 bind/listen/accept/connect ===== */
static int test_connected(void)
{
    TEST_START("H. SEQPACKET 连接式 bind/listen/accept/connect");
    int ls = socket(AF_UNIX, SOCK_SEQPACKET, 0);
    CHECK(ls >= 0, "socket(AF_UNIX,SOCK_SEQPACKET)");
    if (ls < 0) { TEST_DONE(); }

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    const char *name = "\0starry-seqpacket-conn";
    memcpy(addr.sun_path, name, 22);
    socklen_t alen = offsetof(struct sockaddr_un, sun_path) + 22;

    CHECK_RET(bind(ls, (struct sockaddr *)&addr, alen), 0, "bind 抽象地址");
    CHECK_RET(listen(ls, 4), 0, "listen");

    int cs = socket(AF_UNIX, SOCK_SEQPACKET, 0);
    CHECK_RET(connect(cs, (struct sockaddr *)&addr, alen), 0, "connect");
    int as = accept(ls, NULL, NULL);
    CHECK(as >= 0, "accept");

    if (cs >= 0 && as >= 0) {
        CHECK_RET(send(cs, "ping", 4, 0), 4, "client send");
        char b[16];
        CHECK_RET(recv(as, b, sizeof(b), 0), 4, "server recv 完整记录");
        close(cs); cs = -1;
        CHECK_RET(recv(as, b, sizeof(b), 0), 0, "对端关闭 recv 0(EOF)");
    }
    if (cs >= 0) close(cs);
    if (as >= 0) close(as);
    close(ls);
    TEST_DONE();
}

/* ===== I. SCM_RIGHTS 文件描述符跨端传递(SEQPACKET) + MSG_CMSG_CLOEXEC ===== */
static int test_scm_rights(void)
{
    TEST_START("I. SEQPACKET SCM_RIGHTS fd 传递 + MSG_CMSG_CLOEXEC");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置 socketpair 失败"); TEST_DONE(); }

    int tmp = memfd_create("scm-seqpacket", MFD_CLOEXEC);
    if (tmp < 0) tmp = open("/tmp", O_TMPFILE | O_RDWR, 0600);
    CHECK(tmp >= 0, "创建待传递 fd(memfd/tmpfile)");
    if (tmp >= 0) {
        CHECK_RET(write(tmp, "SCMPAYLOAD", 10), 10, "写 10B 到待传 fd");
    }

    char cbuf[CMSG_SPACE(sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    char pay = 'F';
    struct iovec siov = { &pay, 1 };
    struct msghdr smh;
    memset(&smh, 0, sizeof(smh));
    smh.msg_iov = &siov; smh.msg_iovlen = 1;
    smh.msg_control = cbuf; smh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *scm = CMSG_FIRSTHDR(&smh);
    scm->cmsg_level = SOL_SOCKET;
    scm->cmsg_type = SCM_RIGHTS;
    scm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(scm), &tmp, sizeof(int));
    CHECK_RET(sendmsg(sv[0], &smh, 0), 1, "sendmsg 携 SCM_RIGHTS 单 fd(payload 1B)");

    char rcbuf[CMSG_SPACE(sizeof(int))];
    memset(rcbuf, 0, sizeof(rcbuf));
    char rpay = 0;
    struct iovec riov = { &rpay, 1 };
    struct msghdr rmh;
    memset(&rmh, 0, sizeof(rmh));
    rmh.msg_iov = &riov; rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf; rmh.msg_controllen = sizeof(rcbuf);
    ssize_t rn = recvmsg(sv[1], &rmh, MSG_CMSG_CLOEXEC);
    CHECK(rn == 1 && rpay == 'F', "recvmsg 收到 payload(1B, 'F')");
    CHECK((rmh.msg_flags & MSG_CTRUNC) == 0, "控制缓冲充足 -> 无 MSG_CTRUNC");

    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    CHECK(rc != NULL && rc->cmsg_level == SOL_SOCKET && rc->cmsg_type == SCM_RIGHTS,
          "收到 SOL_SOCKET/SCM_RIGHTS 控制消息");
    CHECK(rc != NULL && rc->cmsg_len == CMSG_LEN(sizeof(int)),
          "cmsg_len == CMSG_LEN(1 fd)");
    int gotfd = -1;
    if (rc) memcpy(&gotfd, CMSG_DATA(rc), sizeof(int));
    CHECK(gotfd >= 0 && gotfd != tmp, "收到新的独立 fd(与发端号不同)");

    if (gotfd >= 0) {
        int fdfl = fcntl(gotfd, F_GETFD);
        CHECK(fdfl != -1 && (fdfl & FD_CLOEXEC),
              "MSG_CMSG_CLOEXEC -> 收到 fd 置 FD_CLOEXEC");

        char vb[16];
        memset(vb, 0, sizeof(vb));
        ssize_t vn = pread(gotfd, vb, 10, 0);
        CHECK(vn == 10 && memcmp(vb, "SCMPAYLOAD", 10) == 0,
              "收到 fd 可用且读回原内容(同一 open file description)");

        off_t pos = lseek(gotfd, 3, SEEK_SET);
        CHECK(pos == 3, "收端 lseek(gotfd, 3)");
        off_t spos = lseek(tmp, 0, SEEK_CUR);
        CHECK(spos == 3, "发端 offset 随动(共享 offset)");
        close(gotfd);
    }

    if (tmp >= 0) close(tmp);
    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

/* ===== J. SCM_RIGHTS 控制缓冲过小 -> 多余 fd 丢弃 + MSG_CTRUNC ===== */
static int test_scm_rights_trunc(void)
{
    TEST_START("J. SEQPACKET SCM_RIGHTS 超缓冲截断(多余 fd 丢弃 + MSG_CTRUNC)");
    int sv[2];
    if (sp_seqpacket(sv) != 0) { CHECK(0, "前置 socketpair 失败"); TEST_DONE(); }

    int fds[3];
    for (int i = 0; i < 3; i++) fds[i] = dup(STDOUT_FILENO);
    CHECK(fds[0] >= 0 && fds[1] >= 0 && fds[2] >= 0, "dup 出 3 个待传 fd");

    char cbuf[CMSG_SPACE(3 * sizeof(int))];
    memset(cbuf, 0, sizeof(cbuf));
    char pay = 'M';
    struct iovec siov = { &pay, 1 };
    struct msghdr smh;
    memset(&smh, 0, sizeof(smh));
    smh.msg_iov = &siov; smh.msg_iovlen = 1;
    smh.msg_control = cbuf; smh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *scm = CMSG_FIRSTHDR(&smh);
    scm->cmsg_level = SOL_SOCKET;
    scm->cmsg_type = SCM_RIGHTS;
    scm->cmsg_len = CMSG_LEN(3 * sizeof(int));
    memcpy(CMSG_DATA(scm), fds, 3 * sizeof(int));
    CHECK_RET(sendmsg(sv[0], &smh, 0), 1, "sendmsg 携 3 个 SCM_RIGHTS fd");

    char rcbuf[CMSG_SPACE(1 * sizeof(int))];
    memset(rcbuf, 0, sizeof(rcbuf));
    char rpay = 0;
    struct iovec riov = { &rpay, 1 };
    struct msghdr rmh;
    memset(&rmh, 0, sizeof(rmh));
    rmh.msg_iov = &riov; rmh.msg_iovlen = 1;
    rmh.msg_control = rcbuf; rmh.msg_controllen = sizeof(rcbuf);
    ssize_t rn = recvmsg(sv[1], &rmh, 0);
    CHECK(rn == 1, "recvmsg 收到 payload 1B");
    CHECK((rmh.msg_flags & MSG_CTRUNC) != 0, "fd 超控制缓冲 -> MSG_CTRUNC 置位");

    struct cmsghdr *rc = CMSG_FIRSTHDR(&rmh);
    CHECK(rc != NULL && rc->cmsg_type == SCM_RIGHTS, "仍交付一个 SCM_RIGHTS cmsg");
    int nfds = 0;
    if (rc) nfds = (int)((rc->cmsg_len - CMSG_LEN(0)) / sizeof(int));
    CHECK(nfds >= 1 && nfds < 3, "交付的 fd 数被截断(1..<3, 少于发送的 3)");
    for (int i = 0; i < nfds; i++) {
        int gf;
        memcpy(&gf, CMSG_DATA(rc) + i * sizeof(int), sizeof(int));
        CHECK(gf >= 0 && fcntl(gf, F_GETFD) != -1, "交付的 fd 有效");
        if (gf >= 0) close(gf);
    }

    for (int i = 0; i < 3; i++) if (fds[i] >= 0) close(fds[i]);
    close(sv[0]); close(sv[1]);
    TEST_DONE();
}

int main(void)
{
    signal(SIGPIPE, sigpipe_handler);
    signal(SIGALRM, alarm_handler);
    alarm(60);
    int fail = 0;
    fail |= test_create_and_params();
    fail |= test_message_boundaries();
    fail |= test_truncation();
    fail |= test_recv_flags();
    fail |= test_send_flags();
    fail |= test_errno_paths();
    fail |= test_sockopt();
    fail |= test_connected();
    fail |= test_scm_rights();
    fail |= test_scm_rights_trunc();
    printf("\n==== test-seqpacket 汇总: %s ====\n", fail ? "FAIL" : "PASS");
    return fail;
}
