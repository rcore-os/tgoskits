/*
 * test-kcov-stress.c — KCOV light stress tests
 *
 * Pushes every part of the kcov interface under moderate load to
 * catch races, leaks, and performance regressions without consuming
 * excessive CI time.  Iteration counts are intentionally capped so
 * this stays in the normal (not stress) test group — a full stress
 * suite with larger parameters lives under test-suit/starryos/stress/.
 *
 * Runs independently from correctness / spec tests.
 */

#define _GNU_SOURCE
#include "test_framework.h"
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define KCOV_INIT_TRACE _IOR('c', 1, unsigned long)
#define KCOV_ENABLE _IO('c', 100)
#define KCOV_DISABLE _IO('c', 101)
#define KCOV_TRACE_PC 0
#define KCOV_TRACE_CMP 1

static int open_kcov(void) {
    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0) {
        printf("SKIP: /dev/kcov not available (errno=%d: %s)\n", errno,
               strerror(errno));
        exit(0);
    }
    return fd;
}

static void heavy_burst(int count) {
    for (volatile int i = 0; i < count; i++) {
        getpid();
        getuid();
        getppid();
        struct stat st;
        stat("/", &st);
        stat("/dev", &st);
        char buf[64];
        getcwd(buf, sizeof(buf));
        int fd = open("/dev/null", O_RDONLY);
        if (fd >= 0)
            close(fd);
        fd = open("/dev/zero", O_RDONLY);
        if (fd >= 0)
            close(fd);
    }
}

/* §1: Open/close stress — 100 rapid cycles */
static void stress_open_close(void) {
    for (int i = 0; i < 100; i++) {
        int fd = open_kcov();
        close(fd);
    }
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0,
              "INIT_TRACE after 100 open/close cycles");
    close(fd);
    printf("  INFO: 100 open/close cycles OK\n");
}

/* §2: Max-size buffer — 4096 entries, 1K burst */
static void stress_max_buffer(void) {
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 4096), 0, "INIT_TRACE size=4096");
    size_t sz = 4096 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap 4096 entries");
    if (buf == MAP_FAILED) {
        close(fd);
        return;
    }
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
    heavy_burst(1000);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    uint64_t n = buf[0];
    printf("  INFO: 4096-entry buffer, 1k burst → %lu entries\n", n);
    CHECK(n > 0, "coverage in 4096-entry buffer");
    CHECK(n <= 4096, "count ≤ 4096");
    munmap(buf, sz);
    close(fd);
}

/* §3: Rapid ENABLE/DISABLE cycling — 100 cycles */
static void stress_enable_disable_cycle(void) {
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64), 0, "INIT_TRACE");
    size_t sz = 64 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap");
    if (buf == MAP_FAILED) {
        close(fd);
        return;
    }
    for (int i = 0; i < 100; i++) {
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        getpid();
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    }
    printf("  INFO: 100 ENABLE/DISABLE cycles, count=%lu\n", buf[0]);
    CHECK(buf[0] > 0, "coverage across 100 cycles");
    munmap(buf, sz);
    close(fd);
}

/* §4: Multi-thread — 8 threads, 50 cycles each */
typedef struct {
    int tid;
    uint64_t count;
    int ok;
    int cycles;
} stress_thr_t;
static void *stress_thread(void *arg) {
    stress_thr_t *s = (stress_thr_t *)arg;
    s->ok = 0;
    int fd = open("/dev/kcov", O_RDWR);
    if (fd < 0)
        return NULL;
    if (ioctl(fd, KCOV_INIT_TRACE, 64)) {
        close(fd);
        return NULL;
    }
    size_t sz = 64 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (buf == MAP_FAILED) {
        close(fd);
        return NULL;
    }
    for (int c = 0; c < s->cycles; c++) {
        if (ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC)) {
            munmap(buf, sz);
            close(fd);
            return NULL;
        }
        for (volatile int i = 0; i < 200; i++) {
            gettid();
            getpid();
        }
        ioctl(fd, KCOV_DISABLE, 0);
    }
    s->count = buf[0];
    s->tid = (int)gettid();
    munmap(buf, sz);
    close(fd);
    s->ok = 1;
    return NULL;
}
static void stress_many_threads(void) {
#define N 8
    pthread_t t[N];
    stress_thr_t r[N];
    for (int i = 0; i < N; i++) {
        r[i].cycles = 50;
        pthread_create(&t[i], NULL, stress_thread, &r[i]);
    }
    for (int i = 0; i < N; i++)
        pthread_join(t[i], NULL);
    int all_ok = 1;
    for (int i = 0; i < N; i++) {
        if (!r[i].ok)
            all_ok = 0;
        printf("  INFO: thread %d → %lu\n", r[i].tid, r[i].count);
    }
    CHECK(all_ok, "all 8 threads completed");
#undef N
}

/* §5: Open/init/mmap/enable/disable/close cycles — 100 iterations.
 * Each iteration uses a fresh fd (Linux: KCOV_INIT_TRACE is one-shot per fd). */
static void stress_buffer_replace(void) {
    for (int i = 0; i < 100; i++) {
        int fd = open_kcov();
        size_t sz = (64 + (i % 4) * 64) * sizeof(uint64_t);
        CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 64 + (i % 4) * 64), 0, "INIT");
        uint64_t *buf =
            mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        CHECK_PTR(buf, 1, "mmap");
        if (buf != MAP_FAILED) {
            CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
            for (volatile int j = 0; j < 100; j++)
                getpid();
            CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
            munmap(buf, sz);
        }
        close(fd);
    }
    printf("  INFO: 100 open/init/enable/close cycles OK\n");
}

/* §6: Heavy syscall storm — 1K burst */
static void stress_syscall_storm(void) {
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 1024), 0, "INIT_TRACE size=1024");
    size_t sz = 1024 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap");
    if (buf == MAP_FAILED) {
        close(fd);
        return;
    }
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
    heavy_burst(1000);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    uint64_t n = buf[0];
    printf("  INFO: 1k burst on 1024-entry buffer → %lu entries\n", n);
    CHECK(n > 0, "coverage during storm");
    CHECK(n <= 1024, "count ≤ 1024");
    munmap(buf, sz);
    close(fd);
}

/* §7: 8K buffer, 1K burst */
static void stress_max_entries(void) {
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 8192), 0, "INIT_TRACE size=8192");
    size_t sz = 8192 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap");
    if (buf == MAP_FAILED) {
        close(fd);
        return;
    }
    CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
    heavy_burst(1000);
    CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    uint64_t n = buf[0];
    printf("  INFO: 1k burst on 8192-entry buffer → %lu entries\n", n);
    CHECK(n > 0, "coverage in 8K buffer");
    CHECK(n <= 8192, "count ≤ 8192");
    munmap(buf, sz);
    close(fd);
}

/* §8: Mode toggle — 100 TRACE_PC ENABLE/DISABLE cycles; TRACE_CMP is
 * intentionally rejected with EINVAL until CMP hooks are implemented. */
static void stress_mode_toggle(void) {
    int fd = open_kcov();
    CHECK_RET(ioctl(fd, KCOV_INIT_TRACE, 128), 0, "INIT_TRACE");
    size_t sz = 128 * sizeof(uint64_t);
    uint64_t *buf = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK_PTR(buf, 1, "mmap");
    if (buf == MAP_FAILED) {
        close(fd);
        return;
    }
    /* Verify TRACE_CMP is properly rejected */
    CHECK_ERR(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_CMP), EINVAL, "CMP reject (not yet implemented)");
    /* PC mode toggle stress */
    for (int i = 0; i < 100; i++) {
        CHECK_RET(ioctl(fd, KCOV_ENABLE, KCOV_TRACE_PC), 0, "ENABLE");
        for (volatile int j = 0; j < 50; j++)
            getpid();
        CHECK_RET(ioctl(fd, KCOV_DISABLE, 0), 0, "DISABLE");
    }
    printf("  INFO: 100 PC mode-toggle cycles, count=%lu\n", buf[0]);
    munmap(buf, sz);
    close(fd);
}

int main(void) {
    TEST_START("KCOV stress suite");
    int probe = open_kcov();
    if (probe < 0) {
        printf("SKIP: /dev/kcov not available\n");
        TEST_DONE();
    }
    close(probe);

    stress_open_close();
    stress_buffer_replace();
    stress_mode_toggle();
    stress_enable_disable_cycle();
    stress_max_buffer();
    stress_syscall_storm();
    stress_max_entries();
    stress_many_threads();

    TEST_DONE();
}
