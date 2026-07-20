#define _GNU_SOURCE
#include "test_framework.h"
#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdint.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

/*
 * /proc/net/dev + /proc/mounts 真数据回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   glances 的 NETWORK 区块解析 /proc/net/dev 的每接口 rx/tx 字节与包数;
 *   FILE SYS 区块与 node_exporter filesystem collector 解析 /proc/mounts。
 *   starry 此前 /proc/net/dev 对每接口输出全 0 (假数据), /proc/mounts 只有
 *   一行 proc 挂载 (缺根文件系统)。现内核在网络栈收发中心路径 (router
 *   enqueue_tx / device_rx_worker / loopback inject) 累计真实 rx/tx 计数,
 *   并让 /proc/mounts 输出根 fs + 启动伪文件系统挂载表。
 *
 * man 5 proc:
 *   /proc/net/dev 每接口行: "<iface>: rx_bytes rx_packets rx_errs rx_drop
 *   rx_fifo rx_frame rx_compressed rx_multicast tx_bytes tx_packets tx_errs
 *   tx_drop tx_fifo tx_colls tx_carrier tx_compressed" (16 数值列)。
 *   /proc/mounts 每行: "device mountpoint fstype options dump pass" (6 列)。
 *
 * 断言:
 *   1. /proc/net/dev 含 lo 回环接口行, 数值列数 >= 16。
 *   2. 通过 UDP 回环自发流量后, lo 的 rx/tx 字节与包数真增长 (证明网络计数
 *      已接线且非硬编码假值)。
 *   3. /proc/mounts 含根挂载 ("/") 行, 恰好 6 字段, fstype 非空。
 */

struct netdev {
    unsigned long long rx_bytes;
    unsigned long long rx_packets;
    unsigned long long tx_bytes;
    unsigned long long tx_packets;
};

/*
 * 解析 /proc/net/dev 的 lo 行, 返回该行数值列数; 列数 >= 16 时填 out。
 * 无 lo 行返回 -1, 文件打不开返回 -2。
 */
static int read_lo(struct netdev *out)
{
    FILE *f = fopen("/proc/net/dev", "r");
    if (!f)
        return -2;
    char line[512];
    int rc = -1;
    while (fgets(line, sizeof line, f)) {
        char *colon = strchr(line, ':');
        if (!colon) /* 头两行没有接口冒号 */
            continue;
        char *name = line;
        while (*name == ' ' || *name == '\t')
            name++;
        *colon = '\0';
        char *end = colon;
        while (end > name && (end[-1] == ' ' || end[-1] == '\t'))
            *--end = '\0';
        if (strcmp(name, "lo") != 0)
            continue;
        unsigned long long v[20];
        int n = 0;
        for (char *p = strtok(colon + 1, " \t\n"); p && n < 20; p = strtok(NULL, " \t\n"))
            v[n++] = strtoull(p, NULL, 10);
        if (n >= 16) {
            out->rx_bytes = v[0];
            out->rx_packets = v[1];
            out->tx_bytes = v[8];
            out->tx_packets = v[9];
        }
        rc = n;
        break;
    }
    fclose(f);
    return rc;
}

/* 通过 UDP 向自己 (127.0.0.1) 发送若干数据报, 产生回环流量。成功返回 0。*/
static int gen_loopback_traffic(void)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return -1;

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof addr);
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = 0;
    if (bind(fd, (struct sockaddr *)&addr, sizeof addr) < 0) {
        close(fd);
        return -1;
    }

    socklen_t alen = sizeof addr;
    if (getsockname(fd, (struct sockaddr *)&addr, &alen) < 0) {
        close(fd);
        return -1;
    }
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    struct timeval tv = {.tv_sec = 0, .tv_usec = 200 * 1000};
    (void)setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);

    char buf[256];
    memset(buf, 0x5a, sizeof buf);
    int sent = 0;
    for (int i = 0; i < 16; i++) {
        if (sendto(fd, buf, sizeof buf, 0, (struct sockaddr *)&addr, sizeof addr)
            == (ssize_t)sizeof buf)
            sent++;
        char rbuf[256];
        (void)recvfrom(fd, rbuf, sizeof rbuf, 0, NULL, NULL);
    }
    close(fd);
    return sent > 0 ? 0 : -1;
}

/* 校验 /proc/mounts 的根挂载 ("/") 行有 6 字段且 fstype 非空。*/
static void check_mounts(void)
{
    FILE *f = fopen("/proc/mounts", "r");
    CHECK(f != NULL, "/proc/mounts 可打开");
    if (!f)
        return;
    char line[512];
    int found_root = 0;
    int root_fields = 0;
    char root_fstype[64] = "";
    while (fgets(line, sizeof line, f)) {
        char *toks[8];
        int nt = 0;
        for (char *p = strtok(line, " \t\n"); p && nt < 8; p = strtok(NULL, " \t\n"))
            toks[nt++] = p;
        if (nt >= 2 && strcmp(toks[1], "/") == 0) {
            found_root = 1;
            root_fields = nt;
            if (nt >= 3) {
                strncpy(root_fstype, toks[2], sizeof root_fstype - 1);
                root_fstype[sizeof root_fstype - 1] = '\0';
            }
            break;
        }
    }
    fclose(f);

    char msg[160];
    CHECK(found_root, "/proc/mounts 含根挂载 (mountpoint \"/\") 行");
    snprintf(msg, sizeof msg, "根挂载行恰好 6 字段 (实际 %d)", root_fields);
    CHECK(root_fields == 6, msg);
    snprintf(msg, sizeof msg, "根挂载行 fstype 非空 (=%s)", root_fstype);
    CHECK(root_fstype[0] != '\0', msg);
}

int main(void)
{
    TEST_START("/proc/net/dev + /proc/mounts");
    char msg[192];

    struct netdev lo0 = {0, 0, 0, 0};
    int n = read_lo(&lo0);
    CHECK(n != -2, "/proc/net/dev 可打开");
    CHECK(n >= 0, "/proc/net/dev 含 lo 回环接口行");
    snprintf(msg, sizeof msg, "lo 行数值列数 >= 16 (实际 %d)", n);
    CHECK(n >= 16, msg);

    /* 回环发流量 -> lo rx/tx 计数必须真增长。net poll 任务异步派发, 故重试。*/
    struct netdev lo1 = lo0;
    for (int attempt = 0; attempt < 5; attempt++) {
        gen_loopback_traffic();
        usleep(100 * 1000);
        if (read_lo(&lo1) >= 16 && lo1.tx_bytes > lo0.tx_bytes && lo1.rx_bytes > lo0.rx_bytes)
            break;
    }
    snprintf(msg, sizeof msg, "lo tx_bytes 增长 (%llu -> %llu)", lo0.tx_bytes, lo1.tx_bytes);
    CHECK(lo1.tx_bytes > lo0.tx_bytes, msg);
    snprintf(msg, sizeof msg, "lo rx_bytes 增长 (%llu -> %llu)", lo0.rx_bytes, lo1.rx_bytes);
    CHECK(lo1.rx_bytes > lo0.rx_bytes, msg);
    snprintf(msg, sizeof msg, "lo tx_packets 增长 (%llu -> %llu)", lo0.tx_packets, lo1.tx_packets);
    CHECK(lo1.tx_packets > lo0.tx_packets, msg);
    CHECK(lo1.rx_packets > lo0.rx_packets, "lo rx_packets 增长");

    check_mounts();

    // 11 = 本文件 CHECK 总数(net/dev 7 + mounts 3 + fopen 1); 少跑(如
    // /proc/mounts 打不开而 check_mounts 早退) -> FAIL。
    TEST_DONE(11);
}
