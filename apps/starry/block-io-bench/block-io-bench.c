#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define DEFAULT_BASE_PATH "/root/block-io-bench"
#define DEFAULT_BYTES (4ULL * 1024ULL * 1024ULL)
#define DEFAULT_BLOCK_BYTES (4ULL * 1024ULL)
#define DEFAULT_ROUNDS 5U
#define MAX_ROUNDS 32U

struct bench_config {
    const char *base_path;
    uint64_t bytes;
    size_t block_bytes;
    unsigned rounds;
};

struct bench_sample {
    uint64_t elapsed_us;
};

static void usage(const char *program) {
    fprintf(stderr,
            "usage: %s [--path BASE] [--bytes BYTES] [--block-bytes BYTES] [--rounds N]\n",
            program);
}

static uint64_t parse_u64(const char *name, const char *value, uint64_t min) {
    char *end = NULL;
    errno = 0;
    unsigned long long parsed = strtoull(value, &end, 0);
    if (errno != 0 || end == value || *end != '\0' || parsed < min) {
        fprintf(stderr, "invalid %s: %s\n", name, value);
        exit(2);
    }
    return (uint64_t)parsed;
}

static struct bench_config parse_args(int argc, char **argv) {
    struct bench_config config = {
        .base_path = DEFAULT_BASE_PATH,
        .bytes = DEFAULT_BYTES,
        .block_bytes = DEFAULT_BLOCK_BYTES,
        .rounds = DEFAULT_ROUNDS,
    };

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--path") == 0 && i + 1 < argc) {
            config.base_path = argv[++i];
        } else if (strcmp(argv[i], "--bytes") == 0 && i + 1 < argc) {
            config.bytes = parse_u64("bytes", argv[++i], 1);
        } else if (strcmp(argv[i], "--block-bytes") == 0 && i + 1 < argc) {
            config.block_bytes = (size_t)parse_u64("block-bytes", argv[++i], 1);
        } else if (strcmp(argv[i], "--rounds") == 0 && i + 1 < argc) {
            config.rounds = (unsigned)parse_u64("rounds", argv[++i], 1);
            if (config.rounds > MAX_ROUNDS) {
                fprintf(stderr, "rounds must be <= %u\n", MAX_ROUNDS);
                exit(2);
            }
        } else if (strcmp(argv[i], "--help") == 0) {
            usage(argv[0]);
            exit(0);
        } else {
            usage(argv[0]);
            exit(2);
        }
    }

    if (config.bytes % config.block_bytes != 0) {
        fprintf(stderr, "bytes must be a multiple of block-bytes\n");
        exit(2);
    }
    return config;
}

static uint64_t now_us(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)ts.tv_nsec / 1000ULL;
}

static void fill_buffer(uint8_t *buf, size_t len, unsigned round, uint64_t offset) {
    for (size_t i = 0; i < len; i++) {
        buf[i] = (uint8_t)(((offset + i) * 131ULL + (uint64_t)round * 17ULL) & 0xffU);
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

static int compare_samples(const void *a, const void *b) {
    const struct bench_sample *lhs = a;
    const struct bench_sample *rhs = b;
    return (lhs->elapsed_us > rhs->elapsed_us) - (lhs->elapsed_us < rhs->elapsed_us);
}

static struct bench_sample median_sample(const struct bench_sample *samples, unsigned rounds) {
    struct bench_sample sorted[MAX_ROUNDS];
    memcpy(sorted, samples, rounds * sizeof(sorted[0]));
    qsort(sorted, rounds, sizeof(sorted[0]), compare_samples);
    return sorted[rounds / 2];
}

static uint64_t mib_s_x100(uint64_t bytes, uint64_t elapsed_us) {
    if (elapsed_us == 0) {
        return 0;
    }
    return bytes * 100000000ULL / (1024ULL * 1024ULL) / elapsed_us;
}

static void print_speed(const char *prefix, const char *op, unsigned round, uint64_t bytes,
                        uint64_t elapsed_us, uint64_t checksum) {
    uint64_t speed = mib_s_x100(bytes, elapsed_us);
    printf("%s op=%s round=%u bytes=%llu elapsed_us=%llu mib_s=%llu.%02llu checksum=%llu\n",
           prefix, op, round, (unsigned long long)bytes, (unsigned long long)elapsed_us,
           (unsigned long long)(speed / 100), (unsigned long long)(speed % 100),
           (unsigned long long)checksum);
    fflush(stdout);
}

static void bench_path(char *path, size_t len, const char *base_path, unsigned round) {
    int written = snprintf(path, len, "%s-%u.dat", base_path, round);
    if (written < 0 || (size_t)written >= len) {
        fprintf(stderr, "bench path too long: %s\n", base_path);
        exit(1);
    }
}

static uint64_t run_write_round(const struct bench_config *config, uint8_t *buf, unsigned round,
                                const char *path) {
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0644);
    if (fd < 0) {
        perror("open write");
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=write round=%u path=%s\n", round, path);
    fflush(stdout);

    uint64_t start = now_us();
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        fill_buffer(buf, config->block_bytes, round, done);
        checked_write_all(fd, buf, config->block_bytes);
    }
    printf("BLOCK_BENCH_PROGRESS phase=fsync round=%u path=%s\n", round, path);
    fflush(stdout);
    if (fsync(fd) != 0) {
        perror("fsync");
        exit(1);
    }
    uint64_t end = now_us();

    if (close(fd) != 0) {
        perror("close write");
        exit(1);
    }
    return end - start;
}

static uint64_t run_read_round(const struct bench_config *config, uint8_t *buf, unsigned round,
                               const char *path, uint64_t *checksum) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open read");
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=read round=%u path=%s\n", round, path);
    fflush(stdout);

    uint64_t start = now_us();
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        checked_read_all(fd, buf, config->block_bytes);
        *checksum += checksum_buffer(buf, config->block_bytes);
    }
    uint64_t end = now_us();

    if (close(fd) != 0) {
        perror("close read");
        exit(1);
    }
    return end - start;
}

int main(int argc, char **argv) {
    struct bench_config config = parse_args(argc, argv);
    uint8_t *buf = malloc(config.block_bytes);
    if (buf == NULL) {
        perror("malloc");
        return 1;
    }

    printf("BLOCK_BENCH_CONFIG path=%s rounds=%u bytes=%llu block_bytes=%zu\n",
           config.base_path, config.rounds, (unsigned long long)config.bytes, config.block_bytes);
    fflush(stdout);

    struct bench_sample write_samples[MAX_ROUNDS];
    struct bench_sample read_samples[MAX_ROUNDS];
    uint64_t checksum = 0;

    for (unsigned round = 0; round < config.rounds; round++) {
        char path[256];
        bench_path(path, sizeof(path), config.base_path, round);
        unlink(path);

        uint64_t write_us = run_write_round(&config, buf, round, path);
        write_samples[round].elapsed_us = write_us;
        print_speed("BLOCK_BENCH_ROUND", "write", round, config.bytes, write_us, checksum);

        uint64_t read_us = run_read_round(&config, buf, round, path, &checksum);
        read_samples[round].elapsed_us = read_us;
        print_speed("BLOCK_BENCH_ROUND", "read", round, config.bytes, read_us, checksum);

        unlink(path);
    }

    struct bench_sample write_median = median_sample(write_samples, config.rounds);
    struct bench_sample read_median = median_sample(read_samples, config.rounds);
    print_speed("BLOCK_BENCH_RESULT", "write", config.rounds, config.bytes,
                write_median.elapsed_us, checksum);
    print_speed("BLOCK_BENCH_RESULT", "read", config.rounds, config.bytes,
                read_median.elapsed_us, checksum);

    free(buf);
    return 0;
}
