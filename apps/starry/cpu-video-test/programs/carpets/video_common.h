/* video_common.h - shared pixel-domain primitives for the cpu-video-test carpet.
 *
 * The carpet decodes video to raw planar/packed pixels with the `ffmpeg` CLI (to `-f rawvideo
 * -pix_fmt rgb24|gray`) and asserts in the PIXEL domain against analytically-known or golden
 * references. Nothing here decodes video itself - ffmpeg owns demux/decode/encode; this header
 * owns only the self-written comparison math: a raw-frame reader, PSNR (10*log10(MAX^2/MSE)), the
 * standard windowed SSIM on luma, an 8x8-bicubic luma signature that reproduces the golden's
 * `luma8x8_hex`, a black/white threshold-ratio for the ~1-bit Bad Apple frames, a SHA-256 (FIPS
 * 180-4) for byte-exact frame identity, and a tiny gate harness.
 *
 * Determinism notes established against ffmpeg 6.1.1 on the golden host:
 *   - `ffmpeg -i F -f rawvideo -pix_fmt rgb24`         reproduces golden sha256(rgb24) byte-exact.
 *   - `scale=8:8:flags=bicubic,format=gray`            reproduces golden luma8x8_hex byte-exact.
 *   - rawvideo rgb24 -> ffv1 -> rawvideo rgb24         is byte-identical (lossless round-trip).
 *   - lavfi smptebars rgb24 is bit-reproducible run to run (closed-form bar colors at known cols).
 */
#ifndef VIDEO_COMMON_H
#define VIDEO_COMMON_H

#ifndef _GNU_SOURCE
#define _GNU_SOURCE   /* popen/pclose visibility under -std=c11 */
#endif

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* -------- raw frame buffer -------- */
typedef struct { int w, h, ch; unsigned char *px; long bytes; } frame;

/* Read a whole raw file into a byte buffer. Returns byte count, -1 on error. Caller frees *out. */
static long read_file_bytes(const char *path, unsigned char **out) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    unsigned char *b = (unsigned char *)malloc(sz > 0 ? sz : 1);
    long n = (long)fread(b, 1, sz, f);
    fclose(f);
    *out = b;
    return n;
}

/* Read a raw rgb24/gray frame of expected geometry. ch=3 rgb24, ch=1 gray. Returns 0 ok. */
static int frame_read(const char *path, int w, int h, int ch, frame *fr) {
    unsigned char *b = NULL; long n = read_file_bytes(path, &b);
    long want = (long)w * h * ch;
    if (n != want) { free(b); return -1; }
    fr->w = w; fr->h = h; fr->ch = ch; fr->px = b; fr->bytes = n;
    return 0;
}

static void frame_free(frame *fr) { if (fr && fr->px) { free(fr->px); fr->px = NULL; } }

/* -------- luma + comparison math (all self-written) -------- */

/* Rec.601 luma of an rgb24 pixel triple, integer weights matching ffmpeg's gray conversion domain
 * closely enough for ratio checks; the exact golden 8x8 luma comes from ffmpeg gray directly. */
static int rgb_luma(int r, int g, int b) {
    int y = (77 * r + 150 * g + 29 * b) >> 8;
    return y < 0 ? 0 : (y > 255 ? 255 : y);
}

/* PSNR (dB) between two equal-length byte buffers. MAX=255. Returns 999 when identical. */
static double psnr_bytes(const unsigned char *a, const unsigned char *b, long n) {
    double se = 0.0;
    for (long i = 0; i < n; i++) { double d = (double)a[i] - b[i]; se += d * d; }
    double mse = se / (double)n;
    if (mse <= 0.0) return 999.0;
    return 10.0 * log10((255.0 * 255.0) / mse);
}

/* Convert an rgb24 frame to a freshly-allocated luma (gray) plane. Caller frees. */
static unsigned char *frame_to_luma(const frame *fr) {
    long np = (long)fr->w * fr->h;
    unsigned char *y = (unsigned char *)malloc(np);
    if (fr->ch == 1) { memcpy(y, fr->px, np); return y; }
    for (long i = 0; i < np; i++) {
        int r = fr->px[i*3], g = fr->px[i*3+1], b = fr->px[i*3+2];
        y[i] = (unsigned char)rgb_luma(r, g, b);
    }
    return y;
}

/* Standard SSIM on two luma planes of equal geometry using an 8x8 sliding window (stride 4),
 * uniform weights, C1=(0.01*255)^2, C2=(0.03*255)^2. Returns mean SSIM in [-1,1]. This is the
 * textbook Wang et al. formulation: per-window mean/variance/covariance -> local SSIM, averaged. */
static double ssim_luma(const unsigned char *a, const unsigned char *b, int w, int h) {
    const int win = 8, stride = 4;
    const double C1 = (0.01 * 255) * (0.01 * 255);
    const double C2 = (0.03 * 255) * (0.03 * 255);
    double acc = 0.0; long nwin = 0;
    for (int y0 = 0; y0 + win <= h; y0 += stride) {
        for (int x0 = 0; x0 + win <= w; x0 += stride) {
            double ma = 0, mb = 0;
            for (int y = 0; y < win; y++)
                for (int x = 0; x < win; x++) {
                    ma += a[(y0+y)*w + x0+x];
                    mb += b[(y0+y)*w + x0+x];
                }
            double n = win * win;
            ma /= n; mb /= n;
            double va = 0, vb = 0, cov = 0;
            for (int y = 0; y < win; y++)
                for (int x = 0; x < win; x++) {
                    double da = a[(y0+y)*w + x0+x] - ma;
                    double db = b[(y0+y)*w + x0+x] - mb;
                    va += da*da; vb += db*db; cov += da*db;
                }
            va /= (n - 1); vb /= (n - 1); cov /= (n - 1);
            double s = ((2*ma*mb + C1) * (2*cov + C2)) /
                       ((ma*ma + mb*mb + C1) * (va + vb + C2));
            acc += s; nwin++;
        }
    }
    return nwin ? acc / (double)nwin : 0.0;
}

/* Black/white ratio of a luma plane at a threshold: fraction of pixels with luma >= thr (white).
 * Bad Apple frames are ~1-bit, so this ratio is a stable per-frame descriptor. */
static double white_ratio(const unsigned char *y, long np, int thr) {
    long white = 0;
    for (long i = 0; i < np; i++) if (y[i] >= thr) white++;
    return (double)white / (double)np;
}

/* -------- ffmpeg CLI helpers -------- */
static int sh(const char *cmd) {
    int rc = system(cmd);
    return rc == -1 ? -1 : rc;
}

/* Decode one rgb24 frame of an input at an optional seek (ss<0 => none, else -ss before -i) and an
 * optional frame index sel (sel<0 => first output frame). Writes raw rgb24 of size w*h*3 to out.
 * Returns 0 ok. */
static int ffmpeg_frame_rgb24(const char *in, double ss, const char *vf, const char *out) {
    char cmd[2048], sbuf[64] = "", fbuf[256] = "";
    if (ss >= 0) snprintf(sbuf, sizeof sbuf, "-ss %.6f ", ss);
    if (vf && *vf) snprintf(fbuf, sizeof fbuf, "-vf \"%s\" ", vf);
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y %s-i '%s' %s-vframes 1 -f rawvideo -pix_fmt rgb24 '%s'",
        sbuf, in, fbuf, out);
    return sh(cmd);
}

/* The golden 8x8 luma signature: scale to 8x8 bicubic, gray, one frame. Writes 64 raw gray bytes. */
static int ffmpeg_luma8x8(const char *in, double ss, const char *out) {
    char cmd[2048], sbuf[64] = "";
    if (ss >= 0) snprintf(sbuf, sizeof sbuf, "-ss %.6f ", ss);
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y %s-i '%s' -vframes 1 -vf \"scale=8:8:flags=bicubic,format=gray\" "
        "-f rawvideo -pix_fmt gray '%s'", sbuf, in, out);
    return sh(cmd);
}

/* -------- SHA-256 (FIPS 180-4, self-written) for byte-exact frame identity -------- */
typedef struct { uint32_t h[8]; uint64_t len; unsigned char buf[64]; size_t n; } sha256_ctx;
static uint32_t rotr32(uint32_t x, int c) { return (x >> c) | (x << (32 - c)); }
static void sha256_init(sha256_ctx *c) {
    static const uint32_t iv[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
                                   0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
    memcpy(c->h, iv, sizeof iv); c->len = 0; c->n = 0;
}
static void sha256_block(sha256_ctx *c, const unsigned char *p) {
    static const uint32_t K[64] = {
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2};
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
        w[i] = (p[i*4]<<24)|(p[i*4+1]<<16)|(p[i*4+2]<<8)|p[i*4+3];
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = rotr32(w[i-15],7)^rotr32(w[i-15],18)^(w[i-15]>>3);
        uint32_t s1 = rotr32(w[i-2],17)^rotr32(w[i-2],19)^(w[i-2]>>10);
        w[i] = w[i-16]+s0+w[i-7]+s1;
    }
    uint32_t a=c->h[0],b=c->h[1],cc=c->h[2],d=c->h[3],e=c->h[4],f=c->h[5],g=c->h[6],h=c->h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t S1 = rotr32(e,6)^rotr32(e,11)^rotr32(e,25);
        uint32_t ch = (e&f)^((~e)&g);
        uint32_t t1 = h+S1+ch+K[i]+w[i];
        uint32_t S0 = rotr32(a,2)^rotr32(a,13)^rotr32(a,22);
        uint32_t maj = (a&b)^(a&cc)^(b&cc);
        uint32_t t2 = S0+maj;
        h=g; g=f; f=e; e=d+t1; d=cc; cc=b; b=a; a=t1+t2;
    }
    c->h[0]+=a;c->h[1]+=b;c->h[2]+=cc;c->h[3]+=d;c->h[4]+=e;c->h[5]+=f;c->h[6]+=g;c->h[7]+=h;
}
static void sha256_update(sha256_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += len;
    while (len) {
        size_t take = 64 - c->n; if (take > len) take = len;
        memcpy(c->buf + c->n, p, take); c->n += take; p += take; len -= take;
        if (c->n == 64) { sha256_block(c, c->buf); c->n = 0; }
    }
}
static void sha256_hex(sha256_ctx *c, char out[65]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80; sha256_update(c, &pad, 1);
    unsigned char z = 0; while (c->n != 56) sha256_update(c, &z, 1);
    unsigned char L[8]; for (int i = 0; i < 8; i++) L[i] = (bits >> (56 - i*8)) & 0xff;
    sha256_update(c, L, 8);
    for (int i = 0; i < 8; i++) sprintf(out + i*8, "%08x", c->h[i]);
    out[64] = 0;
}
static int sha256_file(const char *path, char out[65]) {
    FILE *f = fopen(path, "rb"); if (!f) return -1;
    sha256_ctx c; sha256_init(&c);
    unsigned char buf[65536]; size_t r;
    while ((r = fread(buf, 1, sizeof buf, f)) > 0) sha256_update(&c, buf, r);
    fclose(f); sha256_hex(&c, out); return 0;
}
static void sha256_buf(const unsigned char *p, long n, char out[65]) {
    sha256_ctx c; sha256_init(&c); sha256_update(&c, p, (size_t)n); sha256_hex(&c, out);
}

/* Hex-encode a byte buffer into out (needs 2*n+1). */
static void hex_encode(const unsigned char *b, int n, char *out) {
    for (int i = 0; i < n; i++) sprintf(out + i*2, "%02x", b[i]);
    out[n*2] = 0;
}

/* -------- gate harness (name OK <n> on clean pass, matches audio carpet) -------- */
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

#endif /* VIDEO_COMMON_H */
