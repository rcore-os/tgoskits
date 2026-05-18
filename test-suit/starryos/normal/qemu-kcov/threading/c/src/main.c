/* kcov-spec §8: Multi-thread — independent per-task coverage */
#include "test_framework.h"
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_TRACE_PC 0

typedef struct {
    int tid;
    uint64_t n;
    int ok;
} thr_t;

static void *thread_fn(void *arg) {
    thr_t *s = (thr_t *)arg;
    s->ok = 0;
    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0)
        return NULL;
    if (ioctl(fd, KCOV_INIT_TRACE, 128)) {
        close(fd);
        return NULL;
    }
    size_t sz = 128 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (buf == MAP_FAILED) {
        close(fd);
        return NULL;
    }
    if (ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC)) {
        munmap(buf, sz);
        close(fd);
        return NULL;
    }
    for (volatile int i = 0; i < 300; i++) {
        gettid();
        getpid();
    }
    ioctl(fd, KCOV_DISABLE, 0);
    s->n = buf[0];
    s->tid = (int)gettid();
    munmap(buf, sz);
    close(fd);
    s->ok = 1;
    return NULL;
}

int main(void) {
    TEST_START("KCOV §8: per-task independent coverage");

    /* Doc: "each thread separately" — two threads open their own fds */
    pthread_t ta, tb;
    thr_t sa = {0}, sb = {0};
    pthread_create(&ta, NULL, thread_fn, &sa);
    pthread_create(&tb, NULL, thread_fn, &sb);
    pthread_join(ta, NULL);
    pthread_join(tb, NULL);

    CHECK(sa.ok, "thread A completed");
    CHECK(sb.ok, "thread B completed");
    CHECK(sa.n >= 1, "thread A recorded coverage");
    CHECK(sb.n >= 1, "thread B recorded coverage");
    printf("  INFO: thread %d → %lu, thread %d → %lu\n", sa.tid, sa.n, sb.tid,
           sb.n);

    /* Doc: "Tracing automatically gets disabled when a thread exits."
     * Verify a short-lived thread doesn't leave stale state. */
    pthread_t tc;
    thr_t sc = {0};
    pthread_create(&tc, NULL, thread_fn, &sc);
    pthread_join(tc, NULL);
    CHECK(sc.ok, "short-lived thread completed");

    /* Main thread can still use kcov after child threads exit */
    int fd = open("/dev/kcov", O_RDWR);
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0,
              "INIT_TRACE after thread exit");
    close(fd);

    TEST_DONE();
}
