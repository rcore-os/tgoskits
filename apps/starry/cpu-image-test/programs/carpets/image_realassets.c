/* image_realassets - decode real rasters, dimension + channel + downscaled-signature golden (cell 4).
 *
 * Decode the real PNG rasters shipped in ASSET_DIR (honkai3_base 1920x1080, honkai3_wall_home 1024x1024)
 * with stb_image and assert:
 *   - exact width / height and native channel count (base is RGBA, wall is RGB),
 *   - a downscaled 8x8-block signature SHA-256 matches the calibrated golden. The signature averages the
 *     image into an 8x8x4 fingerprint and hashes it, binding the decode to a golden without hardcoding a
 *     multi-megapixel full-frame SHA - robust to nothing (a single changed block flips it), sensitive to
 *     real content change.
 *   - the decoded buffer is non-trivial (not all one value) - a real image, not a blank.
 * The assets are a git submodule that prebuild stages onto the target rootfs, so on-target they are always
 * present. Their absence means a submodule/staging failure, not a legitimate skip - the cell hard-fails so
 * that failure cannot pass unnoticed. A decode/dimension/signature mismatch also fails the cell.
 */
#include "image_common.h"

#define STB_IMAGE_IMPLEMENTATION
#include "third_party/stb_image.h"

typedef struct {
    const char *name;
    int w, h, native_ch;
    const char *sig_sha;
} real_golden;

/* Calibrated host-side against the stb_image pinned in third_party/. */
static const real_golden G[] = {
    { REAL_HONKAI_BASE, 1920, 1080, 4,
      "8b988f508825cd5fe82a92db137a99245f70bd8fbad5bfe65f7834cb604f1912" },
    { REAL_HONKAI_WALL, 1024, 1024, 3,
      "245e39a6b6cd7f7b63ce458d96ee81a7bd1b217ce6ecf83a1238230dcdfec320" },
};
#define NG ((int)(sizeof(G)/sizeof(G[0])))

int main(void) {
    gate g; gate_init(&g, "IMAGE_REALASSETS");
    char path[512];

    /* The rasters are a prebuild-staged submodule; absence is a staging failure, not a skip. Hard-fail. */
    image_path(path, sizeof path, REAL_HONKAI_BASE);
    { FILE *f = fopen(path, "rb");
      if (!f) {
          fprintf(stderr, "  FAIL: ASSET_DIR '%s' missing '%s' - real assets MUST be staged on-target (submodule)\n",
                  image_dir(), REAL_HONKAI_BASE);
          gate_check(&g, 0, "real assets MUST be staged on-target (submodule)");
          return gate_finish(&g);
      }
      fclose(f); }

    int checked = 0;
    for (int i = 0; i < NG; i++) {
        image_path(path, sizeof path, G[i].name);
        int w, h, ch; unsigned char *px = stbi_load(path, &w, &h, &ch, 4);
        gate_check(&g, px != NULL, G[i].name);
        if (!px) continue;
        checked++;
        gate_check(&g, w == G[i].w && h == G[i].h, G[i].name);       /* exact dims */
        gate_check(&g, ch == G[i].native_ch, G[i].name);             /* native channel count */

        unsigned char sig[8*8*4]; rgba_signature(px, w, h, sig);
        char hex[65]; sha256_buf(sig, sizeof sig, hex);
        if (strcmp(hex, G[i].sig_sha) != 0) fprintf(stderr, "  %s sig=%s\n", G[i].name, hex);
        gate_check(&g, strcmp(hex, G[i].sig_sha) == 0, G[i].name);   /* signature golden */

        /* non-trivial: not a flat buffer */
        int varied = 0;
        unsigned char first = px[0];
        for (size_t k = 4; k < (size_t)w * h * 4; k += 4) if (px[k] != first) { varied = 1; break; }
        gate_check(&g, varied, G[i].name);
        stbi_image_free(px);
    }
    gate_check(&g, checked >= 1, "no real assets decoded");
    fprintf(stderr, "  image_realassets: decoded %d real rasters under %s\n", checked, image_dir());

    return gate_finish(&g);
}
