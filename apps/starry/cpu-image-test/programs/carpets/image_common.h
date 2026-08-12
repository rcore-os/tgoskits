/* image_common.h - shared primitives for the cpu-image-test carpet (the "pyte for images").
 *
 * Each cell drives a real decoder/rasterizer - stb_image (png/bmp/tga/jpg/ppm/pgm/gif decode),
 * stb_image_write (png/bmp/tga encode), nanosvg + nanosvgrast (SVG parse + rasterize) - and asserts the
 * output against a golden that is either a closed-form property (a solid buffer is one repeated byte; a
 * filled <circle r> inks exactly the pixels within r of its center) or a value calibrated once host-side
 * with the SAME libraries the image ships (the single-header stb/nanosvg pinned in third_party/). Only the
 * comparison logic - per-pixel diff, PSNR, the SHA-256 over the decoded pixel buffer - and the golden
 * constants are self-written. No PNG/JPEG/SVG codec is reimplemented; the point is to TEST stb/nanosvg.
 *
 * Determinism: stb_image's lossless decoders (PNG/BMP/TGA/PPM/PGM/GIF) are exact integer pipelines, so the
 * SHA-256 of the decoded RGBA buffer is a reproducible golden across arches. stb's baseline JPEG decoder is
 * a fixed integer IDCT, but lossy vs the reference is asserted by PSNR bound, not SHA. nanosvg's rasterizer
 * is a deterministic fixed-point coverage rasterizer, so its per-pixel output is reproducible; the
 * synthetic-SVG legs assert closed-form (inside/outside a circle, a rect, a linear gradient) which is
 * rasterizer-independent, and the real 3DBenchy leg asserts a calibrated golden SHA + structural match.
 */
#ifndef IMAGE_COMMON_H
#define IMAGE_COMMON_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <math.h>

/* -------- image locations -------- */
/* Format-zoo + real rasters live in ASSET_DIR (the render-assets/images submodule mount, staged by
 * prebuild into /opt/cpu-image-test/assets). Host validation points IMAGE_DIR at render-assets directly. */
static const char *image_dir(void) {
    const char *d = getenv("IMAGE_DIR");
    if (d && *d) return d;
    d = getenv("ASSET_DIR");
    if (d && *d) return d;
    return "/opt/cpu-image-test/assets";
}
static const char *image_path(char *buf, size_t n, const char *name) {
    snprintf(buf, n, "%s/%s", image_dir(), name);
    return buf;
}

/* Canonical format-zoo files (the same 640x360 image encoded 6 ways) + real rasters + the SVG golden. */
#define ZOO_REF   "fmt_ref.png"
#define ZOO_BMP   "fmt.bmp"
#define ZOO_TGA   "fmt.tga"
#define ZOO_PPM   "fmt.ppm"
#define ZOO_PGM   "fmt.pgm"
#define ZOO_JPG   "fmt.jpg"
#define REAL_HONKAI_BASE  "honkai3_base.png"
#define REAL_HONKAI_WALL  "honkai3_wall_home.png"
#define SVG_BENCHY        "benchy.svg"
#define SVG_BENCHY_GOLDEN "benchy_svg_raster.png"

/* -------- self-written SHA-256 over a decoded pixel buffer -------- */
typedef struct { uint32_t h[8]; uint64_t len; unsigned char buf[64]; size_t n; } sha256_ctx;
static const uint32_t SHA_K[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2 };
static uint32_t sha_ror(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }
static void sha256_init(sha256_ctx *c) {
    c->h[0]=0x6a09e667; c->h[1]=0xbb67ae85; c->h[2]=0x3c6ef372; c->h[3]=0xa54ff53a;
    c->h[4]=0x510e527f; c->h[5]=0x9b05688c; c->h[6]=0x1f83d9ab; c->h[7]=0x5be0cd19;
    c->len = 0; c->n = 0;
}
static void sha256_block(sha256_ctx *c, const unsigned char *p) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
        w[i] = (p[i*4]<<24)|(p[i*4+1]<<16)|(p[i*4+2]<<8)|p[i*4+3];
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = sha_ror(w[i-15],7)^sha_ror(w[i-15],18)^(w[i-15]>>3);
        uint32_t s1 = sha_ror(w[i-2],17)^sha_ror(w[i-2],19)^(w[i-2]>>10);
        w[i] = w[i-16]+s0+w[i-7]+s1;
    }
    uint32_t a=c->h[0],b=c->h[1],cc=c->h[2],d=c->h[3],e=c->h[4],f=c->h[5],g=c->h[6],h=c->h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t S1 = sha_ror(e,6)^sha_ror(e,11)^sha_ror(e,25);
        uint32_t ch = (e&f)^((~e)&g);
        uint32_t t1 = h+S1+ch+SHA_K[i]+w[i];
        uint32_t S0 = sha_ror(a,2)^sha_ror(a,13)^sha_ror(a,22);
        uint32_t maj = (a&b)^(a&cc)^(b&cc);
        uint32_t t2 = S0+maj;
        h=g; g=f; f=e; e=d+t1; d=cc; cc=b; b=a; a=t1+t2;
    }
    c->h[0]+=a; c->h[1]+=b; c->h[2]+=cc; c->h[3]+=d; c->h[4]+=e; c->h[5]+=f; c->h[6]+=g; c->h[7]+=h;
}
static void sha256_update(sha256_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += len;
    while (len > 0) {
        size_t take = 64 - c->n; if (take > len) take = len;
        memcpy(c->buf + c->n, p, take);
        c->n += take; p += take; len -= take;
        if (c->n == 64) { sha256_block(c, c->buf); c->n = 0; }
    }
}
static void sha256_hex(sha256_ctx *c, char out[65]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80;
    sha256_update(c, &pad, 1);
    unsigned char z = 0;
    while (c->n != 56) sha256_update(c, &z, 1);
    unsigned char lb[8];
    for (int i = 0; i < 8; i++) lb[i] = (bits >> (56 - i*8)) & 0xff;
    sha256_update(c, lb, 8);
    for (int i = 0; i < 8; i++) sprintf(out + i*8, "%08x", c->h[i]);
}
static void sha256_buf(const void *buf, size_t len, char out[65]) {
    sha256_ctx c; sha256_init(&c); sha256_update(&c, buf, len); sha256_hex(&c, out);
}

/* -------- three-gate marker (identical semantics to the audio/video/font carpets) -------- */
typedef struct { int pass, total, fail; const char *name; } gate;
static void gate_init(gate *g, const char *name) { g->pass = g->total = g->fail = 0; g->name = name; }
static void gate_check(gate *g, int cond, const char *msg) {
    g->total++;
    if (cond) g->pass++;
    else { g->fail++; fprintf(stderr, "  FAIL: %s\n", msg); }
}
static int gate_finish(gate *g) {
    if (g->fail == 0 && g->total == g->pass && g->total > 0) {
        printf("%s OK %d\n", g->name, g->total);
        return 0;
    }
    printf("%s FAILED pass=%d total=%d fail=%d\n", g->name, g->pass, g->total, g->fail);
    return 1;
}

/* -------- pixel comparison primitives -------- */
/* Per-channel max absolute difference between two equal-length byte buffers. */
static int buf_maxdiff(const unsigned char *a, const unsigned char *b, size_t n) {
    int m = 0;
    for (size_t i = 0; i < n; i++) { int d = (int)a[i] - (int)b[i]; if (d < 0) d = -d; if (d > m) m = d; }
    return m;
}
/* PSNR in dB between two equal-length 8-bit buffers (255 peak). Returns +INFINITY if identical. */
static double buf_psnr(const unsigned char *a, const unsigned char *b, size_t n) {
    double se = 0.0;
    for (size_t i = 0; i < n; i++) { double d = (double)a[i] - (double)b[i]; se += d * d; }
    if (se == 0.0) return INFINITY;
    double mse = se / (double)n;
    return 10.0 * log10((255.0 * 255.0) / mse);
}
/* Number of differing bytes. */
static size_t buf_ndiff(const unsigned char *a, const unsigned char *b, size_t n) {
    size_t k = 0; for (size_t i = 0; i < n; i++) if (a[i] != b[i]) k++; return k;
}

/* A downscale signature: average each of the 8x8 cell blocks over an RGBA image into a 64-entry-per-channel
 * fingerprint, then SHA-256 it. Robust to exact pixel layout, sensitive to real content changes - used to
 * bind a large real raster to a golden without hardcoding a full-frame SHA. */
static void rgba_signature(const unsigned char *px, int w, int h, unsigned char sig[8*8*4]) {
    for (int by = 0; by < 8; by++) for (int bx = 0; bx < 8; bx++) {
        long acc[4] = {0,0,0,0}; long cnt = 0;
        int x0 = (int)((long)bx * w / 8), x1 = (int)((long)(bx+1) * w / 8);
        int y0 = (int)((long)by * h / 8), y1 = (int)((long)(by+1) * h / 8);
        for (int y = y0; y < y1; y++) for (int x = x0; x < x1; x++) {
            const unsigned char *p = px + ((size_t)y * w + x) * 4;
            acc[0] += p[0]; acc[1] += p[1]; acc[2] += p[2]; acc[3] += p[3]; cnt++;
        }
        if (!cnt) cnt = 1;
        unsigned char *o = &sig[(by*8 + bx) * 4];
        o[0] = (unsigned char)(acc[0]/cnt); o[1] = (unsigned char)(acc[1]/cnt);
        o[2] = (unsigned char)(acc[2]/cnt); o[3] = (unsigned char)(acc[3]/cnt);
    }
}

#endif /* IMAGE_COMMON_H */
