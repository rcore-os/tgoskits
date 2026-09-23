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
    uint64_t before = now_ns();
    pthread_barrier_wait(&start);
    for (int i = 0; i < count; i++) {
        pthread_join(threads[i], NULL);
    }
    uint64_t elapsed = now_ns() - before;
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
