/* image_formats - format matrix / round-trip + magic-byte + header-field golden (cell 2).
 *
 * Take one deterministic synthetic pattern and drive it through the mainstream raster format set,
 * asserting each end to end:
 *
 *   - PNG / BMP / TGA : stb_image_write encodes, stb_image decodes, byte-exact round-trip (lossless);
 *     plus magic-byte detection (PNG \x89PNG, BMP "BM") and stbi_info header fields (w/h) exact.
 *   - JPEG            : stb_image_write (baseline) encodes, stb_image decodes, PSNR bound (lossy).
 *   - PPM (P6) / PGM (P5) : the cell hand-writes the trivial NETPBM header (encoding only - no decoder is
 *     reinvented) then stb_image decodes it, byte-exact round-trip; magic "P6"/"P5" detected.
 *   - GIF             : palette-quantized; prebuild stages a deterministic 4-colour palette GIF (pal.gif,
 *     no dither => lossless). The cell decodes it with stb_image and asserts byte-exact vs the regenerated
 *     4-colour pattern + "GIF8" magic + dims. pal.gif is staged unconditionally by prebuild.
 *   - WebP            : stb_image has no WebP codec, so WebP is neither decoded nor asserted anywhere in
 *     this carpet - it is not claimed as covered. No false pass.
 *
 * Every SHA/round-trip here is closed-form (the pattern is generated in memory) - no external golden file
 * for the stb legs, so the assertion is a genuine encode->decode identity, not a self-hash.
 */
#include "image_common.h"

#define STB_IMAGE_IMPLEMENTATION
#include "third_party/stb_image.h"
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include "third_party/stb_image_write.h"

#define JPEG_PSNR_MIN 20.0   /* stb baseline JPEG @ q90 on this pattern: ~25 dB; bound is conservative */

/* Same pattern as image_raster's synthetic leg (R/G gradients + B checkerboard). */
static void gen_pattern(unsigned char *px, int w, int h) {
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
        unsigned char *p = px + ((size_t)y * w + x) * 3;
        int chk = ((x / 8) ^ (y / 8)) & 1;
        p[0] = (unsigned char)(x * 255 / (w - 1));
        p[1] = (unsigned char)(y * 255 / (h - 1));
        p[2] = chk ? 200 : 40;
    }
}
/* 4-colour palette pattern used for the GIF leg (quantizes losslessly). */
static void gen_palette(unsigned char *px, int w, int h) {
    static const unsigned char lut[4][3] = {{20,20,20},{230,30,30},{30,230,30},{30,30,230}};
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
        int q = ((x/16)&1) | (((y/16)&1)<<1);
        unsigned char *p = px + ((size_t)y * w + x) * 3;
        p[0]=lut[q][0]; p[1]=lut[q][1]; p[2]=lut[q][2];
    }
}

static int file_magic(const char *path, unsigned char *m, int n) {
    FILE *f = fopen(path, "rb"); if (!f) return 0;
    int r = (int)fread(m, 1, n, f); fclose(f); return r;
}

int main(void) {
    gate g; gate_init(&g, "IMAGE_FORMATS");
    const int w = 64, h = 48;
    unsigned char *src = (unsigned char *)malloc((size_t)w * h * 3);
    gen_pattern(src, w, h);

    /* ---- PNG / BMP / TGA: byte-exact lossless round-trip + magic + header ---- */
    struct { const char *name, *ext; int is_png, is_bmp; } LL[] = {
        { "PNG", "png", 1, 0 }, { "BMP", "bmp", 0, 1 }, { "TGA", "tga", 0, 0 },
    };
    for (int i = 0; i < 3; i++) {
        char path[128]; snprintf(path, sizeof path, "/tmp/cpu-image-fmt.%s", LL[i].ext);
        int ok;
        if (LL[i].is_png) ok = stbi_write_png(path, w, h, 3, src, w * 3);
        else if (LL[i].is_bmp) ok = stbi_write_bmp(path, w, h, 3, src);
        else ok = stbi_write_tga(path, w, h, 3, src);
        gate_check(&g, ok != 0, LL[i].name);
        /* magic bytes */
        unsigned char m[8]; int mn = file_magic(path, m, 8);
        if (LL[i].is_png)      gate_check(&g, mn >= 8 && m[0]==0x89 && m[1]=='P' && m[2]=='N' && m[3]=='G', "PNG magic");
        else if (LL[i].is_bmp) gate_check(&g, mn >= 2 && m[0]=='B' && m[1]=='M', "BMP magic");
        /* header via stbi_info */
        int iw, ih, ic; int info = stbi_info(path, &iw, &ih, &ic);
        gate_check(&g, info && iw == w && ih == h, LL[i].name);
        /* decode + byte-exact */
        int dw, dh, dc; unsigned char *dec = stbi_load(path, &dw, &dh, &dc, 3);
        gate_check(&g, dec != NULL, LL[i].name);
        if (dec) {
            gate_check(&g, dw == w && dh == h, LL[i].name);
            gate_check(&g, memcmp(dec, src, (size_t)w * h * 3) == 0, LL[i].name);
            stbi_image_free(dec);
        }
    }

    /* ---- JPEG: lossy, PSNR bound ---- */
    {
        const char *path = "/tmp/cpu-image-fmt.jpg";
        int ok = stbi_write_jpg(path, w, h, 3, src, 90);
        gate_check(&g, ok != 0, "JPEG write");
        int dw, dh, dc; unsigned char *dec = stbi_load(path, &dw, &dh, &dc, 3);
        gate_check(&g, dec != NULL, "JPEG decode");
        if (dec) {
            gate_check(&g, dw == w && dh == h, "JPEG dims");
            double ps = buf_psnr(src, dec, (size_t)w * h * 3);
            if (ps <= JPEG_PSNR_MIN) fprintf(stderr, "  JPEG PSNR=%.3f dB\n", ps);
            gate_check(&g, ps > JPEG_PSNR_MIN, "JPEG PSNR");
            unsigned char m[3]; file_magic(path, m, 3);
            gate_check(&g, m[0]==0xFF && m[1]==0xD8, "JPEG SOI magic");
            stbi_image_free(dec);
        }
    }

    /* ---- PPM (P6) / PGM (P5): hand-written NETPBM encode (no decoder reinvented) + stb decode ---- */
    {
        const char *path = "/tmp/cpu-image-fmt.ppm";
        FILE *f = fopen(path, "wb");
        gate_check(&g, f != NULL, "PPM open");
        if (f) { fprintf(f, "P6\n%d %d\n255\n", w, h); fwrite(src, 1, (size_t)w*h*3, f); fclose(f); }
        unsigned char m[2]; file_magic(path, m, 2);
        gate_check(&g, m[0]=='P' && m[1]=='6', "PPM magic");
        int dw, dh, dc; unsigned char *dec = stbi_load(path, &dw, &dh, &dc, 3);
        gate_check(&g, dec != NULL, "PPM decode");
        if (dec) {
            gate_check(&g, dw == w && dh == h, "PPM dims");
            gate_check(&g, memcmp(dec, src, (size_t)w*h*3) == 0, "PPM round-trip");
            stbi_image_free(dec);
        }
    }
    {
        const char *path = "/tmp/cpu-image-fmt.pgm";
        unsigned char *gray = (unsigned char *)malloc((size_t)w * h);
        for (int i = 0; i < w*h; i++)
            gray[i] = (unsigned char)((src[i*3]*77 + src[i*3+1]*150 + src[i*3+2]*29) >> 8);
        FILE *f = fopen(path, "wb");
        gate_check(&g, f != NULL, "PGM open");
        if (f) { fprintf(f, "P5\n%d %d\n255\n", w, h); fwrite(gray, 1, (size_t)w*h, f); fclose(f); }
        unsigned char m[2]; file_magic(path, m, 2);
        gate_check(&g, m[0]=='P' && m[1]=='5', "PGM magic");
        int dw, dh, dc; unsigned char *dec = stbi_load(path, &dw, &dh, &dc, 1);
        gate_check(&g, dec != NULL, "PGM decode");
        if (dec) {
            gate_check(&g, dw == w && dh == h, "PGM dims");
            gate_check(&g, memcmp(dec, gray, (size_t)w*h) == 0, "PGM round-trip");
            stbi_image_free(dec);
        }
        free(gray);
    }

    /* ---- GIF: prebuild-staged palette pattern, stb decode byte-exact. pal.gif is staged unconditionally,
     * so its absence is a staging failure - hard-fail, do not skip to green. ---- */
    {
        char path[512]; image_path(path, sizeof path, "pal.gif");
        FILE *f = fopen(path, "rb");
        gate_check(&g, f != NULL, "pal.gif MUST be staged (prebuild generates it unconditionally)");
        if (f) {
            fclose(f);
            unsigned char m[6]; file_magic(path, m, 6);
            gate_check(&g, m[0]=='G' && m[1]=='I' && m[2]=='F' && m[3]=='8', "GIF magic");
            int dw, dh, dc; unsigned char *dec = stbi_load(path, &dw, &dh, &dc, 3);
            gate_check(&g, dec != NULL, "GIF decode");
            if (dec) {
                gate_check(&g, dw == w && dh == h, "GIF dims");
                unsigned char *pal = (unsigned char *)malloc((size_t)w*h*3);
                gen_palette(pal, w, h);
                gate_check(&g, memcmp(dec, pal, (size_t)w*h*3) == 0, "GIF palette round-trip byte-exact");
                free(pal); stbi_image_free(dec);
            }
        }
    }

    /* WebP: stb_image has no WebP codec, so WebP is not decoded or asserted here - not claimed as covered. */

    free(src);
    return gate_finish(&g);
}
