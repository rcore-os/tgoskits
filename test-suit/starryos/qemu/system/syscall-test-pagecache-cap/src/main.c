/*
 * test_pagecache_cap.c — 每文件磁盘页缓存的预分配必须有界 (pagecache-cap 回归).
 *
 * 回归背景 (为什么写这个测例):
 *   每个磁盘文件的页缓存是 `LruCache::new(DISK_PAGE_CACHE_CAP)`，此前
 *   DISK_PAGE_CACHE_CAP=8192。`LruCache::new(8192)` 会 **无视实际缓存了几页**，
 *   为每个文件急切预分配一张约 256KB 的 HashMap 表 (8192 entry 经 hashbrown
 *   负载因子放大到 16384 槽 × 16B ≈ 256KB)。打开数百个不同 inode 的文件
 *   (每个是独立的 CachedFileShared，被全局 GLOBAL_CACHED_FILES 缓存，只要仍被
 *   引用就保留) 会按 ~256KB/文件 堆积内核 RustHeap → 耗尽物理内存 OOM。
 *
 * 触发条件 (源码事实, 忠实复现):
 *   - 仅 **磁盘后端** 文件触发: axfs-ng `in_memory = 文件系统名 == "tmpfs"`,
 *     tmpfs 走 `new_unbounded` 不预分配且不入全局缓存。故本测例文件必须落在
 *     磁盘 rootfs (/root, alpine 镜像 nvme 盘)，绝不能用 /tmp、/dev/shm (tmpfs)。
 *   - 保留引用: FileBackendInner 直接持有 CachedFile (含 Arc<CachedFileShared>
 *     + Location)。因此 mmap 一页后即使 close(fd)，映射仍使 strong_count>1，
 *     那张 256KB 表被保留 (bug 才会累积)。只 open+close 不留引用则文件会在下次
 *     注册时被 evict，累积不出来 —— 所以每个文件都用一个存活的 mmap 钉住。
 *
 * 修复 (kernel os/arceos/modules/axfs-ng/src/file/cache.rs):
 *   DISK_PAGE_CACHE_CAP 由 8192 降到 512 → 每文件预分配约 16KB (512 entry 放大到
 *   1024 槽 × 16B)。实际页帧仍按需分配、不受 cap 影响，此改动只砍空表开销。
 *
 * 判别设计 (镜像 #1164 "过量供给 → 未修复必 OOM → 成功即证明有界" 哲学):
 *   建 N=1400 个不同磁盘小文件；每个 mmap(MAP_SHARED) 一页并触碰以实例化其页
 *   缓存，且 **保留全部映射** 让 CachedFileShared 不被逐出。
 *   x86_64/aarch64/riscv64 -m 512M：未修复 1400×256KB≈358MB，叠加内核+alpine
 *   用户态基线在 512M 上 OOM；修复后 1400×16KB≈22MB，宽裕通过。
 *   loongarch64 -m 2G：两种情况都不 OOM，该 arch 仅作回归执行、不做 OOM 判别。
 *   通过判据 = 实例化全部 N 个页缓存且抽样字节正确、guest 不 OOM。
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>

#define PAGE 4096UL
#define N_FILES 1400              /* 见头部内存预算: 512M 上未修复必 OOM, 修复宽裕 */
#define DIR "/root/pgcachecap"    /* 磁盘后端 rootfs; 绝不能用 tmpfs (/tmp,/dev/shm) */

/* 保留全部映射, 让每个文件的 256KB 预分配表被钉住不被逐出 */
static void *g_maps[N_FILES];

static long read_meminfo_kb(const char *key)
{
    FILE *fp = fopen("/proc/meminfo", "r");
    if (!fp)
        return -1;
    char line[128];
    size_t klen = strlen(key);
    long val = -1;
    while (fgets(line, sizeof line, fp)) {
        if (strncmp(line, key, klen) == 0 && line[klen] == ':') {
            if (sscanf(line + klen + 1, "%ld", &val) != 1)
                val = -1;
            break;
        }
    }
    fclose(fp);
    return val;
}

int main(void)
{
    TEST_START("per-file disk page-cache pre-alloc is bounded (pagecache-cap)");

    if (mkdir(DIR, 0755) != 0 && errno != EEXIST) {
        CHECK(0, "mkdir " DIR " on disk-backed rootfs");
        TEST_DONE();
    }
    CHECK(1, "test directory ready on disk-backed rootfs (not tmpfs)");

    long free_before = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree before: %ld kB\n", free_before);

    int created = 0;
    int open_fail = 0, write_fail = 0, mmap_fail = 0, content_fail = 0;
    char path[96];

    for (int i = 0; i < N_FILES; i++) {
        snprintf(path, sizeof path, DIR "/f%04d.dat", i);

        int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            open_fail++;
            break;
        }

        unsigned char seed = (unsigned char)(i * 7 + 1);
        if (write(fd, &seed, 1) != 1) {
            write_fail++;
            close(fd);
            break;
        }

        /* MAP_SHARED 文件映射经 get_or_create 实例化该文件的 CachedFileShared,
         * 其 LruCache::new(cap) 急切分配哈希表。FileBackend 拥有该 CachedFile,
         * 故下面 close(fd) 后 strong_count 仍 >1 → 表被保留 (未修复即在此累积)。 */
        void *m = mmap(NULL, PAGE, PROT_READ, MAP_SHARED, fd, 0);
        close(fd);
        if (m == MAP_FAILED) {
            mmap_fail++;      /* 未修复: 堆内存耗尽在此暴露 */
            break;
        }
        g_maps[i] = m;

        /* 触碰以缺页装入该文件页, 并抽样校验内容正确 */
        unsigned char got = *(volatile unsigned char *)m;
        if (got != seed)
            content_fail++;

        created++;
    }

    CHECK(open_fail == 0, "opened every backing file (no early failure)");
    CHECK(write_fail == 0, "seeded every backing file with one byte");
    CHECK(mmap_fail == 0,
          "every file page mmap'd (page-cache pre-alloc did not exhaust the kernel heap)");
    CHECK(created == N_FILES,
          "instantiated all 1400 distinct file page caches without OOM");
    CHECK(content_fail == 0, "sampled mapped byte matches backing file content");

    long free_after = read_meminfo_kb("MemFree");
    printf("  INFO | MemFree after instantiating %d files: %ld kB (delta=%ld kB)\n",
           created, free_after, free_before - free_after);

    /* 每文件固定开销必须有界: 空表预分配随 cap 线性变化, 是这个 bug 的直接信号。
     * 修复后 1400 个文件仅约 30MB(每文件 ~16KB 预分配 + 一个映射页); 未修复时每文件
     * ~256KB 空哈希表 → 约 360MB。断言 delta 有界比"是否 OOM"更稳健: 即便 512M 上
     * 未修复恰好没触发 OOM, 被空表实际吃掉的内存也会让此断言失败。100MB 阈值给
     * 修复态(~31MB)留约 3 倍余量, 与未修复态(~360MB)干净分离。 */
    if (free_before > 0 && free_after > 0) {
        CHECK(free_before - free_after < 100L * 1024,
              "per-file page-cache overhead bounded (< 100MB for 1400 files)");
    } else {
        printf("  INFO | MemFree unavailable; skipping delta-bound assertion\n");
    }

    /* 清理: 释放映射并删除磁盘文件 (rootfs 在同一 boot 内跨测例持久) */
    for (int i = 0; i < N_FILES; i++) {
        if (g_maps[i])
            munmap(g_maps[i], PAGE);
    }
    for (int i = 0; i < N_FILES; i++) {
        snprintf(path, sizeof path, DIR "/f%04d.dat", i);
        unlink(path);
    }
    rmdir(DIR);

    TEST_DONE();
}
