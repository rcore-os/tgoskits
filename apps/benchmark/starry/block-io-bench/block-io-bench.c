#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define DEFAULT_BASE_PATH "/root/block-io-bench"
#define DEFAULT_BYTES (4ULL * 1024ULL * 1024ULL)
#define DEFAULT_BLOCK_BYTES (4ULL * 1024ULL)
#define DEFAULT_ROUNDS 5U
#define MAX_ROUNDS 32U
#define FNV1A_OFFSET_BASIS 14695981039346656037ULL
#define FNV1A_PRIME 1099511628211ULL

struct bench_config {
    const char *base_path;
    uint64_t bytes;
    size_t block_bytes;
    unsigned rounds;
    int fsync_enabled;
};

struct bench_sample {
    uint64_t elapsed_us;
    uint64_t checksum;
};

struct write_sample {
    uint64_t write_us;
    uint64_t fsync_us;
};

struct coherence_sample {
    uint64_t rewrite_us;
    uint64_t read_us;
    uint64_t checksum;
};

struct disk_counters {
    uint64_t reads;
    uint64_t sectors_read;
    uint64_t writes;
    uint64_t sectors_written;
};

struct disk_probe {
    char device[64];
    int available;
};

static void usage(const char *program) {
    fprintf(stderr,
            "usage: %s [--path BASE] [--bytes BYTES] [--block-bytes BYTES] [--rounds N] "
            "[--no-fsync]\n",
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
        .fsync_enabled = 1,
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
        } else if (strcmp(argv[i], "--no-fsync") == 0) {
            config.fsync_enabled = 0;
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

static int read_disk_counters(const char *device, struct disk_counters *counters) {
    FILE *diskstats = fopen("/proc/diskstats", "r");
    if (diskstats == NULL) {
        return 0;
    }

    char line[512];
    int found = 0;
    while (fgets(line, sizeof(line), diskstats) != NULL) {
        unsigned major;
        unsigned minor;
        char name[64];
        unsigned long long reads;
        unsigned long long reads_merged;
        unsigned long long sectors_read;
        unsigned long long read_ms;
        unsigned long long writes;
        unsigned long long writes_merged;
        unsigned long long sectors_written;
        unsigned long long write_ms;
        unsigned long long inflight;
        unsigned long long io_ms;
        unsigned long long weighted_io_ms;
        int fields = sscanf(
            line,
            "%u %u %63s %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu %llu",
            &major, &minor, name, &reads, &reads_merged, &sectors_read, &read_ms, &writes,
            &writes_merged, &sectors_written, &write_ms, &inflight, &io_ms, &weighted_io_ms);
        if (fields == 14 && strcmp(name, device) == 0) {
            counters->reads = (uint64_t)reads;
            counters->sectors_read = (uint64_t)sectors_read;
            counters->writes = (uint64_t)writes;
            counters->sectors_written = (uint64_t)sectors_written;
            found = 1;
            break;
        }
    }
    fclose(diskstats);
    return found;
}

static void base_block_device(char *device) {
    char *partition = strrchr(device, 'p');
    if (partition != NULL && partition[1] >= '0' && partition[1] <= '9') {
        *partition = '\0';
        return;
    }
    size_t len = strlen(device);
    while (len != 0 && device[len - 1] >= '0' && device[len - 1] <= '9') {
        len--;
    }
    device[len] = '\0';
}

static struct disk_probe discover_root_disk(void) {
    struct disk_probe probe = {{0}, 0};
    FILE *mounts = fopen("/proc/mounts", "r");
    if (mounts == NULL) {
        return probe;
    }

    char source[256];
    char target[256];
    char filesystem[64];
    char options[256];
    while (fscanf(mounts, "%255s %255s %63s %255s %*u %*u", source, target, filesystem,
                  options) == 4) {
        if (strcmp(target, "/") == 0 && strncmp(source, "/dev/", 5) == 0) {
            const char *device = source + 5;
            size_t device_len = strlen(device);
            if (device_len < sizeof(probe.device)) {
                memcpy(probe.device, device, device_len + 1);
            }
            break;
        }
    }
    fclose(mounts);

    struct disk_counters counters;
    if (probe.device[0] != '\0' && read_disk_counters(probe.device, &counters)) {
        probe.available = 1;
        return probe;
    }
    base_block_device(probe.device);
    if (probe.device[0] != '\0' && read_disk_counters(probe.device, &counters)) {
        probe.available = 1;
    }
    return probe;
}

static uint64_t counter_delta(uint64_t after, uint64_t before) {
    return after >= before ? after - before : 0;
}

static void print_disk_delta(const struct disk_probe *probe, unsigned round, const char *phase,
                             const struct disk_counters *before,
                             const struct disk_counters *after) {
    if (!probe->available) {
        printf("BLOCK_BENCH_DISKSTATS round=%u phase=%s device=unresolved status=unavailable\n",
               round, phase);
    } else {
        printf("BLOCK_BENCH_DISKSTATS round=%u phase=%s device=%s reads=%llu "
               "sectors_read=%llu writes=%llu sectors_written=%llu\n",
               round, phase, probe->device,
               (unsigned long long)counter_delta(after->reads, before->reads),
               (unsigned long long)counter_delta(after->sectors_read, before->sectors_read),
               (unsigned long long)counter_delta(after->writes, before->writes),
               (unsigned long long)counter_delta(after->sectors_written,
                                                 before->sectors_written));
    }
    fflush(stdout);
}

static void fill_buffer(uint8_t *buf, size_t len, unsigned round, uint64_t offset) {
    for (size_t i = 0; i < len; i++) {
        buf[i] = (uint8_t)(((offset + i) * 131ULL + (uint64_t)round * 17ULL) & 0xffU);
    }
}

static void verify_buffer(const uint8_t *buf, size_t len, unsigned generation, uint64_t offset,
                          const char *phase) {
    for (size_t i = 0; i < len; i++) {
        uint8_t expected =
            (uint8_t)(((offset + i) * 131ULL + (uint64_t)generation * 17ULL) & 0xffU);
        if (buf[i] != expected) {
            fprintf(stderr,
                    "BLOCK_BENCH_MISMATCH phase=%s generation=%u offset=%llu expected=%u "
                    "actual=%u\n",
                    phase, generation, (unsigned long long)(offset + i), (unsigned)expected,
                    (unsigned)buf[i]);
            exit(1);
        }
    }
}

static uint64_t checksum_update(uint64_t checksum, const uint8_t *buf, size_t len) {
    for (size_t i = 0; i < len; i++) {
        checksum ^= buf[i];
        checksum *= FNV1A_PRIME;
    }
    return checksum;
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

static void checked_seek(int fd, uint64_t offset) {
    if (offset > (uint64_t)INT64_MAX || lseek(fd, (off_t)offset, SEEK_SET) < 0) {
        perror("lseek");
        exit(1);
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

static struct write_sample run_write_round(const struct bench_config *config, uint8_t *buf,
                                           unsigned round, const char *path,
                                           const struct disk_probe *probe, uint64_t *checksum) {
    int fd = open(path, O_CREAT | O_TRUNC | O_RDWR, 0644);
    if (fd < 0) {
        perror("open write");
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=write round=%u path=%s\n", round, path);
    fflush(stdout);

    struct disk_counters before_write = {0};
    struct disk_counters after_write = {0};
    struct disk_counters after_fsync = {0};
    if (probe->available) {
        read_disk_counters(probe->device, &before_write);
    }
    uint64_t write_start = now_us();
    *checksum = FNV1A_OFFSET_BASIS;
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        fill_buffer(buf, config->block_bytes, round, done);
        *checksum = checksum_update(*checksum, buf, config->block_bytes);
        checked_write_all(fd, buf, config->block_bytes);
    }
    uint64_t write_end = now_us();
    if (probe->available) {
        read_disk_counters(probe->device, &after_write);
    }

    uint64_t fsync_start = now_us();
    if (config->fsync_enabled) {
        printf("BLOCK_BENCH_PROGRESS phase=fsync round=%u path=%s\n", round, path);
        fflush(stdout);
        if (fsync(fd) != 0) {
            perror("fsync");
            exit(1);
        }
    }
    uint64_t fsync_end = now_us();
    if (probe->available) {
        read_disk_counters(probe->device, &after_fsync);
    }

    if (close(fd) != 0) {
        perror("close write");
        exit(1);
    }
    print_disk_delta(probe, round, "write", &before_write, &after_write);
    print_disk_delta(probe, round, "fsync", &after_write, &after_fsync);
    return (struct write_sample){
        .write_us = write_end - write_start,
        .fsync_us = fsync_end - fsync_start,
    };
}

static uint64_t run_read_round(const struct bench_config *config, uint8_t *buf, unsigned round,
                               const char *path, uint64_t expected_checksum, uint64_t *checksum,
                               const struct disk_probe *probe) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open read");
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=read round=%u path=%s\n", round, path);
    fflush(stdout);

    struct disk_counters before_read = {0};
    struct disk_counters after_read = {0};
    if (probe->available) {
        read_disk_counters(probe->device, &before_read);
    }
    uint64_t start = now_us();
    *checksum = FNV1A_OFFSET_BASIS;
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        checked_read_all(fd, buf, config->block_bytes);
        verify_buffer(buf, config->block_bytes, round, done, "initial-read");
        *checksum = checksum_update(*checksum, buf, config->block_bytes);
    }
    uint64_t end = now_us();
    if (probe->available) {
        read_disk_counters(probe->device, &after_read);
    }

    if (close(fd) != 0) {
        perror("close read");
        exit(1);
    }
    print_disk_delta(probe, round, "read", &before_read, &after_read);
    if (*checksum != expected_checksum) {
        fprintf(stderr,
                "BLOCK_BENCH_MISMATCH phase=initial-checksum round=%u expected=%llu actual=%llu\n",
                round, (unsigned long long)expected_checksum, (unsigned long long)*checksum);
        exit(1);
    }
    printf("BLOCK_BENCH_VERIFY round=%u phase=initial-read expected_checksum=%llu "
           "actual_checksum=%llu status=pass\n",
           round, (unsigned long long)expected_checksum, (unsigned long long)*checksum);
    fflush(stdout);
    return end - start;
}

static struct coherence_sample run_coherence_round(const struct bench_config *config,
                                                   uint8_t *buf, unsigned round,
                                                   const char *path,
                                                   const struct disk_probe *probe) {
    unsigned generation = round + MAX_ROUNDS;
    int observer = open(path, O_RDONLY);
    if (observer < 0) {
        perror("open coherence observer");
        exit(1);
    }
    int writer = open(path, O_WRONLY | O_TRUNC);
    if (writer < 0) {
        perror("open coherence writer");
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=coherence-rewrite round=%u path=%s\n", round, path);
    fflush(stdout);
    struct disk_counters before_rewrite = {0};
    struct disk_counters after_rewrite = {0};
    struct disk_counters after_read = {0};
    if (probe->available) {
        read_disk_counters(probe->device, &before_rewrite);
    }

    uint64_t expected_checksum = FNV1A_OFFSET_BASIS;
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        fill_buffer(buf, config->block_bytes, generation, done);
        expected_checksum = checksum_update(expected_checksum, buf, config->block_bytes);
    }
    uint64_t rewrite_start = now_us();
    for (uint64_t remaining = config->bytes; remaining != 0;
         remaining -= config->block_bytes) {
        uint64_t offset = remaining - config->block_bytes;
        fill_buffer(buf, config->block_bytes, generation, offset);
        checked_seek(writer, offset);
        checked_write_all(writer, buf, config->block_bytes);
    }
    if (config->fsync_enabled && fsync(writer) != 0) {
        perror("fsync coherence writer");
        exit(1);
    }
    uint64_t rewrite_end = now_us();
    if (close(writer) != 0) {
        perror("close coherence writer");
        exit(1);
    }
    if (probe->available) {
        read_disk_counters(probe->device, &after_rewrite);
    }

    struct stat metadata;
    if (fstat(observer, &metadata) != 0) {
        perror("fstat coherence observer");
        exit(1);
    }
    if (metadata.st_size < 0 || (uint64_t)metadata.st_size != config->bytes) {
        fprintf(stderr,
                "BLOCK_BENCH_MISMATCH phase=coherence-size round=%u expected=%llu actual=%lld\n",
                round, (unsigned long long)config->bytes, (long long)metadata.st_size);
        exit(1);
    }

    printf("BLOCK_BENCH_PROGRESS phase=coherence-read round=%u path=%s\n", round, path);
    fflush(stdout);
    checked_seek(observer, 0);
    uint64_t actual_checksum = FNV1A_OFFSET_BASIS;
    uint64_t read_start = now_us();
    for (uint64_t done = 0; done < config->bytes; done += config->block_bytes) {
        checked_read_all(observer, buf, config->block_bytes);
        verify_buffer(buf, config->block_bytes, generation, done, "coherence-read");
        actual_checksum = checksum_update(actual_checksum, buf, config->block_bytes);
    }
    uint64_t read_end = now_us();
    if (close(observer) != 0) {
        perror("close coherence observer");
        exit(1);
    }
    if (probe->available) {
        read_disk_counters(probe->device, &after_read);
    }
    print_disk_delta(probe, round, "coherence-rewrite", &before_rewrite, &after_rewrite);
    print_disk_delta(probe, round, "coherence-read", &after_rewrite, &after_read);

    if (actual_checksum != expected_checksum) {
        fprintf(stderr,
                "BLOCK_BENCH_MISMATCH phase=coherence-checksum round=%u expected=%llu "
                "actual=%llu\n",
                round, (unsigned long long)expected_checksum, (unsigned long long)actual_checksum);
        exit(1);
    }
    printf("BLOCK_BENCH_VERIFY round=%u phase=truncate-rewrite-cross-fd generation=%u "
           "expected_checksum=%llu actual_checksum=%llu status=pass\n",
           round, generation, (unsigned long long)expected_checksum,
           (unsigned long long)actual_checksum);
    fflush(stdout);

    return (struct coherence_sample){
        .rewrite_us = rewrite_end - rewrite_start,
        .read_us = read_end - read_start,
        .checksum = actual_checksum,
    };
}

int main(int argc, char **argv) {
    struct bench_config config = parse_args(argc, argv);
    uint8_t *buf = malloc(config.block_bytes);
    if (buf == NULL) {
        perror("malloc");
        return 1;
    }

    struct disk_probe probe = discover_root_disk();
    printf("BLOCK_BENCH_CONFIG path=%s rounds=%u bytes=%llu block_bytes=%zu fsync=%d "
           "io_model=buffered-file write_scope=write-syscalls cache_drop=none "
           "verification=bytewise+checksum coherence=truncate-rewrite-cross-fd "
           "diskstats_device=%s\n",
           config.base_path, config.rounds, (unsigned long long)config.bytes, config.block_bytes,
           config.fsync_enabled, probe.available ? probe.device : "unresolved");
    fflush(stdout);

    struct bench_sample write_samples[MAX_ROUNDS];
    struct bench_sample fsync_samples[MAX_ROUNDS];
    struct bench_sample read_samples[MAX_ROUNDS];
    struct bench_sample coherence_rewrite_samples[MAX_ROUNDS];
    struct bench_sample coherence_read_samples[MAX_ROUNDS];
    for (unsigned round = 0; round < config.rounds; round++) {
        char path[256];
        bench_path(path, sizeof(path), config.base_path, round);
        unlink(path);

        uint64_t expected_checksum = 0;
        struct write_sample write =
            run_write_round(&config, buf, round, path, &probe, &expected_checksum);
        write_samples[round] = (struct bench_sample){write.write_us, expected_checksum};
        fsync_samples[round] = (struct bench_sample){write.fsync_us, expected_checksum};
        print_speed("BLOCK_BENCH_ROUND", "write", round, config.bytes, write.write_us,
                    expected_checksum);
        if (config.fsync_enabled) {
            print_speed("BLOCK_BENCH_ROUND", "fsync", round, config.bytes, write.fsync_us,
                        expected_checksum);
        }

        uint64_t read_checksum = 0;
        uint64_t read_us = run_read_round(&config, buf, round, path, expected_checksum,
                                          &read_checksum, &probe);
        read_samples[round] = (struct bench_sample){read_us, read_checksum};
        print_speed("BLOCK_BENCH_ROUND", "read", round, config.bytes, read_us, read_checksum);

        struct coherence_sample coherence =
            run_coherence_round(&config, buf, round, path, &probe);
        coherence_rewrite_samples[round] =
            (struct bench_sample){coherence.rewrite_us, coherence.checksum};
        coherence_read_samples[round] =
            (struct bench_sample){coherence.read_us, coherence.checksum};
        print_speed("BLOCK_BENCH_ROUND", "coherence-rewrite", round, config.bytes,
                    coherence.rewrite_us, coherence.checksum);
        print_speed("BLOCK_BENCH_ROUND", "coherence-read", round, config.bytes,
                    coherence.read_us, coherence.checksum);
        unlink(path);
    }

    struct bench_sample write_median = median_sample(write_samples, config.rounds);
    struct bench_sample read_median = median_sample(read_samples, config.rounds);
    print_speed("BLOCK_BENCH_RESULT", "write", config.rounds, config.bytes,
                write_median.elapsed_us, write_median.checksum);
    if (config.fsync_enabled) {
        struct bench_sample fsync_median = median_sample(fsync_samples, config.rounds);
        print_speed("BLOCK_BENCH_RESULT", "fsync", config.rounds, config.bytes,
                    fsync_median.elapsed_us, fsync_median.checksum);
    }
    print_speed("BLOCK_BENCH_RESULT", "read", config.rounds, config.bytes,
                read_median.elapsed_us, read_median.checksum);
    struct bench_sample coherence_rewrite_median =
        median_sample(coherence_rewrite_samples, config.rounds);
    struct bench_sample coherence_read_median =
        median_sample(coherence_read_samples, config.rounds);
    print_speed("BLOCK_BENCH_RESULT", "coherence-rewrite", config.rounds, config.bytes,
                coherence_rewrite_median.elapsed_us, coherence_rewrite_median.checksum);
    print_speed("BLOCK_BENCH_RESULT", "coherence-read", config.rounds, config.bytes,
                coherence_read_median.elapsed_us, coherence_read_median.checksum);

    free(buf);
    return 0;
}
