#define _GNU_SOURCE

#include <fcntl.h>
#include <inttypes.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__aarch64__)
static const _Alignas(64) uint8_t file_backed_bytes[96] = {
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
    0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
    0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37,
    0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f,
    0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
    0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f,
    0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57,
    0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f,
};

static int unaligned_ldp_q_matches(const uint8_t *source)
{
    _Alignas(16) uint8_t actual[32];

    /* Match glibc's faulting instruction shape exactly: the effective address
     * is source_end - 64 and is deliberately not 16-byte aligned. */
    const uint8_t *source_end = source + 64;
    __asm__ volatile(
        "ldp q0, q1, [%1, #-64]\n\t"
        "stp q0, q1, [%0]"
        :
        : "r"(actual), "r"(source_end)
        : "v0", "v1", "memory");

    for (size_t index = 0; index < sizeof(actual); ++index) {
        if (actual[index] != source[index]) {
            return 0;
        }
    }
    return 1;
}

static int unaligned_ldp_q_file_mmap(void)
{
    const char *path = "/lib/aarch64-linux-gnu/libm.so.6";
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror("open libm");
        return 0;
    }

    const size_t map_length = 4096;
    const uint8_t *mapping = mmap(NULL, map_length, PROT_READ, MAP_PRIVATE, fd, 0);
    if (mapping == MAP_FAILED) {
        perror("mmap libm");
        close(fd);
        return 0;
    }

    int matches = unaligned_ldp_q_matches(mapping + 1);
    if (munmap((void *)mapping, map_length) != 0) {
        perror("munmap libm");
        matches = 0;
    }
    if (close(fd) != 0) {
        perror("close libm");
        matches = 0;
    }
    return matches;
}

static int unaligned_ldp_q_after_mremap(void)
{
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) {
        return 0;
    }

    uint8_t *source = mmap(NULL, (size_t)page_size, PROT_READ | PROT_WRITE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint8_t *target = mmap(NULL, (size_t)page_size, PROT_NONE,
                           MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (source == MAP_FAILED || target == MAP_FAILED) {
        if (source != MAP_FAILED) {
            munmap(source, (size_t)page_size);
        }
        if (target != MAP_FAILED) {
            munmap(target, (size_t)page_size);
        }
        return 0;
    }

    for (size_t index = 0; index < 64; ++index) {
        source[index] = (uint8_t)index;
    }
    void *moved = mremap(source, (size_t)page_size, (size_t)page_size,
                         MREMAP_MAYMOVE | MREMAP_FIXED, target);
    if (moved == MAP_FAILED) {
        perror("mremap");
        munmap(source, (size_t)page_size);
        munmap(target, (size_t)page_size);
        return 0;
    }

    int matches = unaligned_ldp_q_matches((const uint8_t *)moved + 1);
    if (munmap(moved, (size_t)page_size) != 0) {
        perror("munmap mremap target");
        matches = 0;
    }
    return matches;
}
#endif

int main(void)
{
#if !defined(__aarch64__)
    puts("aarch64_unaligned_access=SKIP");
    return 0;
#else
    _Alignas(uint64_t) const uint8_t bytes[] = {
        0xa5, 0x78, 0x56, 0x34, 0x12, 0x00, 0x00, 0x00, 0x00
    };
    const uint8_t *unaligned = bytes + 1;
    uint64_t value;

    __asm__ volatile("ldr %0, [%1]" : "=r"(value) : "r"(unaligned) : "memory");
    if (value != UINT64_C(0x12345678)) {
        printf("aarch64_unaligned_access=FAIL value=%" PRIx64 "\n", value);
        return 1;
    }

    puts("aarch64_unaligned_access=PASS");

    _Alignas(64) uint8_t stack_bytes[96];
    for (size_t index = 0; index < sizeof(stack_bytes); ++index) {
        stack_bytes[index] = (uint8_t)index;
    }
    if (!unaligned_ldp_q_matches(stack_bytes + 1)) {
        puts("aarch64_unaligned_ldp_q_stack=FAIL");
        return 1;
    }
    puts("aarch64_unaligned_ldp_q_stack=PASS");

    if (!unaligned_ldp_q_matches(file_backed_bytes + 1)) {
        puts("aarch64_unaligned_ldp_q_file=FAIL");
        return 1;
    }
    puts("aarch64_unaligned_ldp_q_file=PASS");

    if (!unaligned_ldp_q_file_mmap()) {
        puts("aarch64_unaligned_ldp_q_file_mmap=FAIL");
        return 1;
    }
    puts("aarch64_unaligned_ldp_q_file_mmap=PASS");

    if (!unaligned_ldp_q_after_mremap()) {
        puts("aarch64_unaligned_ldp_q_mremap=FAIL");
        return 1;
    }
    puts("aarch64_unaligned_ldp_q_mremap=PASS");
    return 0;
#endif
}
