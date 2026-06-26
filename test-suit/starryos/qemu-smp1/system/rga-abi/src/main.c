#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

/* --- /dev/rga ioctls (MultiRGA v1.3.1 ABI). --- */
#define RGA_BLIT_SYNC            0x5017u
#define RGA_IOC_GET_DRVIER_VERS  0x801C7201u /* _IOR('r',1,rga_version_t=28)    */
#define RGA_IOC_GET_HW_VERSION   0x80907202u /* _IOR('r',2,rga_hw_versions=144) */
#define RGA_IOC_IMPORT_BUFFER    0xC0107203u /* _IOWR('r',3,rga_buffer_pool=16) */
#define RGA_IOC_RELEASE_BUFFER   0x40107204u /* _IOW('r',4,rga_buffer_pool=16)  */
#define RGA_IOC_REQUEST_CREATE   0x80047205u /* _IOR('r',5,uint32=4)            */

#define RGA_DMA_BUFFER 0u

/* --- /dev/dma_heap (mainline uapi) --- */
struct dma_heap_allocation_data {
    uint64_t len;
    uint32_t fd;
    uint32_t fd_flags;
    uint64_t heap_flags;
};
#define DMA_HEAP_IOCTL_ALLOC 0xC0184800u

/* rga_external_buffer (288 bytes) — userspace fills memory/type, kernel fills handle. */
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
   handle_flag@360 (full_csc is 40 bytes: flag + 3x csc_coe_t@12). rga_img_info_t:
   yrgb_addr@+0, format@+24, act_w@+28, vir_w@+36. */
static void put_u16(unsigned char *b, size_t off, uint16_t v) { memcpy(b + off, &v, 2); }
static void put_u32(unsigned char *b, size_t off, uint32_t v) { memcpy(b + off, &v, 4); }
static void put_u64(unsigned char *b, size_t off, uint64_t v) { memcpy(b + off, &v, 8); }

int main(void)
{
    TEST_START("rga-abi: /dev/rga version + import->blit->release handle plumbing (no hw)");

    int fd = open("/dev/rga", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        printf("RGA_ABI_TEST SKIP: /dev/rga absent\n");
        TEST_DONE();
    }

    /* --- 1. Version queries (librga calls these at init). --- */
    unsigned char drv_ver[28];
    memset(drv_ver, 0, sizeof(drv_ver));
    CHECK(ioctl(fd, RGA_IOC_GET_DRVIER_VERS, drv_ver) == 0, "GET_DRVIER_VERSION ok");
    uint32_t drv_major;
    memcpy(&drv_major, drv_ver, 4);
    CHECK(drv_major == 1, "driver major version == 1");

    unsigned char hw_ver[144];
    memset(hw_ver, 0, sizeof(hw_ver));
    CHECK(ioctl(fd, RGA_IOC_GET_HW_VERSION, hw_ver) == 0, "GET_HW_VERSION ok");
    uint32_t hw_count;
    memcpy(&hw_count, hw_ver + 140, 4); /* rga_hw_versions_t.size @ 140 */
    CHECK(hw_count == 1, "one RGA core reported");

    /* --- 2. Need a real dma-buf to import. --- */
    int heap = open("/dev/dma_heap/system", O_RDWR | O_CLOEXEC);
    if (heap < 0) {
        printf("RGA_ABI_TEST: /dev/dma_heap absent, skipping handle path\n");
        close(fd);
        TEST_DONE();
    }
    struct dma_heap_allocation_data alloc = {0};
    alloc.len = 64 * 1024;
    CHECK(ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &alloc) == 0, "dma_heap alloc ok");
    int dfd = (int)alloc.fd;

    /* --- 3. IMPORT_BUFFER: dma-buf fd -> handle. --- */
    struct rga_external_buffer ext;
    memset(&ext, 0, sizeof(ext));
    ext.memory = (uint64_t)dfd;
    ext.type = RGA_DMA_BUFFER;
    struct rga_buffer_pool pool = {.buffers_ptr = (uint64_t)(uintptr_t)&ext, .size = 1};
    CHECK(ioctl(fd, RGA_IOC_IMPORT_BUFFER, &pool) == 0, "IMPORT_BUFFER ok");
    CHECK(ext.handle != 0, "kernel assigned a non-zero handle");
    uint32_t handle = ext.handle;

    /* --- 4. BLIT_SYNC referencing the handle. With no RGA2 core in QEMU the kernel
       must resolve the handle + parse the request, THEN return ENODEV at the device
       check. ENODEV (not EBADF/EINVAL) proves the import->resolve->parse chain ran. --- */
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
    int rc = ioctl(fd, RGA_BLIT_SYNC, req);
    int e = errno;
    CHECK(rc != 0, "BLIT_SYNC fails on QEMU (no RGA hardware)");
    CHECK(e == ENODEV,
          "BLIT_SYNC returns ENODEV — handle resolved + request parsed before the device check");

    /* --- 5. REQUEST_CREATE returns a request id. --- */
    uint32_t req_id = 0;
    CHECK(ioctl(fd, RGA_IOC_REQUEST_CREATE, &req_id) == 0, "REQUEST_CREATE ok");
    CHECK(req_id != 0, "request id is non-zero");

    /* --- 6. RELEASE_BUFFER frees the handle. --- */
    pool.buffers_ptr = (uint64_t)(uintptr_t)&ext;
    pool.size = 1;
    CHECK(ioctl(fd, RGA_IOC_RELEASE_BUFFER, &pool) == 0, "RELEASE_BUFFER ok");

    /* Releasing the same handle again must fail (it's gone). */
    errno = 0;
    int rc2 = ioctl(fd, RGA_IOC_RELEASE_BUFFER, &pool);
    CHECK(rc2 != 0, "double RELEASE_BUFFER fails (handle already freed)");

    close(dfd);
    close(heap);
    close(fd);
    TEST_DONE();
}
