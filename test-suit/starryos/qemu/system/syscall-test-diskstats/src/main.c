#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <unistd.h>

/*
 * /proc/diskstats 块设备 I/O 计数回归测试。
 *
 * 触发背景 (为什么写这个测例):
 *   glances 的 DISK I/O 区块解析 /proc/diskstats 的每设备读写计数与扇区数。
 *   starry 此前没有 /proc/diskstats -> 该区块无法渲染。现内核在块 I/O 中心
 *   路径 (axfs-ng block runtime submit_io) 累计真实读写计数并导出该文件。
 *
 * man 5 proc §/proc/diskstats:
 *   每行 14 列: "major minor name reads reads_merged sectors_read ms_read
 *   writes writes_merged sectors_written ms_write ios_in_progress ms_io
 *   weighted_ms"。扇区固定 512 字节。
 *
 * 断言:
 *   1. /proc/diskstats 存在, 含 vda 设备行, 字段数 >= 14。
 *   2. reads / sectors_read > 0: 启动挂载 ext4 已产生真实块读, 证明读计数已接线
 *      且非硬编码假值。
 *   3. 写文件 + fsync 强制回写后, writes / sectors_written 真增长; reads /
 *      sectors_read 单调不减。
 */

#define DISKSTATS_PATH "/proc/diskstats"

/*
 * diskstats 令牌 0-based 下标 (Linux 14 列布局):
 *   name=2 reads=3 sectors_read=5 writes=7 sectors_written=9
 * 读取 vda 行, 返回该行令牌数; 令牌数 >= 14 时把
 * [reads, sectors_read, writes, sectors_written] 填入 out。
 * 无 vda 行返回 -1, 文件打不开返回 -2。
 */
static int read_vda(unsigned long long out[4])
{
    FILE *f = fopen(DISKSTATS_PATH, "r");
    if (!f)
        return -2;
    char line[512];
    int nfields = -1;
    while (fgets(line, sizeof line, f)) {
        char *toks[32];
        int n = 0;
        for (char *p = strtok(line, " \t\n"); p && n < 32; p = strtok(NULL, " \t\n"))
            toks[n++] = p;
        if (n >= 3 && strcmp(toks[2], "vda") == 0) {
            nfields = n;
            if (n >= 14) {
                out[0] = strtoull(toks[3], NULL, 10);
                out[1] = strtoull(toks[5], NULL, 10);
                out[2] = strtoull(toks[7], NULL, 10);
                out[3] = strtoull(toks[9], NULL, 10);
            }
            break;
        }
    }
    fclose(f);
    return nfields;
}

/* 在 ext4 根文件系统上写 bytes 字节并 fsync, 强制脏页回写到块设备。成功返回 0。*/
static int write_and_sync(const char *path, size_t bytes)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0)
        return -1;
    char buf[4096];
    memset(buf, 0xa5, sizeof buf);
    size_t left = bytes;
    while (left > 0) {
        size_t chunk = left < sizeof buf ? left : sizeof buf;
        ssize_t w = write(fd, buf, chunk);
        if (w <= 0) {
            close(fd);
            return -1;
        }
        left -= (size_t)w;
    }
    int rc = fsync(fd);
    close(fd);
    return rc;
}

/* 选一个 ext4 根文件系统上可写的探测路径 (避开可能是 tmpfs 的 /tmp)。*/
static const char *pick_probe_path(void)
{
    static const char *cands[] = {
        "/root/.diskstats_probe.tmp",
        "/.diskstats_probe.tmp",
        "/usr/.diskstats_probe.tmp",
    };
    for (size_t i = 0; i < sizeof cands / sizeof cands[0]; i++) {
        int fd = open(cands[i], O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd >= 0) {
            close(fd);
            unlink(cands[i]);
            return cands[i];
        }
    }
    return NULL;
}

int main(void)
{
    TEST_START("/proc/diskstats block I/O accounting");
    char msg[192];

    unsigned long long s0[4] = {0, 0, 0, 0};
    int nf = read_vda(s0);

    CHECK(nf != -2, "/proc/diskstats 可打开");
    CHECK(nf >= 0, "/proc/diskstats 含 vda 设备行");
    snprintf(msg, sizeof msg, "vda 行字段数 >= 14 (实际 %d)", nf);
    CHECK(nf >= 14, msg);

    /* 启动挂载 ext4 必然产生块读 -> 证明 read 计数已接线且非造假。*/
    snprintf(msg, sizeof msg, "vda reads > 0 (启动块读, 实际 %llu)", s0[0]);
    CHECK(s0[0] > 0, msg);
    snprintf(msg, sizeof msg, "vda sectors_read > 0 (实际 %llu)", s0[1]);
    CHECK(s0[1] > 0, msg);

    /* 写文件 + fsync 强制回写 -> writes / sectors_written 必须真增长。*/
    const char *path = pick_probe_path();
    CHECK(path != NULL, "找到可写 ext4 探测路径");
    if (path) {
        int ok = 1;
        for (int i = 0; i < 3; i++)
            ok &= (write_and_sync(path, 512 * 1024) == 0);
        sync();
        CHECK(ok, "写 3×512KiB 文件 + fsync 成功");
        unlink(path);

        unsigned long long s1[4] = {0, 0, 0, 0};
        int nf1 = read_vda(s1);
        CHECK(nf1 >= 14, "写后再读 /proc/diskstats vda 行完整");

        snprintf(msg, sizeof msg, "写+fsync 后 writes 增长 (%llu -> %llu)", s0[2], s1[2]);
        CHECK(s1[2] > s0[2], msg);
        snprintf(msg, sizeof msg, "写+fsync 后 sectors_written 增长 (%llu -> %llu)", s0[3], s1[3]);
        CHECK(s1[3] > s0[3], msg);

        CHECK(s1[0] >= s0[0], "reads 单调不减");
        CHECK(s1[1] >= s0[1], "sectors_read 单调不减");
    }

    // 12 = 本文件 CHECK 总数; 少跑(如探测路径不可写跳过写块) -> FAIL。
    TEST_DONE(12);
}
