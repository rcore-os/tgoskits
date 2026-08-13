/* audio_resample - resampling carpet (leg B, resample axis).
 *
 * A pure tone at a fixed physical frequency f is invariant under sample-rate conversion: after
 * resampling 44100<->48000 the tone is still f Hz, so its FFT peak MUST migrate to bin round(f*N/fs')
 * for the new rate fs'. A correct polyphase resampler also must NOT fold energy above the new Nyquist
 * (no aliasing images). We assert both:
 *
 *   - encode a 3000 Hz tone at fs_in, ffmpeg-resample to fs_out, decode, FFT: peak lands on the bin
 *     predicted from the SAME physical frequency at fs_out (both 44100->48000 and 48000->44100).
 *   - downsample a tone that is ABOVE the target Nyquist (e.g. 23000 Hz -> 44100, Nyquist 22050): the
 *     resampler's anti-alias filter must remove it, so the decoded band has no strong tone (no alias
 *     image appears at 44100-23000 = 21100 Hz).
 *   - a below-Nyquist tone survives the same downsample (positive control).
 *   - sample_count scales by fs_out/fs_in within one frame.
 */
#include "audio_common.h"

#define NFFT 8192

static const char *TMP = "/tmp/audioresample";

static int write_tone_wav(const char *path, int fs, double freq, double amp, long frames) {
    long ndata = frames * 2;   /* mono */
    unsigned char hdr[44]; uint32_t chunk = 36 + ndata, byte_rate = fs * 2;
    memcpy(hdr, "RIFF", 4);
    hdr[4]=chunk&0xff;hdr[5]=(chunk>>8)&0xff;hdr[6]=(chunk>>16)&0xff;hdr[7]=(chunk>>24)&0xff;
    memcpy(hdr+8,"WAVEfmt ",8);
    hdr[16]=16;hdr[17]=0;hdr[18]=0;hdr[19]=0; hdr[20]=1;hdr[21]=0; hdr[22]=1;hdr[23]=0;
    hdr[24]=fs&0xff;hdr[25]=(fs>>8)&0xff;hdr[26]=(fs>>16)&0xff;hdr[27]=(fs>>24)&0xff;
    hdr[28]=byte_rate&0xff;hdr[29]=(byte_rate>>8)&0xff;hdr[30]=(byte_rate>>16)&0xff;hdr[31]=(byte_rate>>24)&0xff;
    hdr[32]=2;hdr[33]=0; hdr[34]=16;hdr[35]=0; memcpy(hdr+36,"data",4);
    hdr[40]=ndata&0xff;hdr[41]=(ndata>>8)&0xff;hdr[42]=(ndata>>16)&0xff;hdr[43]=(ndata>>24)&0xff;
    FILE *f = fopen(path, "wb"); if (!f) return -1;
    fwrite(hdr,1,44,f);
    for (long i = 0; i < frames; i++) {
        int s = (int)lround(amp * sin(2.0*M_PI*freq*i/fs) * 32767.0);
        if (s>32767) s=32767;
        if (s<-32768) s=-32768;
        int16_t v=(int16_t)s; fwrite(&v,2,1,f);
    }
    fclose(f); return 0;
}

/* Resample a mono wav to fs_out with ffmpeg (soxr), decode to raw s16le mono, FFT peak + full mag. */
static int resample_and_peak(const char *inwav, int fs_out, double *peakmag, double *bandmax_out,
                             int band_lo, int band_hi, long *nframes) {
    char cmd[1024], out[256], raw[256];
    snprintf(out, sizeof out, "%s/rs.wav", TMP);
    snprintf(raw, sizeof raw, "%s/rs.raw", TMP);
    snprintf(cmd, sizeof cmd, "ffmpeg -v error -y -i '%s' -ar %d -ac 1 -af aresample=resampler=soxr '%s'", inwav, fs_out, out);
    if (sh(cmd) != 0) return -1;
    if (ffmpeg_decode_raw(out, raw, fs_out, 1) != 0) return -2;
    int16_t *pcm = NULL; long n = read_raw_s16(raw, &pcm);
    if (n < NFFT) { free(pcm); return -3; }
    if (nframes) *nframes = n;
    double *x = (double *)malloc(sizeof(double)*NFFT);
    double *mag = (double *)malloc(sizeof(double)*(NFFT/2+1));
    /* window into the steady-state middle to avoid transient/edge effects */
    long start = n/2 - NFFT/2; if (start < 0) start = 0;
    for (int i = 0; i < NFFT; i++) x[i] = pcm[start+i] / 32768.0;
    real_fft_mag(x, NFFT, mag);
    int pk = peak_bin(mag, 1, NFFT/2);
    if (peakmag) peakmag[0] = mag[pk];
    if (bandmax_out) { double bm=0; for (int k=band_lo;k<=band_hi&&k<=NFFT/2;k++) if (mag[k]>bm) bm=mag[k]; *bandmax_out=bm; }
    int result_pk = pk;
    free(x); free(mag); free(pcm);
    return result_pk;
}

int main(void) {
    gate g; gate_init(&g, "AUDIO_RESAMPLE");
    char cmd[512], src[256];
    snprintf(cmd, sizeof cmd, "mkdir -p %s", TMP); sh(cmd);

    /* ---- 1. 3000 Hz tone: 44100 -> 48000, peak migrates to the fs_out bin ---- */
    double f = 3000.0;
    snprintf(src, sizeof src, "%s/t44.wav", TMP);
    write_tone_wav(src, 44100, f, 0.7, 44100);
    double pm; long nf;
    int pk = resample_and_peak(src, 48000, &pm, NULL, 0, 0, &nf);
    int exp48 = (int)lround(f * NFFT / 48000);
    gate_check(&g, pk >= 0, "resample 44->48 failed");
    gate_check(&g, abs(pk - exp48) <= 1, "44->48: peak did not migrate to fs_out bin");
    gate_check(&g, pm > 0.2, "44->48: tone magnitude collapsed");
    long exp_nf_48 = (long)llround(44100.0 * 48000.0 / 44100.0);
    gate_check(&g, labs(nf - exp_nf_48) <= 2, "44->48: sample count did not scale");

    /* ---- 2. 3000 Hz tone: 48000 -> 44100, peak migrates the other way ---- */
    snprintf(src, sizeof src, "%s/t48.wav", TMP);
    write_tone_wav(src, 48000, f, 0.7, 48000);
    int pk2 = resample_and_peak(src, 44100, &pm, NULL, 0, 0, &nf);
    int exp44 = (int)lround(f * NFFT / 44100);
    gate_check(&g, pk2 >= 0, "resample 48->44 failed");
    gate_check(&g, abs(pk2 - exp44) <= 1, "48->44: peak did not migrate to fs_out bin");
    gate_check(&g, pm > 0.2, "48->44: tone magnitude collapsed");
    gate_check(&g, exp44 != exp48, "the two rates should map the same Hz to different bins");
    long exp_nf_44 = (long)llround(48000.0 * 44100.0 / 48000.0);
    gate_check(&g, labs(nf - exp_nf_44) <= 2, "48->44: sample count did not scale");

    /* ---- 3. anti-aliasing: 23000 Hz tone downsampled 48000 -> 44100 (Nyquist 22050) is removed ---- */
    /* the alias image would appear at |44100 - 23000| = 21100 Hz -> assert that band stays quiet */
    double fhi = 23000.0;
    snprintf(src, sizeof src, "%s/thi.wav", TMP);
    write_tone_wav(src, 48000, fhi, 0.7, 48000);
    double bandmax;
    int alias_lo = (int)lround(20000.0 * NFFT / 44100) , alias_hi = (int)lround(22000.0 * NFFT / 44100);
    int pk3 = resample_and_peak(src, 44100, &pm, &bandmax, alias_lo, alias_hi, NULL);
    gate_check(&g, pk3 >= 0, "resample high-tone failed");
    /* the whole decoded spectrum should be near-silent (tone was above target Nyquist, filtered out) */
    gate_check(&g, pm < 0.05, "anti-alias: above-Nyquist tone survived downsample");
    gate_check(&g, bandmax < 0.05, "anti-alias: alias image appeared in 20-22 kHz band");

    /* ---- 4. positive control: a 10000 Hz tone (below target Nyquist) SURVIVES the same downsample ---- */
    double fok = 10000.0;
    snprintf(src, sizeof src, "%s/tok.wav", TMP);
    write_tone_wav(src, 48000, fok, 0.7, 48000);
    int pk4 = resample_and_peak(src, 44100, &pm, NULL, 0, 0, NULL);
    int exp_ok = (int)lround(fok * NFFT / 44100);
    gate_check(&g, pk4 >= 0 && abs(pk4 - exp_ok) <= 1, "below-Nyquist tone lost in downsample");
    gate_check(&g, pm > 0.2, "below-Nyquist tone magnitude collapsed");

    return gate_finish(&g);
}
