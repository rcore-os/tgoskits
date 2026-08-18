/* audio_common.h - shared signal-domain primitives for the cpu-audio-test carpet.
 *
 * Everything here is self-written and deterministic: a clean iterative radix-2 Cooley-Tukey FFT, a
 * minimal RIFF/WAVE PCM-s16le parser, an ffmpeg-CLI subprocess helper that decodes any container to
 * interleaved s16le on stdout, plus the reference math (RMS, SNR, THD+N, spectral peak, PSNR). No
 * codec, FFT library or DSP dependency is pulled in - the expected FFT bins / magnitudes / RMS are
 * derived independently in-code so each assertion is a closed-form check, not a self-comparison.
 *
 * The FFT peak convention: for a real cosine of frequency f sampled at fs over N points, the
 * two-sided FFT puts half the energy in bin k=round(f*N/fs) and half in bin N-k. We test the
 * lower-half bins [0, N/2] and treat bin k as the tone.
 */
#ifndef AUDIO_COMMON_H
#define AUDIO_COMMON_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdint.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* -------- fixed-point-free complex + radix-2 iterative FFT (self-written, O(N log N)) -------- */
typedef struct { double re, im; } cpx;

static int is_pow2(int n) { return n > 0 && (n & (n - 1)) == 0; }

/* In-place radix-2 DIT FFT. n must be a power of two. sign=-1 forward, +1 inverse (unnormalized). */
static void fft_run(cpx *a, int n, int sign) {
    /* bit-reversal permutation */
    for (int i = 1, j = 0; i < n; i++) {
        int bit = n >> 1;
        for (; j & bit; bit >>= 1) j ^= bit;
        j ^= bit;
        if (i < j) { cpx t = a[i]; a[i] = a[j]; a[j] = t; }
    }
    for (int len = 2; len <= n; len <<= 1) {
        double ang = sign * 2.0 * M_PI / len;
        cpx wlen = { cos(ang), sin(ang) };
        for (int i = 0; i < n; i += len) {
            cpx w = { 1.0, 0.0 };
            for (int k = 0; k < len / 2; k++) {
                cpx u = a[i + k];
                cpx v;
                v.re = a[i + k + len / 2].re * w.re - a[i + k + len / 2].im * w.im;
                v.im = a[i + k + len / 2].re * w.im + a[i + k + len / 2].im * w.re;
                a[i + k].re = u.re + v.re;   a[i + k].im = u.im + v.im;
                a[i + k + len / 2].re = u.re - v.re; a[i + k + len / 2].im = u.im - v.im;
                double nre = w.re * wlen.re - w.im * wlen.im;
                double nim = w.re * wlen.im + w.im * wlen.re;
                w.re = nre; w.im = nim;
            }
        }
    }
}

/* magnitude spectrum of a real signal x[0..n-1] into mag[0..n/2]; mag = |X_k| / n (single-sided-ish,
 * not doubled - callers reason about the raw two-sided bin energy). */
static void real_fft_mag(const double *x, int n, double *mag) {
    cpx *a = (cpx *)malloc(sizeof(cpx) * n);
    for (int i = 0; i < n; i++) { a[i].re = x[i]; a[i].im = 0.0; }
    fft_run(a, n, -1);
    for (int k = 0; k <= n / 2; k++)
        mag[k] = sqrt(a[k].re * a[k].re + a[k].im * a[k].im) / n;
    free(a);
}

/* Index of the largest bin in mag[lo..hi] inclusive. */
static int peak_bin(const double *mag, int lo, int hi) {
    int best = lo; double bv = mag[lo];
    for (int k = lo + 1; k <= hi; k++) if (mag[k] > bv) { bv = mag[k]; best = k; }
    return best;
}

/* SNR in dB: peak-bin power over the summed power of every other bin in [1..n/2] (skip DC). */
static double snr_db(const double *mag, int n, int pk) {
    double sig = mag[pk] * mag[pk], noise = 0.0;
    for (int k = 1; k <= n / 2; k++) if (k != pk) noise += mag[k] * mag[k];
    if (noise <= 0.0) return 999.0;
    return 10.0 * log10(sig / noise);
}

/* THD+N in dB for a fundamental at bin pk: (harmonics+noise power) / total signal power. Lower (more
 * negative) is purer. Excludes DC and the fundamental's immediate skirt (+/-1 bin) from "distortion"? No -
 * we count everything except the fundamental bin itself as distortion+noise, the strict definition. */
static double thdn_db(const double *mag, int n, int pk) {
    double fund = mag[pk] * mag[pk], rest = 0.0;
    for (int k = 1; k <= n / 2; k++) if (k != pk) rest += mag[k] * mag[k];
    if (fund <= 0.0) return 999.0;
    return 10.0 * log10(rest / fund);
}

/* RMS of a float buffer. */
static double rms_f(const double *x, int n) {
    double s = 0.0; for (int i = 0; i < n; i++) s += x[i] * x[i];
    return sqrt(s / n);
}

/* RMS of interleaved int16 normalized by 32768 (matches the golden pipeline). */
static double rms_i16(const int16_t *x, long n) {
    double s = 0.0; for (long i = 0; i < n; i++) { double v = x[i] / 32768.0; s += v * v; }
    return sqrt(s / (double)n);
}

/* -------- WAV (RIFF/WAVE) minimal reader: PCM s16le only -------- */
typedef struct { int sample_rate; int channels; long frames; int16_t *pcm; long nsamp; } wavbuf;

static int read_le16(const unsigned char *p) { return p[0] | (p[1] << 8); }
static uint32_t read_le32(const unsigned char *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

/* Parse a whole WAV file into wavbuf (int16 interleaved). Returns 0 ok. Caller frees w->pcm. */
static int wav_read_file(const char *path, wavbuf *w) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    unsigned char *buf = (unsigned char *)malloc(sz);
    if (fread(buf, 1, sz, f) != (size_t)sz) { fclose(f); free(buf); return -2; }
    fclose(f);
    if (sz < 44 || memcmp(buf, "RIFF", 4) || memcmp(buf + 8, "WAVE", 4)) { free(buf); return -3; }
    memset(w, 0, sizeof(*w));
    long off = 12; int have_fmt = 0; long data_off = -1; uint32_t data_len = 0;
    while (off + 8 <= sz) {
        char id[5] = {0}; memcpy(id, buf + off, 4);
        uint32_t clen = read_le32(buf + off + 4);
        long body = off + 8;
        if (!memcmp(id, "fmt ", 4)) {
            int fmt = read_le16(buf + body);
            w->channels = read_le16(buf + body + 2);
            w->sample_rate = (int)read_le32(buf + body + 4);
            int bits = read_le16(buf + body + 14);
            if ((fmt != 1 && fmt != 0xFFFE) || bits != 16) { free(buf); return -4; }
            have_fmt = 1;
        } else if (!memcmp(id, "data", 4)) {
            data_off = body; data_len = clen;
        }
        off = body + clen + (clen & 1);
    }
    if (!have_fmt || data_off < 0) { free(buf); return -5; }
    if (data_off + (long)data_len > sz) data_len = (uint32_t)(sz - data_off); /* tolerate trailing */
    w->nsamp = data_len / 2;
    w->frames = w->channels ? w->nsamp / w->channels : 0;
    w->pcm = (int16_t *)malloc(data_len);
    memcpy(w->pcm, buf + data_off, data_len);
    free(buf);
    return 0;
}

/* -------- ffmpeg CLI helpers -------- */
/* Run a shell command, return its exit status (0 ok). */
static int sh(const char *cmd) {
    int rc = system(cmd);
    if (rc == -1) return -1;
    return rc; /* caller can inspect */
}

/* Decode any audio file to interleaved s16le raw PCM at (rate,ch); channels/rate forced if >0.
 * Writes to out_path. Returns 0 on success. */
static int ffmpeg_decode_raw(const char *in, const char *out_raw, int rate, int ch) {
    char cmd[2048];
    char ropt[64] = "", copt[64] = "";
    if (rate > 0) snprintf(ropt, sizeof(ropt), "-ar %d ", rate);
    if (ch > 0)   snprintf(copt, sizeof(copt), "-ac %d ", ch);
    snprintf(cmd, sizeof(cmd),
        "ffmpeg -v error -y -i '%s' -f s16le -acodec pcm_s16le %s%s '%s'",
        in, ropt, copt, out_raw);
    return sh(cmd);
}

/* Read a raw s16le file fully into an int16 buffer. Returns sample count (int16 units), -1 on error. */
static long read_raw_s16(const char *path, int16_t **out) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    int16_t *b = (int16_t *)malloc(sz > 0 ? sz : 2);
    long n = (long)fread(b, 1, sz, f) / 2;
    fclose(f);
    *out = b;
    return n;
}

/* SHA-256 (self-written, FIPS 180-4) so lossless round-trips can be byte-compared to the golden. */
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

/* -------- tiny assertion harness: pass/total, print name OK <n> on clean pass -------- */
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

#endif /* AUDIO_COMMON_H */
