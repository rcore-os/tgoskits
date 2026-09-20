#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define EXPECTED_CPUS 4
#define MAX_LEAVES 64U
#define MAX_ROUNDS 31U
#define MAX_NODES (MAX_LEAVES * 2U - 1U)
#define MAX_ACTIVE EXPECTED_CPUS
#define DEFAULT_WORK_DIR "/tmp/compile-sim-bench"
#define FNV1A_OFFSET_BASIS UINT64_C(14695981039346656037)
#define FNV1A_PRIME UINT64_C(1099511628211)

struct bench_config {
    const char *work_dir;
    unsigned leaves;
    size_t source_bytes;
    size_t work_bytes;
    unsigned passes;
    size_t output_bytes;
    unsigned rounds;
    unsigned expected_cpus;
    int smoke;
};

struct build_node {
    unsigned dependencies[2];
    unsigned dependency_count;
    unsigned remaining_dependencies;
    unsigned parent;
    int has_parent;
    int completed;
};

struct active_child {
    pid_t pid;
    unsigned node;
};

struct sample {
    uint64_t elapsed_us;
    uint64_t checksum;
};

static const char *self_path;

static void usage(const char *program)
{
    fprintf(stderr,
            "usage: %s [--smoke|--benchmark] [--work-dir PATH] "
            "[--leaves N] [--source-bytes N] [--work-bytes N] "
            "[--passes N] [--output-bytes N] [--rounds N] "
            "[--expected-cpus N]\n",
            program);
}

static void fail_errno(const char *operation)
{
    fprintf(stderr, "%s failed: %s\n", operation, strerror(errno));
    exit(1);
}

static uint64_t parse_u64(const char *name, const char *text, uint64_t min,
                          uint64_t max)
{
    char *end = NULL;
    unsigned long long value;

    errno = 0;
    value = strtoull(text, &end, 0);
    if (errno != 0 || end == text || *end != '\0' || value < min ||
        value > max) {
        fprintf(stderr, "invalid %s: %s\n", name, text);
        exit(2);
    }
    return (uint64_t)value;
}

static int is_power_of_two(size_t value)
{
    return value != 0 && (value & (value - 1U)) == 0;
}

static struct bench_config parse_args(int argc, char **argv)
{
    struct bench_config config = {
        .work_dir = DEFAULT_WORK_DIR,
        .leaves = 16,
        .source_bytes = 256U * 1024U,
        .work_bytes = 4U * 1024U * 1024U,
        .passes = 8,
        .output_bytes = 128U * 1024U,
        .rounds = 5,
        .expected_cpus = EXPECTED_CPUS,
        .smoke = 0,
    };
    int execution_selected = 0;

    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--smoke") == 0) {
            if (execution_selected) {
                fprintf(stderr, "select exactly one execution mode\n");
                exit(2);
            }
            config.leaves = 4;
            config.source_bytes = 64U * 1024U;
            config.work_bytes = 512U * 1024U;
            config.passes = 2;
            config.output_bytes = 32U * 1024U;
            config.rounds = 1;
            config.smoke = 1;
            execution_selected = 1;
        } else if (strcmp(argv[i], "--benchmark") == 0) {
            if (execution_selected) {
                fprintf(stderr, "select exactly one execution mode\n");
                exit(2);
            }
            config.smoke = 0;
            execution_selected = 1;
        } else if (strcmp(argv[i], "--work-dir") == 0 && i + 1 < argc) {
            config.work_dir = argv[++i];
        } else if (strcmp(argv[i], "--leaves") == 0 && i + 1 < argc) {
            config.leaves = (unsigned)parse_u64("leaves", argv[++i], 2, MAX_LEAVES);
        } else if (strcmp(argv[i], "--source-bytes") == 0 && i + 1 < argc) {
            config.source_bytes =
                (size_t)parse_u64("source-bytes", argv[++i], 4096, 64U * 1024U * 1024U);
        } else if (strcmp(argv[i], "--work-bytes") == 0 && i + 1 < argc) {
            config.work_bytes =
                (size_t)parse_u64("work-bytes", argv[++i], 4096, 64U * 1024U * 1024U);
        } else if (strcmp(argv[i], "--passes") == 0 && i + 1 < argc) {
            config.passes = (unsigned)parse_u64("passes", argv[++i], 1, 1024);
        } else if (strcmp(argv[i], "--output-bytes") == 0 && i + 1 < argc) {
            config.output_bytes =
                (size_t)parse_u64("output-bytes", argv[++i], 4096, 16U * 1024U * 1024U);
        } else if (strcmp(argv[i], "--rounds") == 0 && i + 1 < argc) {
            config.rounds = (unsigned)parse_u64("rounds", argv[++i], 1, MAX_ROUNDS);
        } else if (strcmp(argv[i], "--expected-cpus") == 0 && i + 1 < argc) {
            config.expected_cpus =
                (unsigned)parse_u64("expected-cpus", argv[++i], EXPECTED_CPUS, CPU_SETSIZE);
        } else if (strcmp(argv[i], "--worker") == 0) {
            break;
        } else if (strcmp(argv[i], "--help") == 0) {
            usage(argv[0]);
            exit(0);
        } else {
            usage(argv[0]);
            exit(2);
        }
    }

    if (!is_power_of_two(config.leaves)) {
        fprintf(stderr, "leaves must be a power of two\n");
        exit(2);
    }
    if (!is_power_of_two(config.work_bytes) || config.work_bytes % sizeof(uint64_t) != 0) {
        fprintf(stderr, "work-bytes must be a power of two and a multiple of 8\n");
        exit(2);
    }
    if (config.output_bytes > config.work_bytes) {
        fprintf(stderr, "output-bytes must not exceed work-bytes\n");
        exit(2);
    }
    if (!config.smoke && (config.rounds < 3 || config.rounds % 2 == 0)) {
        fprintf(stderr, "benchmark rounds must be an odd integer of at least 3\n");
        exit(2);
    }
    return config;
}

static uint64_t now_us(void)
{
    struct timespec time;

    if (clock_gettime(CLOCK_MONOTONIC, &time) != 0) {
        fail_errno("clock_gettime");
    }
    return (uint64_t)time.tv_sec * UINT64_C(1000000) +
           (uint64_t)time.tv_nsec / UINT64_C(1000);
}

static uint64_t rotate_left(uint64_t value, unsigned shift)
{
    shift &= 63U;
    return shift == 0 ? value : (value << shift) | (value >> (64U - shift));
}

static uint64_t fnv1a(const uint8_t *data, size_t length, uint64_t hash)
{
    for (size_t i = 0; i < length; i++) {
        hash ^= data[i];
        hash *= FNV1A_PRIME;
    }
    return hash;
}

static void write_all(int fd, const void *buffer, size_t length)
{
    const uint8_t *cursor = buffer;

    while (length != 0) {
        ssize_t written = write(fd, cursor, length);
        if (written < 0) {
            if (errno == EINTR) {
                continue;
            }
            fail_errno("write");
        }
        if (written == 0) {
            fprintf(stderr, "write made no progress\n");
            exit(1);
        }
        cursor += (size_t)written;
        length -= (size_t)written;
    }
}

static uint8_t *read_file(const char *path, size_t *length)
{
    struct stat metadata;
    uint8_t *buffer;
    size_t offset = 0;
    int fd = open(path, O_RDONLY);

    if (fd < 0) {
        fail_errno(path);
    }
    if (fstat(fd, &metadata) != 0) {
        fail_errno("fstat");
    }
    if (metadata.st_size <= 0 || (uint64_t)metadata.st_size > SIZE_MAX) {
        fprintf(stderr, "invalid input size for %s\n", path);
        exit(1);
    }
    *length = (size_t)metadata.st_size;
    buffer = malloc(*length);
    if (buffer == NULL) {
        fail_errno("malloc input");
    }
    while (offset < *length) {
        ssize_t count = read(fd, buffer + offset, *length - offset);
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            fail_errno("read");
        }
        if (count == 0) {
            fprintf(stderr, "short read from %s\n", path);
            exit(1);
        }
        offset += (size_t)count;
    }
    if (close(fd) != 0) {
        fail_errno("close input");
    }
    return buffer;
}

static void create_directory(const char *path)
{
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        fail_errno(path);
    }
}

static void format_source_path(char *path, size_t size, const char *work_dir,
                               unsigned leaf)
{
    int length = snprintf(path, size, "%s/source-%03u.bin", work_dir, leaf);
    if (length < 0 || (size_t)length >= size) {
        fprintf(stderr, "source path is too long\n");
        exit(1);
    }
}

static void format_output_path(char *path, size_t size, const char *work_dir,
                               unsigned node)
{
    int length = snprintf(path, size, "%s/node-%03u.o", work_dir, node);
    if (length < 0 || (size_t)length >= size) {
        fprintf(stderr, "output path is too long\n");
        exit(1);
    }
}

static void prepare_sources(const struct bench_config *config)
{
    uint8_t *buffer = malloc(config->source_bytes);

    if (buffer == NULL) {
        fail_errno("malloc source");
    }
    create_directory("/tmp");
    create_directory(config->work_dir);
    for (unsigned leaf = 0; leaf < config->leaves; leaf++) {
        char path[PATH_MAX];
        int fd;

        for (size_t offset = 0; offset < config->source_bytes; offset++) {
            uint64_t value = (uint64_t)leaf * UINT64_C(0x9e3779b97f4a7c15) + offset;
            value ^= value >> 17;
            value *= UINT64_C(0xbf58476d1ce4e5b9);
            buffer[offset] = (uint8_t)(value >> 29);
        }
        format_source_path(path, sizeof(path), config->work_dir, leaf);
        fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        if (fd < 0) {
            fail_errno(path);
        }
        write_all(fd, buffer, config->source_bytes);
        if (close(fd) != 0) {
            fail_errno("close source");
        }
    }
    free(buffer);
}

static void remove_outputs(const struct bench_config *config, unsigned node_count)
{
    for (unsigned node = 0; node < node_count; node++) {
        char path[PATH_MAX];
        format_output_path(path, sizeof(path), config->work_dir, node);
        if (unlink(path) != 0 && errno != ENOENT) {
            fail_errno(path);
        }
    }
}

static unsigned build_graph(struct build_node *nodes, unsigned leaves)
{
    unsigned node_count = leaves;
    unsigned level_offset = 0;
    unsigned level_count = leaves;

    memset(nodes, 0, sizeof(*nodes) * MAX_NODES);
    while (level_count > 1) {
        unsigned parent_offset = node_count;
        unsigned parent_count = level_count / 2U;

        for (unsigned parent = 0; parent < parent_count; parent++) {
            unsigned left = level_offset + parent * 2U;
            unsigned right = left + 1U;
            unsigned parent_node = parent_offset + parent;

            nodes[parent_node].dependencies[0] = left;
            nodes[parent_node].dependencies[1] = right;
            nodes[parent_node].dependency_count = 2;
            nodes[parent_node].remaining_dependencies = 2;
            nodes[left].parent = parent_node;
            nodes[left].has_parent = 1;
            nodes[right].parent = parent_node;
            nodes[right].has_parent = 1;
        }
        node_count += parent_count;
        level_offset = parent_offset;
        level_count = parent_count;
    }
    return node_count;
}

static void set_affinity(const cpu_set_t *mask)
{
    if (sched_setaffinity(0, sizeof(*mask), mask) != 0) {
        fail_errno("sched_setaffinity");
    }
}

static cpu_set_t select_cpus(const cpu_set_t *allowed, unsigned cpu_count)
{
    cpu_set_t selected;
    unsigned remaining = cpu_count;

    CPU_ZERO(&selected);
    for (int cpu = 0; cpu < CPU_SETSIZE && remaining != 0; cpu++) {
        if (CPU_ISSET(cpu, allowed)) {
            CPU_SET(cpu, &selected);
            remaining--;
        }
    }
    if (remaining != 0) {
        fprintf(stderr, "requested %u CPUs but the original mask has only %d\n",
                cpu_count, CPU_COUNT(allowed));
        exit(1);
    }
    return selected;
}

static unsigned long long low_mask(const cpu_set_t *mask)
{
    unsigned long long value = 0;
    int limit = CPU_SETSIZE < 64 ? CPU_SETSIZE : 64;

    for (int cpu = 0; cpu < limit; cpu++) {
        if (CPU_ISSET(cpu, mask)) {
            value |= 1ULL << cpu;
        }
    }
    return value;
}

static pid_t spawn_worker(const struct bench_config *config,
                          const struct build_node *nodes, unsigned node)
{
    char node_text[32];
    char work_text[32];
    char pass_text[32];
    char output_bytes_text[32];
    char output[PATH_MAX];
    char input_left[PATH_MAX];
    char input_right[PATH_MAX];
    char *arguments[13];
    size_t argument_count = 0;
    pid_t pid;

    snprintf(node_text, sizeof(node_text), "%u", node);
    snprintf(work_text, sizeof(work_text), "%zu", config->work_bytes);
    snprintf(pass_text, sizeof(pass_text), "%u", config->passes);
    snprintf(output_bytes_text, sizeof(output_bytes_text), "%zu", config->output_bytes);
    format_output_path(output, sizeof(output), config->work_dir, node);
    if (nodes[node].dependency_count == 0) {
        format_source_path(input_left, sizeof(input_left), config->work_dir, node);
    } else {
        format_output_path(input_left, sizeof(input_left), config->work_dir,
                           nodes[node].dependencies[0]);
        format_output_path(input_right, sizeof(input_right), config->work_dir,
                           nodes[node].dependencies[1]);
    }

    arguments[argument_count++] = (char *)self_path;
    arguments[argument_count++] = "--worker";
    arguments[argument_count++] = node_text;
    arguments[argument_count++] = work_text;
    arguments[argument_count++] = pass_text;
    arguments[argument_count++] = output_bytes_text;
    arguments[argument_count++] = output;
    arguments[argument_count++] = input_left;
    if (nodes[node].dependency_count != 0) {
        arguments[argument_count++] = input_right;
    }
    arguments[argument_count] = NULL;

    pid = fork();
    if (pid == 0) {
        execv(self_path, arguments);
        fprintf(stderr, "execv(%s) failed: %s\n", self_path, strerror(errno));
        _exit(127);
    }
    return pid;
}

static void stop_children(struct active_child *active, unsigned active_count)
{
    for (unsigned i = 0; i < active_count; i++) {
        if (kill(active[i].pid, SIGKILL) != 0 && errno != ESRCH) {
            fprintf(stderr, "kill(%ld) failed: %s\n", (long)active[i].pid,
                    strerror(errno));
        }
    }
    while (waitpid(-1, NULL, 0) > 0 || errno == EINTR) {
        if (errno == EINTR) {
            errno = 0;
        }
    }
}

static uint64_t checksum_file(const char *path)
{
    size_t length;
    uint8_t *buffer = read_file(path, &length);
    uint64_t checksum = fnv1a(buffer, length, FNV1A_OFFSET_BASIS);

    free(buffer);
    return checksum;
}

static struct sample run_build(const struct bench_config *config, unsigned jobs,
                               const cpu_set_t *allowed)
{
    struct build_node nodes[MAX_NODES];
    struct active_child active[MAX_ACTIVE];
    unsigned ready[MAX_NODES];
    unsigned ready_head = 0;
    unsigned ready_tail = 0;
    unsigned active_count = 0;
    unsigned completed = 0;
    unsigned node_count = build_graph(nodes, config->leaves);
    cpu_set_t selected = select_cpus(allowed, jobs);
    struct sample sample;
    uint64_t start;
    char final_path[PATH_MAX];

    remove_outputs(config, node_count);
    set_affinity(&selected);
    for (unsigned leaf = 0; leaf < config->leaves; leaf++) {
        ready[ready_tail++] = leaf;
    }

    start = now_us();
    while (completed < node_count) {
        while (active_count < jobs && ready_head < ready_tail) {
            unsigned node = ready[ready_head++];
            pid_t pid = spawn_worker(config, nodes, node);
            if (pid < 0) {
                int saved_errno = errno;
                stop_children(active, active_count);
                errno = saved_errno;
                fail_errno("fork");
            }
            active[active_count].pid = pid;
            active[active_count].node = node;
            active_count++;
        }
        if (active_count == 0) {
            fprintf(stderr, "build graph made no progress\n");
            exit(1);
        }

        int status;
        pid_t pid;
        do {
            pid = waitpid(-1, &status, 0);
        } while (pid < 0 && errno == EINTR);
        if (pid < 0) {
            stop_children(active, active_count);
            fail_errno("waitpid");
        }

        unsigned slot = active_count;
        for (unsigned i = 0; i < active_count; i++) {
            if (active[i].pid == pid) {
                slot = i;
                break;
            }
        }
        if (slot == active_count) {
            stop_children(active, active_count);
            fprintf(stderr, "reaped unknown worker %ld\n", (long)pid);
            exit(1);
        }
        unsigned node = active[slot].node;
        active[slot] = active[active_count - 1U];
        active_count--;
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            stop_children(active, active_count);
            fprintf(stderr, "worker node=%u pid=%ld failed status=%d\n", node,
                    (long)pid, status);
            exit(1);
        }

        nodes[node].completed = 1;
        completed++;
        if (nodes[node].has_parent) {
            struct build_node *parent = &nodes[nodes[node].parent];
            if (parent->remaining_dependencies == 0) {
                stop_children(active, active_count);
                fprintf(stderr, "dependency count underflow for node %u\n",
                        nodes[node].parent);
                exit(1);
            }
            parent->remaining_dependencies--;
            if (parent->remaining_dependencies == 0) {
                ready[ready_tail++] = nodes[node].parent;
            }
        }
    }
    sample.elapsed_us = now_us() - start;
    set_affinity(allowed);
    format_output_path(final_path, sizeof(final_path), config->work_dir, node_count - 1U);
    sample.checksum = checksum_file(final_path);
    return sample;
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static uint64_t summarize(const struct bench_config *config, unsigned jobs,
                          const struct sample *samples)
{
    uint64_t elapsed[MAX_ROUNDS];
    uint64_t checksum = samples[0].checksum;

    printf("COMPILE_SIM_RESULT jobs=%u rounds=%u samples_us=", jobs, config->rounds);
    for (unsigned round = 0; round < config->rounds; round++) {
        if (samples[round].checksum != checksum) {
            fprintf(stderr,
                    "checksum mismatch jobs=%u round=%u expected=0x%016llx actual=0x%016llx\n",
                    jobs, round + 1U, (unsigned long long)checksum,
                    (unsigned long long)samples[round].checksum);
            exit(1);
        }
        elapsed[round] = samples[round].elapsed_us;
        printf("%s%llu", round == 0 ? "" : ",",
               (unsigned long long)samples[round].elapsed_us);
    }
    qsort(elapsed, config->rounds, sizeof(elapsed[0]), compare_u64);
    uint64_t median = elapsed[config->rounds / 2U];
    printf(" median_us=%llu checksum=0x%016llx\n", (unsigned long long)median,
           (unsigned long long)checksum);
    return median;
}

static int worker_main(int argc, char **argv)
{
    if (argc != 8 && argc != 9) {
        fprintf(stderr, "invalid worker argument count: %d\n", argc);
        return 2;
    }

    unsigned node = (unsigned)parse_u64("worker-node", argv[2], 0, MAX_NODES - 1U);
    size_t work_bytes = (size_t)parse_u64("worker-work-bytes", argv[3], 4096,
                                          64U * 1024U * 1024U);
    unsigned passes = (unsigned)parse_u64("worker-passes", argv[4], 1, 1024);
    size_t output_bytes = (size_t)parse_u64("worker-output-bytes", argv[5], 4096,
                                            16U * 1024U * 1024U);
    const char *output = argv[6];
    uint64_t hash = FNV1A_OFFSET_BASIS ^ node;
    uint64_t *working;
    size_t words;

    if (!is_power_of_two(work_bytes) || output_bytes > work_bytes) {
        fprintf(stderr, "invalid worker memory geometry\n");
        return 2;
    }
    for (int input_index = 7; input_index < argc; input_index++) {
        size_t length;
        uint8_t *input = read_file(argv[input_index], &length);
        hash = fnv1a(input, length, hash);
        free(input);
    }

    working = malloc(work_bytes);
    if (working == NULL) {
        fail_errno("malloc working set");
    }
    words = work_bytes / sizeof(*working);
    for (size_t i = 0; i < words; i++) {
        uint64_t value = hash + i * UINT64_C(0x9e3779b97f4a7c15);
        value ^= value >> 30;
        value *= UINT64_C(0xbf58476d1ce4e5b9);
        value ^= value >> 27;
        value *= UINT64_C(0x94d049bb133111eb);
        working[i] = value ^ (value >> 31);
    }

    uint64_t accumulator = hash;
    size_t mask = words - 1U;
    for (unsigned pass = 0; pass < passes; pass++) {
        for (size_t i = 0; i < words; i++) {
            size_t index = (size_t)(accumulator ^ working[i]) & mask;
            uint64_t value = working[i] ^ rotate_left(working[index] + accumulator,
                                                      (unsigned)(i + pass));
            value *= UINT64_C(0xd6e8feb86659fd93);
            accumulator = rotate_left(accumulator ^ value, 17) + i + pass;
            working[i] = value ^ accumulator;
        }
    }

    int fd = open(output, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) {
        fail_errno(output);
    }
    write_all(fd, working, output_bytes);
    if (close(fd) != 0) {
        fail_errno("close output");
    }
    free(working);
    return 0;
}

static int benchmark_main(const struct bench_config *config)
{
    cpu_set_t allowed;
    long online = sysconf(_SC_NPROCESSORS_ONLN);
    struct sample one_cpu[MAX_ROUNDS];
    struct sample four_cpu[MAX_ROUNDS];
    uint64_t warmup_checksum;
    uint64_t one_median;
    uint64_t four_median;

    if (online < 0) {
        fail_errno("sysconf(_SC_NPROCESSORS_ONLN)");
    }
    CPU_ZERO(&allowed);
    if (sched_getaffinity(0, sizeof(allowed), &allowed) != 0) {
        fail_errno("sched_getaffinity");
    }
    printf("COMPILE_SIM_SOURCE path=%s model=process_dag_v1\n", self_path);
    printf("COMPILE_SIM_TOPOLOGY online=%ld allowed=%d mask=0x%llx\n", online,
           CPU_COUNT(&allowed), low_mask(&allowed));
    printf("COMPILE_SIM_CONFIG execution=%s leaves=%u nodes=%u source_bytes=%zu "
           "work_bytes=%zu passes=%u output_bytes=%zu rounds=%u expected_cpus=%d\n",
           config->smoke ? "smoke" : "benchmark", config->leaves,
           config->leaves * 2U - 1U, config->source_bytes, config->work_bytes,
           config->passes, config->output_bytes, config->rounds, config->expected_cpus);
    fflush(stdout);
    if (online != (long)config->expected_cpus ||
        CPU_COUNT(&allowed) != (int)config->expected_cpus) {
        fprintf(stderr, "expected exactly %u online and allowed CPUs\n",
                config->expected_cpus);
        return 1;
    }
    if (self_path[0] != '/') {
        fprintf(stderr, "benchmark executable path must be absolute: %s\n", self_path);
        return 1;
    }

    prepare_sources(config);
    struct sample warmup_one = run_build(config, 1, &allowed);
    struct sample warmup_four = run_build(config, 4, &allowed);
    warmup_checksum = warmup_one.checksum;
    if (warmup_four.checksum != warmup_checksum) {
        fprintf(stderr, "warmup checksum mismatch between jobs=1 and jobs=4\n");
        return 1;
    }
    printf("COMPILE_SIM_WARMUP jobs=1 elapsed_us=%llu checksum=0x%016llx\n",
           (unsigned long long)warmup_one.elapsed_us,
           (unsigned long long)warmup_one.checksum);
    printf("COMPILE_SIM_WARMUP jobs=4 elapsed_us=%llu checksum=0x%016llx\n",
           (unsigned long long)warmup_four.elapsed_us,
           (unsigned long long)warmup_four.checksum);

    for (unsigned round = 0; round < config->rounds; round++) {
        if (round % 2U == 0) {
            one_cpu[round] = run_build(config, 1, &allowed);
            four_cpu[round] = run_build(config, 4, &allowed);
        } else {
            four_cpu[round] = run_build(config, 4, &allowed);
            one_cpu[round] = run_build(config, 1, &allowed);
        }
        if (one_cpu[round].checksum != warmup_checksum ||
            four_cpu[round].checksum != warmup_checksum) {
            fprintf(stderr, "sample checksum mismatch round=%u\n", round + 1U);
            return 1;
        }
        printf("COMPILE_SIM_SAMPLE jobs=1 round=%u elapsed_us=%llu checksum=0x%016llx\n",
               round + 1U, (unsigned long long)one_cpu[round].elapsed_us,
               (unsigned long long)one_cpu[round].checksum);
        printf("COMPILE_SIM_SAMPLE jobs=4 round=%u elapsed_us=%llu checksum=0x%016llx\n",
               round + 1U, (unsigned long long)four_cpu[round].elapsed_us,
               (unsigned long long)four_cpu[round].checksum);
        fflush(stdout);
    }

    one_median = summarize(config, 1, one_cpu);
    four_median = summarize(config, 4, four_cpu);
    uint64_t speedup_milli = one_median * UINT64_C(1000) / four_median;
    printf("COMPILE_SIM_SPEEDUP one_job_median_us=%llu four_job_median_us=%llu "
           "speedup_milli=%llu speedup=%llu.%03llux\n",
           (unsigned long long)one_median, (unsigned long long)four_median,
           (unsigned long long)speedup_milli,
           (unsigned long long)(speedup_milli / UINT64_C(1000)),
           (unsigned long long)(speedup_milli % UINT64_C(1000)));
    printf("COMPILE_SIM_BENCH_PASSED\n");
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    self_path = argv[0];
    if (argc >= 2 && strcmp(argv[1], "--worker") == 0) {
        return worker_main(argc, argv);
    }
    struct bench_config config = parse_args(argc, argv);
    return benchmark_main(&config);
}
