/* image_svg - vector rasterization -> per-pixel closed form (cell 3).
 *
 * Drive nanosvg (parse) + nanosvgrast (rasterize) and assert per-pixel closed-form output for synthetic
 * SVGs with analytically-known geometry, then bind the real 3DBenchy SVG to a calibrated golden.
 *
 * Closed-form legs (rasterizer-independent - the geometry is exact, only anti-aliased edges are excluded
 * from the strict region):
 *   - <circle cx=50 cy=50 r=30 fill=red> at 100x100: every pixel with dist<28 of center is solid red
 *     (a==255, R>200, G/B<40); every pixel with dist>32 is fully transparent (a==0). The center is red,
 *     the corner is transparent.
 *   - <rect x=20 y=30 w=40 h=25 fill=green>: the interior is solid green, the exterior transparent, and
 *     the left edge is sharp (x=21 opaque, x=18 transparent) - a hard boundary, not a blur.
 *   - two-stop horizontal <linearGradient> black->white over a full-cover rect: each pixel's R==G==B and
 *     equals the closed-form linear interpolation x*255/100 within +/-4; the row is monotonic
 *     non-decreasing; the ends are ~0 and ~251.
 *   - fill-rule even-odd vs nonzero on nested same-direction rects: even-odd punches the inner hole
 *     (center a==0), nonzero fills it solid (center a==255) - a real discriminator between the two rules.
 *
 * Scale invariance: the circle rasterized at 1x (100x100) and 2x (200x200, scale=2) inks ~4x as many
 * pixels (area scales with scale^2) within 3%.
 *
 * Real leg (references ASSET_DIR): rasterize the 3DBenchy SVG at a fixed 512-wide size and assert the
 * nanosvg output SHA-256 matches the calibrated golden (reproducible with the pinned nanosvg), the inked
 * pixel count, and the parsed viewBox-derived dimensions. Honest-skip if the SVG is absent.
 */
#include "image_common.h"

#define STB_IMAGE_IMPLEMENTATION
#include "third_party/stb_image.h"
#define NANOSVG_IMPLEMENTATION
#include "third_party/nanosvg.h"
#define NANOSVGRAST_IMPLEMENTATION
#include "third_party/nanosvgrast.h"

/* nanosvgParse mutates its input, so always hand it a writable copy (no strdup: musl-safe manual dup). */
static char *dup_str(const char *s) {
    size_t L = strlen(s) + 1; char *d = (char *)malloc(L); memcpy(d, s, L); return d;
}
static unsigned char *rasterize(const char *svg, int W, int H, float scale) {
    char *d = dup_str(svg);
    NSVGimage *img = nsvgParse(d, "px", 96.0f);
    free(d);
    if (!img) return NULL;
    unsigned char *px = (unsigned char *)calloc((size_t)W * H * 4, 1);
    NSVGrasterizer *r = nsvgCreateRasterizer();
    nsvgRasterize(r, img, 0, 0, scale, px, W, H, W * 4);
    nsvgDeleteRasterizer(r); nsvgDelete(img);
    return px;
}
#define A4(px, x, y, W) ((px) + ((size_t)(y) * (W) + (x)) * 4)

/* Calibrated golden for the real benchy raster (nanosvg pinned in third_party/, scale=512/width). */
#define BENCHY_RASTER_SHA "a43cc8b9ad829f82dc1d4a6bdfdb0897172eca741599828a6060fa99b8ff013c"
#define BENCHY_INK        993

int main(void) {
    gate g; gate_init(&g, "IMAGE_SVG");

    /* ---- circle: closed-form inside/outside ---- */
    {
        const char *svg = "<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">"
                          "<circle cx=\"50\" cy=\"50\" r=\"30\" fill=\"rgb(255,0,0)\"/></svg>";
        unsigned char *px = rasterize(svg, 100, 100, 1.0f);
        gate_check(&g, px != NULL, "circle parse/raster");
        if (px) {
            unsigned char *c = A4(px, 50, 50, 100), *k = A4(px, 5, 5, 100);
            gate_check(&g, c[0] > 200 && c[1] < 40 && c[2] < 40 && c[3] == 255, "circle center solid red");
            gate_check(&g, k[3] == 0, "circle far corner transparent");
            long inside = 0, inside_exp = 0, outside_nz = 0;
            for (int y = 0; y < 100; y++) for (int x = 0; x < 100; x++) {
                double dd = (x-50)*(x-50) + (y-50)*(y-50);
                unsigned char *p = A4(px, x, y, 100);
                if (dd <= 28.0*28.0) { inside_exp++; if (p[3]==255 && p[0]>200 && p[1]<40 && p[2]<40) inside++; }
                if (dd >= 32.0*32.0 && p[3] != 0) outside_nz++;
            }
            gate_check(&g, inside == inside_exp && inside_exp > 0, "circle interior all solid red");
            gate_check(&g, outside_nz == 0, "circle exterior all transparent");
            free(px);
        }
    }

    /* ---- rect: closed-form interior + sharp edge ---- */
    {
        const char *svg = "<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">"
                          "<rect x=\"20\" y=\"30\" width=\"40\" height=\"25\" fill=\"rgb(0,255,0)\"/></svg>";
        unsigned char *px = rasterize(svg, 100, 100, 1.0f);
        gate_check(&g, px != NULL, "rect parse/raster");
        if (px) {
            gate_check(&g, A4(px,40,42,100)[3] == 255 && A4(px,40,42,100)[1] > 200, "rect interior solid green");
            gate_check(&g, A4(px,10,10,100)[3] == 0, "rect exterior transparent");
            gate_check(&g, A4(px,21,42,100)[3] == 255 && A4(px,18,42,100)[3] == 0, "rect left edge sharp");
            long fill = 0;
            for (int y = 31; y < 54; y++) for (int x = 21; x < 59; x++) {
                unsigned char *p = A4(px, x, y, 100);
                if (p[3]==255 && p[1]>200 && p[0]<40 && p[2]<40) fill++;
            }
            gate_check(&g, fill == 38 * 23, "rect interior pixel count exact");
            free(px);
        }
    }

    /* ---- linear gradient: per-pixel closed-form interpolation ---- */
    {
        const char *svg = "<svg width=\"100\" height=\"20\" xmlns=\"http://www.w3.org/2000/svg\">"
            "<defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"100\" y2=\"0\" gradientUnits=\"userSpaceOnUse\">"
            "<stop offset=\"0\" stop-color=\"rgb(0,0,0)\"/><stop offset=\"1\" stop-color=\"rgb(255,255,255)\"/>"
            "</linearGradient></defs><rect x=\"0\" y=\"0\" width=\"100\" height=\"20\" fill=\"url(#g)\"/></svg>";
        unsigned char *px = rasterize(svg, 100, 20, 1.0f);
        gate_check(&g, px != NULL, "gradient parse/raster");
        if (px) {
            int maxerr = 0, mono = 1, gray = 1, prev = -1;
            for (int x = 0; x < 100; x++) {
                unsigned char *p = A4(px, x, 10, 100);
                if (!(p[0] == p[1] && p[1] == p[2])) gray = 0;      /* black->white => grayscale */
                int expect = x * 255 / 100;
                int e = p[0] - expect; if (e < 0) e = -e; if (e > maxerr) maxerr = e;
                if (p[0] < prev) mono = 0;
                prev = p[0];
            }
            gate_check(&g, gray, "gradient row is grayscale (R==G==B)");
            gate_check(&g, maxerr <= 4, "gradient per-pixel within +/-4 of closed-form linear");
            gate_check(&g, mono, "gradient monotonic non-decreasing");
            gate_check(&g, A4(px,0,10,100)[0] <= 4 && A4(px,99,10,100)[0] >= 247, "gradient endpoints ~0 and ~255");
            free(px);
        }
    }

    /* ---- fill-rule: even-odd vs nonzero on nested same-direction rects ---- */
    {
        const char *eo = "<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">"
            "<path fill-rule=\"evenodd\" fill=\"rgb(0,0,255)\" d=\"M10 10 H90 V90 H10 Z M30 30 H70 V70 H30 Z\"/></svg>";
        const char *nz = "<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">"
            "<path fill-rule=\"nonzero\" fill=\"rgb(0,0,255)\" d=\"M10 10 H90 V90 H10 Z M30 30 H70 V70 H30 Z\"/></svg>";
        unsigned char *e = rasterize(eo, 100, 100, 1.0f), *n = rasterize(nz, 100, 100, 1.0f);
        gate_check(&g, e && n, "fill-rule parse/raster");
        if (e && n) {
            gate_check(&g, A4(e,50,50,100)[3] == 0,   "even-odd punches inner hole");
            gate_check(&g, A4(e,20,50,100)[3] == 255,  "even-odd fills outer ring");
            gate_check(&g, A4(n,50,50,100)[3] == 255,  "nonzero fills inner region");
            gate_check(&g, A4(n,20,50,100)[3] == 255,  "nonzero fills outer ring");
        }
        free(e); free(n);
    }

    /* ---- scale invariance: 1x vs 2x circle ink ratio ~4 ---- */
    {
        const char *svg = "<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">"
                          "<circle cx=\"50\" cy=\"50\" r=\"30\" fill=\"rgb(255,0,0)\"/></svg>";
        unsigned char *p1 = rasterize(svg, 100, 100, 1.0f);
        unsigned char *p2 = rasterize(svg, 200, 200, 2.0f);
        gate_check(&g, p1 && p2, "scale parse/raster");
        if (p1 && p2) {
            long ink1 = 0, ink2 = 0;
            for (int i = 0; i < 100*100; i++) if (p1[i*4+3] > 128) ink1++;
            for (int i = 0; i < 200*200; i++) if (p2[i*4+3] > 128) ink2++;
            gate_check(&g, ink1 > 0 && ink2 > 0, "scale both inked");
            double ratio = (double)ink2 / (double)ink1;
            if (!(ratio > 3.85 && ratio < 4.15)) fprintf(stderr, "  scale ratio=%.4f (ink1=%ld ink2=%ld)\n", ratio, ink1, ink2);
            gate_check(&g, ratio > 3.85 && ratio < 4.15, "2x circle inks ~4x (area scales scale^2)");
        }
        free(p1); free(p2);
    }

    /* ---- real 3DBenchy SVG vs calibrated golden (honest-skip if absent) ---- */
    {
        char path[512]; image_path(path, sizeof path, SVG_BENCHY);
        FILE *f = fopen(path, "rb");
        if (f) {
            fclose(f);
            NSVGimage *img = nsvgParseFromFile(path, "px", 96.0f);
            gate_check(&g, img != NULL, "benchy parse");
            if (img) {
                gate_check(&g, img->width > 0 && img->height > 0, "benchy has dimensions");
                float scale = 512.0f / img->width;
                int W = 512, H = 512;
                unsigned char *px = (unsigned char *)calloc((size_t)W * H * 4, 1);
                NSVGrasterizer *r = nsvgCreateRasterizer();
                nsvgRasterize(r, img, 0, 0, scale, px, W, H, W * 4);
                char hex[65]; sha256_buf(px, (size_t)W * H * 4, hex);
                if (strcmp(hex, BENCHY_RASTER_SHA) != 0) fprintf(stderr, "  benchy sha=%s\n", hex);
                gate_check(&g, strcmp(hex, BENCHY_RASTER_SHA) == 0, "benchy raster SHA vs golden");
                long ink = 0; for (int i = 0; i < W*H; i++) if (px[i*4+3] > 0) ink++;
                gate_check(&g, ink == BENCHY_INK, "benchy inked pixel count vs golden");
                nsvgDeleteRasterizer(r); free(px); nsvgDelete(img);
            }
        } else {
            fprintf(stderr, "  SKIP: benchy.svg absent - real vector leg skipped (documented)\n");
        }
    }

    return gate_finish(&g);
}
