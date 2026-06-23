#define _GNU_SOURCE
#include "test_framework.h"

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

/* linux/dma-heap.h + linux/dma-buf.h — mainline-stable uapi. */
struct dma_heap_allocation_data {
    uint64_t len;
    uint32_t fd;
    uint32_t fd_flags;
    uint64_t heap_flags;
};
struct dma_buf_sync {
    uint64_t flags;
};
#define DMA_HEAP_IOCTL_ALLOC 0xC0184800u
#define DMA_BUF_IOCTL_SYNC   0x40086200u
#define DMA_BUF_SYNC_WRITE   (2u << 0)
#define DMA_BUF_SYNC_START   (0u << 2)
#define DMA_BUF_SYNC_END     (1u << 2)

#define LEN (64 * 1024)

static unsigned char pat(int i) { return (unsigned char)(i * 7 + 1); }

int main(void)
{
    TEST_START("dma-heap: alloc/mmap/lifetime/sync");

    int heap = open("/dev/dma_heap/system", O_RDWR | O_CLOEXEC);
    if (heap < 0) {
        /* Kernel built without the dma-heap feature — nothing to exercise here. */
        printf("DMABUF_TEST SKIP: /dev/dma_heap/system absent\n");
        TEST_DONE();
    }

    struct dma_heap_allocation_data alloc = {0};
    alloc.len = LEN;
    CHECK(ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &alloc) == 0, "DMA_HEAP_IOCTL_ALLOC succeeds");
    CHECK(alloc.fd > 2, "returned dma-buf fd is valid");
    int dfd = (int)alloc.fd;

    unsigned char *p = mmap(0, LEN, PROT_READ | PROT_WRITE, MAP_SHARED, dfd, 0);
    CHECK(p != MAP_FAILED, "mmap(dmabuf_fd) succeeds");

    struct dma_buf_sync s_start = {.flags = DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE};
    CHECK(ioctl(dfd, DMA_BUF_IOCTL_SYNC, &s_start) == 0, "DMA_BUF_IOCTL_SYNC START");
    for (int i = 0; i < LEN; i++) p[i] = pat(i);
    struct dma_buf_sync s_end = {.flags = DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE};
    CHECK(ioctl(dfd, DMA_BUF_IOCTL_SYNC, &s_end) == 0, "DMA_BUF_IOCTL_SYNC END");

    /* Lifetime: close the fd BEFORE munmap. The mapping must stay valid because the VMA anchors
       the backing object's Arc. */
    close(dfd);
    int ok = 1;
    for (int i = 0; i < LEN; i++) {
        if (p[i] != pat(i)) { ok = 0; break; }
    }
    CHECK(ok, "buffer readable after fd close, before munmap (anchor holds pages)");

    CHECK(munmap(p, LEN) == 0, "munmap succeeds");
    close(heap);

    TEST_DONE();
}
