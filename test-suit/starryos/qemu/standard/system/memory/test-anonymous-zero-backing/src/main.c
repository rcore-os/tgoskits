#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

static void require(int condition, const char *operation)
{
    if (!condition) {
        fprintf(stderr, "ANONYMOUS_ZERO_BACKING_FAILED: %s (errno=%d)\n", operation, errno);
        exit(1);
    }
}

static void expect_zero(const volatile unsigned char *bytes, size_t size)
{
    for (size_t offset = 0; offset < size; ++offset) {
        require(bytes[offset] == 0, "zero contents or sibling isolation");
    }
}

static unsigned char *map_anonymous(size_t size)
{
    void *address = (void *)syscall(SYS_mmap, NULL, size, PROT_READ | PROT_WRITE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(address != MAP_FAILED, "mmap private anonymous");
    return address;
}

static void kernel_write(unsigned char *destination)
{
    require(syscall(SYS_uname, destination) == 0, "uname into read-faulted page");
    const struct utsname *name = (const struct utsname *)destination;
    require(name->sysname[0] != '\0', "uname copied bytes into private destination");
}

int main(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    require(page_size > 0 && (size_t)page_size >= sizeof(struct utsname), "page size");
    size_t page = (size_t)page_size;
    size_t size = 4 * page;
    unsigned char *memory = map_anonymous(size);
    expect_zero(memory, size);

    unsigned char resident[4] = {0};
    require(syscall(SYS_mincore, memory, size, resident) == 0, "mincore zero backing");
    for (size_t index = 0; index < 4; ++index) {
        require((resident[index] & 1) != 0, "read-faulted zero page is resident");
    }

    pid_t child = fork();
    require(child >= 0, "fork zero-backed mappings");
    if (child == 0) {
        require(syscall(SYS_mprotect, memory, page, PROT_READ) == 0, "mprotect read-only");
        require(syscall(SYS_mprotect, memory, page, PROT_READ | PROT_WRITE) == 0,
                "mprotect first write");
        memory[0] = 0x5a;
        require(memory[0] == 0x5a, "child first write");
        expect_zero(memory + 1, page - 1);
        kernel_write(memory + page);
        expect_zero(memory + 2 * page, 2 * page);
        require(syscall(SYS_munmap, memory, size) == 0, "child munmap");
        _exit(0);
    }
    int status = 0;
    require(waitpid(child, &status, 0) == child, "waitpid");
    require(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child zero isolation checks");
    expect_zero(memory, size);

    kernel_write(memory + page);
    expect_zero(memory, page);
    expect_zero(memory + 2 * page, 2 * page);
    require(syscall(SYS_madvise, memory + page, page, MADV_DONTNEED) == 0,
            "discard materialized anonymous page");
    expect_zero(memory, size);

    unsigned char *destination = map_anonymous(size);
    void *moved = (void *)syscall(SYS_mremap, memory, size, size,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, destination);
    require(moved == destination, "move zero-backed mappings");
    expect_zero(destination, size);
    destination[2 * page] = 0xa5;
    require(destination[2 * page] == 0xa5, "write moved zero page");
    expect_zero(destination, 2 * page);
    expect_zero(destination + 2 * page + 1, 2 * page - 1);

    require(syscall(SYS_mprotect, destination, page, PROT_NONE) == 0,
            "protect zero backing with no access");
    require(syscall(SYS_madvise, destination, size, MADV_DONTNEED) == 0,
            "discard mixed zero/private and no-access pages");
    require(syscall(SYS_mprotect, destination, page, PROT_READ | PROT_WRITE) == 0,
            "restore discarded zero page access");
    expect_zero(destination, size);
    require(syscall(SYS_munmap, destination, size) == 0, "final munmap");
    puts("ANONYMOUS_ZERO_BACKING_PASSED");
    return 0;
}
