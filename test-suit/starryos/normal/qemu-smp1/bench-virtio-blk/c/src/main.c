/*
 * bench-virtio-blk.c -- VirtIO block device performance benchmark
 *
 * Measures sequential read/write throughput at various block sizes,
 * exercising the full I/O path: VFS -> filesystem -> block cache -> virtio-blk.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>

#define BENCH_FILE "/root/virtio_bench_data"
#define TOTAL_SIZE (10 * 1024 * 1024) /* 10 MB */
#define BUF_SIZE (1024 * 1024)        /* 1 MB buffer */

static double get_time_sec(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static int create_test_file(void)
{
    int fd = open(BENCH_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fprintf(stderr, "CREATE FAIL: open %s: %s\n", BENCH_FILE, strerror(errno));
        return -1;
    }

    char buf[BUF_SIZE];
    memset(buf, 0xAB, BUF_SIZE);
    ssize_t remaining = TOTAL_SIZE;
    while (remaining > 0) {
        ssize_t to_write = remaining > BUF_SIZE ? BUF_SIZE : remaining;
        ssize_t n = write(fd, buf, to_write);
        if (n != to_write) {
            fprintf(stderr, "CREATE FAIL: write: %s\n", strerror(errno));
            close(fd);
            return -1;
        }
        remaining -= n;
    }
    fsync(fd);
    close(fd);
    return 0;
}

static double bench_read(int block_size)
{
    int fd = open(BENCH_FILE, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "READ FAIL: open: %s\n", strerror(errno));
        return -1;
    }

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return -1;
    }

    double start = get_time_sec();
    ssize_t total = 0;
    ssize_t n;
    while ((n = read(fd, buf, block_size)) > 0) {
        total += n;
    }
    double end = get_time_sec();

    free(buf);
    close(fd);

    double elapsed = end - start;
    if (elapsed <= 0)
        elapsed = 0.001;
    double throughput = total / elapsed / (1024.0 * 1024.0);
    printf("READ bs=%d total=%zd elapsed=%.3f throughput=%.2f MB/s\n",
           block_size, total, elapsed, throughput);
    return throughput;
}

static double bench_write(int block_size)
{
    int fd = open(BENCH_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fprintf(stderr, "WRITE FAIL: open: %s\n", strerror(errno));
        return -1;
    }

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return -1;
    }
    memset(buf, 0xCD, block_size);

    double start = get_time_sec();
    ssize_t total = 0;
    ssize_t remaining = TOTAL_SIZE;
    while (remaining > 0) {
        ssize_t to_write = remaining > block_size ? block_size : remaining;
        ssize_t n = write(fd, buf, to_write);
        if (n != to_write) {
            fprintf(stderr, "WRITE FAIL: write: %s\n", strerror(errno));
            break;
        }
        total += n;
        remaining -= n;
    }
    fsync(fd);
    double end = get_time_sec();

    free(buf);
    close(fd);

    double elapsed = end - start;
    if (elapsed <= 0)
        elapsed = 0.001;
    double throughput = total / elapsed / (1024.0 * 1024.0);
    printf("WRITE bs=%d total=%zd elapsed=%.3f throughput=%.2f MB/s\n",
           block_size, total, elapsed, throughput);
    return throughput;
}

static void bench_read_random(int block_size, int num_ops)
{
    int fd = open(BENCH_FILE, O_RDONLY);
    if (fd < 0) {
        fprintf(stderr, "RAND READ FAIL: open: %s\n", strerror(errno));
        return;
    }

    char *buf = malloc(block_size);
    if (!buf) {
        close(fd);
        return;
    }

    /* Simple LCG PRNG for reproducible offsets */
    unsigned int seed = 42;
    int max_offset = TOTAL_SIZE - block_size;

    double start = get_time_sec();
    for (int i = 0; i < num_ops; i++) {
        seed = seed * 1103515245 + 12345;
        off_t offset = ((seed >> 16) % (max_offset / block_size)) * block_size;
        lseek(fd, offset, SEEK_SET);
        ssize_t n = read(fd, buf, block_size);
        if (n != block_size) {
            fprintf(stderr, "RAND READ FAIL at offset %ld: %s\n",
                    (long)offset, strerror(errno));
            break;
        }
    }
    double end = get_time_sec();

    free(buf);
    close(fd);

    double elapsed = end - start;
    if (elapsed <= 0)
        elapsed = 0.001;
    double iops = num_ops / elapsed;
    printf("RAND_READ bs=%d ops=%d elapsed=%.3f iops=%.0f latency=%.1f us\n",
           block_size, num_ops, elapsed, iops, elapsed / num_ops * 1e6);
}

int main(void)
{
    printf("=== VIRTIO-BLK BENCHMARK START ===\n");
    printf("total_size=%d bytes (%d MB)\n", TOTAL_SIZE, TOTAL_SIZE / (1024 * 1024));

    /* Phase 1: Create test file (measures write throughput at 1M block size) */
    printf("\n--- Phase 1: File Creation ---\n");
    double t0 = get_time_sec();
    if (create_test_file() < 0) {
        printf("BENCH_FAIL\n");
        return 1;
    }
    double t1 = get_time_sec();
    printf("FILE_CREATE total=%d elapsed=%.3f throughput=%.2f MB/s\n",
           TOTAL_SIZE, t1 - t0, TOTAL_SIZE / (t1 - t0) / (1024.0 * 1024.0));

    /* Phase 2: Sequential reads at different block sizes */
    printf("\n--- Phase 2: Sequential Reads ---\n");
    int block_sizes[] = {512, 4096, 8192, 65536, 262144, 1048576};
    int num_sizes = sizeof(block_sizes) / sizeof(block_sizes[0]);
    double read_results[6];

    for (int i = 0; i < num_sizes; i++) {
        read_results[i] = bench_read(block_sizes[i]);
    }

    /* Phase 3: Sequential writes (only 4K and 1M to keep benchmark fast) */
    printf("\n--- Phase 3: Sequential Writes ---\n");
    int write_sizes[] = {4096, 1048576};
    int num_write_sizes = 2;
    double write_results[2];
    for (int i = 0; i < num_write_sizes; i++) {
        write_results[i] = bench_write(write_sizes[i]);
    }

    /* Phase 4: Random 4K reads */
    printf("\n--- Phase 4: Random 4K Reads ---\n");
    bench_read_random(4096, 1000);
    bench_read_random(4096, 5000);

    /* Summary */
    printf("\n=== BENCHMARK SUMMARY ===\n");
    printf("Sequential Read (MB/s):\n");
    for (int i = 0; i < num_sizes; i++) {
        printf("  bs=%-8d %.2f\n", block_sizes[i], read_results[i]);
    }
    printf("Sequential Write (MB/s):\n");
    for (int i = 0; i < num_write_sizes; i++) {
        printf("  bs=%-8d %.2f\n", write_sizes[i], write_results[i]);
    }

    /* Clean up */
    unlink(BENCH_FILE);

    printf("\nBENCH_PASS\n");
    return 0;
}
