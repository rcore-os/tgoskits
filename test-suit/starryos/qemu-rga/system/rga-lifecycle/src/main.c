#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

/*
 * /dev/rga open-file-description (OFD) handle-lifetime regressions (no RGA2 hardware needed).
 *
 * Each open("/dev/rga") gets its own per-open session holding that open's handle table;
 * dup/fork share the session (same underlying file object) and it is freed only when the
 * last reference closes. Three cases lock that in:
 *
 *   A. same fd, cross-thread: a handle imported by one thread must resolve (and release)
 *      from a sibling thread sharing the same fd (they share one session).
 *   B. two independent opens: each open is its own session, so importing on fd_b then
 *      closing fd_a must leave fd_b's handle valid.
 *   C. fork: a child sharing the inherited fd sees the parent's handle (shared session), and
 *      the parent's handle stays valid after the child exits (freed only at last close).
 *      This is the case the process-id (tgid) model got wrong: the child's tgid differs, so
 *      it could not see the parent's handle.
 *
 * With no RGA2 core on QEMU virt, a resolved handle makes RGA_BLIT_SYNC reach the device
 * check and fail with ENODEV; an unresolved handle short-circuits to EBADF at lookup. So
 * "ENODEV, not EBADF" is the signal that the handle was found.
 */

/* --- /dev/rga ioctls (MultiRGA v1.3.1 ABI). --- */
#define RGA_BLIT_SYNC          0x5017u
#define RGA_IOC_IMPORT_BUFFER  0xC0107203u /* _IOWR('r',3,rga_buffer_pool) */
#define RGA_IOC_RELEASE_BUFFER 0x40107204u /* _IOW('r',4,rga_buffer_pool)  */
#define RGA_DMA_BUFFER 0u

/* --- /dev/dma_heap (mainline uapi). --- */
struct dma_heap_allocation_data {
    uint64_t len;
    uint32_t fd;
    uint32_t fd_flags;
    uint64_t heap_flags;
};
#define DMA_HEAP_IOCTL_ALLOC 0xC0184800u

/* rga_external_buffer (288 bytes): userspace fills memory/type, kernel fills handle. */
struct rga_external_buffer {
    uint64_t memory;
    uint32_t type;
    uint32_t handle;
    uint32_t width, height, format, size; /* rga_memory_parm */
    uint8_t  reserve[252];
};
struct rga_buffer_pool {
    uint64_t buffers_ptr;
    uint32_t size;
};

/* Verified rga_req field offsets (LP64, librga rga_ioctl.h): render_mode@0, src@8, dst@64,
   handle_flag@360. rga_img_info_t: yrgb_addr@+0, format@+24, act_w@+28, vir_w@+36. */
static void put_u16(unsigned char *b, size_t off, uint16_t v) { memcpy(b + off, &v, 2); }
static void put_u32(unsigned char *b, size_t off, uint32_t v) { memcpy(b + off, &v, 4); }
static void put_u64(unsigned char *b, size_t off, uint64_t v) { memcpy(b + off, &v, 8); }

/* Allocate a dma-buf from /dev/dma_heap/system. Returns the fd, or -1 on failure. */
static int alloc_dmabuf(int heap, uint64_t len)
{
    struct dma_heap_allocation_data alloc;
    memset(&alloc, 0, sizeof(alloc));
    alloc.len = len;
    if (ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &alloc) != 0) {
        return -1;
    }
    return (int)alloc.fd;
}

/* Import a dma-buf fd on rga_fd. Returns the assigned handle, or 0 on failure. */
static uint32_t import_dmabuf(int rga_fd, int dfd)
{
    struct rga_external_buffer ext;
    memset(&ext, 0, sizeof(ext));
    ext.memory = (uint64_t)dfd;
    ext.type = RGA_DMA_BUFFER;
    struct rga_buffer_pool pool = {
        .buffers_ptr = (uint64_t)(uintptr_t)&ext,
        .size = 1,
    };
    if (ioctl(rga_fd, RGA_IOC_IMPORT_BUFFER, &pool) != 0) {
        return 0;
    }
    return ext.handle;
}

/* Release a handle on rga_fd. Returns the ioctl rc (0 == success). */
static int release_handle(int rga_fd, uint32_t handle)
{
    struct rga_external_buffer ext;
    memset(&ext, 0, sizeof(ext));
    ext.handle = handle;
    struct rga_buffer_pool pool = {
        .buffers_ptr = (uint64_t)(uintptr_t)&ext,
        .size = 1,
    };
    return ioctl(rga_fd, RGA_IOC_RELEASE_BUFFER, &pool);
}

/* Same-size 64x64 RGBA copy referencing `handle` for src+dst. Returns the errno the
   BLIT_SYNC failed with (0 if it unexpectedly succeeded). */
static int blit_with_handle(int rga_fd, uint32_t handle)
{
    unsigned char req[512];
    memset(req, 0, sizeof(req));
    req[0] = 0;                /* render_mode = BITBLT */
    put_u64(req, 8, handle);   /* src.yrgb_addr = handle */
    put_u32(req, 8 + 24, 0);   /* src.format = RGBA_8888 */
    put_u16(req, 8 + 28, 64);  /* src.act_w */
    put_u16(req, 8 + 30, 64);  /* src.act_h */
    put_u16(req, 8 + 36, 64);  /* src.vir_w */
    put_u16(req, 8 + 38, 64);  /* src.vir_h */
    put_u64(req, 64, handle);  /* dst.yrgb_addr = handle */
    put_u32(req, 64 + 24, 0);  /* dst.format = RGBA_8888 */
    put_u16(req, 64 + 28, 64); /* dst.act_w */
    put_u16(req, 64 + 30, 64); /* dst.act_h */
    put_u16(req, 64 + 36, 64); /* dst.vir_w */
    put_u16(req, 64 + 38, 64); /* dst.vir_h */
    req[360] = 1;              /* handle_flag = 1 (addrs are handles) */

    errno = 0;
    if (ioctl(rga_fd, RGA_BLIT_SYNC, req) == 0) {
        return 0;
    }
    return errno;
}

/* Regression A: a sibling thread blits + releases a handle imported by the main thread on
   the SAME fd. */
struct cross_thread_ctx {
    int rga_fd;
    uint32_t handle;
    int blit_errno;
    int release_rc;
};
static void *cross_thread_fn(void *arg)
{
    struct cross_thread_ctx *c = (struct cross_thread_ctx *)arg;
    c->blit_errno = blit_with_handle(c->rga_fd, c->handle);
    c->release_rc = release_handle(c->rga_fd, c->handle);
    return NULL;
}

int main(void)
{
    TEST_START("rga-lifecycle: /dev/rga open-file-description handle lifetime (no hw)");

    int fd = open("/dev/rga", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("RGA_LIFECYCLE SKIP: /dev/rga absent\n");
        TEST_DONE();
    }
    int heap = open("/dev/dma_heap/system", O_RDWR | O_CLOEXEC);
    if (heap < 0) {
        printf("RGA_LIFECYCLE SKIP: /dev/dma_heap/system absent\n");
        close(fd);
        TEST_DONE();
    }

    /* ---- Regression A: same fd, cross-thread handle visibility. ---- */
    int d0 = alloc_dmabuf(heap, 64 * 1024);
    CHECK(d0 >= 0, "A: dma_heap alloc for the cross-thread buffer");
    uint32_t h0 = import_dmabuf(fd, d0);
    CHECK(h0 != 0, "A: main thread imported a handle");

    struct cross_thread_ctx ctx = {
        .rga_fd = fd,
        .handle = h0,
        .blit_errno = -1,
        .release_rc = -1,
    };
    pthread_t th;
    CHECK(pthread_create(&th, NULL, cross_thread_fn, &ctx) == 0, "A: spawn sibling thread");
    CHECK(pthread_join(th, NULL) == 0, "A: join sibling thread");
    CHECK(ctx.blit_errno == ENODEV,
          "A: sibling-thread BLIT_SYNC returns ENODEV (main thread's handle resolved cross-thread)");
    CHECK(ctx.release_rc == 0, "A: sibling thread released the shared handle");
    close(d0);

    /* ---- Regression B: two independent opens; closing one keeps the other's handle valid. ---- */
    int fd_a = open("/dev/rga", O_RDWR | O_CLOEXEC);
    int fd_b = open("/dev/rga", O_RDWR | O_CLOEXEC);
    CHECK(fd_a >= 0 && fd_b >= 0, "B: two independent /dev/rga opens");

    int db = alloc_dmabuf(heap, 64 * 1024);
    CHECK(db >= 0, "B: dma_heap alloc for the fd_b buffer");
    uint32_t hb = import_dmabuf(fd_b, db);
    CHECK(hb != 0, "B: imported a handle via fd_b");

    /* Precondition: the handle resolves while both fds are open. */
    CHECK(blit_with_handle(fd_b, hb) == ENODEV, "B: fd_b handle resolves before close (ENODEV)");

    close(fd_a); /* close ONE of two open descriptions */

    /* fd_a and fd_b are independent opens, hence independent sessions, so closing fd_a
       cannot touch fd_b's handle. (The earlier process-wide model reclaimed all of a
       process's handles on any close, which returned EBADF here.) */
    CHECK(blit_with_handle(fd_b, hb) == ENODEV,
          "B: fd_b handle still resolves after closing fd_a (ENODEV, not EBADF)");

    close(db);
    close(fd_b);

    /* ---- Regression C: fork shares the per-open session; child exit doesn't free it. ---- */
    int dc = alloc_dmabuf(heap, 64 * 1024);
    CHECK(dc >= 0, "C: dma_heap alloc for the fork buffer");
    uint32_t hc = import_dmabuf(fd, dc);
    CHECK(hc != 0, "C: parent imported a handle");

    pid_t pid = fork();
    CHECK(pid >= 0, "C: fork");
    if (pid == 0) {
        /* Child shares the inherited fd's session, so the parent's handle must resolve.
           Use _exit so the inherited stdio buffer is not flushed (no duplicate output). */
        int child_errno = blit_with_handle(fd, hc);
        _exit(child_errno == ENODEV ? 0 : 1);
    }
    int wstatus = 0;
    CHECK(waitpid(pid, &wstatus, 0) == pid, "C: waitpid child");
    CHECK(WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0,
          "C: child resolved the parent's handle over fork (shared session)");
    /* The child closed its inherited fd on exit, but the parent still holds fd, so the
       session and its handle must survive (freed only at last close). */
    CHECK(blit_with_handle(fd, hc) == ENODEV,
          "C: parent handle still valid after child exit (session freed only at last close)");
    CHECK(release_handle(fd, hc) == 0, "C: parent released the handle");
    close(dc);

    close(heap);
    close(fd);
    TEST_DONE();
}
