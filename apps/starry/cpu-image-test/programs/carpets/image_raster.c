/* image_raster - bitmap format decode -> pixels, deterministic per-pixel golden (cell 1).
 *
 * Two legs:
 *
 *  A) Format-zoo (references ASSET_DIR): the same 640x360 image encoded 6 ways lives in render-assets.
 *     Decode each with stb_image and assert:
 *       - the four LOSSLESS rasters (PNG/BMP/TGA/PPM) decode BYTE-EXACT to one identical RGB buffer -
 *         a single shared SHA-256 (0f4ff65a...), and each equals the reference. This is the strongest
 *         possible assertion: four independent format decoders converge on the same pixels bit-for-bit.
 *       - PGM (grayscale) decodes to the calibrated gray SHA at the exact dimensions (its own golden -
 *         ffmpeg's PGM luma differs from stb's RGB->luma so it is a self-consistent gray golden).
 *       - JPEG decodes within a PSNR bound (>35 dB) of the reference RGB (lossy, so no SHA).
 *       - dimensions (640x360) and native channel counts (PNG/TGA=4, BMP/PPM=3, PGM=1) exact.
 *     Honest-skip: if ASSET_DIR is absent the zoo legs are skipped (documented), the synthetic leg still
 *     runs so the cell always has assertions.
 *
 *  B) Synthetic round-trip (NO assets): generate a known checkerboard+gradient RGB pattern in memory,
 *     encode via stb_image_write to PNG/BMP/TGA, decode back with stb_image, assert byte-exact round-trip
 *     (lossless) - a closed-form golden independent of any external file. The generator's own SHA is
 *     pinned so a generator drift is also caught.
 *
 * stb_image's lossless decoders are exact integer pipelines, so all SHAs are reproducible across arches.
 */
#include "image_common.h"

#define STB_IMAGE_IMPLEMENTATION
#include "third_party/stb_image.h"
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include "third_party/stb_image_write.h"

/* Calibrated once host-side against the stb_image pinned in third_party/. */
#define ZOO_W 640
#define ZOO_H 360
#define LOSSLESS_RGB_SHA "0f4ff65a798fd65c4bef8f9a264338240d8ae9079621bc2ef6a5cd09d752294f"
#define PGM_GRAY_SHA     "efc07b742d10e0160a2508dff56283054516d4dea0fd7240b9514f58be8a8110"
#define JPEG_PSNR_MIN    35.0
#define SYN_SRC_SHA      "7087a5cce1837c424f10b7ba4fa9870d8c6e8bf2d9fca7c547ee9de4dbf1a059"

/* Deterministic synthetic pattern: R gradient over x, G gradient over y, B checkerboard. */
static void gen_pattern(unsigned char *px, int w, int h) {
    for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
        unsigned char *p = px + ((size_t)y * w + x) * 3;
        int chk = ((x / 8) ^ (y / 8)) & 1;
        p[0] = (unsigned char)(x * 255 / (w - 1));
        p[1] = (unsigned char)(y * 255 / (h - 1));
        p[2] = chk ? 200 : 40;
    }
}

/* Decode a file to req channels; returns malloc'd buffer + dims, or NULL. */
static unsigned char *decode(const char *path, int req, int *w, int *h, int *native_ch) {
    return stbi_load(path, w, h, native_ch, req);
}

/* stbi_write_* to a scratch file, then read it back and compare byte-exact to src. */
static int roundtrip(gate *g, const char *label, int fmt, const unsigned char *src, int w, int h) {
    char path[256]; snprintf(path, sizeof path, "/tmp/cpu-image-syn.%s",
                             fmt == 0 ? "png" : fmt == 1 ? "bmp" : "tga");
    int ok = 0;
    if (fmt == 0) ok = stbi_write_png(path, w, h, 3, src, w * 3);
    else if (fmt == 1) ok = stbi_write_bmp(path, w, h, 3, src);
    else ok = stbi_write_tga(path, w, h, 3, src);
    gate_check(g, ok != 0, label);
    if (!ok) return 0;
    int dw, dh, dc; unsigned char *dec = decode(path, 3, &dw, &dh, &dc);
    gate_check(g, dec != NULL, label);
    if (!dec) return 0;
    gate_check(g, dw == w && dh == h, label);
    gate_check(g, memcmp(dec, src, (size_t)w * h * 3) == 0, label);  /* byte-exact lossless round-trip */
    stbi_image_free(dec);
    return 1;
}

int main(void) {
    gate g; gate_init(&g, "IMAGE_RASTER");
    char path[512];

    /* ---- Leg A: format zoo (assets) ---- */
    const char *ref_path = image_path(path, sizeof path, ZOO_REF);
    int haveassets = 0;
    { FILE *f = fopen(ref_path, "rb"); if (f) { haveassets = 1; fclose(f); } }

    if (haveassets) {
        struct { const char *name; int req; int native; } LL[] = {
            { ZOO_REF, 3, 4 }, { ZOO_BMP, 3, 3 }, { ZOO_TGA, 3, 4 }, { ZOO_PPM, 3, 3 },
        };
        unsigned char *ref_rgb = NULL;
        for (int i = 0; i < 4; i++) {
            image_path(path, sizeof path, LL[i].name);
            int w, h, ch; unsigned char *px = decode(path, 3, &w, &h, &ch);
            gate_check(&g, px != NULL, LL[i].name);
            if (!px) continue;
            gate_check(&g, w == ZOO_W && h == ZOO_H, LL[i].name);      /* exact dims */
            gate_check(&g, ch == LL[i].native, LL[i].name);            /* native channel count */
            char hex[65]; sha256_buf(px, (size_t)w * h * 3, hex);
            gate_check(&g, strcmp(hex, LOSSLESS_RGB_SHA) == 0, LL[i].name); /* shared byte-exact SHA */
            if (i == 0) ref_rgb = px; else {
                /* redundant cross-check: identical to the reference buffer byte for byte */
                gate_check(&g, ref_rgb && memcmp(px, ref_rgb, (size_t)w * h * 3) == 0, LL[i].name);
                stbi_image_free(px);
            }
        }

        /* PGM grayscale */
        image_path(path, sizeof path, ZOO_PGM);
        { int w, h, ch; unsigned char *px = decode(path, 1, &w, &h, &ch);
          gate_check(&g, px != NULL, ZOO_PGM);
          if (px) {
              gate_check(&g, w == ZOO_W && h == ZOO_H && ch == 1, ZOO_PGM);
              char hex[65]; sha256_buf(px, (size_t)w * h, hex);
              gate_check(&g, strcmp(hex, PGM_GRAY_SHA) == 0, ZOO_PGM);
              stbi_image_free(px);
          } }

        /* JPEG lossy vs reference RGB by PSNR */
        image_path(path, sizeof path, ZOO_JPG);
        { int w, h, ch; unsigned char *jp = decode(path, 3, &w, &h, &ch);
          gate_check(&g, jp != NULL, ZOO_JPG);
          if (jp && ref_rgb) {
              gate_check(&g, w == ZOO_W && h == ZOO_H, ZOO_JPG);
              double ps = buf_psnr(ref_rgb, jp, (size_t)w * h * 3);
              if (ps <= JPEG_PSNR_MIN) fprintf(stderr, "  JPEG PSNR=%.3f dB\n", ps);
              gate_check(&g, ps > JPEG_PSNR_MIN, ZOO_JPG);           /* lossy within bound */
              stbi_image_free(jp);
          } }
        if (ref_rgb) stbi_image_free(ref_rgb);
    } else {
        fprintf(stderr, "  SKIP: ASSET_DIR absent - format-zoo legs skipped (documented); synthetic leg runs\n");
    }

    /* ---- Leg B: synthetic round-trip (no assets) ---- */
    {
        int w = 64, h = 48;
        unsigned char *src = (unsigned char *)malloc((size_t)w * h * 3);
        gen_pattern(src, w, h);
        char hex[65]; sha256_buf(src, (size_t)w * h * 3, hex);
        gate_check(&g, strcmp(hex, SYN_SRC_SHA) == 0, "synthetic pattern generator drift");
        roundtrip(&g, "synthetic PNG round-trip", 0, src, w, h);
        roundtrip(&g, "synthetic BMP round-trip", 1, src, w, h);
        roundtrip(&g, "synthetic TGA round-trip", 2, src, w, h);
        /* known-position pixels in the pattern: (0,0) is chk=0 -> B=40; corner gradient endpoints */
        gate_check(&g, src[2] == 40, "pattern (0,0) B checkerboard");
        gate_check(&g, src[((size_t)0 * w + (w - 1)) * 3 + 0] == 255, "pattern right edge R=255");
        gate_check(&g, src[((size_t)(h - 1) * w + 0) * 3 + 1] == 255, "pattern bottom edge G=255");
        free(src);
    }

    return gate_finish(&g);
}
