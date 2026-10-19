#define _GNU_SOURCE
#include "test_framework.h"
#include <fcntl.h>
#include <stdint.h>
#include <unistd.h>

/*
 * /proc/diskstats 块设备 I/O 计数回归测试。
 *
 * 通过 ext4 根设备上的实际写入与 fsync 检查完成计数和 flush 接线。
 * Linux v6.12 block/genhd.c: diskstats_show 的每行含设备标识三列及
 * 17 个统计字段；扇区固定 512 字节，时间向下截断为毫秒。
 *
 * 本用例检查 20 列均可解析、当前未实现的 merge/discard 为零，以及实际
 * 读写与 flush 计数。精确时间换算和字段顺序由生产 formatter 的 axtest
 * 使用固定快照检查，不对 QEMU I/O 延迟设下限。
 */

#define DISKSTATS_PATH "/proc/diskstats"

enum {
    READS = 3,
    READS_MERGED = 4,
    SECTORS_READ = 5,
    WRITES = 7,
    WRITES_MERGED = 8,
    SECTORS_WRITTEN = 9,
    DISCARDS = 14,
    DISCARDS_MERGED = 15,
    SECTORS_DISCARDED = 16,
    DISCARD_MS = 17,
    FLUSHES = 18,
    DISKSTATS_FIELDS = 20,
};

/*
 * 按原始列号保存数值，跳过设备名列。返回字段数；无设备行为 -1，
 * 文件打不开为 -2，非十进制无符号数或溢出为 -3。
 */
static int read_nvme(unsigned long long out[DISKSTATS_FIELDS])
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
        if (n >= 3 && strcmp(toks[2], "nvme0n1") == 0) {
            nfields = n;
            if (n == DISKSTATS_FIELDS) {
                for (int i = 0; i < n; i++) {
                    if (i == 2)
                        continue;
                    char *end;
                    errno = 0;
                    out[i] = strtoull(toks[i], &end, 10);
                    if (toks[i][0] < '0' || toks[i][0] > '9' || *end != '\0' || errno != 0) {
                        nfields = -3;
                        break;
                    }
                }
            }
            break;
        }
    }
    fclose(f);
    return nfields;
}

static int unsupported_counters_are_zero(const unsigned long long stats[DISKSTATS_FIELDS])
{
    return stats[READS_MERGED] == 0 && stats[WRITES_MERGED] == 0 &&
           stats[DISCARDS] == 0 && stats[DISCARDS_MERGED] == 0 &&
           stats[SECTORS_DISCARDED] == 0 && stats[DISCARD_MS] == 0;
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

    unsigned long long s0[DISKSTATS_FIELDS] = {0};
    int nf = read_nvme(s0);

    CHECK(nf != -2, "/proc/diskstats 可打开");
    CHECK(nf != -1 && nf != -2, "/proc/diskstats 含 nvme0n1 设备行");
    snprintf(msg, sizeof msg, "nvme0n1 行完整解析 20 列 (实际 %d)", nf);
    CHECK(nf == DISKSTATS_FIELDS, msg);
    CHECK(unsupported_counters_are_zero(s0), "未实现的 merge/discard 计数为零");

    /* 此程序运行前，挂载 ext4 与读取程序文件已完成真实块读。*/
    snprintf(msg, sizeof msg, "nvme0n1 reads > 0 (启动块读, 实际 %llu)", s0[READS]);
    CHECK(s0[READS] > 0, msg);
    snprintf(msg, sizeof msg, "nvme0n1 sectors_read > 0 (实际 %llu)", s0[SECTORS_READ]);
    CHECK(s0[SECTORS_READ] > 0, msg);

    /* 写文件 + fsync 强制回写 -> writes / sectors_written 必须真增长。*/
    const char *path = pick_probe_path();
    CHECK(path != NULL, "找到可写 ext4 探测路径");
    if (path) {
        int ok = 1;
        for (int i = 0; i < 3; i++)
            ok &= (write_and_sync(path, 512 * 1024) == 0);
        CHECK(ok, "写 3×512KiB 文件 + fsync 成功");

        unsigned long long s1[DISKSTATS_FIELDS] = {0};
        int nf1 = read_nvme(s1);
        CHECK(nf1 == DISKSTATS_FIELDS, "写后再读 nvme0n1 行完整解析 20 列");
        CHECK(unsupported_counters_are_zero(s1), "写后 merge/discard 保留列仍为零");

        snprintf(msg, sizeof msg, "写+fsync 后 writes 增长 (%llu -> %llu)", s0[WRITES], s1[WRITES]);
        CHECK(s1[WRITES] > s0[WRITES], msg);
        snprintf(msg, sizeof msg, "写+fsync 后 sectors_written 增长 (%llu -> %llu)",
                 s0[SECTORS_WRITTEN], s1[SECTORS_WRITTEN]);
        CHECK(s1[SECTORS_WRITTEN] > s0[SECTORS_WRITTEN], msg);
        snprintf(msg, sizeof msg, "写+fsync 后 flushes 增长 (%llu -> %llu)",
                 s0[FLUSHES], s1[FLUSHES]);
        CHECK(s1[FLUSHES] > s0[FLUSHES], msg);

        CHECK(s1[READS] >= s0[READS], "reads 单调不减");
        CHECK(s1[SECTORS_READ] >= s0[SECTORS_READ], "sectors_read 单调不减");
        unlink(path);
    }

    // 15 = 本文件 CHECK 总数; 少跑(如探测路径不可写跳过写块) -> FAIL。
    TEST_DONE(15);
}
