#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

/*
 * rga-blit-hw — userspace end-to-end RGA2 blit test over /dev/rga + /dev/dma_heap.
 *
 * This replaces the old in-kernel `rga-selftest` boot hook: instead of driving the kernel RGA
 * driver from a Starry boot hook, it validates the SAME hardware through the userspace ABI a real
 * librga client uses (alloc dma-buf -> mmap -> import handle -> RGA_BLIT_SYNC -> read back pixels).
 *
 * Dual role:
 *   - On QEMU there is no RGA2 engine: the first BLIT_SYNC returns ENODEV (after the kernel has
 *     resolved the imported handle + parsed the request), so we print SKIP and exit 0. The
 *     no-device ABI plumbing is already covered by the rga-abi case; here QEMU only proves the
 *     program builds and the import/parse path is reached.
 *   - On the OrangePi-5-Plus board the engine runs the blit; we assert the produced pixels:
 *       copy      RGBA same-size            -> dst == src                (exact)
 *       downscale solid-colour W*H -> W/2*H/2 -> dst == colour           (exact; uniform invariant)
 *       CSC       YUYV422 -> RGB888 (BT.601) -> neutral-grey output      (band; the real tennis op)
 */

/* --- /dev/rga ioctls (MultiRGA v1.3.1 ABI; see rga-abi/src/main.c). --- */
#define RGA_BLIT_SYNC           0x5017u
#define RGA_IOC_GET_DRVIER_VERS 0x801C7201u /* _IOR('r',1,rga_version_t=28)    */
#define RGA_IOC_GET_HW_VERSION  0x80907202u /* _IOR('r',2,rga_hw_versions=144) */
#define RGA_IOC_IMPORT_BUFFER   0xC0107203u /* _IOWR('r',3,rga_buffer_pool=16) */
#define RGA_DMA_BUFFER          0u

/* rga_img_info_t.format — kernel rga_surf_format enum (un-shifted; the kernel mapper also
   accepts librga's <<8 RK_FORMAT form). See librga_abi.rs rk_format_to_pixel. */
#define RGA_FMT_RGBA8888 0x00u
#define RGA_FMT_RGB888   0x02u
#define RGA_FMT_YUYV422  0x1cu

/* --- /dev/dma_heap (mainline uapi). --- */
struct dma_heap_allocation_data {
    uint64_t len;
    uint32_t fd;
    uint32_t fd_flags;
    uint64_t heap_flags;
};
#define DMA_HEAP_IOCTL_ALLOC 0xC0184800u

/* rga_external_buffer (288 bytes) / rga_buffer_pool (16 bytes) — IMPORT_BUFFER argument. */
struct rga_external_buffer {
    uint64_t memory;
    uint32_t type;
    uint32_t handle;
    uint32_t width, height, format, size;
    uint8_t  reserve[252];
};
struct rga_buffer_pool {
    uint64_t buffers_ptr;
    uint32_t size;
};

/* rga_req field offsets (LP64): render_mode@0, src@8, dst@64, handle_flag@360. rga_img_info_t:
   yrgb_addr@+0, format@+24, act_w@+28, act_h@+30, vir_w@+36, vir_h@+38. */
static void put_u16(unsigned char *b, size_t off, uint16_t v) { memcpy(b + off, &v, 2); }
static void put_u32(unsigned char *b, size_t off, uint32_t v) { memcpy(b + off, &v, 4); }
static void put_u64(unsigned char *b, size_t off, uint64_t v) { memcpy(b + off, &v, 8); }

static int g_rga  = -1;
static int g_heap = -1;

/* A dma-heap buffer: dma-buf fd + UNCACHED mmap + the /dev/rga import handle. */
struct buf {
    int            fd;
    unsigned char *p;
    uint32_t       handle;
};

/* Allocate `len` bytes from /dev/dma_heap, mmap them, and import the dma-buf into /dev/rga.
   Returns 0 and fills *b on success, -1 otherwise. */
static int buf_alloc(struct buf *b, size_t len)
{
    b->fd = -1;
    b->p  = MAP_FAILED;

    struct dma_heap_allocation_data a = {0};
    a.len = len;
    if (ioctl(g_heap, DMA_HEAP_IOCTL_ALLOC, &a) != 0)
        return -1;
    b->fd = (int)a.fd;

    b->p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
    if (b->p == MAP_FAILED)
        return -1;

    struct rga_external_buffer ext;
    memset(&ext, 0, sizeof(ext));
    ext.memory = (uint64_t)b->fd;
    ext.type   = RGA_DMA_BUFFER;
    struct rga_buffer_pool pool = {.buffers_ptr = (uint64_t)(uintptr_t)&ext, .size = 1};
    if (ioctl(g_rga, RGA_IOC_IMPORT_BUFFER, &pool) != 0)
        return -1;
    b->handle = ext.handle;
    return b->handle != 0 ? 0 : -1;
}

/* Build a BITBLT rga_req that blits src(handle,fmt,sw*sh) -> dst(handle,fmt,dw*dh). Equal dims +
   equal RGB formats encode as a Copy; differing dims as a scaling Blit; a YUV->RGB format pair
   auto-applies BT.601 CSC (csc_for in librga_abi.rs). */
static void build_blit(unsigned char *req, uint32_t shandle, uint32_t sfmt, int sw, int sh,
                       uint32_t dhandle, uint32_t dfmt, int dw, int dh)
{
    memset(req, 0, 512);
    req[0] = 0; /* render_mode = RENDER_BITBLT */
    put_u64(req, 8, shandle);
    put_u32(req, 8 + 24, sfmt);
    put_u16(req, 8 + 28, (uint16_t)sw);
    put_u16(req, 8 + 30, (uint16_t)sh);
    put_u16(req, 8 + 36, (uint16_t)sw);
    put_u16(req, 8 + 38, (uint16_t)sh);
    put_u64(req, 64, dhandle);
    put_u32(req, 64 + 24, dfmt);
    put_u16(req, 64 + 28, (uint16_t)dw);
    put_u16(req, 64 + 30, (uint16_t)dh);
    put_u16(req, 64 + 36, (uint16_t)dw);
    put_u16(req, 64 + 38, (uint16_t)dh);
    req[360] = 1; /* handle_flag = 1 (src/dst addrs are import handles) */
}

#define W 64
#define H 48
#define POISON_RGBA 0xDEADBEEFu

static void fill_rgba(unsigned char *p, int npix, uint32_t color)
{
    uint32_t *px = (uint32_t *)p;
    for (int i = 0; i < npix; i++)
        px[i] = color;
}

/* Every RGBA pixel == expected? On mismatch, print a fail line for the board fail_regex. */
static int verify_rgba(unsigned char *p, int npix, uint32_t expected, const char *label)
{
    const uint32_t *px = (const uint32_t *)p;
    for (int i = 0; i < npix; i++) {
        if (px[i] != expected) {
            printf("RGA_BLIT_HW_FAIL: %s px[%d]=0x%08x want=0x%08x\n", label, i, px[i], expected);
            return 0;
        }
    }
    return 1;
}

/* RGB888 output of a neutral YUYV (Y=128,U=V=128) is mid-grey ~130 (BT.601 limited). Assert the
   sampled pixels are grey (R~=G~=B) in a band around mid-grey — robust without pinning the exact
   CSC coefficients, and rejects the 0xAB poison (171, out of band). */
static int verify_gray(unsigned char *p, int npix, const char *label)
{
    const int idx[3] = {0, npix / 2, npix - 1};
    for (int k = 0; k < 3; k++) {
        int i = idx[k];
        int r = p[i * 3 + 0], g = p[i * 3 + 1], b = p[i * 3 + 2];
        int mn = r < g ? (r < b ? r : b) : (g < b ? g : b);
        int mx = r > g ? (r > b ? r : b) : (g > b ? g : b);
        if (r < 100 || r > 160 || g < 100 || g > 160 || b < 100 || b > 160 || (mx - mn) > 16) {
            printf("RGA_BLIT_HW_FAIL: %s px[%d]=(%d,%d,%d) not neutral-grey\n", label, i, r, g, b);
            return 0;
        }
    }
    return 1;
}

int main(void)
{
    TEST_START("rga-blit-hw: real /dev/rga blit + pixel verify (board); SKIP on QEMU");

    g_rga = open("/dev/rga", O_RDWR | O_CLOEXEC);
    if (g_rga < 0) {
        printf("RGA_BLIT_HW SKIP: /dev/rga absent\n");
        TEST_DONE();
    }
    g_heap = open("/dev/dma_heap/system", O_RDWR | O_CLOEXEC);
    if (g_heap < 0) {
        printf("RGA_BLIT_HW SKIP: /dev/dma_heap absent\n");
        close(g_rga);
        TEST_DONE();
    }

    /* Version sanity (librga queries these at init; the kernel answers regardless of hardware). */
    unsigned char drv_ver[28];
    memset(drv_ver, 0, sizeof(drv_ver));
    CHECK(ioctl(g_rga, RGA_IOC_GET_DRVIER_VERS, drv_ver) == 0, "GET_DRVIER_VERSION ok");
    unsigned char hw_ver[144];
    memset(hw_ver, 0, sizeof(hw_ver));
    CHECK(ioctl(g_rga, RGA_IOC_GET_HW_VERSION, hw_ver) == 0, "GET_HW_VERSION ok");

    unsigned char req[512];

    /* --- 1. Copy (RGBA same-size). Also the device probe: ENODEV here => no RGA2 (QEMU) => SKIP. --- */
    struct buf src, dst;
    if (buf_alloc(&src, (size_t)W * H * 4) != 0 || buf_alloc(&dst, (size_t)W * H * 4) != 0) {
        printf("RGA_BLIT_HW_FAIL: dma-heap alloc/import failed\n");
        TEST_DONE();
    }
    fill_rgba(src.p, W * H, 0x11223344u);
    fill_rgba(dst.p, W * H, POISON_RGBA);
    build_blit(req, src.handle, RGA_FMT_RGBA8888, W, H, dst.handle, RGA_FMT_RGBA8888, W, H);
    errno = 0;
    int rc = ioctl(g_rga, RGA_BLIT_SYNC, req);
    if (rc != 0 && errno == ENODEV) {
        printf("RGA_BLIT_HW SKIP: no RGA2 engine (QEMU), handle resolved + request parsed\n");
        TEST_DONE();
    }
    CHECK(rc == 0, "copy: RGA_BLIT_SYNC ok");
    CHECK(verify_rgba(dst.p, W * H, 0x11223344u, "copy"), "copy: dst == src (exact)");

    /* --- 2. Downscale (solid colour W*H -> W/2*H/2). Uniform source -> same uniform colour. --- */
    const int dw = W / 2, dh = H / 2;
    const uint32_t solid = 0x1188aaffu;
    struct buf rs_src, rs_dst;
    if (buf_alloc(&rs_src, (size_t)W * H * 4) != 0 || buf_alloc(&rs_dst, (size_t)dw * dh * 4) != 0) {
        printf("RGA_BLIT_HW_FAIL: downscale alloc/import failed\n");
        TEST_DONE();
    }
    fill_rgba(rs_src.p, W * H, solid);
    fill_rgba(rs_dst.p, dw * dh, POISON_RGBA);
    build_blit(req, rs_src.handle, RGA_FMT_RGBA8888, W, H, rs_dst.handle, RGA_FMT_RGBA8888, dw, dh);
    CHECK(ioctl(g_rga, RGA_BLIT_SYNC, req) == 0, "downscale: RGA_BLIT_SYNC ok");
    CHECK(verify_rgba(rs_dst.p, dw * dh, solid, "downscale"), "downscale: dst == colour (exact)");

    /* --- 3. CSC YUYV422 -> RGB888 (the real tennis op). Mid-grey YUYV -> neutral-grey RGB. --- */
    struct buf y_src, rgb_dst;
    if (buf_alloc(&y_src, (size_t)W * H * 2) != 0 || buf_alloc(&rgb_dst, (size_t)W * H * 3) != 0) {
        printf("RGA_BLIT_HW_FAIL: csc alloc/import failed\n");
        TEST_DONE();
    }
    for (int i = 0; i < W * H * 2; i++)
        y_src.p[i] = 128; /* packed YUYV, all components mid (Y=128, U=V=128) -> neutral chroma */
    memset(rgb_dst.p, 0xAB, (size_t)W * H * 3); /* poison: distinguishes NOWRITE from a real write */
    build_blit(req, y_src.handle, RGA_FMT_YUYV422, W, H, rgb_dst.handle, RGA_FMT_RGB888, W, H);
    CHECK(ioctl(g_rga, RGA_BLIT_SYNC, req) == 0, "csc: RGA_BLIT_SYNC ok");
    CHECK(verify_gray(rgb_dst.p, W * H, "csc"), "csc: YUYV->RGB neutral-grey");

    TEST_DONE();
}
