#define _GNU_SOURCE
#include "test_framework.h"
#include <net/if.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

/*
 * SIOCETHTOOL ioctl 回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   glances 的 NETWORK 区块通过 psutil 的 net_if_stats() 采集每个接口的状态,
 *   其中 speed/duplex 用 SIOCETHTOOL (ETHTOOL_GSET) 查询 PHY 链路。starry 没有
 *   模拟 PHY, 此前 SIOCETHTOOL 落到 socket ioctl 的默认分支返回 ENOTTY。psutil
 *   只把 EOPNOTSUPP 当作"无 ethtool"优雅降级, 遇到任何其它 errno 会 abort 整个
 *   interface-status 探测 -> glances NETWORK 区块渲染崩溃。内核现在像虚拟网卡
 *   (loopback / tun/tap) 一样对 SIOCETHTOOL 返回 EOPNOTSUPP。
 *
 * Linux 参照 (fresh host, kernel 6.6):
 *   ioctl(AF_INET sock, SIOCETHTOOL, {ifr_name="lo"}) => -1 errno=EOPNOTSUPP(95);
 *   loopback 无 ethtool_ops, dev_ethtool() 返回 -EOPNOTSUPP。
 *
 * 断言:
 *   1. AF_INET SOCK_DGRAM 套接字创建成功。
 *   2. 对 "lo" 发 SIOCETHTOOL 返回 -1 (查询被拒)。
 *   3. errno == EOPNOTSUPP (优雅"无 ethtool"; 修复前是 ENOTTY, 会让 psutil abort)。
 */

/* linux-raw-sys / <linux/sockios.h> 常量, arch 无关; 避免依赖 <linux/ethtool.h>。*/
#ifndef SIOCETHTOOL
#define SIOCETHTOOL 0x8946
#endif
#define ETHTOOL_GSET 0x00000001u

int main(void)
{
    TEST_START("SIOCETHTOOL returns EOPNOTSUPP on virtual NIC");
    char msg[160];

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd >= 0, "socket(AF_INET, SOCK_DGRAM) 创建成功");
    if (fd < 0) {
        TEST_DONE(3);
    }

    /*
     * ifr_data 指向一个 ethtool 命令字 (ETHTOOL_GSET)。内核在读取 ifr_data 之前
     * 就返回 EOPNOTSUPP, 但仍按 psutil 的真实调用形态填好请求。
     */
    unsigned int ethcmd = ETHTOOL_GSET;
    struct ifreq ifr;
    memset(&ifr, 0, sizeof ifr);
    strncpy(ifr.ifr_name, "lo", IFNAMSIZ - 1);
    ifr.ifr_data = (void *)&ethcmd;

    errno = 0;
    int ret = ioctl(fd, SIOCETHTOOL, &ifr);
    int saved_errno = errno;
    close(fd);

    snprintf(msg, sizeof msg, "SIOCETHTOOL 查询被拒 ret == -1 (实际 %d)", ret);
    CHECK(ret == -1, msg);

    snprintf(msg, sizeof msg,
             "errno == EOPNOTSUPP(%d) 优雅降级 (实际 %d %s; 修复前为 ENOTTY)",
             EOPNOTSUPP, saved_errno, strerror(saved_errno));
    CHECK(saved_errno == EOPNOTSUPP, msg);

    /*
     * 未知接口名: 内核应先解析接口再判 ethtool, 因此返回 ENODEV 而非无条件
     * EOPNOTSUPP。固定与 Linux 一致的错误优先级, 也与本分支其它 SIOC*IF* 一致。
     */
    int fd2 = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd2 >= 0, "socket(AF_INET, SOCK_DGRAM) 二次创建成功");

    struct ifreq bad;
    memset(&bad, 0, sizeof bad);
    strncpy(bad.ifr_name, "nonexist99", IFNAMSIZ - 1);
    bad.ifr_data = (void *)&ethcmd;

    errno = 0;
    int bad_ret = ioctl(fd2, SIOCETHTOOL, &bad);
    int bad_errno = errno;
    close(fd2);

    snprintf(msg, sizeof msg, "SIOCETHTOOL 未知接口被拒 ret == -1 (实际 %d)", bad_ret);
    CHECK(bad_ret == -1, msg);

    snprintf(msg, sizeof msg,
             "errno == ENODEV(%d) 接口校验先于 EOPNOTSUPP (实际 %d %s)", ENODEV,
             bad_errno, strerror(bad_errno));
    CHECK(bad_errno == ENODEV, msg);

    /*
     * 合法接口但 ifr_data 是坏指针: 内核读取 ethtool 命令字时先 fault, 返回
     * EFAULT 而非 EOPNOTSUPP。错误优先级为 ENODEV -> EFAULT -> EOPNOTSUPP。
     */
    int fd3 = socket(AF_INET, SOCK_DGRAM, 0);
    CHECK(fd3 >= 0, "socket(AF_INET, SOCK_DGRAM) 三次创建成功");

    struct ifreq faulty;
    memset(&faulty, 0, sizeof faulty);
    strncpy(faulty.ifr_name, "lo", IFNAMSIZ - 1);
    faulty.ifr_data = (void *)1;

    errno = 0;
    int fault_ret = ioctl(fd3, SIOCETHTOOL, &faulty);
    int fault_errno = errno;
    close(fd3);

    snprintf(msg, sizeof msg, "SIOCETHTOOL 坏 ifr_data 被拒 ret == -1 (实际 %d)", fault_ret);
    CHECK(fault_ret == -1, msg);

    snprintf(msg, sizeof msg,
             "errno == EFAULT(%d) 坏指针先于 EOPNOTSUPP (实际 %d %s)", EFAULT,
             fault_errno, strerror(fault_errno));
    CHECK(fault_errno == EFAULT, msg);

    // 9 = 本文件 CHECK 总数。
    TEST_DONE(9);
}
