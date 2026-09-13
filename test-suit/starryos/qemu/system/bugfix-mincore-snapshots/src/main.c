#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static int failures;
#define CHECK(condition, name) do { \
    if (condition) printf("PASS: %s\n", name); \
    else { printf("FAIL: %s errno=%d\n", name, errno); failures++; } \
} while (0)
#define REQUIRE(condition) do { if (!(condition)) { perror(#condition); exit(1); } } while (0)

static void shared_residency_from_another_mapping(size_t page)
{
    unsigned char *shared = mmap(NULL, page, PROT_READ | PROT_WRITE,
                                 MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    REQUIRE(shared != MAP_FAILED);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) { shared[0] = 0x5a; _exit(0); }
    int status = 0;
    REQUIRE(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    unsigned char resident = 0xa5;
    long result = syscall(SYS_mincore, shared, page, &resident);
    CHECK(result == 0 && resident == 1, "mincore observes a shared page populated by another MM");
    munmap(shared, page);
}

static void file_residency_access_policy(size_t page)
{
    int fd = open("/bin/sh", O_RDONLY);
    REQUIRE(fd >= 0);
    struct stat metadata;
    REQUIRE(fstat(fd, &metadata) == 0);
    REQUIRE(metadata.st_uid == 0 && !(metadata.st_mode & S_IWOTH));
    size_t length = (((size_t)metadata.st_size + page - 1) / page + 1) * page;
    unsigned char *mapping = mmap(NULL, length, PROT_READ, MAP_PRIVATE, fd, 0);
    REQUIRE(mapping != MAP_FAILED);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        if (geteuid() == 0 && setuid(1000) != 0) _exit(2);
        unsigned char result = 0;
        long status = syscall(SYS_mincore, mapping + length - page, page, &result);
        _exit(status == 0 && result == 1 ? 0 : 3);
    }
    int status = 0;
    REQUIRE(waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "mincore follows file residency access policy");
    munmap(mapping, length);
    close(fd);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    REQUIRE(page > 0);
    unsigned char *mapping = mmap(NULL, page * 67, PROT_READ | PROT_WRITE,
                                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(mapping != MAP_FAILED);
    REQUIRE(madvise(mapping, page * 67, MADV_NOHUGEPAGE) == 0);
    mapping[0] = 1;
    mapping[page * 64] = 2;
    unsigned char residency[67];
    memset(residency, 0xa5, sizeof(residency));
    REQUIRE(mprotect(mapping, page * 67, PROT_NONE) == 0);
    long result = syscall(SYS_mincore, mapping, page * 67, residency);
    CHECK(result == 0, "mincore accepts PROT_NONE VMAs");
    CHECK(residency[0] == 1 && residency[64] == 1,
          "resident pages remain resident after PROT_NONE");
    CHECK(residency[1] == 0 && residency[65] == 0,
          "untouched PROT_NONE pages remain nonresident");

    REQUIRE(mprotect(mapping, page * 67, PROT_READ | PROT_WRITE) == 0);
    REQUIRE(munmap(mapping + page * 65, page * 2) == 0);
    memset(residency, 0xa5, sizeof(residency));
    errno = 0;
    result = syscall(SYS_mincore, mapping, page * 66, residency);
    CHECK(result == -1 && errno == ENOMEM, "mincore reports a later VMA hole");
    CHECK(residency[0] == 1 && residency[64] == 1,
          "mincore publishes its completed prefix before a later hole");
    CHECK(residency[1] == 0 && residency[63] == 0 && residency[65] == 0xa5,
          "mincore preserves prefix bits and leaves the hole output untouched");
    munmap(mapping, page * 65);

    const size_t pages = 4097;
    mapping = mmap(NULL, page * pages, PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    REQUIRE(mapping != MAP_FAILED);
    REQUIRE(madvise(mapping, page * pages, MADV_NOHUGEPAGE) == 0);
    unsigned char *large_result = malloc(pages);
    REQUIRE(large_result != NULL);
    for (size_t i = 0; i < pages; i += 257) mapping[i * page] = 0x5a;
    memset(large_result, 0xa5, pages);
    REQUIRE(syscall(SYS_mincore, mapping, page * pages, large_result) == 0);
    int matches = 1;
    for (size_t i = 0; i < pages; i++) {
        if (large_result[i] != (i % 257 == 0)) matches = 0;
    }
    CHECK(matches, "mincore retains exact residency across multiple bounded batches");
    free(large_result);
    munmap(mapping, page * pages);
    shared_residency_from_another_mapping(page);
    file_residency_access_policy(page);
    printf("MINCORE_SNAPSHOTS_%s failures=%d\n", failures ? "FAILED" : "PASSED", failures);
    return failures != 0;
}
