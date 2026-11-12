// VMA inheritance must survive splits and relocation without sharing wiped pages.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "madvise-inheritance:%d: %s (errno=%d)\n", \
            __LINE__, #condition, errno); exit(1); } } while (0)

static size_t page;

static void advise(void *addr, size_t size, int advice)
{
    CHECK(syscall(SYS_madvise, addr, size, advice) == 0);
}

static void wait_ok(pid_t child)
{
    int status;
    CHECK(child > 0);
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static void check_pages(const unsigned char *base, const unsigned char *values,
                        size_t count)
{
    for (size_t i = 0; i < count; i++)
        for (size_t j = 0; j < page; j++)
            CHECK(base[i * page + j] == values[i]);
}

static void inheritance(void)
{
    unsigned char *base = mmap(NULL, 4 * page, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(base != MAP_FAILED);
    memset(base, 0x5a, 4 * page);
    advise(base + page, 2 * page, MADV_WIPEONFORK);
    // Split and rejoin permissions without losing inheritance metadata.
    CHECK(mprotect(base + page, page, PROT_READ) == 0);
    CHECK(mprotect(base + page, page, PROT_READ | PROT_WRITE) == 0);
    advise(base + 2 * page, page, MADV_KEEPONFORK);
    // mremap must carry the policy, not leave it at the former VA.
    void *target = mmap(NULL, 4 * page, PROT_NONE,
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(target != MAP_FAILED);
    CHECK(mremap(base + page, page, page, MREMAP_MAYMOVE | MREMAP_FIXED,
                 (char *)target + page) == (char *)target + page);
    unsigned char *wiped = (unsigned char *)target + page;
    CHECK(wiped[0] == 0x5a);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        const unsigned char zero[] = {0};
        check_pages(wiped, zero, 1);
        CHECK(base[0] == 0x5a && base[2 * page] == 0x5a);
        memset(wiped, 0xa5, page);
        pid_t grandchild = fork();
        CHECK(grandchild >= 0);
        if (grandchild == 0) {
            check_pages(wiped, zero, 1);
            _exit(0);
        }
        wait_ok(grandchild);
        CHECK(wiped[0] == 0xa5);
        advise(wiped, page, MADV_KEEPONFORK);
        grandchild = fork();
        CHECK(grandchild >= 0);
        if (grandchild == 0) {
            const unsigned char kept[] = {0xa5};
            check_pages(wiped, kept, 1);
            wiped[0] = 0x33;
            _exit(0);
        }
        wait_ok(grandchild);
        CHECK(wiped[0] == 0xa5);
        _exit(0);
    }
    wait_ok(child);
    CHECK(wiped[0] == 0x5a && base[2 * page] == 0x5a);
    CHECK(munmap(base, 4 * page) == 0);
    CHECK(munmap(target, 4 * page) == 0);
}

static void rejection_and_partial_progress(void)
{
    unsigned char *base = mmap(NULL, 3 * page, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(base != MAP_FAILED);
    memset(base, 0x71, 3 * page);
    CHECK(munmap(base + page, page) == 0);
    errno = 0;
    CHECK(syscall(SYS_madvise, base, 3 * page, MADV_WIPEONFORK) == -1);
    CHECK(errno == ENOMEM);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        CHECK(base[0] == 0 && base[2 * page] == 0);
        _exit(0);
    }
    wait_ok(child);
    CHECK(base[0] == 0x71 && base[2 * page] == 0x71);
    CHECK(munmap(base, 3 * page) == 0);

    FILE *file = tmpfile();
    CHECK(file != NULL && ftruncate(fileno(file), page) == 0);
    void *private_file = mmap(NULL, page, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE, fileno(file), 0);
    void *shared = mmap(NULL, page, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(private_file != MAP_FAILED && shared != MAP_FAILED);
    void *invalid[] = {private_file, shared};
    for (size_t i = 0; i < 2; i++) {
        errno = 0;
        CHECK(syscall(SYS_madvise, invalid[i], page, MADV_WIPEONFORK) == -1);
        CHECK(errno == EINVAL);
        advise(invalid[i], page, MADV_KEEPONFORK);
        CHECK(munmap(invalid[i], page) == 0);
    }
    CHECK(fclose(file) == 0);
}

int main(void)
{
    page = (size_t)sysconf(_SC_PAGESIZE);
    inheritance();
    rejection_and_partial_progress();
    puts("MADVISE_INHERITANCE_PASSED");
    return 0;
}
