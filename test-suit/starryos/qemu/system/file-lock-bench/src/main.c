#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <time.h>
#include <unistd.h>

#define THREADS 4
#define SAMPLES 1000
#define RECORDS 256

enum operation { POSIX, OFD, QUERY, FLOCK };

struct lock_timing {
    uint64_t calls;
    uint64_t wait_ns;
    uint64_t held_ns;
};

struct lock_metrics {
    struct lock_timing index;
    struct lock_timing reaper;
    struct lock_timing inode;
    uint64_t states;
    uint64_t idle;
    uint64_t capacity;
    uint64_t entry_size;
    uint64_t state_size;
    uint64_t key_size;
    int available;
};

struct worker {
    int fd;
    int id;
    enum operation operation;
    pthread_barrier_t *start;
    uint64_t durations[SAMPLES];
    int error;
};

static uint64_t now_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static int record_op(int fd, int cmd, short type, int id, int iteration)
{
    struct flock lock = {
        .l_type = type,
        .l_whence = SEEK_SET,
        .l_start = (id * SAMPLES + iteration) * 2,
        .l_len = 1,
    };
    return fcntl(fd, cmd, &lock);
}

static void *run_worker(void *arg)
{
    struct worker *worker = arg;
    pthread_barrier_wait(worker->start);
    for (int i = 0; i < SAMPLES; i++) {
        uint64_t before = now_ns();
        int result;
        switch (worker->operation) {
        case POSIX:
        case OFD: {
            int cmd = worker->operation == OFD ? F_OFD_SETLK : F_SETLK;
            result = record_op(worker->fd, cmd, F_RDLCK, worker->id, i);
            if (result == 0) {
                result = record_op(worker->fd, cmd, F_UNLCK, worker->id, i);
            }
            break;
        }
        case QUERY:
            result = record_op(worker->fd, F_GETLK, F_WRLCK, worker->id, i);
            break;
        case FLOCK:
            result = flock(worker->fd, LOCK_SH | LOCK_NB);
            if (result == 0) {
                result = flock(worker->fd, LOCK_UN);
            }
            break;
        default:
            result = -1;
            errno = EINVAL;
        }
        worker->durations[i] = now_ns() - before;
        if (result != 0) {
            worker->error = errno;
            return NULL;
        }
    }
    return NULL;
}

static int compare_u64(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;
    return (a > b) - (a < b);
}

static struct lock_metrics read_lock_metrics(enum operation operation)
{
    static const char *operations[] = {"posix_set", "ofd_set", "getlk", "flock"};
    const char *index = operation == FLOCK ? "flock_index" : "fcntl_index";
    const char *reaper = operation == FLOCK ? "flock_reap" : "fcntl_reap";
    const char *state = operation == FLOCK ? "flock" : "fcntl";
    struct lock_metrics metrics = {0};
    FILE *file = fopen("/sys/kernel/debug/file_lock_metrics", "r");
    if (file == NULL) {
        return metrics;
    }
    metrics.available = 1;
    char key[96];
    unsigned long long value;
    while (fscanf(file, "%95s %llu", key, &value) == 2) {
        char expected[96];
#define READ_METRIC(prefix, suffix, field) do {                         \
    snprintf(expected, sizeof(expected), "%s_%s", prefix, suffix);    \
    if (strcmp(key, expected) == 0) { field = (uint64_t)value; }        \
} while (0)
        READ_METRIC(index, "calls", metrics.index.calls);
        READ_METRIC(index, "wait_ns", metrics.index.wait_ns);
        READ_METRIC(index, "held_ns", metrics.index.held_ns);
        READ_METRIC(reaper, "calls", metrics.reaper.calls);
        READ_METRIC(reaper, "wait_ns", metrics.reaper.wait_ns);
        READ_METRIC(reaper, "held_ns", metrics.reaper.held_ns);
        READ_METRIC(operations[operation], "calls", metrics.inode.calls);
        READ_METRIC(operations[operation], "wait_ns", metrics.inode.wait_ns);
        READ_METRIC(operations[operation], "held_ns", metrics.inode.held_ns);
        READ_METRIC(state, "states", metrics.states);
        READ_METRIC(state, operation == FLOCK ? "idle" : "pending", metrics.idle);
        READ_METRIC(state, "capacity", metrics.capacity);
        READ_METRIC(state, "entry_size", metrics.entry_size);
        READ_METRIC(state, "state_size", metrics.state_size);
        if (strcmp(key, "inode_key_size") == 0) {
            metrics.key_size = (uint64_t)value;
        }
#undef READ_METRIC
    }
    fclose(file);
    return metrics;
}

static int run_case(enum operation operation, int count, int distinct)
{
    static const char *names[] = {"posix", "ofd", "getlk", "flock"};
    pthread_t threads[THREADS];
    struct worker workers[THREADS] = {0};
    pthread_barrier_t start;
    uint64_t durations[THREADS * SAMPLES];
    char path[96];

    if (pthread_barrier_init(&start, NULL, (unsigned)count + 1) != 0) {
        return -1;
    }
    for (int i = 0; i < count; i++) {
        snprintf(path, sizeof(path), "/tmp/file-lock-bench-%d-%d", operation,
                 distinct ? i : 0);
        workers[i].fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
        workers[i].id = i;
        workers[i].operation = operation;
        workers[i].start = &start;
        if (workers[i].fd < 0) {
            perror("open");
            return -1;
        }
        if (operation != FLOCK && (distinct || i == 0)) {
            for (int record = 0; record < RECORDS; record++) {
                struct flock seed = {
                    .l_type = F_RDLCK,
                    .l_whence = SEEK_SET,
                    .l_start = THREADS * SAMPLES * 2 + record * 2,
                    .l_len = 1,
                };
                if (fcntl(workers[i].fd, operation == OFD ? F_OFD_SETLK : F_SETLK,
                          &seed) != 0) {
                    perror("seed record lock");
                    return -1;
                }
            }
        }
        if (pthread_create(&threads[i], NULL, run_worker,
                                                 &workers[i]) != 0) {
            perror("pthread_create");
            return -1;
        }
    }
    struct lock_metrics start_metrics = read_lock_metrics(operation);
    uint64_t before = now_ns();
    pthread_barrier_wait(&start);
    for (int i = 0; i < count; i++) {
        pthread_join(threads[i], NULL);
    }
    uint64_t elapsed = now_ns() - before;
    struct lock_metrics end_metrics = read_lock_metrics(operation);
    pthread_barrier_destroy(&start);
    for (int i = 0; i < count; i++) {
        if (workers[i].error != 0) {
            errno = workers[i].error;
            perror("file lock operation");
            return -1;
        }
        memcpy(&durations[i * SAMPLES], workers[i].durations,
               sizeof(workers[i].durations));
        close(workers[i].fd);
    }
    qsort(durations, (size_t)count * SAMPLES, sizeof(durations[0]), compare_u64);
    int total = count * SAMPLES;
    printf("FILE_LOCK_BENCH mode=%s threads=%d files=%d records=%d ops=%d "
           "elapsed_ns=%llu ops_per_s=%llu p50_ns=%llu p95_ns=%llu p99_ns=%llu\n",
           names[operation], count, distinct ? count : 1,
           operation == FLOCK ? 0 : RECORDS, total,
           (unsigned long long)elapsed,
           (unsigned long long)((uint64_t)total * 1000000000ULL / elapsed),
           (unsigned long long)durations[total / 2],
           (unsigned long long)durations[total * 95 / 100],
           (unsigned long long)durations[total * 99 / 100]);
    if (start_metrics.available && end_metrics.available) {
        printf("FILE_LOCK_KERNEL mode=%s threads=%d files=%d "
               "index_calls=%llu index_wait_ns=%llu index_held_ns=%llu "
               "reap_calls=%llu reap_wait_ns=%llu reap_held_ns=%llu "
               "inode_calls=%llu inode_wait_ns=%llu inode_held_ns=%llu "
               "states=%llu idle=%llu capacity=%llu "
               "key_size=%llu state_size=%llu entry_size=%llu\n",
               names[operation], count, distinct ? count : 1,
               (unsigned long long)(end_metrics.index.calls - start_metrics.index.calls),
               (unsigned long long)(end_metrics.index.wait_ns - start_metrics.index.wait_ns),
               (unsigned long long)(end_metrics.index.held_ns - start_metrics.index.held_ns),
               (unsigned long long)(end_metrics.reaper.calls - start_metrics.reaper.calls),
               (unsigned long long)(end_metrics.reaper.wait_ns - start_metrics.reaper.wait_ns),
               (unsigned long long)(end_metrics.reaper.held_ns - start_metrics.reaper.held_ns),
               (unsigned long long)(end_metrics.inode.calls - start_metrics.inode.calls),
               (unsigned long long)(end_metrics.inode.wait_ns - start_metrics.inode.wait_ns),
               (unsigned long long)(end_metrics.inode.held_ns - start_metrics.inode.held_ns),
               (unsigned long long)end_metrics.states,
               (unsigned long long)end_metrics.idle,
               (unsigned long long)end_metrics.capacity,
               (unsigned long long)end_metrics.key_size,
               (unsigned long long)end_metrics.state_size,
               (unsigned long long)end_metrics.entry_size);
    }
    fflush(stdout);
    return 0;
}

int main(void)
{
    for (int mode = POSIX; mode <= FLOCK; mode++) {
        for (int scenario = 0; scenario < 3; scenario++) {
            int count = scenario == 0 ? 1 : THREADS;
            if (run_case((enum operation)mode, count, scenario == 1) != 0) {
                return 1;
            }
        }
    }
    puts("FILE_LOCK_BENCH_PASSED");
    return 0;
}
