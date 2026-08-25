#include <stdint.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

#define LARGE_ALLOCATION_BYTES (4U * 1024U * 1024U)
#define PAGE_BYTES 4096U
#define LAZY_PAGE_SENTINEL UINT64_C(0x6d616c6c6f636e67)

static void write_all(const char *buffer, size_t length)
{
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, buffer, length);
        if (written <= 0) {
            _Exit(EXIT_FAILURE);
        }
        buffer += (size_t)written;
        length -= (size_t)written;
    }
}

static void write_literal(const char *text, size_t length)
{
    write_all(text, length);
}

#define WRITE_LITERAL(text) write_literal((text), sizeof(text) - 1)

static int test_first_store_after_lazy_mmap(void)
{
    volatile uint64_t *page = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        WRITE_LITERAL("LAZY_MMAP_FAILED\n");
        return -1;
    }

    *page = LAZY_PAGE_SENTINEL;
    int result = 0;
    if (*page != LAZY_PAGE_SENTINEL) {
        WRITE_LITERAL("LAZY_MMAP_FIRST_STORE_LOST\n");
        result = -1;
    }
    if (munmap((void *)page, PAGE_BYTES) != 0) {
        WRITE_LITERAL("LAZY_MUNMAP_FAILED\n");
        result = -1;
    }
    return result;
}

static int test_mallocng_allocations(void)
{
    unsigned char *malloc_allocation = malloc(LARGE_ALLOCATION_BYTES);
    if (malloc_allocation == NULL) {
        WRITE_LITERAL("MALLOCNG_MALLOC_NULL\n");
        return -1;
    }
    malloc_allocation[0] = 0xa5;
    malloc_allocation[LARGE_ALLOCATION_BYTES - 1] = 0x5a;
    if (malloc_allocation[0] != 0xa5 ||
        malloc_allocation[LARGE_ALLOCATION_BYTES - 1] != 0x5a) {
        WRITE_LITERAL("MALLOCNG_MALLOC_WRITE_LOST\n");
        free(malloc_allocation);
        return -1;
    }
    free(malloc_allocation);

    unsigned char *allocation = calloc(LARGE_ALLOCATION_BYTES, 1);
    if (allocation == NULL) {
        WRITE_LITERAL("MALLOCNG_CALLOC_NULL\n");
        return -1;
    }

    for (size_t index = 0; index < LARGE_ALLOCATION_BYTES; ++index) {
        if (allocation[index] != 0) {
            WRITE_LITERAL("MALLOCNG_CALLOC_NONZERO\n");
            free(allocation);
            return -1;
        }
    }

    free(allocation);
    return 0;
}

int main(void)
{
    WRITE_LITERAL("STARRY_CALLOC_MALLOCNG_BEGIN\n");
    if (test_first_store_after_lazy_mmap() != 0 || test_mallocng_allocations() != 0) {
        return EXIT_FAILURE;
    }
    WRITE_LITERAL("STARRY_CALLOC_MALLOCNG_PASSED\n");
    return EXIT_SUCCESS;
}
