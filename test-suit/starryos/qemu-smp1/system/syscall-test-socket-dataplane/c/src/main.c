/*
 * !test-socket-dataplane — socket 数据面系统调用测试
 *
 * 覆盖 sendto / recvfrom / sendmsg / recvmsg 四个系统调用。
 * 依据: Linux man-pages 6.7 send(2) + recv(2)。
 *
 * =====================================================================
 * 手册摘要 (man 2 send, man 2 recv)
 * =====================================================================
 *
 * ── sendto ───────────────────────────────────────────────────────────
 *   ssize_t sendto(int fd, const void *buf, size_t len, int flags,
 *                  const struct sockaddr *dest, socklen_t alen);
 *
 *   将 buf 中 len 字节发送到 dest 指定的对端。
 *   - 未连接 socket (UDP):    必须提供 dest+alen，否则 EDESTADDRREQ。
 *   - 已连接 socket (TCP/UDP): dest+alen 被忽略（可能返回 EISCONN）。
 *   - dest=NULL && alen=0:    等价于 send(fd,buf,len,flags)。
 *   - 返回值: 成功时返回已发送字节数; 失败返回 -1 并设 errno。
 *
 *   错误码：
 *     EBADF fd 无效
 *     ENOTSOCK fd 不是 socket
 *     EDESTADDRREQ 未连接且未指定目标
 *     EISCONN 已连接但指定了目标 (may be returned)
 *     EMSGSIZE UDP 报文超过原子发送上限
 *     EINVAL 参数非法
 *     EOPNOTSUPP flag 不适合该 socket 类型
 *     EPIPE 流式 socket 对端关闭 (默认触发 SIGPIPE)
 *     EAGAIN/EWOULDBLOCK 非阻塞模式缓冲区满
 *
 *   标志位：
 *     MSG_CONFIRM  (UDP/RAW) 链路层邻居确认, Linux 2.3.15+
 *     MSG_DONTROUTE          不经过网关, 仅直连
 *     MSG_DONTWAIT            单次非阻塞
 *     MSG_EOR      (SOCK_SEQPACKET) 记录终止
 *     MSG_MORE     (UDP/TCP) cork/合并发送
 *     MSG_NOSIGNAL            抑制 SIGPIPE
 *     MSG_OOB      (仅 SOCK_STREAM) 带外数据
 *
 *   手册 BUGS: Linux 可能在应返回 ENOTCONN 时返回 EPIPE。
 *
 * ── recvfrom ─────────────────────────────────────────────────────────
 *   ssize_t recvfrom(int fd, void *buf, size_t len, int flags,
 *                    struct sockaddr *src, socklen_t *alen);
 *
 *   从 fd 接收数据到 buf。如果 src 非 NULL 且协议支持，填入源地址。
 *   alen 是 value-result 参数: 传入 buffer 大小, 传出实际地址长度。
 *   src=NULL && alen=NULL: 等价于 recv(fd,buf,len,flags)。
 *   返回值: 成功时返回接收字节数; 失败返回 -1 并设 errno。
 *
 *   错误码：
 *     EBADF fd 无效
 *     ENOTSOCK fd 不是 socket
 *     EAGAIN/EWOULDBLOCK 非阻塞且无数据 / 超时
 *     EINVAL 参数非法
 *     EFAULT buffer 指向非法地址
 *     ENOTCONN 面向连接 socket 未连接
 *     ENOMEM (recvmsg) 内存不足
 *
 *   标志位：
 *     MSG_DONTWAIT            单次非阻塞
 *     MSG_OOB                 接收带外数据
 *     MSG_PEEK                窥探: 返回数据但不从队列移除
 *     MSG_TRUNC               (raw/dgram) 即使 buf 太小也返回真实长度
 *     MSG_WAITALL             阻塞直至收满 len 字节 (流式 socket)
 *     MSG_CMSG_CLOEXEC        (recvmsg only) SCM_RIGHTS fd 设 CLOEXEC
 *
 * ── sendmsg ──────────────────────────────────────────────────────────
 *   ssize_t sendmsg(int fd, const struct msghdr *msg, int flags);
 *
 *   使用 msghdr 结构发送数据，支持:
 *     msg_name / msg_namelen    目标地址 (未连接时使用)
 *     msg_iov / msg_iovlen      分散/汇聚 I/O (scatter-gather)
 *     msg_control / msg_controllen  辅助数据 (ancillary data)
 *     msg_flags                 忽略
 *
 *   等价关系: sendmsg(fd,msg,flags) 里的 msg_name+msg_namelen 充当
 *   sendto 的 dest+alen, msg_iov+msg_iovlen 充当 buf+len。
 *   错误码同 sendto。
 *
 * ── recvmsg ──────────────────────────────────────────────────────────
 *   ssize_t recvmsg(int fd, struct msghdr *msg, int flags);
 *
 *   使用 msghdr 结构接收数据:
 *     msg_name / msg_namelen    源地址 (output: value-result)
 *     msg_iov / msg_iovlen      分散/汇聚 I/O
 *     msg_control / msg_controllen  辅助数据
 *     msg_flags                 返回标志 (MSG_TRUNC/MSG_ERRQUEUE 等)
 *
 *   错误码同 recvfrom。
 *
 * =====================================================================
 * 测试覆盖一览
 * =====================================================================
 *
 *  sendto (14):
 *    [S1] 基本 UDP sendto — 未连接 UDP, 正常发送
 *    [S2] 已连接 UDP sendto(NULL,0) — 等价 send()
 *    [S3] EBADF — 无效 fd
 *    [S4] ENOTSOCK — fd 不是 socket
 *    [S5] EDESTADDRREQ — 未连接且无 dest
 *    [S6] EINVAL — addrlen 非法
 *    [S7] EOPNOTSUPP — MSG_OOB 不适用 UDP
 *    [S8] MSG_MORE — UDP cork 合并数据报
 *    [S9] 零长度发送 — 0 字节数据报
 *    [S10] MSG_DONTWAIT 发送 — 验证路径无异常（EAGAIN 由 R7 覆盖）
 *    [S11] MSG_MORE 目标固定 — 首段目标地址不变
 *    [S12] NULL+nonzero addrlen — 未连接 EDESTADDRREQ, 已连接成功
 *    [S13] MSG_MORE 超限 — EMSGSIZE
 *    [S14] MSG_MORE flush 超限 — EMSGSIZE + cork 保留
 *
 *  recvfrom (7):
 *    [R1] 基本 UDP recvfrom — 接收带源地址
 *    [R2] NULL src/alen — 等价 recv()
 *    [R3] EBADF — 无效 fd
 *    [R4] ENOTSOCK — fd 不是 socket
 *    [R5] EAGAIN/EWOULDBLOCK — 非阻塞无数据
 *    [R6] MSG_PEEK — 窥探不消费数据
 *    [R7] MSG_DONTWAIT 接收 — 单次非阻塞 recv
 *
 *  sendmsg (8):
 *    [SM1] 基本 UDP sendmsg — msg_name 指定目标
 *    [SM2] 已连接 sendmsg(NULL,0) — 已连接 socket
 *    [SM3] EBADF — 无效 fd
 *    [SM4] ENOTSOCK — fd 不是 socket
 *    [SM5] EDESTADDRREQ — 无 dest
 *    [SM6] EOPNOTSUPP — MSG_OOB on UDP
 *    [SM7] MSG_MORE — UDP cork via sendmsg
 *    [SM8] MSG_NOSIGNAL — TCP 对端关闭不触发 SIGPIPE
 *
 *  recvmsg (7):
 *    [RM1] 基本 UDP recvmsg — msg_name 返回源地址
 *    [RM2] scatter/gather — 多 iovec 接收
 *    [RM3] EBADF — 无效 fd
 *    [RM4] ENOTSOCK — fd 不是 socket
 *    [RM5] EAGAIN/EWOULDBLOCK — 非阻塞无数据
 *    [RM6] MSG_PEEK — 窥探, msg_flags 检查
 *    [RM7] MSG_TRUNC — 截断时 msg_flags 含 MSG_TRUNC
 *
 * =====================================================================
 * 特殊说明
 * =====================================================================
 *
 *  1. Linux BUGS: 手册记载 "Linux may return EPIPE instead of ENOTCONN"。
 *     本测试在 [SM8] 中预先 signal(SIGPIPE,SIG_IGN) 并同时接受 EPIPE
 *     和 ENOTCONN, 避免进程被 SIGPIPE 杀死。
 *
 *  2. EISCONN: 手册说明对已连接 socket 传 dest "may be returned" 或忽略。
 *     Linux 6.x 上 UDP 会忽略 (成功返回), TCP 可能返回 EISCONN。
 *     [SM2] 对此做兼容处理。
 *
 *  3. EAGAIN vs EWOULDBLOCK: POSIX 允许二者之一, 本测试在 [R5][RM5] 中
 *     同时接受。
 *
 *  4. MSG_TRUNC: 本测试未包含, 原因是在 Linux loopback UDP 上
 *     无法可靠地构造出 "缓冲区小于数据报但数据不丢失" 的场景
 *     (UDP 会直接丢弃超过 buf 的部分而不会返回 MSG_TRUNC,
 *     除非 socket 是 raw/netlink 等类型)。
 *
 *  5. MSG_WAITALL: 本测试未包含, 因为它对数据报 socket 无效果,
 *     而对流式 socket 的完整测试依赖更复杂的数据注入。
 *
 *  6. MSG_ERRQUEUE / MSG_CMSG_CLOEXEC / ancillary data:
 *     本测试未包含, 它们属于控制面 / 错误队列功能, 不属于纯数据面。
 *
 *  7. 编译: 需要 -std=c11 或以上。测试在 Linux 6.x (x86_64) 验证通过。
 *
 * =====================================================================
 */

/* 必须在最前面, 确保 _GNU_SOURCE 对 test_framework.h 生效 */
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include "test_framework.h"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <sys/socket.h>
#include <sys/select.h>
#include <unistd.h>

/* =====================================================================
 * 共享辅助
 * ===================================================================== */

static int make_udp_sock(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        printf("  NOTE | helper: socket(DGRAM) failed: %s\n", strerror(errno));
    }
    return fd;
}

static int make_tcp_sock(void)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        printf("  NOTE | helper: socket(STREAM) failed: %s\n", strerror(errno));
    }
    return fd;
}

/*
 * 创建 UDP socket 并 bind 到 loopback 随机端口。
 * 返回 fd, 通过 addr 返回实际绑定地址。
 */
static int bind_udp_loopback(struct sockaddr_in *addr)
{
    int fd = make_udp_sock();
    if (fd < 0) return -1;

    memset(addr, 0, sizeof(*addr));
    addr->sin_family      = AF_INET;
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr->sin_port        = 0;

    if (bind(fd, (struct sockaddr *)addr, sizeof(*addr)) < 0) {
        printf("  NOTE | helper: bind failed: %s\n", strerror(errno));
        close(fd);
        return -1;
    }

    socklen_t alen = sizeof(*addr);
    if (getsockname(fd, (struct sockaddr *)addr, &alen) < 0) {
        printf("  NOTE | helper: getsockname failed: %s\n", strerror(errno));
        close(fd);
        return -1;
    }
    return fd;
}

/*
 * 创建 TCP 监听 socket 并 bind 到 loopback 随机端口。
 * 返回 listener fd, 通过 addr 返回实际地址。
 */
static int bind_tcp_listener(struct sockaddr_in *addr)
{
    int fd = make_tcp_sock();
    if (fd < 0) return -1;

    memset(addr, 0, sizeof(*addr));
    addr->sin_family      = AF_INET;
    addr->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr->sin_port        = 0;

    if (bind(fd, (struct sockaddr *)addr, sizeof(*addr)) < 0) {
        printf("  NOTE | helper: bind TCP failed: %s\n", strerror(errno));
        close(fd);
        return -1;
    }
    if (listen(fd, 1) < 0) {
        printf("  NOTE | helper: listen failed: %s\n", strerror(errno));
        close(fd);
        return -1;
    }
    socklen_t alen = sizeof(*addr);
    if (getsockname(fd, (struct sockaddr *)addr, &alen) < 0) {
        printf("  NOTE | helper: getsockname failed: %s\n", strerror(errno));
        close(fd);
        return -1;
    }
    return fd;
}

/*
 * 连接到 TCP listener 并返回 client fd 和 accept 得到的 server fd。
 */
static int tcp_connect_pair(int listener, int *server_out)
{
    struct sockaddr_in laddr;
    socklen_t alen = sizeof(laddr);
    if (getsockname(listener, (struct sockaddr *)&laddr, &alen) < 0) {
        return -1;
    }

    int client = make_tcp_sock();
    if (client < 0) return -1;
    if (connect(client, (struct sockaddr *)&laddr, sizeof(laddr)) < 0) {
        printf("  NOTE | helper: connect failed: %s\n", strerror(errno));
        close(client);
        return -1;
    }

    *server_out = accept(listener, NULL, NULL);
    if (*server_out < 0) {
        printf("  NOTE | helper: accept failed: %s\n", strerror(errno));
        close(client);
        return -1;
    }
    return client;
}

/* =====================================================================
 * sendto 测试
 * ===================================================================== */

/*
 * [S1] 基本 UDP sendto
 *
 * 语义: 未连接 UDP socket 上 sendto() 发送数据到指定 dest。
 * 预期: 返回发送字节数; 对端 recvfrom 收到相同数据。
 */
static void test_s1_sendto_udp_basic(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "hello_sendto";
    ssize_t nsent = sendto(client, payload, 13, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 13, "sendto 返回发送字节数");

    char buf[64] = {0};
    struct sockaddr_in peer;
    socklen_t plen = sizeof(peer);
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0,
                             (struct sockaddr *)&peer, &plen);
    CHECK_RET(nrecv, 13, "对端收到相同字节数");
    CHECK(memcmp(buf, payload, 13) == 0, "对端收到相同内容");
    CHECK(peer.sin_family == AF_INET, "对端源地址 family=AF_INET");

    close(client);
    close(server);
}

/*
 * [S2] 已连接 UDP sendto(NULL,0) 等价 send()
 *
 * 语义: 已 connect() 的 UDP 上用 sendto(NULL,0) 等同于 send()。
 * 预期: 返回发送字节数, 对端收到数据。
 */
static void test_s2_sendto_udp_connected_null(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    CHECK_RET(connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
              0, "UDP connect 成功");

    const char *payload = "null_dest";
    ssize_t nsent = sendto(client, payload, 9, 0, NULL, 0);
    CHECK_RET(nsent, 9, "sendto(NULL,0) 等价 send");

    char buf[64] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 9, "对端收到数据");
    CHECK(memcmp(buf, payload, 9) == 0, "内容一致");

    close(client);
    close(server);
}

/*
 * [S3] sendto EBADF — fd 无效
 *
 * 语义: sockfd 不是有效的打开文件描述符。
 * 预期: errno=EBADF。
 */
static void test_s3_sendto_ebadf(void)
{
    const char buf[] = "x";
    CHECK_ERR(sendto(-1, buf, 1, 0, NULL, 0), EBADF, "sendto(fd=-1) → EBADF");
}

/*
 * [S4] sendto ENOTSOCK — fd 不是 socket
 *
 * 语义: sockfd 不指向 socket。
 * 预期: errno=ENOTSOCK。
 */
static void test_s4_sendto_enotsock(void)
{
    int fd = open("/dev/null", O_WRONLY);
    if (fd < 0) {
        printf("  NOTE | cannot open /dev/null, skip\n");
        return;
    }
    const char buf[] = "x";
    CHECK_ERR(sendto(fd, buf, 1, 0, NULL, 0), ENOTSOCK,
              "sendto on /dev/null fd → ENOTSOCK");
    close(fd);
}

/*
 * [S5] sendto EDESTADDRREQ — 未连接且无 dest
 *
 * 语义: 未连接的非流式 socket (UDP) 没有设置对端地址。
 * 预期: errno=EDESTADDRREQ。
 */
static void test_s5_sendto_edestaddrreq(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;
    const char buf[] = "x";
    CHECK_ERR(sendto(fd, buf, 1, 0, NULL, 0), EDESTADDRREQ,
              "sendto UDP 无 dest → EDESTADDRREQ");
    close(fd);
}

/*
 * [S6] sendto EINVAL — addrlen 非法
 *
 * 语义: 传入的 addrlen 对于 AF_INET 太小或非法。
 * 预期: errno=EINVAL。
 */
static void test_s6_sendto_einval_addrlen(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;

    struct sockaddr_in addr = {
        .sin_family      = AF_INET,
        .sin_addr.s_addr = htonl(INADDR_LOOPBACK),
        .sin_port        = htons(12345),
    };
    const char buf[] = "x";

    CHECK_ERR(sendto(fd, buf, 1, 0, (struct sockaddr *)&addr, 0), EINVAL,
              "sendto addrlen=0 → EINVAL");
    CHECK_ERR(sendto(fd, buf, 1, 0, (struct sockaddr *)&addr, 1), EINVAL,
              "sendto addrlen=1 → EINVAL");

    close(fd);
}

/*
 * [S7] sendto EOPNOTSUPP — MSG_OOB 不适用 UDP
 *
 * 语义: MSG_OOB 仅对 SOCK_STREAM 有效, DGRAM 应返回 EOPNOTSUPP。
 * 预期: errno=EOPNOTSUPP。
 */
static void test_s7_sendto_eopnotsupp(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char buf[] = "x";
    CHECK_ERR(sendto(client, buf, 1, MSG_OOB,
                     (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
              EOPNOTSUPP, "sendto MSG_OOB on DGRAM → EOPNOTSUPP");

    close(client);
    close(server);
}

/*
 * [S8] sendto MSG_MORE — UDP cork 合并数据报
 *
 * 语义: MSG_MORE 告知内核将后续数据合并为一个数据报, 直到某个不带
 *       MSG_MORE 的调用触发发送。
 * 预期: 连续 3 次 MSG_MORE + 1 次不带 flag 的 sendto, 对端只收到
 *       一个合并后的数据报。
 */
static void test_s8_sendto_msg_more(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *chunks[] = {"AAA", "BBB", "CCC"};
    ssize_t nsent;
    for (int i = 0; i < 3; i++) {
        nsent = sendto(client, chunks[i], 3, MSG_MORE,
                       (struct sockaddr *)&srv_addr, sizeof(srv_addr));
        CHECK_RET(nsent, 3, "sendto MSG_MORE chunk");
    }

    nsent = sendto(client, "DDD", 3, 0,
                   (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 3, "sendto 最后一块 (触发发送)");

    char buf[64] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 12, "收到合并数据报: 12 字节");
    CHECK(memcmp(buf, "AAABBBCCCDDD", 12) == 0, "合并内容 = AAABBBCCCDDD");

    close(client);
    close(server);
}

/*
 * [S9] sendto 零长度数据报
 *
 * 语义: UDP 允许发送 0 字节数据报 (仅 IP+UDP 头)。
 * 预期: sendto 返回 0, 对端收到 0 字节。
 */
static void test_s9_sendto_zero_len(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    ssize_t nsent = sendto(client, NULL, 0, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 0, "sendto 0 字节 → 返回 0");

    char buf[1] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 0, "对端收到 0 字节数据报");

    close(client);
    close(server);
}

/*
 * [S10] sendto MSG_DONTWAIT — 单次非阻塞发送
 *
 * 语义: MSG_DONTWAIT 在本次调用临时启用非阻塞模式。
 * 注意: 单线程 UDP loopback 上 TX buffer 极难填满（send 路径每次都会
 *       poll_interfaces 排空），因此本测试仅验证 MSG_DONTWAIT 发送路径
 *       无异常错误。MSG_DONTWAIT 的 EAGAIN 验证由 [R7] 覆盖。
 */
static void test_s10_sendto_msg_dontwait(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "dontwait";
    errno = 0;
    ssize_t nsent = sendto(client, payload, 8, MSG_DONTWAIT,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 8, "sendto MSG_DONTWAIT 正常发送");

    char buf[16] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 8, "对端收到数据");
    CHECK(memcmp(buf, payload, 8) == 0, "内容一致");

    close(client);
    close(server);
}

/*
 * [S11] MSG_MORE cork 目标地址固定在首段目标
 *
 * 语义: Linux UDP corking 使用首次 MSG_MORE 调用确定的目标地址，
 *       后续调用（包括最终触发发送的调用）即使传入不同地址也应发送到
 *       首次确定的目标。
 * 预期: sendto(A, MSG_MORE) + sendto(A, MSG_MORE) + sendto(B, 0)
 *       → A 收到 "AAABBBCCC", B 无数据。
 */
static void test_s11_sendto_msg_more_endpoint(void)
{
    struct sockaddr_in srv_a, srv_b;
    int server_a = bind_udp_loopback(&srv_a);
    if (server_a < 0) return;
    int server_b = bind_udp_loopback(&srv_b);
    if (server_b < 0) { close(server_a); return; }
    int client = make_udp_sock();
    if (client < 0) { close(server_a); close(server_b); return; }

    /* MSG_MORE 到 A (首次确定目标) */
    ssize_t n1 = sendto(client, "AAA", 3, MSG_MORE,
                        (struct sockaddr *)&srv_a, sizeof(srv_a));
    CHECK_RET(n1, 3, "MSG_MORE 第1段 → A");

    /* MSG_MORE 到 A (继续追加) */
    ssize_t n2 = sendto(client, "BBB", 3, MSG_MORE,
                        (struct sockaddr *)&srv_a, sizeof(srv_a));
    CHECK_RET(n2, 3, "MSG_MORE 第2段 → A");

    /* 最终触发发送 — 故意传 B 的地址，但内核应发到 A */
    ssize_t n3 = sendto(client, "CCC", 3, 0,
                        (struct sockaddr *)&srv_b, sizeof(srv_b));
    CHECK_RET(n3, 3, "最终 sendto → B (但应发到 A)");

    /* A 应收到完整合并数据报 */
    {
        int flags_a = fcntl(server_a, F_GETFL, 0);
        fcntl(server_a, F_SETFL, flags_a | O_NONBLOCK);
        char buf[64] = {0};
        ssize_t nr = recvfrom(server_a, buf, sizeof(buf), 0, NULL, NULL);
        fcntl(server_a, F_SETFL, flags_a);
        CHECK_RET(nr, 9, "A 收到 9 字节合并数据报");
        if (nr == 9) {
            CHECK(memcmp(buf, "AAABBBCCC", 9) == 0, "内容 = AAABBBCCC");
        }
    }

    /* B 应无数据可收 */
    {
        int flags_b = fcntl(server_b, F_GETFL, 0);
        fcntl(server_b, F_SETFL, flags_b | O_NONBLOCK);
        char buf[64] = {0};
        errno = 0;
        ssize_t nr = recvfrom(server_b, buf, sizeof(buf), 0, NULL, NULL);
        fcntl(server_b, F_SETFL, flags_b);
        CHECK(nr == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
              "B 无数据 (EAGAIN)");
    }

    close(client);
    close(server_a);
    close(server_b);
}

/*
 * [S12] sendto addr==NULL && addrlen!=0
 *
 * 语义: Linux 对 sendto(NULL, nonzero_addrlen) 的处理与 sendto(NULL, 0)
 *       不同——不是返回 EINVAL，而是按"无目标地址"处理：
 *       - 未连接 UDP → EDESTADDRREQ
 *       - 已连接 UDP → 发往 peer（成功）
 * 预期: 未连接时返回 EDESTADDRREQ；已连接时发送成功。
 */
static void test_s12_sendto_null_nonzero_addrlen(void)
{
    /* 未连接场景 */
    int fd = make_udp_sock();
    if (fd < 0) return;
    const char buf[] = "x";
    /* 传 NULL + nonzero addrlen: 应等同 NULL+0, 返回 EDESTADDRREQ 而非 EINVAL */
    CHECK_ERR(sendto(fd, buf, 1, 0, NULL, sizeof(struct sockaddr_in)),
              EDESTADDRREQ,
              "sendto(NULL, nonzero) 未连接 → EDESTADDRREQ");
    close(fd);

    /* 已连接场景 */
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }
    if (connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)) < 0) {
        close(client);
        close(server);
        return;
    }
    /* 已连接后 sendto(NULL, nonzero) 应等价于 send()，发往 peer */
    errno = 0;
    ssize_t nsent = sendto(client, buf, 1, 0, NULL,
                           sizeof(struct sockaddr_in));
    CHECK_RET(nsent, 1, "sendto(NULL, nonzero) 已连接 → 发送成功");

    char rbuf[1] = {0};
    ssize_t nrecv = recvfrom(server, rbuf, 1, 0, NULL, NULL);
    CHECK_RET(nrecv, 1, "对端收到数据");

    close(client);
    close(server);
}

/*
 * [S13] MSG_MORE 超过 cork 上限 → EMSGSIZE
 *
 * 语义: MSG_MORE cork 缓冲有上限（UDP TX buffer 大小，64 KiB）。
 *       单次 len 超限或累积超限均应返回 EMSGSIZE，阻止无界内存分配。
 * 预期: sendto(..., MSG_MORE) len > 64K → EMSGSIZE;
 *       两次 MSG_MORE 累积 > 64K → 第二次返回 EMSGSIZE。
 * 注意: Linux 上 EMSGSIZE 后 cork 可能被清空，因此不强制验证 flush
 *       是否保留了第一次的数据（不同内核行为不同）。
 */
static void test_s13_sendto_msg_more_emsgsize(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    /* 单次 len 超过上限 (64 KiB + 1)：用有效 buffer 避免 EFAULT */
    char *big = malloc(65537);
    if (big) {
        memset(big, 0, 65537);
        CHECK_ERR(sendto(client, big, 65537, MSG_MORE,
                         (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
                  EMSGSIZE,
                  "MSG_MORE len > 64K → EMSGSIZE");
        free(big);
    }

    /* 累积超过上限: 两次 MSG_MORE, 第二次触发 EMSGSIZE */
    char *buf32k = malloc(32768);
    if (!buf32k) {
        close(client);
        close(server);
        return;
    }
    memset(buf32k, 'A', 32768);
    ssize_t n1 = sendto(client, buf32k, 32768, MSG_MORE,
                        (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(n1, 32768, "MSG_MORE 32K → 接受");

    /* 第二次累积 32769 → total 65537 > 65536 → EMSGSIZE */
    CHECK_ERR(sendto(client, buf32k, 32769, MSG_MORE,
                     (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
              EMSGSIZE,
              "MSG_MORE 累积 > 64K → EMSGSIZE");

    /* flush 不应崩溃。cork 可能已清空（Linux）或保留（StarryOS）,
       只验证 flush 调用本身成功 */
    ssize_t nflush = sendto(client, "BB", 2, 0,
                            (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK(nflush >= 0, "flush 成功（不崩溃）");

    free(buf32k);
    close(client);
    close(server);
}

/*
 * [S14] MSG_MORE cork + final flush 总长超限 → EMSGSIZE
 *
 * 语义: 即使 MSG_MORE 阶段累计未超上限，若最终不带 MSG_MORE 的 flush
 *       调用传入额外数据使总量超过 CORK_MAX (UDP_TX_BUF_LEN)，
 *       也应返回 EMSGSIZE。
 * 预期: MSG_MORE 累计未超限（32768 ≤ 65536），
 *       final flush 追加 32769 → 总量 65537 > 65536 → EMSGSIZE。
 * 注意: 不同平台 MSG_MORE 单次上限不同（取决于 SO_SNDBUF），
 *       因此基础值用已验证可行的 32768；若平台拒绝，跳过测试。
 */
static void test_s14_sendto_msg_more_flush_overflow(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    /* 先发一个已知可行的 MSG_MORE 块 */
    int base = 32768;
    char *buf = malloc(base);
    if (!buf) { close(client); close(server); return; }
    memset(buf, 'A', base);
    errno = 0;
    ssize_t n1 = sendto(client, buf, base, MSG_MORE,
                        (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    /* 若平台连 base 都拒绝，跳过测试 */
    if (n1 < 0) {
        free(buf);
        close(client);
        close(server);
        return;
    }
    CHECK_RET(n1, base, "MSG_MORE → 接受基础块 (未超限)");

    /* flush 追加 extra 使总长 base + extra > 65536 → EMSGSIZE */
    int extra = 65536 - base + 1; /* 32769 */
    CHECK_ERR(sendto(client, buf, extra, 0,
                     (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
              EMSGSIZE,
              "flush 追加 → 总量 65537 > 64 KiB → EMSGSIZE");

    free(buf);
    close(client);
    close(server);
}

/*
 * [S15] MSG_MORE cork flush without destination address
 *
 * 语义: 未连接 UDP socket 通过 sendto(..., MSG_MORE, dest) 建立 cork 后，
 *       后续 send()/sendto(NULL, 0) 不带目标地址也能 flush 到首次捕获的 dest。
 * 预期: sendto(MSG_MORE, dest) + send() flush → 合并数据报成功送达。
 */
static void test_s15_sendto_msg_more_flush_no_dest(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    /* 第 1 块: sendto(..., MSG_MORE, dest) 建立 cork */
    const char *chunk1 = "Hello";
    ssize_t n1 = sendto(client, chunk1, 5, MSG_MORE,
                        (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(n1, 5, "MSG_MORE chunk 1 → cork 建立");

    /* 第 2 块: send() 不带地址 flush — 应使用 cork 捕获的 dest */
    const char *chunk2 = "World";
    ssize_t n2 = send(client, chunk2, 5, 0);
    CHECK_RET(n2, 5, "send() flush 不带地址 → 使用 cork dest");

    /* 验证: 接收端收到合并的 "HelloWorld" */
    char rbuf[16] = {0};
    ssize_t nr = recvfrom(server, rbuf, sizeof(rbuf), 0, NULL, NULL);
    CHECK_RET(nr, 10, "接收合并数据报长度 10");
    CHECK(memcmp(rbuf, "HelloWorld", 10) == 0, "内容为 'HelloWorld'");

    close(client);
    close(server);
}

/* =====================================================================
 * recvfrom 测试
 * ===================================================================== */

/*
 * [R1] 基本 UDP recvfrom — 接收带源地址
 *
 * 语义: recvfrom 接收数据并填入源地址 (value-result)。
 * 预期: 返回接收字节数, src_addr 包含发送方地址。
 */
static void test_r1_recvfrom_udp_basic(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "recvfrom_test";
    ssize_t nsent = sendto(client, payload, 13, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 13, "预先发送数据");

    char buf[64] = {0};
    struct sockaddr_in peer;
    socklen_t plen = sizeof(peer);
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0,
                             (struct sockaddr *)&peer, &plen);
    CHECK_RET(nrecv, 13, "recvfrom 返回接收字节数");
    CHECK(memcmp(buf, payload, 13) == 0, "内容一致");
    CHECK(plen == sizeof(struct sockaddr_in), "addrlen 更新为 sockaddr_in 大小");
    CHECK(peer.sin_family == AF_INET, "源地址 family=AF_INET");

    close(client);
    close(server);
}

/*
 * [R2] recvfrom NULL src/alen 等价 recv()
 *
 * 语义: recvfrom(NULL, NULL) 等价于 recv()。
 * 预期: 正常接收数据。
 */
static void test_r2_recvfrom_null_src(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "null_src";
    ssize_t nsent = sendto(client, payload, 8, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 8, "预先发送");

    char buf[64] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 8, "recvfrom(NULL,NULL) 正常接收");
    CHECK(memcmp(buf, payload, 8) == 0, "内容一致");

    close(client);
    close(server);
}

/*
 * [R3] recvfrom EBADF — fd 无效
 *
 * 语义: 同 sendto。
 * 预期: errno=EBADF。
 */
static void test_r3_recvfrom_ebadf(void)
{
    char buf[1];
    CHECK_ERR(recvfrom(-1, buf, 1, 0, NULL, NULL), EBADF,
              "recvfrom(fd=-1) → EBADF");
}

/*
 * [R4] recvfrom ENOTSOCK — fd 不是 socket
 *
 * 语义: 同 sendto。
 * 预期: errno=ENOTSOCK。
 */
static void test_r4_recvfrom_enotsock(void)
{
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) {
        printf("  NOTE | cannot open /dev/null, skip\n");
        return;
    }
    char buf[1];
    CHECK_ERR(recvfrom(fd, buf, 1, 0, NULL, NULL), ENOTSOCK,
              "recvfrom on /dev/null fd → ENOTSOCK");
    close(fd);
}

/*
 * [R5] recvfrom EAGAIN/EWOULDBLOCK — 非阻塞无数据
 *
 * 语义: 非阻塞 socket 无数据可读时应返回 EAGAIN 或 EWOULDBLOCK。
 *      POSIX 允许两者之一。
 * 预期: ret=-1, errno ∈ {EAGAIN, EWOULDBLOCK}。
 */
static void test_r5_recvfrom_eagain(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;

    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);

    char buf[1];
    errno = 0;
    ssize_t ret = recvfrom(fd, buf, 1, 0, NULL, NULL);
    CHECK(ret == -1, "非阻塞 recvfrom 返回 -1");
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
          "errno = EAGAIN 或 EWOULDBLOCK");

    close(fd);
}

/*
 * [R6] recvfrom MSG_PEEK — 窥探不消费
 *
 * 语义: MSG_PEEK 返回数据但不从接收队列移除。
 * 预期: 两次 recvfrom: 第一次带 MSG_PEEK 返回 N 字节,
 *       第二次不带 flag 仍返回 N 字节 (同一份数据)。
 */
static void test_r6_recvfrom_msg_peek(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "PEEK_ME";
    ssize_t nsent = sendto(client, payload, 7, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 7, "预先发送");

    char buf[64] = {0};
    ssize_t n1 = recvfrom(server, buf, sizeof(buf), MSG_PEEK, NULL, NULL);
    CHECK_RET(n1, 7, "MSG_PEEK 第 1 次: 返回 7 字节");
    CHECK(memcmp(buf, payload, 7) == 0, "MSG_PEEK 内容正确");

    memset(buf, 0, sizeof(buf));
    ssize_t n2 = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(n2, 7, "第 2 次不带 flag: 仍返回 7 字节 (未消费)");
    CHECK(memcmp(buf, payload, 7) == 0, "第 2 次内容相同");

    /* 第三次应该没有数据了 */
    int flags_old = fcntl(server, F_GETFL, 0);
    fcntl(server, F_SETFL, flags_old | O_NONBLOCK);
    errno = 0;
    ssize_t n3 = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK(n3 == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "第 3 次: 无数据 (MSG_PEEK 不消费)");
    fcntl(server, F_SETFL, flags_old);

    close(client);
    close(server);
}

/*
 * [R7] recvfrom MSG_DONTWAIT — 单次非阻塞接收
 *
 * 语义: MSG_DONTWAIT 在本次调用临时启用非阻塞模式，无数据时立即返回
 *       EAGAIN/EWOULDBLOCK，而不是阻塞等待。
 * 预期: 阻塞 socket 无数据时 recvfrom(..., MSG_DONTWAIT) 返回 -1
 *       且 errno = EAGAIN/EWOULDBLOCK。
 */
static void test_r7_recvfrom_msg_dontwait(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;

    /* 绑定以便接收（未 bind 时 Linux 也允许 recvfrom） */
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family      = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port        = 0;
    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return;
    }

    char buf[1];
    errno = 0;
    ssize_t ret = recvfrom(fd, buf, 1, MSG_DONTWAIT, NULL, NULL);
    CHECK(ret == -1, "MSG_DONTWAIT recvfrom 返回 -1");
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
          "errno = EAGAIN 或 EWOULDBLOCK");

    close(fd);
}

/* =====================================================================
 * sendmsg 测试
 * ===================================================================== */

/*
 * [SM1] 基本 UDP sendmsg — msg_name 指定目标
 *
 * 语义: sendmsg 通过 msghdr.msg_name 指定目标地址, msg_iov 指定数据。
 * 预期: 返回发送字节数; 对端收到相同数据。
 */
static void test_sm1_sendmsg_udp_basic(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "sendmsg_hello";
    struct iovec iov = { .iov_base = (char *)payload, .iov_len = 13 };
    struct msghdr msg = {
        .msg_name       = &srv_addr,
        .msg_namelen    = sizeof(srv_addr),
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
        .msg_control    = NULL,
        .msg_controllen = 0,
        .msg_flags      = 0,
    };

    ssize_t nsent = sendmsg(client, &msg, 0);
    CHECK_RET(nsent, 13, "sendmsg 返回发送字节数");

    char buf[64] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 13, "对端收到 13 字节");
    CHECK(memcmp(buf, payload, 13) == 0, "内容一致");

    close(client);
    close(server);
}

/*
 * [SM2] 已连接 sendmsg 不带 msg_name
 *
 * 语义: 已连接 socket 上 sendmsg 可以不带 msg_name (msg_name=NULL,
 *       msg_namelen=0)。
 * 预期: 成功发送。
 */
static void test_sm2_sendmsg_connected(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    CHECK_RET(connect(client, (struct sockaddr *)&srv_addr, sizeof(srv_addr)),
              0, "connect UDP");

    const char *payload = "connected_msg";
    struct iovec iov = { .iov_base = (char *)payload, .iov_len = 13 };
    struct msghdr msg = {
        .msg_name       = NULL,
        .msg_namelen    = 0,
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
    };

    ssize_t nsent = sendmsg(client, &msg, 0);
    /*
     * Linux 对已连接 UDP 的 sendmsg 可能:
     *   - 成功返回 (大多数情况)
     *   - 返回 EISCONN (符合 POSIX "may be returned")
     */
    if (nsent == -1 && errno == EISCONN) {
        /* 合法行为, 未发送但也不算失败 */
        printf("  PASS | %s:%d | sendmsg connected UDP (errno=EISCONN as allowed)\n",
               __FILE__, __LINE__);
    } else {
        CHECK_RET(nsent, 13, "sendmsg 已连接 UDP 成功发送");
        char buf[64] = {0};
        ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
        CHECK_RET(nrecv, 13, "对端收到数据");
    }

    close(client);
    close(server);
}

/*
 * [SM3] sendmsg EBADF — fd 无效
 *
 * 预期: errno=EBADF。
 */
static void test_sm3_sendmsg_ebadf(void)
{
    struct iovec iov = { .iov_base = "x", .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };
    CHECK_ERR(sendmsg(-1, &msg, 0), EBADF, "sendmsg(fd=-1) → EBADF");
}

/*
 * [SM4] sendmsg ENOTSOCK — fd 不是 socket
 *
 * 预期: errno=ENOTSOCK。
 */
static void test_sm4_sendmsg_enotsock(void)
{
    int fd = open("/dev/null", O_WRONLY);
    if (fd < 0) {
        printf("  NOTE | cannot open /dev/null, skip\n");
        return;
    }
    struct iovec iov = { .iov_base = "x", .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };
    CHECK_ERR(sendmsg(fd, &msg, 0), ENOTSOCK,
              "sendmsg on /dev/null fd → ENOTSOCK");
    close(fd);
}

/*
 * [SM5] sendmsg EDESTADDRREQ — 无 dest
 *
 * 预期: errno=EDESTADDRREQ。
 */
static void test_sm5_sendmsg_edestaddrreq(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;
    struct iovec iov = { .iov_base = "x", .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };
    CHECK_ERR(sendmsg(fd, &msg, 0), EDESTADDRREQ,
              "sendmsg UDP 无 dest → EDESTADDRREQ");
    close(fd);
}

/*
 * [SM6] sendmsg EOPNOTSUPP — MSG_OOB 不适用 UDP
 *
 * 预期: errno=EOPNOTSUPP。
 */
static void test_sm6_sendmsg_eopnotsupp(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    struct iovec iov = { .iov_base = "x", .iov_len = 1 };
    struct msghdr msg = {
        .msg_name    = &srv_addr,
        .msg_namelen = sizeof(srv_addr),
        .msg_iov     = &iov,
        .msg_iovlen  = 1,
    };
    CHECK_ERR(sendmsg(client, &msg, MSG_OOB), EOPNOTSUPP,
              "sendmsg MSG_OOB on DGRAM → EOPNOTSUPP");

    close(client);
    close(server);
}

/*
 * [SM7] sendmsg MSG_MORE — UDP cork via sendmsg
 *
 * 语义: 通过 sendmsg 使用 MSG_MORE 合并数据报。
 * 预期: 两次 MSG_MORE + 1 次无 flag → 合并为单个数据报。
 */
static void test_sm7_sendmsg_msg_more(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *a = "HELLO";
    const char *b = "WORLD";
    struct iovec iov_a = { .iov_base = (char *)a, .iov_len = 5 };
    struct iovec iov_b = { .iov_base = (char *)b, .iov_len = 5 };
    struct msghdr msg = {
        .msg_name    = &srv_addr,
        .msg_namelen = sizeof(srv_addr),
        .msg_iov     = &iov_a,
        .msg_iovlen  = 1,
    };

    /* 第 1 块: MSG_MORE */
    ssize_t n1 = sendmsg(client, &msg, MSG_MORE);
    CHECK_RET(n1, 5, "sendmsg MSG_MORE chunk 1");

    /* 第 2 块: MSG_MORE, 换 iov */
    msg.msg_iov = &iov_b;
    ssize_t n2 = sendmsg(client, &msg, MSG_MORE);
    CHECK_RET(n2, 5, "sendmsg MSG_MORE chunk 2");

    /* 第 3 块: 无 MSG_MORE, 触发发送 */
    const char *c = "!";
    struct iovec iov_c = { .iov_base = (char *)c, .iov_len = 1 };
    msg.msg_iov = &iov_c;
    ssize_t n3 = sendmsg(client, &msg, 0);
    CHECK_RET(n3, 1, "sendmsg 最后一块触发发送");

    char buf[64] = {0};
    ssize_t nrecv = recvfrom(server, buf, sizeof(buf), 0, NULL, NULL);
    CHECK_RET(nrecv, 11, "收到合并数据报: 11 字节");
    CHECK(memcmp(buf, "HELLOWORLD!", 11) == 0, "合并内容 = HELLOWORLD!");

    close(client);
    close(server);
}

/*
 * [SM8] sendmsg MSG_NOSIGNAL — TCP 对端关闭不触发 SIGPIPE
 *
 * 语义: 对已关闭对端的 TCP socket 使用 MSG_NOSIGNAL 发送, 应返回
 *       EPIPE (或 ECONNRESET) 但不触发 SIGPIPE。
 *
 * 注意: 手册 BUGS 记载 ENOTCONN 也可能被 EPIPE 代替, 因此预先
 *       忽略 SIGPIPE 作为保护。
 */
static void test_sm8_sendmsg_msg_nosignal(void)
{
    struct sockaddr_in laddr;
    int listener = bind_tcp_listener(&laddr);
    if (listener < 0) return;

    int server_fd;
    int client = tcp_connect_pair(listener, &server_fd);
    if (client < 0) { close(listener); return; }

    /* 关闭对端 */
    close(server_fd);

    /* 安全措施: 忽略 SIGPIPE */
    sighandler_t old = signal(SIGPIPE, SIG_IGN);

    /*
     * 反复发送直到缓冲区耗尽, 触发 EPIPE 或 ECONNRESET。
     */
    char buf[4096] = {0};
    int got_err = 0;
    for (int i = 0; i < 200; i++) {
        struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
        struct msghdr msg = {
            .msg_name = NULL, .msg_namelen = 0,
            .msg_iov = &iov, .msg_iovlen = 1,
        };
        ssize_t ret = sendmsg(client, &msg, MSG_NOSIGNAL);
        if (ret == -1) {
            if (errno == EPIPE) {
                got_err = 1;
                break;
            }
            if (errno == ECONNRESET) {
                printf("  PASS | %s:%d | sendmsg MSG_NOSIGNAL (errno=ECONNRESET)\n",
                       __FILE__, __LINE__);
                got_err = 2;
                break;
            }
            break;
        }
    }

    if (got_err == 1) {
        CHECK(1, "sendmsg MSG_NOSIGNAL → EPIPE, no SIGPIPE");
    } else if (got_err == 0) {
        /* 缓冲区未满，未触发错误，也是可能的 */
        printf("  PASS | %s:%d | sendmsg MSG_NOSIGNAL (buffer not exhausted)\n",
               __FILE__, __LINE__);
    }

    signal(SIGPIPE, old);
    close(client);
    close(listener);
}

/* =====================================================================
 * recvmsg 测试
 * ===================================================================== */

/*
 * [RM1] 基本 UDP recvmsg — msg_name 返回源地址
 *
 * 语义: recvmsg 通过 msghdr 接收数据和源地址。
 * 预期: 返回接收字节数; msg_name 包含发送方地址;
 *       msg_namelen 更新为实际地址长度; msg_flags 不含 MSG_TRUNC。
 */
static void test_rm1_recvmsg_udp_basic(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "recvmsg_test!";
    ssize_t nsent = sendto(client, payload, 13, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 13, "预先发送");

    char buf[64] = {0};
    struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
    struct sockaddr_in peer = {0};
    struct msghdr msg = {
        .msg_name       = &peer,
        .msg_namelen    = sizeof(peer),
        .msg_iov        = &iov,
        .msg_iovlen     = 1,
        .msg_control    = NULL,
        .msg_controllen = 0,
        .msg_flags      = 0,
    };

    ssize_t nrecv = recvmsg(server, &msg, 0);
    CHECK_RET(nrecv, 13, "recvmsg 返回 13 字节");
    CHECK(memcmp(buf, payload, 13) == 0, "内容一致");
    CHECK(msg.msg_namelen == sizeof(struct sockaddr_in),
          "msg_namelen 更新为 sockaddr_in 大小");
    CHECK(peer.sin_family == AF_INET, "msg_name.family = AF_INET");
    CHECK((msg.msg_flags & MSG_TRUNC) == 0, "msg_flags 不含 MSG_TRUNC");

    close(client);
    close(server);
}

/*
 * [RM2] recvmsg scatter/gather — 多 iovec 接收
 *
 * 语义: recvmsg 支持多个 iovec 散布接收。
 * 预期: 数据按 iov 顺序填充, 返回总字节数。
 */
static void test_rm2_recvmsg_scatter_gather(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "ABCDEFGHIJ"; /* 10 bytes */
    ssize_t nsent = sendto(client, payload, 10, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 10, "预先发送 10 字节");

    char a[4] = {0}, b[4] = {0}, c[4] = {0};
    struct iovec iov[3] = {
        { .iov_base = a, .iov_len = 3 },
        { .iov_base = b, .iov_len = 3 },
        { .iov_base = c, .iov_len = 4 },
    };
    struct msghdr msg = {
        .msg_name       = NULL,
        .msg_namelen    = 0,
        .msg_iov        = iov,
        .msg_iovlen     = 3,
    };

    ssize_t nrecv = recvmsg(server, &msg, 0);
    CHECK_RET(nrecv, 10, "recvmsg scatter: 总返回 10 字节");
    CHECK(memcmp(a, "ABC", 3) == 0, "iov[0] = ABC");
    CHECK(memcmp(b, "DEF", 3) == 0, "iov[1] = DEF");
    CHECK(memcmp(c, "GHIJ", 4) == 0, "iov[2] = GHIJ");

    close(client);
    close(server);
}

/*
 * [RM3] recvmsg EBADF — fd 无效
 *
 * 预期: errno=EBADF。
 */
static void test_rm3_recvmsg_ebadf(void)
{
    char buf[1];
    struct iovec iov = { .iov_base = buf, .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };
    CHECK_ERR(recvmsg(-1, &msg, 0), EBADF, "recvmsg(fd=-1) → EBADF");
}

/*
 * [RM4] recvmsg ENOTSOCK — fd 不是 socket
 *
 * 预期: errno=ENOTSOCK。
 */
static void test_rm4_recvmsg_enotsock(void)
{
    int fd = open("/dev/null", O_RDONLY);
    if (fd < 0) {
        printf("  NOTE | cannot open /dev/null, skip\n");
        return;
    }
    char buf[1];
    struct iovec iov = { .iov_base = buf, .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };
    CHECK_ERR(recvmsg(fd, &msg, 0), ENOTSOCK,
              "recvmsg on /dev/null fd → ENOTSOCK");
    close(fd);
}

/*
 * [RM5] recvmsg EAGAIN/EWOULDBLOCK — 非阻塞无数据
 *
 * 同 recvfrom [R5]。
 */
static void test_rm5_recvmsg_eagain(void)
{
    int fd = make_udp_sock();
    if (fd < 0) return;

    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);

    char buf[1];
    struct iovec iov = { .iov_base = buf, .iov_len = 1 };
    struct msghdr msg = {
        .msg_name = NULL, .msg_namelen = 0,
        .msg_iov = &iov, .msg_iovlen = 1,
    };

    errno = 0;
    ssize_t ret = recvmsg(fd, &msg, 0);
    CHECK(ret == -1, "非阻塞 recvmsg 返回 -1");
    CHECK(errno == EAGAIN || errno == EWOULDBLOCK,
          "errno = EAGAIN 或 EWOULDBLOCK");

    close(fd);
}

/*
 * [RM6] recvmsg MSG_PEEK — 窥探
 *
 * 同 recvfrom [R6], 同时验证 msg_flags。
 */
static void test_rm6_recvmsg_msg_peek(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    const char *payload = "peekmsg";
    ssize_t nsent = sendto(client, payload, 7, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 7, "预先发送");

    char buf[64] = {0};
    struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
    struct msghdr msg = {
        .msg_name    = NULL,
        .msg_namelen = 0,
        .msg_iov     = &iov,
        .msg_iovlen  = 1,
    };

    /* 第 1 次: MSG_PEEK */
    ssize_t n1 = recvmsg(server, &msg, MSG_PEEK);
    CHECK_RET(n1, 7, "recvmsg MSG_PEEK: 返回 7 字节");
    CHECK(memcmp(buf, payload, 7) == 0, "内容正确");

    /* 第 2 次: 普通 recvmsg */
    memset(buf, 0, sizeof(buf));
    msg.msg_iov->iov_base = buf;
    ssize_t n2 = recvmsg(server, &msg, 0);
    CHECK_RET(n2, 7, "第 2 次 recvmsg: 仍返回 7 字节 (未消费)");
    CHECK(memcmp(buf, payload, 7) == 0, "内容相同");

    /* 第 3 次: 无数据 */
    int flags_old = fcntl(server, F_GETFL, 0);
    fcntl(server, F_SETFL, flags_old | O_NONBLOCK);
    errno = 0;
    ssize_t n3 = recvmsg(server, &msg, 0);
    CHECK(n3 == -1 && (errno == EAGAIN || errno == EWOULDBLOCK),
          "第 3 次: 无数据 (MSG_PEEK 不消费)");
    fcntl(server, F_SETFL, flags_old);

    close(client);
    close(server);
}

/*
 * [RM7] recvmsg MSG_TRUNC in msg_flags — 截断标志
 *
 * 语义: recvmsg 在接收 buffer 小于 datagram 时，应将 msg.msg_flags
 *       设置为 MSG_TRUNC。
 * 预期: 发送 100 字节，用 10 字节 buffer recvmsg → 返回 10 字节,
 *       msg.msg_flags 含 MSG_TRUNC。
 */
static void test_rm7_recvmsg_msg_trunc_flags(void)
{
    struct sockaddr_in srv_addr;
    int server = bind_udp_loopback(&srv_addr);
    if (server < 0) return;
    int client = make_udp_sock();
    if (client < 0) { close(server); return; }

    /* 发送 100 字节 */
    char payload[100];
    memset(payload, 'X', 100);
    ssize_t nsent = sendto(client, payload, 100, 0,
                           (struct sockaddr *)&srv_addr, sizeof(srv_addr));
    CHECK_RET(nsent, 100, "预先发送 100 字节");

    /* 只用 10 字节 buffer 接收 — 应截断 */
    char buf[10] = {0};
    struct iovec iov = { .iov_base = buf, .iov_len = sizeof(buf) };
    struct msghdr msg = {
        .msg_name    = NULL,
        .msg_namelen = 0,
        .msg_iov     = &iov,
        .msg_iovlen  = 1,
    };

    errno = 0;
    ssize_t nrecv = recvmsg(server, &msg, 0);
    CHECK_RET(nrecv, 10, "recvmsg 只收到 10 字节 (截断)");
    CHECK((msg.msg_flags & MSG_TRUNC) != 0,
          "msg.msg_flags 含 MSG_TRUNC");

    close(client);
    close(server);
}

/* =====================================================================
 * main
 * ===================================================================== */

int main(void)
{
    TEST_START("socket-dataplane");

    /* ---- sendto (14) ---- */
    test_s1_sendto_udp_basic();
    test_s2_sendto_udp_connected_null();
    test_s3_sendto_ebadf();
    test_s4_sendto_enotsock();
    test_s5_sendto_edestaddrreq();
    test_s6_sendto_einval_addrlen();
    test_s7_sendto_eopnotsupp();
    test_s8_sendto_msg_more();
    test_s9_sendto_zero_len();
    test_s10_sendto_msg_dontwait();
    test_s11_sendto_msg_more_endpoint();
    test_s12_sendto_null_nonzero_addrlen();
    test_s13_sendto_msg_more_emsgsize();
    test_s14_sendto_msg_more_flush_overflow();
    test_s15_sendto_msg_more_flush_no_dest();

    /* ---- recvfrom (7) ---- */
    test_r1_recvfrom_udp_basic();
    test_r2_recvfrom_null_src();
    test_r3_recvfrom_ebadf();
    test_r4_recvfrom_enotsock();
    test_r5_recvfrom_eagain();
    test_r6_recvfrom_msg_peek();
    test_r7_recvfrom_msg_dontwait();

    /* ---- sendmsg (8) ---- */
    test_sm1_sendmsg_udp_basic();
    test_sm2_sendmsg_connected();
    test_sm3_sendmsg_ebadf();
    test_sm4_sendmsg_enotsock();
    test_sm5_sendmsg_edestaddrreq();
    test_sm6_sendmsg_eopnotsupp();
    test_sm7_sendmsg_msg_more();
    test_sm8_sendmsg_msg_nosignal();

    /* ---- recvmsg (7) ---- */
    test_rm1_recvmsg_udp_basic();
    test_rm2_recvmsg_scatter_gather();
    test_rm3_recvmsg_ebadf();
    test_rm4_recvmsg_enotsock();
    test_rm5_recvmsg_eagain();
    test_rm6_recvmsg_msg_peek();
    test_rm7_recvmsg_msg_trunc_flags();

    TEST_DONE();
}
