/* video_frames - frame-exact decode carpet (cell 1).
 *
 * Two legs, both deterministic and pixel-exact:
 *
 *   A. Bad Apple binary-frame leg (references $ASSET_DIR, honest-skips if absent).
 *      Bad Apple is a ~1-bit black/white silhouette animation: a decoded frame is essentially binary,
 *      so it is deterministically comparable pixel-exact. For each of the 16 golden frames we:
 *        - decode the PNG to raw rgb24, assert sha256(rgb24) == golden (byte-exact whole frame),
 *        - assert the 8x8-bicubic-gray signature == golden luma8x8_hex (the exact golden recipe),
 *        - threshold each pixel to B/W (Rec.601 luma >= 128) and assert the white-pixel ratio ==
 *          the golden ratio within a tight epsilon (the binary silhouette descriptor).
 *      The golden sha/luma/ratio table is embedded so this leg is self-checking against the staged
 *      frames without any network.
 *
 *   B. Synthetic testsrc leg (always runs, fully independent of any asset).
 *      Generate a frame from a known test pattern via ffmpeg lavfi, decode to raw rgb24, and assert
 *      closed-form pixel values:
 *        - smptebars: the seven 75% color bars sit at known column ranges; assert each bar center
 *          pixel equals the known bar color within tolerance (deterministic, no golden file).
 *        - a synthesized rgb24 gradient (built in C, encoded ffv1 lossless, decoded back): assert the
 *          decoded frame is byte-identical to the source (lossless round-trip) and that the closed-form
 *          linear ramp holds pixel-exact.
 *        - a synthesized checkerboard (built in C): assert the alternating 8x8 tiles decode exactly.
 *
 * The gate needs >=1 executed assertion and fail==0; the synthetic leg alone always supplies many
 * assertions, so an absent-asset run is never vacuous.
 */
#include "video_common.h"
#include <sys/stat.h>

#define BA_W 1920
#define BA_H 1080

static const char *asset_dir(void) {
    const char *d = getenv("ASSET_DIR");
    return (d && *d) ? d : "assets";
}
static int file_exists(const char *p) { struct stat st; return stat(p, &st) == 0; }

/* Golden table for the 16 Bad Apple frames: filename, white-ratio(thr=128, Rec.601 int weights),
 * sha256(rgb24), luma8x8_hex. Reproduced host-side against render-assets/golden. */
typedef struct { const char *fn; double white; const char *sha; const char *luma; } bagold;
static const bagold BA[] = {
  {"frame_00.png", 0.467196, "c3e500c0631b8ae80e1a5a319cf22d0cddb5bbebd39ba3da45fb6cf31316d033",
   "08f5f9ef2900b51108ffffe60000b81207feffec0000b41207fefffe1200dd0d07fdffff0e1cf90a07fefffd010feb0c08fffff80000711606f4fac300151002"},
  {"frame_01.png", 0.064259, "dd8a48bc365ba99cb5b3a47d350f2c735bb4fff5a65f4e6954fec5de4a3a4f35",
   "0000000000000000000000001c550000000000001ebf00000000000033e7b00100000000235f8305000000000000000000000000000000000000000000000000"},
  {"frame_02.png", 0.594969, "0420e0e6c1ea7ef1913b69bd8d6d50cd5ba901f606f68cb92f5dc8c8578e2b5f",
   "08f5f9f9f9f9f50808ffffffffffff0807fffbc0fffffe0707fdff8a7fffff0707feff5200eeff0706feff3100ceff0708ffffae0038e50e06f3f8ff52001b01"},
  {"frame_03.png", 0.336464, "c9ba232205409ef24b70d2df973f4f52cc4ab2155a94767d19887f91ad2a9eef",
   "0000000021b87a00000000008cffcf00000000008bffd500000d720f6ed26900008eb27e1ce8cc00007b7aaca8ffff0700350665b0ffff090000000064f9f305"},
  {"frame_04.png", 0.244096, "d26937ace25197b4ff3e4540eac3c8b998f8155fd2ff83d08d6a8019e1a1ddd8",
   "00001562000000000000b4ff150000000000e9ff390000000000ddff4c0000000000d7ff710000000000d1fe3d0000000000f4ff5f000000001dfaf972000000"},
  {"frame_05.png", 0.371288, "1b59b67e24d09931ffff2ed8508c4c6a5fee472020e9cbc3489de3e7dcb8c937",
   "00000007f5faf50800000008feffff080000005fa3fffe070000006b95fffe07000000669afffe070000008876fffe070000006b8fffff080000008b6cf8f406"},
  {"frame_06.png", 0.425425, "333c5a723320a241745c11eed5ea9f6f565f745b7bc3919d21d79cf94da58cf7",
   "0012009cfbfce900008f255dffffea00009d6916dfffcc0000c6b1006fff940000bbac003dffac00005de5003effeb000093ff1841fffe000066be1057db7a00"},
  {"frame_07.png", 0.196481, "58b81138710e90b8647f2a032ecff6c14fb80d6ad2e5ff793e7fd9b60306665b",
   "00000000000004000000000d28000000000000b2fa080000000000b6ff21000000000078ff110000000000b4ff090000000002f2ff82000000003ef8f5f42500"},
  {"frame_08.png", 0.227065, "4ab9035c5d77e0cc9c78d91b61330cc534094473cfab81a6915a02647082376d",
   "000000863d0000000000006094000000000000ecff340000000000efff6700000000009bff7700000000004fff7f000000000084ff950000000000d1f9bc0000"},
  {"frame_09.png", 0.616767, "16960f6f6f86d1ec7cf4b61d9145e89680fc0ae0ae500c08b06e7de17fd7fcc1",
   "08f5fefbf9f9f50808ffddf1ffffff0807ff90c1fffffe0706ff88dafc89fc0807ff6fd6fb75fe0808ff51e8d970ff0709ff30d3e786ff0808ee1abce588f706"},
  {"frame_10.png", 0.000000, "750e977efcc9a29385af01b58cd7e5358f502aa8bd2ac14f532cf5fc04ac298a",
   "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"},
  {"frame_11.png", 0.451667, "0cdf4df2dea92cc13fb2499a4da28927120da20f541cabf9a138980d0338a316",
   "08f7e10578fbf50808ffb40078ffff0807ffce5383cef30a07ffc6ad4700400007ffe7420000710c07fffb9c00006f1708fffa970039e50c06f6e63000b4fb06"},
  {"frame_12.png", 0.012966, "715bd8d18e1bac24f80bd735d7ac528ad98d8bf183431e563c48cbfb3b471434",
   "00000100000000000000000000000000000000000000000000000000000000000000000002000000000000000200000000000000020000000000000000000000"},
  {"frame_13.png", 0.035234, "ac471caf22fb6c1b172614c4347be745f8ddb7782f0eea47f86388fb7a54b340",
   "0000698e000000000000289f00000000000000790000000000000015000000000000000000000000000000000000000000000000000000000000000000000000"},
  {"frame_14.png", 0.366555, "db943540649cdbaa473e994bdb0257aa96ead52807f5ac6e2f6d9532749a3f0b",
   "0015eefafefbe30a007bffffa3e1b50d00c1ffffbaea650700e5d4a0ffdc150400d1200048190b07007e153900002e1000311b4500007616000000000008d20c"},
  {"frame_15.png", 0.013936, "5ae2057debb91c716c971cfb38a9fb2ada0b2463b70743a4f9f4d8d9456c6c88",
   "0000000000000000000000211c000000000000110f00000000000000000000000000000000000000000000040300000000000000000000000000000000000000"},
};
static const int NBA = sizeof(BA) / sizeof(BA[0]);

static const char *TMP = "/tmp/videoframes";

/* ---- leg A: Bad Apple binary frames vs embedded golden ---- */
static int badapple_leg(gate *g, const char *AD) {
    char dir[512]; snprintf(dir, sizeof dir, "%s/video/badapple_frames", AD);
    char probe[600]; snprintf(probe, sizeof probe, "%s/%s", dir, BA[0].fn);
    if (!file_exists(probe)) {
        fprintf(stderr, "  (Bad Apple frames absent at %s - binary-frame leg honest-skipped)\n", dir);
        return 0; /* skipped, contributes no assertions */
    }
    int processed = 0;
    for (int i = 0; i < NBA; i++) {
        char png[700], rgb[512], gray[512];
        snprintf(png, sizeof png, "%s/%s", dir, BA[i].fn);
        snprintf(rgb, sizeof rgb, "%s/f.rgb", TMP);
        snprintf(gray, sizeof gray, "%s/f.gray", TMP);
        if (!file_exists(png)) { fprintf(stderr, "  (missing %s)\n", png); continue; }

        /* whole-frame rgb24 sha byte-exact */
        if (ffmpeg_frame_rgb24(png, -1, "", rgb) != 0) { gate_check(g, 0, "rgb24 decode"); continue; }
        char h[65];
        gate_check(g, sha256_file(rgb, h) == 0 && strcmp(h, BA[i].sha) == 0,
                   "badapple: rgb24 frame sha != golden");

        /* 8x8 bicubic-gray signature byte-exact (golden luma8x8_hex) */
        if (ffmpeg_luma8x8(png, -1, gray) != 0) { gate_check(g, 0, "luma8x8 decode"); continue; }
        unsigned char *lb = NULL; long ln = read_file_bytes(gray, &lb);
        char lhex[129] = {0};
        if (ln == 64) hex_encode(lb, 64, lhex);
        gate_check(g, ln == 64 && strcmp(lhex, BA[i].luma) == 0,
                   "badapple: 8x8 luma signature != golden");
        free(lb);

        /* binary white-ratio: threshold rgb24 -> luma>=128, fraction white == golden within eps */
        frame fr;
        if (frame_read(rgb, BA_W, BA_H, 3, &fr) == 0) {
            unsigned char *y = frame_to_luma(&fr);
            double wr = white_ratio(y, (long)BA_W * BA_H, 128);
            gate_check(g, fabs(wr - BA[i].white) < 1e-4, "badapple: binary white-ratio != golden");
            /* Bad Apple is a ~1-bit silhouette: the vast majority of pixels are near-black or
             * near-white. Most sampled frames are <4%% mid-tone; one sampled shot (frame_06) is a
             * grayscale gradient at ~19%%, so the bound tolerates that while still rejecting a frame
             * that decoded to noise (which would push mid-tone fraction far higher). */
            long mid = 0, np = (long)BA_W * BA_H;
            for (long p = 0; p < np; p++) if (y[p] > 40 && y[p] < 215) mid++;
            gate_check(g, (double)mid / np < 0.20, "badapple: frame not ~binary (too many mid-tones)");
            free(y); frame_free(&fr);
        } else gate_check(g, 0, "badapple: rgb24 geometry mismatch");
        processed++;
    }
    gate_check(g, processed == NBA, "badapple: not all 16 golden frames processed");
    fprintf(stderr, "  badapple: processed %d/%d frames\n", processed, NBA);
    return processed;
}

/* ---- leg B1: smptebars closed-form bar colors ---- */
/* 75% SMPTE bars center colors as ffmpeg's smptebars emits them (measured host-side, rgb24). */
static void smptebars_leg(gate *g) {
    const int W = 320, H = 240;
    static const unsigned char bar[7][3] = {
        {190,190,190}, {192,190,0}, {0,190,189}, {0,188,0},
        {190,0,191},   {191,0,0},   {0,0,191},
    };
    char raw[512]; snprintf(raw, sizeof raw, "%s/smpte.rgb", TMP);
    char cmd[1024];
    snprintf(cmd, sizeof cmd,
        "ffmpeg -v error -y -f lavfi -i \"smptebars=size=%dx%d:rate=1:duration=1\" "
        "-vframes 1 -f rawvideo -pix_fmt rgb24 '%s'", W, H, raw);
    if (sh(cmd) != 0) { gate_check(g, 0, "smptebars generate"); return; }
    frame fr;
    if (frame_read(raw, W, H, 3, &fr) != 0) { gate_check(g, 0, "smptebars geometry"); return; }
    int y = 40; /* inside the top bar band */
    for (int b = 0; b < 7; b++) {
        int x = (int)((b + 0.5) * W / 7);
        long i = ((long)y * W + x) * 3;
        int dr = abs((int)fr.px[i] - bar[b][0]);
        int dg = abs((int)fr.px[i+1] - bar[b][1]);
        int db = abs((int)fr.px[i+2] - bar[b][2]);
        gate_check(g, dr <= 3 && dg <= 3 && db <= 3, "smptebars: bar color off (closed form)");
    }
    frame_free(&fr);
}

/* ---- leg B2/B3: synthesized gradient + checkerboard, ffv1 lossless round-trip ---- */
static void synth_lossless_leg(gate *g) {
    const int W = 128, H = 96;
    long np = (long)W * H, nb = np * 3;
    unsigned char *grad = (unsigned char *)malloc(nb);
    unsigned char *chk  = (unsigned char *)malloc(nb);
    for (int y = 0; y < H; y++)
        for (int x = 0; x < W; x++) {
            long i = ((long)y * W + x) * 3;
            grad[i]   = (unsigned char)(x * 255 / (W - 1));   /* R linear in x   */
            grad[i+1] = (unsigned char)(y * 255 / (H - 1));   /* G linear in y   */
            grad[i+2] = (unsigned char)((x + y) & 0xff);      /* B ramp diagonal */
            int tile = ((x / 8) + (y / 8)) & 1;
            unsigned char v = tile ? 255 : 0;
            chk[i] = chk[i+1] = chk[i+2] = v;
        }
    struct { const char *name; unsigned char *src; } pats[2] = {
        {"gradient", grad}, {"checker", chk}
    };
    for (int p = 0; p < 2; p++) {
        char src[512], enc[512], dec[512], cmd[1400];
        snprintf(src, sizeof src, "%s/%s.rgb", TMP, pats[p].name);
        snprintf(enc, sizeof enc, "%s/%s.mkv", TMP, pats[p].name);
        snprintf(dec, sizeof dec, "%s/%s_out.rgb", TMP, pats[p].name);
        FILE *f = fopen(src, "wb");
        if (!f) { gate_check(g, 0, "synth: write src"); continue; }
        fwrite(pats[p].src, 1, nb, f); fclose(f);

        /* rgb24 raw -> ffv1 (rgb24) -> rgb24 raw: byte-identical lossless round-trip */
        snprintf(cmd, sizeof cmd,
            "ffmpeg -v error -y -f rawvideo -pix_fmt rgb24 -s %dx%d -r 5 -i '%s' "
            "-c:v ffv1 -pix_fmt rgb24 '%s'", W, H, src, enc);
        if (sh(cmd) != 0) { gate_check(g, 0, "synth: ffv1 encode"); continue; }
        if (ffmpeg_frame_rgb24(enc, -1, "", dec) != 0) { gate_check(g, 0, "synth: decode"); continue; }

        unsigned char *out = NULL; long on = read_file_bytes(dec, &out);
        gate_check(g, on == nb && memcmp(out, pats[p].src, nb) == 0,
                   "synth: ffv1 lossless round-trip not byte-exact");

        /* closed-form check on the decoded gradient: sampled pixels equal the analytic ramp */
        if (on == nb && strcmp(pats[p].name, "gradient") == 0) {
            int ok = 1;
            for (int s = 0; s < 8 && ok; s++) {
                int x = s * (W - 1) / 7, y = s * (H - 1) / 7;
                long i = ((long)y * W + x) * 3;
                unsigned char er = (unsigned char)(x * 255 / (W - 1));
                unsigned char eg = (unsigned char)(y * 255 / (H - 1));
                unsigned char eb = (unsigned char)((x + y) & 0xff);
                if (out[i] != er || out[i+1] != eg || out[i+2] != eb) ok = 0;
            }
            gate_check(g, ok, "synth: decoded gradient != closed-form ramp");
        }
        if (on == nb && strcmp(pats[p].name, "checker") == 0) {
            /* alternating 8x8 tiles: corner tile black, adjacent tile white */
            gate_check(g, out[0] == 0 && out[8*3] == 255,
                       "synth: decoded checkerboard tiles wrong");
        }
        free(out);
    }
    free(grad); free(chk);
}

int main(void) {
    gate g; gate_init(&g, "VIDEO_FRAMES");
    char cmd[256]; snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    badapple_leg(&g, asset_dir());   /* leg A: honest-skips if assets absent */
    smptebars_leg(&g);               /* leg B1: always runs */
    synth_lossless_leg(&g);          /* leg B2/B3: always runs */

    return gate_finish(&g);
}
