/* User input import and memfd write-seal ordering for the write family.
 * Linux validates descriptor/address geometry before write_begin(), where
 * shmem checks seals. Actual payload faults follow write_begin(). Test this
 * through raw syscalls, including checked allocation of the input copy.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <unistd.h>

#ifndef F_ADD_SEALS
#define F_ADD_SEALS 1033
#endif
#ifndef F_SEAL_WRITE
#define F_SEAL_WRITE 0x0008
#endif
#ifndef MFD_ALLOW_SEALING
#define MFD_ALLOW_SEALING 0x0002
#endif

static int passed, failed;
static const char *const operations[] = {
    "write", "writev", "pwrite64", "pwritev", "pwritev2"
};

static long write_input(int operation, int fd, const void *buffer, size_t length)
{
    struct iovec iov = { .iov_base = (void *)buffer, .iov_len = length };
    switch (operation) {
    case 0: return syscall(SYS_write, fd, buffer, length);
    case 1: return syscall(SYS_writev, fd, &iov, 1);
    case 2: return syscall(SYS_pwrite64, fd, buffer, length, 0);
    case 3: return syscall(SYS_pwritev, fd, &iov, 1, 0, 0);
    default: return syscall(SYS_pwritev2, fd, &iov, 1, 0, 0, 0);
    }
}

static void check_result(int operation, const char *scenario, long result,
                         long expected, int expected_errno)
{
    if (result == expected && (expected != -1 || errno == expected_errno)) {
        printf("PASS: %s %s\n", operations[operation], scenario);
        passed++;
    } else {
        printf("FAIL: %s %s: result=%ld errno=%d; expected=%ld errno=%d\n",
               operations[operation], scenario, result, errno,
               expected, expected_errno);
        failed++;
    }
}

int main(void)
{
    int fd = (int)syscall(SYS_memfd_create, "write-input-order", MFD_ALLOW_SEALING);
    if (fd < 0 || ftruncate(fd, 4096) != 0) {
        perror("prepare memfd");
        return 1;
    }
    void *bad = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (bad == MAP_FAILED || munmap(bad, 4096) != 0) {
        perror("prepare unmapped input");
        close(fd);
        return 1;
    }
    const char valid[] = "copy";
    for (int op = 0; op < 5; op++) {
        check_result(op, "valid input", write_input(op, fd, valid, sizeof(valid)),
                     sizeof(valid), 0);
        check_result(op, "unmapped input", write_input(op, fd, bad, sizeof(valid)),
                     -1, EFAULT);
    }
    if (fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE) != 0) {
        perror("seal memfd");
        close(fd);
        return 1;
    }
    for (int op = 0; op < 5; op++) {
        check_result(op, "sealed valid input", write_input(op, fd, valid, sizeof(valid)),
                     -1, EPERM);
        check_result(op, "seal precedes payload fault", write_input(op, fd, bad, sizeof(valid)),
                     -1, EPERM);
        check_result(op, "address geometry precedes seal",
                     write_input(op, fd, (void *)UINTPTR_MAX, sizeof(valid)), -1, EFAULT);
        check_result(op, "sealed empty write", write_input(op, fd, valid, 0), 0, 0);
    }
    for (int op = 1; op < 5; op++) {
        if (op == 2)
            continue;
        long number = op == 1 ? SYS_writev : op == 3 ? SYS_pwritev : SYS_pwritev2;
        long result = syscall(number, fd, (void *)1, 1, 0, 0, 0);
        check_result(op, "descriptor import precedes seal", result, -1, EFAULT);
    }
    close(fd);

    fd = (int)syscall(SYS_memfd_create, "large-write-input", 0);
    if (fd < 0) {
        perror("prepare large import");
        return 1;
    }
    fflush(stdout);
    check_result(0, "large input validated before copy allocation",
                 syscall(SYS_write, fd, bad, (size_t)1 << 46), -1, EFAULT);
    close(fd);
    printf("write import: %d passed, %d failed\n", passed, failed);
    printf("%s\n", failed ? "TEST FAILED" : "TEST PASSED");
    return failed != 0;
}
