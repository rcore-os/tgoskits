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
    pageout(region, PAGE * PAGES);
    unsigned char residency[PAGES];
    REQUIRE(mincore(region, PAGE * PAGES, residency) == 0);
    size_t evicted = 0;
    for (size_t i = 0; i < PAGES; i++) evicted += !(residency[i] & 1);
    REQUIRE(evicted > 0);
    printf("SWAP_NONRESIDENT pages=%zu\n", evicted);
    fflush(stdout);

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

    REQUIRE(mlock(region, PAGE) == 0);
    errno = 0;
    REQUIRE(syscall(SYS_madvise, region, PAGE, MADV_PAGEOUT) == -1 && errno == EINVAL);
    REQUIRE(mincore(region, PAGE, residency) == 0 && (residency[0] & 1));
    REQUIRE(munlock(region, PAGE) == 0);

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
