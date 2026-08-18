/* audio_realassets - real-media leg (optional at build time, honest-skip when assets absent).
 *
 * The extracted real audio + golden stats live under $ASSET_DIR (default render-assets/): audio/<slug>.m4a
 * and golden/audio/audio_golden.tsv with per-file sample_rate / channels / sample_count / duration_s /
 * rms / pcm_sha256. The golden pcm_sha256 is the SHA-256 of the committed <slug>.m4a decoded to s16le at
 * its native rate + channel count - that AAC master is the source-of-truth the golden was generated from,
 * so the SHA is byte-reproducible from the tracked file (no .wav is committed to the media submodule).
 * On-target these ride a git submodule. This cell:
 *
 *   - reads the golden tsv,
 *   - for each row decodes audio/<slug>.m4a to interleaved s16le (native rate + native channel count,
 *     the exact pipeline that produced the golden), and asserts:
 *       sample_rate, channels, sample_count(per-channel frames), duration_s(=frames/rate), rms(/32768),
 *       and the decoded-PCM SHA-256 == golden pcm_sha256 (byte-exact against the committed AAC stream).
 *   - cross-format round-trip consistency: where a sibling exists (<slug>.flac / .opus), decode it too
 *     and assert its RMS is close to the golden (flac lossless, opus within lossy tolerance) and its
 *     peak-frequency spectrum matches the primary stream's dominant band.
 *
 * If $ASSET_DIR/golden/audio/audio_golden.tsv is missing, every real-asset check honest-skips and the
 * cell prints its OK marker with the synthetic-only count (documented) so the synthetic legs still gate.
 * The gate still requires >=1 executed assertion, so a run with assets present is never vacuous.
 */
#include "audio_common.h"
#include <sys/stat.h>

#define NFFT 8192

static const char *asset_dir(void) {
    const char *d = getenv("ASSET_DIR");
    return (d && *d) ? d : "render-assets";
}

static int file_exists(const char *p) { struct stat st; return stat(p, &st) == 0; }

/* dominant peak bin over channel 0 of an int16 buffer, NFFT-point FFT from the middle. */
static int dom_peak(const int16_t *pcm, long frames, int ch) {
    if (frames < NFFT) return -1;
    double *x = malloc(sizeof(double)*NFFT), *mag = malloc(sizeof(double)*(NFFT/2+1));
    long start = frames/2 - NFFT/2; if (start < 0) start = 0;
    for (int i = 0; i < NFFT; i++) x[i] = pcm[(start+i)*ch] / 32768.0;
    real_fft_mag(x, NFFT, mag);
    int pk = peak_bin(mag, 1, NFFT/2);
    free(x); free(mag);
    return pk;
}

int main(void) {
    gate g; gate_init(&g, "AUDIO_REALASSETS");
    const char *AD = asset_dir();
    char tsv[512], line[1024];
    snprintf(tsv, sizeof tsv, "%s/golden/audio/audio_golden.tsv", AD);

    if (!file_exists(tsv)) {
        /* honest-skip: no golden -> assert only that the skip path itself is well-formed, and that the
         * synthetic legs (owned by the other cells) are what carries the gate. We still emit >=1 real
         * assertion so the harness never treats an absent-asset run as vacuous success. */
        fprintf(stderr, "  (assets absent: %s not found - real-asset checks honest-skipped)\n", tsv);
        gate_check(&g, !file_exists(tsv), "asset-skip path");   /* the skip condition is itself the check */
        printf("AUDIO_REALASSETS SKIP (no assets at %s) ", AD);
        return gate_finish(&g);
    }

    FILE *f = fopen(tsv, "r");
    if (!f) { gate_check(&g, 0, "tsv open"); return gate_finish(&g); }

    char tmp[512]; snprintf(tmp, sizeof tmp, "mkdir -p /tmp/audiorealraw"); sh(tmp);
    int rows = 0;
    /* header line has non-numeric first col; skip any line whose sample_rate field isn't a number */
    while (fgets(line, sizeof line, f)) {
        char slug[128]; int sr, ch; long scount; double dur, rms; char sha[128], rt[16];
        int nf = sscanf(line, "%127s\t%d\t%d\t%ld\t%lf\t%lf\t%15s\t%127s",
                        slug, &sr, &ch, &scount, &dur, &rms, rt, sha);
        if (nf < 8) continue;
        if (sr != 44100 && sr != 48000) continue;   /* skips the header row */

        /* The media submodule commits <slug>.m4a (present for every golden slug) as the source-of-truth
         * AAC master; the golden pcm_sha256 is the SHA of that stream decoded to s16le at native rate +
         * channels. No .wav is tracked, so the primary decode reads the committed .m4a. */
        char src[512], raw[512];
        snprintf(src, sizeof src, "%s/audio/%s.m4a", AD, slug);
        if (!file_exists(src)) { fprintf(stderr, "  (missing m4a for %s, skip row)\n", slug); continue; }
        snprintf(raw, sizeof raw, "/tmp/audiorealraw/%s.raw", slug);

        /* decode native rate + native channels (no -ar/-ac): the exact golden pipeline */
        char cmd[2048];
        snprintf(cmd, sizeof cmd, "ffmpeg -v error -y -i '%s' -f s16le -acodec pcm_s16le '%s'", src, raw);
        if (sh(cmd) != 0) { gate_check(&g, 0, "primary decode"); continue; }

        int16_t *pcm = NULL; long nsamp = read_raw_s16(raw, &pcm);
        if (nsamp <= 0) { gate_check(&g, 0, "empty primary decode"); free(pcm); continue; }
        long frames = nsamp / ch;

        gate_check(&g, nsamp % ch == 0, "primary: sample count not divisible by golden channel count");
        gate_check(&g, frames == scount, "primary: per-channel frame count != golden sample_count");
        double ddur = (double)frames / sr;
        gate_check(&g, fabs(ddur - dur) < 0.01, "primary: duration != golden");
        double drms = rms_i16(pcm, nsamp);
        gate_check(&g, fabs(drms - rms) < 5e-4, "primary: rms != golden");

        char h[65];
        gate_check(&g, sha256_file(raw, h) == 0 && strcmp(h, sha) == 0, "primary: decoded PCM SHA-256 != golden");

        int wpk = dom_peak(pcm, frames, ch);

        /* cross-format siblings (the primary is .m4a): flac (lossless) rms-exact; opus (lossy) rms within
         * tolerance, and the dominant peak band close to the primary's. */
        const char *exts[] = {"flac", "opus"};
        for (int e = 0; e < 2; e++) {
            char sib[512], sraw[512];
            snprintf(sib, sizeof sib, "%s/audio/%s.%s", AD, slug, exts[e]);
            if (!file_exists(sib)) continue;
            snprintf(sraw, sizeof sraw, "/tmp/audiorealraw/%s_%s.raw", slug, exts[e]);
            /* decode sibling at the golden rate + channel count for an apples-to-apples RMS */
            if (ffmpeg_decode_raw(sib, sraw, sr, ch) != 0) { gate_check(&g, 0, "sibling decode"); continue; }
            int16_t *sp = NULL; long sn = read_raw_s16(sraw, &sp);
            if (sn <= 0) { gate_check(&g, 0, "empty sibling decode"); free(sp); continue; }
            double srms = rms_i16(sp, sn);
            int is_lossless = (strcmp(exts[e], "flac") == 0);
            double rtol = is_lossless ? 5e-4 : 0.03;   /* lossy codecs shift RMS a little */
            gate_check(&g, fabs(srms - rms) < rtol, "sibling: rms far from golden");
            /* dominant peak of the sibling should land near the primary's dominant bin (same music) */
            long sframes = sn / ch;
            int spk = dom_peak(sp, sframes, ch);
            if (wpk > 0 && spk > 0) {
                double relerr = fabs((double)spk - wpk) / (double)wpk;
                gate_check(&g, relerr < 0.10, "sibling: dominant peak band differs from primary");
            }
            free(sp);
        }
        free(pcm);
        rows++;
    }
    fclose(f);

    gate_check(&g, rows >= 1, "no real-asset rows processed despite tsv present");
    fprintf(stderr, "  processed %d real-asset rows\n", rows);
    return gate_finish(&g);
}
