// Supplemental file and partial-progress coverage absent from madv_populate.c.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef MADV_POPULATE_READ
#define MADV_POPULATE_READ 22
#define MADV_POPULATE_WRITE 23
#endif
#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "madvise-populate:%d: %s (errno=%d)\n", \
            __LINE__, #condition, errno); exit(1); } } while (0)

static size_t page;

static void advise(void *addr, size_t size, int advice)
{
    CHECK(syscall(SYS_madvise, addr, size, advice) == 0);
}

static void reject(void *addr, size_t size, int advice, int expected)
{
    errno = 0;
    CHECK(syscall(SYS_madvise, addr, size, advice) == -1);
    CHECK(errno == expected);
}

static int resident(void *addr)
{
    unsigned char value;
    CHECK(syscall(SYS_mincore, addr, page, &value) == 0);
    return value & 1;
}

static void errors_preserve_completed_prefix(void)
{
    unsigned char *base = mmap(NULL, 3 * page, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(base != MAP_FAILED);
    CHECK(mprotect(base + page, page, PROT_NONE) == 0);
    reject(base, 3 * page, MADV_POPULATE_READ, EINVAL);
    CHECK(resident(base) && !resident(base + 2 * page));
    CHECK(mprotect(base + page, page, PROT_READ) == 0);
    reject(base + page, page, MADV_POPULATE_WRITE, EINVAL);
    CHECK(!resident(base + page));
    advise(base + page, page, MADV_POPULATE_READ);
    CHECK(resident(base + page));
    CHECK(munmap(base + page, page) == 0);
    reject(base, 3 * page, MADV_POPULATE_WRITE, ENOMEM);
    CHECK(!resident(base + 2 * page));
    // Check address admission before the zero-length fast path.
    reject(base + 1, 0, MADV_POPULATE_READ, EINVAL);
    advise(base + page, 0, MADV_POPULATE_WRITE);
    CHECK(munmap(base, 3 * page) == 0);
}

static void file_backing_errors(void)
{
    FILE *file = tmpfile();
    CHECK(file != NULL && ftruncate(fileno(file), page) == 0);
    unsigned char value = 0x63;
    CHECK(pwrite(fileno(file), &value, 1, 0) == 1);
    unsigned char *base = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE, fileno(file), 0);
    CHECK(base != MAP_FAILED);
    // EOF is reported, not delivered as SIGBUS and not swallowed as success.
    reject(base, 2 * page, MADV_POPULATE_READ, EFAULT);
    CHECK(resident(base) && base[0] == value);
    advise(base, page, MADV_POPULATE_WRITE);
    base[0] = 0x34;
    unsigned char actual = 0;
    CHECK(pread(fileno(file), &actual, 1, 0) == 1 && actual == value);
    CHECK(munmap(base, 2 * page) == 0);

    base = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_SHARED, fileno(file), 0);
    CHECK(base != MAP_FAILED);
    advise(base, page, MADV_POPULATE_READ);
    advise(base, page, MADV_POPULATE_WRITE);
    CHECK(base[0] == value);
    base[0] = 0x78;
    CHECK(msync(base, page, MS_SYNC) == 0);
    CHECK(pread(fileno(file), &actual, 1, 0) == 1 && actual == 0x78);
    CHECK(munmap(base, page) == 0);
    CHECK(fclose(file) == 0);
}

int main(void)
{
    page = (size_t)sysconf(_SC_PAGESIZE);
    errors_preserve_completed_prefix();
    file_backing_errors();
    puts("MADVISE_POPULATE_SUPPLEMENT_PASSED");
    return 0;
}
