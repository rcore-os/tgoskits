#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

static void require(int condition, const char *operation)
{
    if (!condition) {
        int saved_errno = errno;
        fprintf(stderr, "PRIVATE_CACHE_BACKING_FAILED: %s (errno=%d: %s)\n",
                operation, saved_errno, strerror(saved_errno));
        exit(1);
    }
}

static void expect_bytes(const volatile unsigned char *bytes, size_t length,
                         unsigned char expected)
{
    for (size_t offset = 0; offset < length; ++offset) {
        if (bytes[offset] != expected) {
            fprintf(stderr, "mapping mismatch: offset=%zu expected=0x%02x actual=0x%02x\n",
                    offset, expected, bytes[offset]);
        }
        require(bytes[offset] == expected, "file mapping bytes or private isolation");
    }
}

static unsigned char *map_file(int fd, size_t length, off_t offset, int protection)
{
    void *address = (void *)syscall(SYS_mmap, NULL, length, protection, MAP_PRIVATE, fd, offset);
    require(address != MAP_FAILED, "mmap private file");
    return address;
}

static void fill_file(int fd, size_t page, unsigned char first)
{
    unsigned char *bytes = malloc(page);
    require(bytes != NULL, "allocate file preparation buffer");
    for (size_t index = 0; index < 4; ++index) {
        memset(bytes, first + index, page);
        require(syscall(SYS_pwrite64, fd, bytes, page, index * page) == (long)page,
                "pwrite complete page");
    }
    free(bytes);
    require(syscall(SYS_fsync, fd) == 0, "fsync prepared file");
}

static void private_writes_and_kernel_copies(int fd, size_t page)
{
    unsigned char *first = map_file(fd, 3 * page, 0, PROT_READ | PROT_WRITE);
    unsigned char *second = map_file(fd, 3 * page, 0, PROT_READ);
    for (size_t index = 0; index < 3; ++index) {
        expect_bytes(first + index * page, page, 0x30 + index);
        expect_bytes(second + index * page, page, 0x30 + index);
    }
    pid_t child = fork();
    require(child >= 0, "fork cache-backed mappings");
    if (child == 0) {
        require(syscall(SYS_mprotect, first, page, PROT_READ) == 0, "revoke private write");
        require(syscall(SYS_mprotect, first, page, PROT_READ | PROT_WRITE) == 0,
                "restore private write");
        first[0] = 0xa5;
        require(first[0] == 0xa5, "private child store");
        expect_bytes(second, page, 0x30);
        require(syscall(SYS_uname, first + page) == 0, "kernel copy into cache-backed page");
        require(((struct utsname *)(first + page))->sysname[0] != '\0', "uname produced bytes");
        expect_bytes(second + page, page, 0x31);
        _exit(0);
    }
    int status = 0;
    require(waitpid(child, &status, 0) == child, "wait for private writer");
    require(WIFEXITED(status) && WEXITSTATUS(status) == 0, "child private write checks");
    for (size_t index = 0; index < 3; ++index) {
        expect_bytes(first + index * page, page, 0x30 + index);
    }
    first[0] = 0x69;
    expect_bytes(second, page, 0x30);
    require(syscall(SYS_pread64, fd, first + 2 * page, 32, 0) == 32,
            "read into a cache-backed user destination");
    expect_bytes(first + 2 * page, 32, 0x30);
    expect_bytes(first + 2 * page + 32, page - 32, 0x32);
    expect_bytes(second + 2 * page, page, 0x32);
    require(syscall(SYS_msync, first, 3 * page, MS_SYNC) == 0, "msync private mapping");
    require(syscall(SYS_madvise, first, page, MADV_DONTNEED) == 0, "discard private copy");
    expect_bytes(first, page, 0x30);
    require(syscall(SYS_munmap, first, 3 * page) == 0, "unmap first view");
    require(syscall(SYS_munmap, second, 3 * page) == 0, "unmap second view");
}

static void fork_inaccessible_pages(int fd, size_t page)
{
    unsigned char *bytes = map_file(fd, 2 * page, 0, PROT_READ | PROT_WRITE);
    expect_bytes(bytes, page, 0x30);
    expect_bytes(bytes + page, page, 0x31);
    bytes[page] = 0xab;
    require(syscall(SYS_mprotect, bytes, 2 * page, PROT_NONE) == 0,
            "make cache and private pages inaccessible before fork");
    pid_t child = fork();
    require(child >= 0, "fork inaccessible pages");
    if (child == 0) {
        require(syscall(SYS_mprotect, bytes, 2 * page, PROT_READ | PROT_WRITE) == 0,
                "restore child inaccessible pages");
        expect_bytes(bytes, page, 0x30);
        expect_bytes(bytes + page, 1, 0xab);
        expect_bytes(bytes + page + 1, page - 1, 0x31);
        bytes[page] = 0x72;
        require(bytes[page] == 0x72, "child write after inaccessible fork");
        _exit(0);
    }
    int status = 0;
    require(waitpid(child, &status, 0) == child, "wait for inaccessible-page child");
    require(WIFEXITED(status) && WEXITSTATUS(status) == 0,
            "inaccessible fork preserves private contents");
    require(syscall(SYS_mprotect, bytes, 2 * page, PROT_READ) == 0,
            "restore parent inaccessible pages");
    expect_bytes(bytes, page, 0x30);
    expect_bytes(bytes + page, 1, 0xab);
    expect_bytes(bytes + page + 1, page - 1, 0x31);
    require(syscall(SYS_munmap, bytes, 2 * page) == 0, "unmap inaccessible-fork views");
}

static void relocate_missing_and_resident_pages(int fd, size_t page)
{
    unsigned char *source = map_file(fd, 2 * page, page, PROT_READ | PROT_WRITE);
    expect_bytes(source, page, 0x31);
    // Leave page 2 missing. Its first fault after moving must retain its file offset.
    void *destination = (void *)syscall(SYS_mmap, NULL, 2 * page, PROT_NONE,
                                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(destination != MAP_FAILED, "reserve relocation target");
    void *moved = (void *)syscall(SYS_mremap, source, 2 * page, 2 * page,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, destination);
    require(moved == destination, "mremap private file");
    unsigned char *bytes = moved;
    expect_bytes(bytes, page, 0x31);
    expect_bytes(bytes + page, page, 0x32);
    require(syscall(SYS_mprotect, bytes, page, PROT_NONE) == 0, "split with PROT_NONE");
    require(syscall(SYS_mprotect, bytes, page, PROT_READ | PROT_WRITE) == 0,
            "restore split page");
    bytes[0] = 0x55;
    expect_bytes(bytes + 1, page - 1, 0x31);
    require(syscall(SYS_munmap, bytes, 2 * page) == 0, "unmap relocated file");
}

static void relocate_inaccessible_pages(int fd, size_t page)
{
    // Use one VMA so this checks page ownership, not merging adjacent VMAs.
    unsigned char *bytes = map_file(fd, 2 * page, page, PROT_READ | PROT_WRITE);
    expect_bytes(bytes, page, 0x31);
    expect_bytes(bytes + page, page, 0x32);
    bytes[0] = 0x55;
    require(syscall(SYS_mprotect, bytes, 2 * page, PROT_NONE) == 0,
            "make file and private pages inaccessible before move");
    void *destination = (void *)syscall(SYS_mmap, NULL, 2 * page, PROT_NONE,
                                       MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    require(destination != MAP_FAILED, "reserve inaccessible relocation target");
    void *moved = (void *)syscall(SYS_mremap, bytes, 2 * page, 2 * page,
                                 MREMAP_MAYMOVE | MREMAP_FIXED, destination);
    require(moved == destination, "mremap inaccessible file");
    bytes = moved;
    require(syscall(SYS_mprotect, bytes, 2 * page, PROT_READ) == 0,
            "restore relocated inaccessible file");
    expect_bytes(bytes, 1, 0x55);
    expect_bytes(bytes + 1, page - 1, 0x31);
    expect_bytes(bytes + page, page, 0x32);
    require(syscall(SYS_munmap, bytes, 2 * page) == 0, "unmap inaccessible relocation");
}

static void truncate_regrow_and_partial_eof(int fd, size_t page)
{
    unsigned char *bytes = map_file(fd, 4 * page, 0, PROT_READ);
    for (size_t index = 0; index < 4; ++index) {
        expect_bytes(bytes + index * page, page, 0x30 + index);
    }
    require(syscall(SYS_ftruncate, fd, 0) == 0, "truncate mapped file");
    fill_file(fd, page, 0x40);
    for (size_t index = 0; index < 4; ++index) {
        expect_bytes(bytes + index * page, page, 0x40 + index);
    }
    require(syscall(SYS_munmap, bytes, 4 * page) == 0, "unmap regrown file");
    require(syscall(SYS_ftruncate, fd, page + 9) == 0, "truncate partial EOF page");
    bytes = map_file(fd, page, page, PROT_READ | PROT_WRITE);
    expect_bytes(bytes, 9, 0x41);
    expect_bytes(bytes + 9, page - 9, 0);
    bytes[0] = 0x77;
    require(syscall(SYS_msync, bytes, page, MS_SYNC) == 0, "msync partial private page");
    unsigned char file_byte = 0;
    require(syscall(SYS_pread64, fd, &file_byte, 1, page) == 1, "read unchanged file byte");
    require(file_byte == 0x41, "private write must not modify file");
    require(syscall(SYS_munmap, bytes, page) == 0, "unmap partial EOF page");
}

int main(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    require(page_size > 0 && (size_t)page_size >= sizeof(struct utsname), "page size");
    size_t page = (size_t)page_size;
    // The root directory is on the test disk, unlike the guest's tmpfs /tmp.
    char path[] = "/root/private-cache-backing-XXXXXX";
    int fd = mkstemp(path);
    require(fd >= 0, "create disk-backed test file");
    require(unlink(path) == 0, "unlink only this generated test file");
    fill_file(fd, page, 0x30);
    private_writes_and_kernel_copies(fd, page);
    fork_inaccessible_pages(fd, page);
    relocate_missing_and_resident_pages(fd, page);
    relocate_inaccessible_pages(fd, page);
    truncate_regrow_and_partial_eof(fd, page);
    require(close(fd) == 0, "close test file");
    puts("PRIVATE_CACHE_BACKING_PASSED");
    return 0;
}
