#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/sysinfo.h>
#include <sys/wait.h>
#include <unistd.h>

#define PAGE 4096
#define PAGES 128
#define THREADS 4
static unsigned char *region;
static atomic_int ready, stop, failed;

#define REQUIRE(test) do { if (!(test)) { \
    fprintf(stderr, "SWAP_FAILED line=%d: %s errno=%d\n", __LINE__, #test, errno); \
    exit(1); } } while (0)

static void pageout(void *address, size_t bytes) {
    REQUIRE(syscall(SYS_madvise, address, bytes, MADV_PAGEOUT) == 0);
}

static uint64_t proc_value(const char *path, const char *key) {
    FILE *file = fopen(path, "r");
    REQUIRE(file != NULL);
    char line[256], name[64];
    unsigned long long value;
    while (fgets(line, sizeof(line), file)) {
        if (sscanf(line, "%63s %llu", name, &value) == 2 && !strcmp(name, key)) {
            REQUIRE(fclose(file) == 0);
            return value;
        }
    }
    fprintf(stderr, "missing swap statistic %s in %s\n", key, path);
    REQUIRE(0);
    return 0;
}

struct swap_stats {
    uint64_t total, free, compressed, process, in, out;
};

static struct swap_stats swap_stats(void) {
    struct sysinfo info;
    REQUIRE(syscall(SYS_sysinfo, &info) == 0 && info.mem_unit > 0);
    struct swap_stats stats = {
        .total = (uint64_t)info.totalswap * info.mem_unit,
        .free = (uint64_t)info.freeswap * info.mem_unit,
        .compressed = proc_value("/proc/meminfo", "SwapCompressed:") * 1024,
        .process = proc_value("/proc/self/status", "VmSwap:") * 1024,
        .in = proc_value("/proc/vmstat", "pswpin"),
        .out = proc_value("/proc/vmstat", "pswpout"),
    };
    REQUIRE(stats.total > 0 && stats.free <= stats.total);
    REQUIRE(proc_value("/proc/meminfo", "SwapTotal:") == stats.total / 1024);
    REQUIRE(proc_value("/proc/meminfo", "SwapFree:") == stats.free / 1024);
    return stats;
}

static uint64_t word(size_t page, size_t index, unsigned round) {
    return ((uint64_t)round << 32) ^ ((uint64_t)page << 16) ^ (index % 32);
}

static void fill(size_t first, size_t count, unsigned round) {
    for (size_t page = first; page < first + count; page++) {
        uint64_t *words = (uint64_t *)(region + PAGE * page);
        for (size_t i = 0; i < PAGE / sizeof(*words); i++) words[i] = word(page, i, round);
    }
}

static int verify(size_t first, size_t count, unsigned round) {
    for (size_t page = first; page < first + count; page++) {
        uint64_t *words = (uint64_t *)(region + PAGE * page);
        for (size_t i = 0; i < PAGE / sizeof(*words); i++)
            if (words[i] != word(page, i, round)) return 0;
    }
    return 1;
}

static void *writer(void *argument) {
    size_t first = (uintptr_t)argument * (PAGES / THREADS);
    unsigned round = 1;
    atomic_fetch_add(&ready, 1);
    do {
        fill(first, PAGES / THREADS, round);
        if (!verify(first, PAGES / THREADS, round)) {
            atomic_store(&failed, 1);
            break;
        }
        round++;
    } while (!atomic_load(&stop));
    return NULL;
}

int main(void) {
    REQUIRE(sysconf(_SC_PAGESIZE) == PAGE);
    struct sysinfo info;
    REQUIRE(sysinfo(&info) == 0 && info.totalswap > 0);
    region = mmap(NULL, PAGE * PAGES, PROT_READ | PROT_WRITE,
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(region != MAP_FAILED);
    fill(0, PAGES, 17);
    /* The isolated guest has no competing swap workload during this phase. */
    struct swap_stats before = swap_stats();
    pageout(region, PAGE * PAGES);
    unsigned char residency[PAGES];
    REQUIRE(mincore(region, PAGE * PAGES, residency) == 0);
    size_t evicted = 0;
    for (size_t i = 0; i < PAGES; i++) evicted += !(residency[i] & 1);
    REQUIRE(evicted > 0);
    printf("SWAP_NONRESIDENT pages=%zu\n", evicted);
    fflush(stdout);

    struct swap_stats swapped = swap_stats();
    REQUIRE(swapped.total == before.total);
    REQUIRE(before.free >= evicted * PAGE);
    REQUIRE(swapped.free == before.free - evicted * PAGE);
    REQUIRE(swapped.process == before.process + evicted * PAGE);
    REQUIRE(swapped.compressed > before.compressed);
    REQUIRE(swapped.out == before.out + evicted && swapped.in == before.in);
    REQUIRE(verify(0, PAGES, 17));
    struct swap_stats restored = swap_stats();
    REQUIRE(restored.total == before.total && restored.free == before.free);
    REQUIRE(restored.process == before.process && restored.compressed == before.compressed);
    REQUIRE(restored.in == before.in + evicted && restored.out == swapped.out);
    puts("SWAP_ACCOUNTING_PASSED");

    /* Keep the fork check on nonresident content after the accounting round trip. */
    pageout(region, PAGE * PAGES);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        REQUIRE(verify(0, PAGES, 17));
        fill(0, PAGES, 23);
        pageout(region, PAGE * PAGES);
        REQUIRE(verify(0, PAGES, 23));
        _exit(0);
    }
    REQUIRE(verify(0, PAGES, 17));
    int status;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    REQUIRE(verify(0, PAGES, 17));

    pageout(region, PAGE * PAGES);
    void *destination = mmap(NULL, PAGE * PAGES, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(destination != MAP_FAILED);
    void *moved = mremap(region, PAGE * PAGES, PAGE * PAGES,
                        MREMAP_MAYMOVE | MREMAP_FIXED, destination);
    REQUIRE(moved == destination);
    region = moved;
    REQUIRE(verify(0, PAGES, 17));

    pageout(region, PAGE);
    REQUIRE(mincore(region, PAGE, residency) == 0 && !(residency[0] & 1));
    REQUIRE(syscall(SYS_mlock, region, PAGE) == 0);
    /* Check residency before reading: verify() itself would fault the page in. */
    REQUIRE(mincore(region, PAGE, residency) == 0 && (residency[0] & 1));
    REQUIRE(verify(0, 1, 17));
    errno = 0;
    REQUIRE(syscall(SYS_madvise, region, PAGE, MADV_PAGEOUT) == -1 && errno == EINVAL);
    REQUIRE(mincore(region, PAGE, residency) == 0 && (residency[0] & 1));
    REQUIRE(munlock(region, PAGE) == 0);
    puts("SWAP_MLOCK_NONRESIDENT_PASSED");

    pthread_t threads[THREADS];
    for (uintptr_t i = 0; i < THREADS; i++)
        REQUIRE(pthread_create(&threads[i], NULL, writer, (void *)i) == 0);
    while (atomic_load(&ready) != THREADS) sched_yield();
    for (int round = 0; round < 64; round++) pageout(region, PAGE * PAGES);
    atomic_store(&stop, 1);
    for (int i = 0; i < THREADS; i++) REQUIRE(pthread_join(threads[i], NULL) == 0);
    REQUIRE(!atomic_load(&failed));

    pageout(region, PAGE * PAGES);
    REQUIRE(madvise(region, PAGE * PAGES, MADV_DONTNEED) == 0);
    for (size_t i = 0; i < PAGE * PAGES; i++) REQUIRE(region[i] == 0);
    fill(0, PAGES, 19);
    pageout(region, PAGE * PAGES);
    REQUIRE(madvise(region, PAGE * PAGES, MADV_WILLNEED) == 0);
    REQUIRE(verify(0, PAGES, 19));
    pageout(region, PAGE * PAGES);
    REQUIRE(mmap(region, PAGE * PAGES, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == region);
    for (size_t i = 0; i < PAGE * PAGES; i++) REQUIRE(region[i] == 0);
    REQUIRE(munmap(region, PAGE * PAGES) == 0);
    puts("SWAP_TEST_PASSED");
    return 0;
}
