#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
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
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MADV_POPULATE_READ
#define MADV_POPULATE_READ 22
#define MADV_POPULATE_WRITE 23
#endif

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

static uint64_t read_proc_value(const char *path, const char *key, int method) {
    int fd = open(path, O_RDONLY);
    REQUIRE(fd >= 0);
    char buffer[8192], name[64];
    size_t used = 0;
    for (;;) {
        size_t remaining = sizeof(buffer) - 1 - used;
        REQUIRE(remaining > 0);
        long count;
        if (method == SYS_readv) {
            size_t first = remaining < 17 ? remaining : 17;
            struct iovec parts[] = {
                {buffer + used, first}, {buffer + used + first, remaining - first},
            };
            count = syscall(SYS_readv, fd, parts, 2);
        } else if (method == SYS_pread64) {
            count = syscall(SYS_pread64, fd, buffer + used, remaining, (off_t)used);
        } else {
            count = syscall(SYS_read, fd, buffer + used, remaining);
        }
        REQUIRE(count >= 0 && (size_t)count <= remaining);
        if (!count) break;
        used += count;
    }
    if (method == SYS_pread64) REQUIRE(lseek(fd, 0, SEEK_CUR) == 0);
    REQUIRE(close(fd) == 0);
    buffer[used] = '\0';
    unsigned long long value;
    for (char *line = buffer; line; ) {
        if (sscanf(line, "%63s %llu", name, &value) == 2 && !strcmp(name, key)) return value;
        char *end = strchr(line, '\n');
        line = end ? end + 1 : NULL;
    }
    fprintf(stderr, "missing swap statistic %s in %s\n", key, path);
    REQUIRE(0);
    return 0;
}

static uint64_t proc_value(const char *path, const char *key) {
    uint64_t value = read_proc_value(path, key, SYS_read);
    REQUIRE(read_proc_value(path, key, SYS_readv) == value);
    REQUIRE(read_proc_value(path, key, SYS_pread64) == value);
    return value;
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

static void swap_out_one(void) {
    pageout(region, PAGE);
    unsigned char residency;
    REQUIRE(mincore(region, PAGE, &residency) == 0 && !(residency & 1));
}

static void check_swapped_lifecycle(void) {
    region = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(region != MAP_FAILED);
    fill(0, 1, 31);

    const int inheritance[] = {MADV_DONTFORK, MADV_WIPEONFORK};
    for (size_t i = 0; i < sizeof(inheritance) / sizeof(inheritance[0]); i++) {
        REQUIRE(syscall(SYS_madvise, region, PAGE, inheritance[i]) == 0);
        swap_out_one();
        uint64_t parent_swap_kb = proc_value("/proc/self/status", "VmSwap:");
        REQUIRE(parent_swap_kb >= PAGE / 1024);
        pid_t child = fork();
        REQUIRE(child >= 0);
        if (!child) {
            REQUIRE(proc_value("/proc/self/status", "VmSwap:") == parent_swap_kb - PAGE / 1024);
            unsigned char residency;
            if (inheritance[i] == MADV_DONTFORK) {
                errno = 0;
                REQUIRE(mincore(region, PAGE, &residency) == -1 && errno == ENOMEM);
            } else {
                for (size_t j = 0; j < PAGE; j++) REQUIRE(region[j] == 0);
                fill(0, 1, 32);
            }
            _exit(0);
        }
        int status;
        REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
        REQUIRE(verify(0, 1, 31));
        REQUIRE(syscall(SYS_madvise, region, PAGE,
                        inheritance[i] == MADV_DONTFORK ? MADV_DOFORK : MADV_KEEPONFORK) == 0);
    }
    puts("SWAP_INHERITANCE_PASSED");

    const int populate[] = {MADV_POPULATE_READ, MADV_POPULATE_WRITE};
    for (size_t i = 0; i < sizeof(populate) / sizeof(populate[0]); i++) {
        swap_out_one();
        REQUIRE(syscall(SYS_madvise, region, PAGE, populate[i]) == 0);
        unsigned char residency;
        REQUIRE(mincore(region, PAGE, &residency) == 0 && (residency & 1));
        REQUIRE(verify(0, 1, 31));
        fill(0, 1, 31);
    }
    swap_out_one();
    REQUIRE(syscall(SYS_mlock2, region, PAGE, 0) == 0);
    unsigned char residency;
    REQUIRE(mincore(region, PAGE, &residency) == 0 && (residency & 1));
    REQUIRE(verify(0, 1, 31));
    REQUIRE(munlock(region, PAGE) == 0);
    puts("SWAP_POPULATE_PASSED");

    swap_out_one();
    unsigned char *source = region;
    region = mremap(source, PAGE, PAGE, MREMAP_MAYMOVE | MREMAP_DONTUNMAP);
    REQUIRE(region != MAP_FAILED && region != source);
    REQUIRE(mincore(region, PAGE, &residency) == 0 && !(residency & 1));
    REQUIRE(mincore(source, PAGE, &residency) == 0 && !(residency & 1));
    REQUIRE(verify(0, 1, 31));
    for (size_t j = 0; j < PAGE; j++) REQUIRE(source[j] == 0);
    memset(source, 0x5a, PAGE);
    REQUIRE(verify(0, 1, 31));
    REQUIRE(munmap(source, PAGE) == 0);

    struct swap_stats before = swap_stats();
    swap_out_one();
    struct swap_stats swapped = swap_stats();
    REQUIRE(swapped.free + PAGE == before.free && swapped.process == before.process + PAGE);
    REQUIRE(munmap(region, PAGE) == 0);
    errno = 0;
    REQUIRE(mincore(region, PAGE, &residency) == -1 && errno == ENOMEM);
    struct swap_stats released = swap_stats();
    REQUIRE(released.free == before.free && released.process == before.process);
    REQUIRE(released.compressed == before.compressed);
    REQUIRE(released.in == swapped.in);
    REQUIRE(mmap(region, PAGE, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == region);
    for (size_t j = 0; j < PAGE; j++) REQUIRE(region[j] == 0);
    REQUIRE(munmap(region, PAGE) == 0);
    puts("SWAP_MOVE_UNMAP_PASSED");
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
    check_swapped_lifecycle();
    puts("SWAP_TEST_PASSED");
    return 0;
}
