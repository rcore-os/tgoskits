/* audio_fft - synthetic known-signal spectral carpet (leg A, self-contained, no ffmpeg needed).
 *
 * Generate signals whose spectrum is analytically known, FFT them with the in-tree radix-2 FFT, and
 * assert the spectrum matches the closed form:
 *   - pure sine at a bin-exact f -> single peak at bin round(f*N/fs), magnitude == amplitude/2, SNR high.
 *   - dual-tone (DTMF) -> two peaks at the two known bins, both dominant, nothing else.
 *   - linear chirp -> energy spread across the swept band, no single dominant bin.
 *   - silence -> all bins ~0.
 *   - impulse -> flat magnitude spectrum (every bin == 1/N).
 *   - THD+N -> a pure tone's harmonic+noise energy is far below the fundamental; adding harmonics raises it.
 *   - channel separation -> tone hard-panned LEFT: L has the tone, R == 0 (and the mirror case).
 *   - DC offset -> bin-0 energy == the offset; clipping -> full-scale sample count in the int16 domain.
 *
 * Tones are placed at bin-exact frequencies f = k*fs/N so a rectangular-window DFT has zero leakage and
 * the peak magnitude is exactly A/2 - every expected bin/magnitude is derived from f, fs, N in-code, so
 * each assertion is a closed-form check. Deterministic: pure double math, no RNG.
 */
#include "audio_common.h"

#define FS 44100
#define N  4096   /* FFT size; power of two */

/* bin-exact frequency for FFT bin k */
static double freq_of_bin(int k) { return (double)k * FS / (double)N; }

int main(void) {
    gate g; gate_init(&g, "AUDIO_FFT");
    double *x = (double *)malloc(sizeof(double) * N);
    double *mag = (double *)malloc(sizeof(double) * (N/2 + 1));

    /* ---- 1. pure sine near 440 Hz (bin 41 == 441.43 Hz): peak bin, magnitude == A/2, SNR ---- */
    int k1 = 41; double f1 = freq_of_bin(k1), A = 0.8;
    for (int i = 0; i < N; i++) x[i] = A * sin(2.0 * M_PI * f1 * i / FS);
    real_fft_mag(x, N, mag);
    int pk = peak_bin(mag, 1, N/2);
    gate_check(&g, pk == k1, "sine441 peak bin != round(f*N/fs)");
    gate_check(&g, fabs(mag[pk] - A/2.0) < 1e-4, "sine441 peak magnitude != A/2");
    gate_check(&g, snr_db(mag, N, pk) > 120.0, "sine441 SNR too low (leakage?)");

    /* ---- 2. pure sine near 1000 Hz (bin 93): peak migrates to the new closed-form bin ---- */
    int k2 = 93; double f2 = freq_of_bin(k2);
    for (int i = 0; i < N; i++) x[i] = 0.5 * sin(2.0 * M_PI * f2 * i / FS);
    real_fft_mag(x, N, mag);
    int pk2 = peak_bin(mag, 1, N/2);
    gate_check(&g, pk2 == k2, "sine1001 peak bin != round(f*N/fs)");
    gate_check(&g, fabs(mag[pk2] - 0.25) < 1e-4, "sine1001 magnitude != A/2");
    gate_check(&g, pk2 != k1, "1001Hz peak collided with 441Hz bin (should differ)");

    /* ---- 3. DTMF dual-tone (bins 65 + 112, ~700 + ~1206 Hz): two dominant peaks, nothing else ---- */
    int kl = 65, kh = 112; double dl = freq_of_bin(kl), dh = freq_of_bin(kh);
    for (int i = 0; i < N; i++)
        x[i] = 0.4 * sin(2.0 * M_PI * dl * i / FS) + 0.4 * sin(2.0 * M_PI * dh * i / FS);
    real_fft_mag(x, N, mag);
    gate_check(&g, fabs(mag[kl] - 0.2) < 1e-4 && fabs(mag[kh] - 0.2) < 1e-4, "DTMF: tones not at expected magnitude");
    double othermax = 0.0;
    for (int k = 1; k <= N/2; k++) { if (k == kl || k == kh) continue; if (mag[k] > othermax) othermax = mag[k]; }
    gate_check(&g, othermax < 1e-6, "DTMF: spurious energy outside the two tones");
    gate_check(&g, kl != kh, "DTMF bins collapsed");

    /* ---- 4. linear chirp 500->5000 Hz: energy spread, contained inside the swept band ---- */
    double f0 = 500.0, f1c = 5000.0, T = (double)N / FS;
    for (int i = 0; i < N; i++) {
        double t = i / (double)FS;
        double inst = 2.0 * M_PI * (f0 * t + (f1c - f0) / (2.0 * T) * t * t);
        x[i] = 0.6 * sin(inst);
    }
    real_fft_mag(x, N, mag);
    int cpk = peak_bin(mag, 1, N/2);
    int spread = 0; for (int k = 1; k <= N/2; k++) if (mag[k] > 0.1 * mag[cpk]) spread++;
    gate_check(&g, spread > 20, "chirp energy not spread across band");
    int klo = (int)lround(f0 * N / FS) - 3, khi = (int)lround(f1c * N / FS) + 3;
    double inband = 0, outband = 0;
    for (int k = 1; k <= N/2; k++) { if (k >= klo && k <= khi) inband += mag[k]*mag[k]; else outband += mag[k]*mag[k]; }
    gate_check(&g, inband > 50.0 * outband, "chirp energy leaked outside swept band");

    /* ---- 5. silence: all bins ~0 ---- */
    for (int i = 0; i < N; i++) x[i] = 0.0;
    real_fft_mag(x, N, mag);
    double smax = 0; for (int k = 0; k <= N/2; k++) if (mag[k] > smax) smax = mag[k];
    gate_check(&g, smax < 1e-12, "silence has nonzero spectrum");

    /* ---- 6. impulse: flat magnitude spectrum, every bin == 1/N ---- */
    for (int i = 0; i < N; i++) x[i] = 0.0;
    x[0] = 1.0;
    real_fft_mag(x, N, mag);
    double fmin = 1e9, fmax = 0;
    for (int k = 0; k <= N/2; k++) { if (mag[k] < fmin) fmin = mag[k]; if (mag[k] > fmax) fmax = mag[k]; }
    gate_check(&g, fabs(fmax - 1.0/N) < 1e-12 && fabs(fmin - 1.0/N) < 1e-12, "impulse spectrum not flat at 1/N");

    /* ---- 7. THD+N: pure tone -> distortion+noise far below fundamental; add harmonics -> it rises ---- */
    for (int i = 0; i < N; i++) x[i] = 0.7 * sin(2.0 * M_PI * f1 * i / FS);
    real_fft_mag(x, N, mag);
    int tpk = peak_bin(mag, 1, N/2);
    double thdn = thdn_db(mag, N, tpk);
    gate_check(&g, thdn < -120.0, "pure sine THD+N above -120 dB");
    for (int i = 0; i < N; i++)   /* add 2nd + 3rd harmonic at -20 dB (also bin-exact: 2*k1, 3*k1) */
        x[i] = 0.7 * sin(2.0*M_PI*f1*i/FS) + 0.07*sin(2.0*M_PI*freq_of_bin(2*k1)*i/FS)
                                           + 0.07*sin(2.0*M_PI*freq_of_bin(3*k1)*i/FS);
    real_fft_mag(x, N, mag);
    int tpk2 = peak_bin(mag, 1, N/2);
    double thdn2 = thdn_db(mag, N, tpk2);
    gate_check(&g, thdn2 > thdn + 40.0, "harmonic distortion not reflected in THD+N");
    gate_check(&g, mag[2*k1] > 0.01 && mag[3*k1] > 0.01, "harmonic bins missing");

    /* ---- 8. channel separation: hard-pan a tone to LEFT -> L has tone, R == 0; then mirror ---- */
    double *L = (double *)malloc(sizeof(double)*N), *R = (double *)malloc(sizeof(double)*N);
    double *ml = (double *)malloc(sizeof(double)*(N/2+1)), *mr = (double *)malloc(sizeof(double)*(N/2+1));
    for (int i = 0; i < N; i++) { L[i] = 0.9 * sin(2.0*M_PI*f1*i/FS); R[i] = 0.0; }
    real_fft_mag(L, N, ml); real_fft_mag(R, N, mr);
    int lpk = peak_bin(ml, 1, N/2);
    gate_check(&g, lpk == k1 && ml[lpk] > 0.4, "L-panned: tone absent in L");
    double rmax = 0; for (int k = 0; k <= N/2; k++) if (mr[k] > rmax) rmax = mr[k];
    gate_check(&g, rmax < 1e-12, "L-panned: leakage into R");
    for (int i = 0; i < N; i++) { R[i] = 0.9 * sin(2.0*M_PI*f1*i/FS); L[i] = 0.0; }
    real_fft_mag(L, N, ml); real_fft_mag(R, N, mr);
    int rpk = peak_bin(mr, 1, N/2);
    double lmax = 0; for (int k = 0; k <= N/2; k++) if (ml[k] > lmax) lmax = ml[k];
    gate_check(&g, rpk == k1 && mr[rpk] > 0.4 && lmax < 1e-12, "R-panned: separation broken");

    /* ---- 9. DC offset: constant + tone -> bin 0 == DC magnitude, tone still at its bin ---- */
    double dc = 0.3;
    for (int i = 0; i < N; i++) x[i] = dc + 0.5 * sin(2.0*M_PI*f1*i/FS);
    real_fft_mag(x, N, mag);
    gate_check(&g, fabs(mag[0] - dc) < 1e-6, "DC offset not at bin 0 with expected magnitude");
    gate_check(&g, peak_bin(mag, 1, N/2) == k1, "tone bin shifted by DC offset");

    /* ---- 10. clipping detection in the int16 domain: overdrive clips, clean tone does not ---- */
    int clipped = 0;
    for (int i = 0; i < N; i++) {
        double v = 1.6 * sin(2.0*M_PI*f1*i/FS);
        if (v > 1.0) v = 1.0; else if (v < -1.0) v = -1.0;
        int s = (int)lround(v * 32767.0);
        if (s >= 32767 || s <= -32767) clipped++;
    }
    gate_check(&g, clipped > 0, "clipping: no full-scale samples on overdriven tone");
    int clean_clip = 0;
    for (int i = 0; i < N; i++) {
        int s = (int)lround(0.5 * sin(2.0*M_PI*f1*i/FS) * 32767.0);
        if (s >= 32767 || s <= -32767) clean_clip++;
    }
    gate_check(&g, clean_clip == 0, "clipping: clean tone false-flagged");

    free(x); free(mag); free(L); free(R); free(ml); free(mr);
    return gate_finish(&g);
}
