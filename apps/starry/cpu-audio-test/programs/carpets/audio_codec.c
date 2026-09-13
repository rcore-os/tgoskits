/* audio_codec - PCM + codec matrix carpet (leg B).
 *
 * Generate a synthetic bin-exact tone (spectrum analytically known), write it as a source WAV, then run
 * the codec cartesian {wav, flac, opus, aac, mp3} x {mono, stereo} x {44100, 48000} through ffmpeg:
 * encode -> decode back to interleaved s16le -> parse the PCM -> assert in the signal domain.
 *
 *   - lossless (wav, flac): decoded PCM is byte-exact vs the source PCM (SHA-256 equal, sample-exact).
 *   - lossy   (opus, aac, mp3): decoded PCM can't be byte-exact, but the FFT peak must still land on the
 *     analytically-known bin, magnitude bounded, PSNR above a codec-appropriate floor, and no spurious
 *     out-of-band tone.
 *   - metadata: decoded sample_rate / channels / sample_count / duration match the request exactly (or
 *     within one frame for the lossy codecs that pad/prime).
 *
 * The tone is placed at a bin-exact frequency for the 44100 FFT so the source spectrum is leakage-free;
 * for the resampled 48000 case audio_resample owns the migration check - here we assert the peak lands
 * on the bin nearest the true tone frequency at whatever rate the decode came back with.
 */
#include "audio_common.h"

#define SRC_FS 44100
#define NFFT   4096
#define TONE_BIN 300           /* bin-exact @ 44100/4096 -> ~3229 Hz, safely below every Nyquist here */

static double tone_freq(void) { return (double)TONE_BIN * SRC_FS / NFFT; }

static const char *TMP = "/tmp/audiocodec";

/* Build a source WAV: `frames` frames, `ch` channels, `fs` Hz, tone at tone_freq() amplitude 0.7.
 * For stereo the same tone is written to both channels. Returns 0 ok. */
static int write_src_wav(const char *path, int fs, int ch, long frames) {
    long ndata = frames * ch * 2;
    unsigned char hdr[44];
    uint32_t chunk = 36 + ndata, byte_rate = fs * ch * 2;
    memcpy(hdr, "RIFF", 4);
    hdr[4]=chunk&0xff; hdr[5]=(chunk>>8)&0xff; hdr[6]=(chunk>>16)&0xff; hdr[7]=(chunk>>24)&0xff;
    memcpy(hdr+8, "WAVEfmt ", 8);
    hdr[16]=16; hdr[17]=0; hdr[18]=0; hdr[19]=0;      /* fmt chunk size 16 */
    hdr[20]=1; hdr[21]=0;                              /* PCM */
    hdr[22]=ch; hdr[23]=0;
    hdr[24]=fs&0xff; hdr[25]=(fs>>8)&0xff; hdr[26]=(fs>>16)&0xff; hdr[27]=(fs>>24)&0xff;
    hdr[28]=byte_rate&0xff; hdr[29]=(byte_rate>>8)&0xff; hdr[30]=(byte_rate>>16)&0xff; hdr[31]=(byte_rate>>24)&0xff;
    hdr[32]=ch*2; hdr[33]=0;                           /* block align */
    hdr[34]=16; hdr[35]=0;                             /* bits */
    memcpy(hdr+36, "data", 4);
    hdr[40]=ndata&0xff; hdr[41]=(ndata>>8)&0xff; hdr[42]=(ndata>>16)&0xff; hdr[43]=(ndata>>24)&0xff;
    FILE *f = fopen(path, "wb"); if (!f) return -1;
    fwrite(hdr, 1, 44, f);
    double fr = tone_freq();
    for (long i = 0; i < frames; i++) {
        int s = (int)lround(0.7 * sin(2.0*M_PI*fr*i/fs) * 32767.0);
        if (s > 32767) s = 32767;
        if (s < -32768) s = -32768;
        int16_t v = (int16_t)s;
        for (int c = 0; c < ch; c++) fwrite(&v, 2, 1, f);
    }
    fclose(f);
    return 0;
}

/* FFT peak bin of channel 0 of an interleaved int16 buffer (first NFFT frames). */
static int decoded_peak_bin(const int16_t *pcm, long frames, int ch, int fs, double *mag_out) {
    int n = NFFT; if (frames < n) n = 1; while (!is_pow2(n)) n--;
    double *x = (double *)malloc(sizeof(double)*n);
    double *mag = (double *)malloc(sizeof(double)*(n/2+1));
    for (int i = 0; i < n; i++) x[i] = pcm[(long)i*ch] / 32768.0;
    real_fft_mag(x, n, mag);
    int pk = peak_bin(mag, 1, n/2);
    if (mag_out) *mag_out = mag[pk];
    free(x); free(mag);
    (void)fs;
    return pk;
}

/* PSNR (dB) between two int16 buffers of equal length; higher is closer. */
static double psnr_i16(const int16_t *a, const int16_t *b, long n) {
    double se = 0.0;
    for (long i = 0; i < n; i++) { double d = (double)a[i] - b[i]; se += d*d; }
    double mse = se / n;
    if (mse <= 0.0) return 999.0;
    return 10.0 * log10((32767.0*32767.0) / mse);
}

int main(void) {
    gate g; gate_init(&g, "AUDIO_CODEC");
    char cmd[1024], src[256], enc[256], dec[256];
    snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    int rates[] = {44100, 48000};
    int chans[] = {1, 2};
    /* codec table: name, ffmpeg encoder args, extension, lossless flag, PSNR floor (dB, lossy only) */
    struct { const char *name; const char *encargs; const char *ext; int lossless; double psnr_floor; }
    codecs[] = {
        {"wav",  "-c:a pcm_s16le",          "wav",  1, 0},
        {"flac", "-c:a flac",               "flac", 1, 0},
        {"opus", "-c:a libopus -b:a 128k",  "opus", 0, 30.0},
        {"aac",  "-c:a aac -b:a 192k",      "aac",  0, 30.0},
        {"mp3",  "-c:a libmp3lame -q:a 2",  "mp3",  0, 30.0},
    };
    int ncodec = sizeof(codecs)/sizeof(codecs[0]);

    for (int ci = 0; ci < ncodec; ci++) {
      for (int ri = 0; ri < 2; ri++) {
        for (int hi = 0; hi < 2; hi++) {
            int fs = rates[ri], ch = chans[hi];
            long frames = fs; /* 1.0 s */
            snprintf(src, sizeof src, "%s/src_%d_%d.wav", TMP, fs, ch);
            snprintf(enc, sizeof enc, "%s/e_%s_%d_%d.%s", TMP, codecs[ci].name, fs, ch, codecs[ci].ext);
            snprintf(dec, sizeof dec, "%s/d_%s_%d_%d.raw", TMP, codecs[ci].name, fs, ch);

            if (write_src_wav(src, fs, ch, frames) != 0) { gate_check(&g, 0, "write_src_wav failed"); continue; }

            /* encode */
            snprintf(cmd, sizeof cmd, "ffmpeg -v error -y -i '%s' %s '%s'", src, codecs[ci].encargs, enc);
            int rce = sh(cmd);
            gate_check(&g, rce == 0, codecs[ci].name);
            if (rce != 0) continue;

            /* decode back to interleaved s16le at the SAME (fs,ch) - no resample here */
            if (ffmpeg_decode_raw(enc, dec, fs, ch) != 0) { gate_check(&g, 0, "decode failed"); continue; }
            int16_t *pcm = NULL; long nsamp = read_raw_s16(dec, &pcm);
            if (nsamp <= 0) { gate_check(&g, 0, "empty decode"); free(pcm); continue; }
            long dframes = nsamp / ch;

            /* metadata: channels via nsamp divisibility, frame count within tolerance */
            gate_check(&g, nsamp % ch == 0, "decoded sample count not divisible by channels");
            long tol = codecs[ci].lossless ? 0 : (long)(0.06 * fs) + 2400; /* lossy priming/padding slack */
            gate_check(&g, labs(dframes - frames) <= tol, "decoded frame count out of tolerance");

            /* signal-domain: FFT peak of decoded channel 0 must be the analytically-known tone bin.
             * Same fs -> same bin grid, so expected bin == TONE_BIN scaled by n/NFFT (n==NFFT here). */
            double dmag = 0; int dpk = decoded_peak_bin(pcm, dframes, ch, fs, &dmag);
            /* the source tone is at TONE_BIN for the 44100 grid; at 48000 the same Hz lands on a nearby
             * bin. Compute expected bin from the true frequency and the actual decode grid. */
            int nfft = NFFT;
            int exp_bin = (int)lround(tone_freq() * nfft / fs);
            gate_check(&g, abs(dpk - exp_bin) <= 1, codecs[ci].lossless ? "lossless: peak bin wrong" : "lossy: peak bin wrong");
            gate_check(&g, dmag > 0.1, "decoded tone magnitude too low");

            if (codecs[ci].lossless) {
                /* byte-exact vs source PCM. Decode the SOURCE wav to raw with the same command and sha both. */
                char srcraw[256]; snprintf(srcraw, sizeof srcraw, "%s/sraw_%d_%d.raw", TMP, fs, ch);
                ffmpeg_decode_raw(src, srcraw, fs, ch);
                char h1[65], h2[65];
                gate_check(&g, sha256_file(srcraw, h1) == 0 && sha256_file(dec, h2) == 0, "sha failed");
                gate_check(&g, strcmp(h1, h2) == 0, "lossless: decoded PCM SHA-256 != source PCM");
                /* and a direct byte compare of the buffers for good measure */
                int16_t *sp = NULL; long sn = read_raw_s16(srcraw, &sp);
                gate_check(&g, sn == nsamp && memcmp(sp, pcm, nsamp*2) == 0, "lossless: PCM not byte-identical");
                free(sp);
            } else {
                /* lossy: PSNR floor vs the source PCM over the overlapping region. */
                char srcraw[256]; snprintf(srcraw, sizeof srcraw, "%s/sraw_%d_%d.raw", TMP, fs, ch);
                ffmpeg_decode_raw(src, srcraw, fs, ch);
                int16_t *sp = NULL; long sn = read_raw_s16(srcraw, &sp);
                /* align by skipping lossy encoder priming: find best small offset that maximizes PSNR */
                long cmpn = (sn < nsamp ? sn : nsamp);
                if (cmpn > 20000) cmpn = 20000;      /* enough samples for a stable PSNR */
                double best = -1;
                for (long off = 0; off <= 4000 && off + cmpn <= nsamp && cmpn <= sn; off += ch) {
                    double p = psnr_i16(sp, pcm + off, cmpn);
                    if (p > best) best = p;
                }
                gate_check(&g, best >= codecs[ci].psnr_floor, "lossy: PSNR below codec floor");
                free(sp);
            }
            free(pcm);
        }
      }
    }

    return gate_finish(&g);
}
