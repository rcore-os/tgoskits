#define _GNU_SOURCE
#include "test_framework.h"
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>

/*
 * IP_PKTINFO / IPV6_RECVPKTINFO / IPV6_PKTINFO setsockopt 被接受的回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   consul 的 UDP DNS 服务器 (miekg/dns) 与 serf/memberlist 会对数据报套接字
 *   开启 IP_PKTINFO / IPV6_RECVPKTINFO 以获取辅助的目的地址投递。starry 的
 *   setsockopt 白名单 (syscall/net/opt.rs) 旧实现对这些 optname 返回
 *   ENOPROTOOPT, 导致 consul agent 在 DNS 服务器刚启动后即被杀掉。
 *
 *   根因修复: 像 Linux 一样接受这些选项 (在单一 loopback 地址上无需 cmsg 投递
 *   即可正常工作); getsockopt 侧报告为 "已接受但禁用" (返回 0 而非 ENOPROTOOPT),
 *   让探测客户端看到一致的值。
 *
 * 断言 (旧内核在 set 断言上 FAIL=ENOPROTOOPT, 修复后全 PASS):
 *   1. IPv4 数据报套接字创建成功。
 *   2. setsockopt(IPPROTO_IP, IP_PKTINFO) 返回 0。
 *   3. getsockopt(IPPROTO_IP, IP_PKTINFO) 返回 0。
 *   4. getsockopt 报告值为 0 (accepted-but-disabled 契约)。
 *   5. IPv6 数据报套接字创建成功。
 *   6. setsockopt(IPPROTO_IPV6, IPV6_RECVPKTINFO) 返回 0。
 *   7. setsockopt(IPPROTO_IPV6, IPV6_PKTINFO) 返回 0。
 *   8. getsockopt(IPPROTO_IPV6, IPV6_RECVPKTINFO) 返回 0。
 */

#ifndef IP_PKTINFO
#define IP_PKTINFO 8
#endif
#ifndef IPV6_RECVPKTINFO
#define IPV6_RECVPKTINFO 49
#endif
#ifndef IPV6_PKTINFO
#define IPV6_PKTINFO 50
#endif

int main(void)
{
    TEST_START("IP_PKTINFO / IPV6_(RECV)PKTINFO setsockopt must be accepted");

    int one = 1;

    int s4 = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(s4 >= 0, "创建 AF_INET SOCK_DGRAM 套接字");

    CHECK(setsockopt(s4, IPPROTO_IP, IP_PKTINFO, &one, sizeof one) == 0,
          "setsockopt(IP_PKTINFO) 返回 0 (非 ENOPROTOOPT)");

    int v4 = -1;
    socklen_t l4 = sizeof v4;
    CHECK(getsockopt(s4, IPPROTO_IP, IP_PKTINFO, &v4, &l4) == 0,
          "getsockopt(IP_PKTINFO) 返回 0");
    CHECK(v4 == 0, "getsockopt(IP_PKTINFO) 报告已接受但禁用 (值=0)");

    int s6 = socket(AF_INET6, SOCK_DGRAM, 0);
    CHECK(s6 >= 0, "创建 AF_INET6 SOCK_DGRAM 套接字");

    CHECK(setsockopt(s6, IPPROTO_IPV6, IPV6_RECVPKTINFO, &one, sizeof one) == 0,
          "setsockopt(IPV6_RECVPKTINFO) 返回 0 (非 ENOPROTOOPT)");
    CHECK(setsockopt(s6, IPPROTO_IPV6, IPV6_PKTINFO, &one, sizeof one) == 0,
          "setsockopt(IPV6_PKTINFO) 返回 0 (非 ENOPROTOOPT)");

    int v6 = -1;
    socklen_t l6 = sizeof v6;
    CHECK(getsockopt(s6, IPPROTO_IPV6, IPV6_RECVPKTINFO, &v6, &l6) == 0,
          "getsockopt(IPV6_RECVPKTINFO) 返回 0");

    // 通用接受分支不得绕过既有协议族校验: AF_INET socket 上的 IPv6 pktinfo sockopt
    // 仍须返回 ENOPROTOOPT, 与 IPV6_TCLASS 等其他 IPv6 选项一致。
    errno = 0;
    CHECK(setsockopt(s4, IPPROTO_IPV6, IPV6_RECVPKTINFO, &one, sizeof one) == -1
              && errno == ENOPROTOOPT,
          "setsockopt(AF_INET, IPV6_RECVPKTINFO) 返回 ENOPROTOOPT");
    int vx = -1;
    socklen_t lx = sizeof vx;
    errno = 0;
    CHECK(getsockopt(s4, IPPROTO_IPV6, IPV6_RECVPKTINFO, &vx, &lx) == -1
              && errno == ENOPROTOOPT,
          "getsockopt(AF_INET, IPV6_RECVPKTINFO) 返回 ENOPROTOOPT");

    if (s4 >= 0)
        close(s4);
    if (s6 >= 0)
        close(s6);

    // 10 = 本文件 CHECK 总数; 少跑 -> FAIL, 堵死假阳性。
    TEST_DONE(10);
}
