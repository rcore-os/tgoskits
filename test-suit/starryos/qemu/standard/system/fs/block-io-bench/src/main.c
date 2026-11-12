#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define BENCH_PATH_PATTERN "/root/block-io-bench-%d.dat"
#define BENCH_BYTES (1024U * 1024U)
#define BENCH_BLOCK_BYTES (4U * 1024U)
#define BENCH_ROUNDS 3

static int64_t now_us(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (int64_t)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

static void fill_buffer(uint8_t *buf, size_t len, int round, size_t offset) {
    for (size_t i = 0; i < len; i++) {
        buf[i] = (uint8_t)(((offset + i) * 131U + (unsigned)round * 17U) & 0xffU);
    }
}

static uint64_t checksum_buffer(const uint8_t *buf, size_t len) {
    uint64_t sum = 0;
    for (size_t i = 0; i < len; i++) {
        sum += buf[i];
    }
    return sum;
}

static void checked_write_all(int fd, const uint8_t *buf, size_t len) {
    size_t done = 0;
    while (done < len) {
        ssize_t ret = write(fd, buf + done, len - done);
        if (ret < 0) {
            perror("write");
            exit(1);
        }
        if (ret == 0) {
            fprintf(stderr, "write returned zero\n");
            exit(1);
        }
        done += (size_t)ret;
    }
}

static void checked_read_all(int fd, uint8_t *buf, size_t len) {
    size_t done = 0;
    while (done < len) {
        ssize_t ret = read(fd, buf + done, len - done);
        if (ret < 0) {
            perror("read");
            exit(1);
        }
        if (ret == 0) {
            fprintf(stderr, "short read: %zu/%zu\n", done, len);
            exit(1);
        }
        done += (size_t)ret;
    }
}

static int compare_u64(const void *a, const void *b) {
    const uint64_t lhs = *(const uint64_t *)a;
    const uint64_t rhs = *(const uint64_t *)b;
    return (lhs > rhs) - (lhs < rhs);
}

static uint64_t median_us(uint64_t values[BENCH_ROUNDS]) {
    qsort(values, BENCH_ROUNDS, sizeof(values[0]), compare_u64);
    return values[BENCH_ROUNDS / 2];
}

static uint64_t mib_s_x100(uint64_t bytes, uint64_t elapsed_us) {
    if (elapsed_us == 0) {
        return 0;
    }
    return bytes * 100000000ULL / (1024ULL * 1024ULL) / elapsed_us;
}

static void print_result(const char *op, uint64_t bytes, uint64_t elapsed_us, uint64_t checksum) {
    const uint64_t speed = mib_s_x100(bytes, elapsed_us);
    printf(
        "BLOCK_BENCH_RESULT op=%s bytes=%llu elapsed_us=%llu mib_s=%llu.%02llu checksum=%llu\n",
        op,
        (unsigned long long)bytes,
        (unsigned long long)elapsed_us,
        (unsigned long long)(speed / 100),
        (unsigned long long)(speed % 100),
        (unsigned long long)checksum);
    fflush(stdout);
}

static void print_progress(const char *phase, int round) {
    printf("BLOCK_BENCH_PROGRESS phase=%s round=%d\n", phase, round);
    fflush(stdout);
}

int main(void) {
    uint8_t *buf = malloc(BENCH_BLOCK_BYTES);
    if (buf == NULL) {
        perror("malloc");
        return 1;
    }

    uint64_t write_us[BENCH_ROUNDS];
    uint64_t read_us[BENCH_ROUNDS];
    uint64_t checksum = 0;

    for (int round = 0; round < BENCH_ROUNDS; round++) {
        char path[64];
        int written = snprintf(path, sizeof(path), BENCH_PATH_PATTERN, round);
        if (written < 0 || (size_t)written >= sizeof(path)) {
            fprintf(stderr, "bench path too long\n");
            return 1;
        }

        int fd = open(path, O_RDWR);
        if (fd < 0) {
            perror("open write");
            return 1;
        }
        if (lseek(fd, 0, SEEK_SET) < 0) {
            perror("lseek write");
            return 1;
        }

        print_progress("write", round);
        int64_t start = now_us();
        for (size_t done = 0; done < BENCH_BYTES; done += BENCH_BLOCK_BYTES) {
            fill_buffer(buf, BENCH_BLOCK_BYTES, round, done);
            checked_write_all(fd, buf, BENCH_BLOCK_BYTES);
        }
        print_progress("fsync", round);
        if (fsync(fd) != 0) {
            perror("fsync write");
            return 1;
        }
        int64_t end = now_us();
        if (close(fd) != 0) {
            perror("close write");
            return 1;
        }
        write_us[round] = (uint64_t)(end - start);

        fd = open(path, O_RDONLY);
        if (fd < 0) {
            perror("open read");
            return 1;
        }
        if (lseek(fd, 0, SEEK_SET) < 0) {
            perror("lseek read");
            return 1;
        }

        print_progress("read", round);
        start = now_us();
        for (size_t done = 0; done < BENCH_BYTES; done += BENCH_BLOCK_BYTES) {
            checked_read_all(fd, buf, BENCH_BLOCK_BYTES);
            checksum += checksum_buffer(buf, BENCH_BLOCK_BYTES);
        }
        end = now_us();
        if (close(fd) != 0) {
            perror("close read");
            return 1;
        }
        read_us[round] = (uint64_t)(end - start);

    }

    const uint64_t median_write = median_us(write_us);
    const uint64_t median_read = median_us(read_us);
    print_result("write", BENCH_BYTES, median_write, checksum);
    print_result("read", BENCH_BYTES, median_read, checksum);

    free(buf);
    return 0;
}
