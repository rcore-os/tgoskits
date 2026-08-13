# cpu-audio-test - the "pyte for audio"

An industrial-grade audio test carpet for StarryOS. Where `pyte` gives a headless terminal you can assert
against cell-by-cell, this gives a headless audio pipeline you can assert against **in the signal domain**:
every cell decodes audio to in-memory PCM and checks the result against an analytically-known or golden
reference - FFT bins, magnitudes, SNR, THD+N, PSNR, byte-exact SHA-256 - never a smoke test.

Runtime dependency is only the Alpine musl `ffmpeg` CLI (libavcodec/libavformat/libswresample with
flac/opus/aac/mp3 + the soxr resampler). No MP3/AAC decoder is reinvented; the FFT, the RIFF/WAVE parser,
the SHA-256 and all the reference math (expected bins, RMS, SNR, THD+N, PSNR) are self-written in the cells
so each assertion is an independent closed-form check, not a self-comparison.

## Cells

Each cell prints `AUDIO_<CELL> OK <n>` only when `fail==0 && total==pass==<n>` (three-gate). `run_all.sh`
gates on the capability manifest: `fail==0 && total==EXPECTED==pass`, EXPECTED>=1 floor.

### `audio_fft` - synthetic known-signal spectral leg (self-contained, no ffmpeg) - 23 assertions
Signals whose spectrum is analytically known are generated in-code, FFT'd with the in-tree radix-2
Cooley-Tukey FFT, and checked against the closed form. Tones are placed at **bin-exact** frequencies
`f = k*fs/N` so a rectangular-window DFT has zero leakage and the peak magnitude is exactly `A/2`.

- pure sine (bin 41 ~441 Hz, bin 93 ~1001 Hz): peak bin `== round(f*N/fs)`, magnitude `== A/2`, SNR > 120 dB.
- dual-tone (DTMF, bins 65 + 112): both tones at exactly `0.4/2`, nothing else above 1e-6.
- linear chirp 500->5000 Hz: energy spread over >20 bins, contained inside the swept band.
- silence: every bin < 1e-12. Impulse: flat spectrum, every bin `== 1/N`.
- THD+N: pure tone < -120 dB; adding 2nd+3rd harmonics raises it by > 40 dB and the harmonic bins carry energy.
- channel separation: hard-pan a tone LEFT -> L has the tone, R `== 0`; then the mirror (pan RIGHT).
- DC offset: `mag[0] == offset`, tone bin unshifted. Clipping: overdriven tone hits full-scale int16, clean -6 dBFS tone does not (negative control).

### `audio_codec` - PCM + codec cartesian - 136 assertions
`{wav, flac, opus, aac, mp3} x {mono, stereo} x {44100, 48000}` - 20 combinations. A synthetic bin-exact
tone is written as a source WAV, encoded with ffmpeg, decoded back to interleaved s16le, then:

- **lossless (wav, flac)**: decoded PCM is byte-exact vs the source PCM - SHA-256 equal **and** `memcmp` identical.
- **lossy (opus, aac, mp3)**: FFT peak still lands on the analytically-known bin (+/-1), magnitude bounded,
  and PSNR (best small-offset alignment against the source, to absorb encoder priming) above a 30 dB floor.
- metadata: sample count divisible by channels; decoded frame count exact (lossless) or within the codec's
  priming/padding slack (lossy).

### `audio_resample` - resample axis - 14 assertions
A tone at a fixed physical frequency is invariant under sample-rate conversion, so its FFT peak must migrate
to `round(f*N/fs')`:

- 3000 Hz tone 44100 -> 48000 and 48000 -> 44100: peak migrates to the correct new bin, magnitude survives,
  the two rates map the same Hz to different bins, sample count scales by `fs_out/fs_in`.
- anti-aliasing: a 23000 Hz tone downsampled 48000 -> 44100 (Nyquist 22050) is filtered out - no residual
  tone, no alias image in the 20-22 kHz band.
- positive control: a 10000 Hz tone (below the target Nyquist) **survives** the same downsample.

### `audio_realassets` - real-media leg (optional) - 59 assertions with assets present
Reads `$ASSET_DIR/golden/audio/audio_golden.tsv` and, per row, decodes `audio/<slug>.m4a` to interleaved
s16le at the native rate + native channel count (the exact pipeline that produced the golden) and asserts:
sample_rate, channels, per-channel sample_count, duration (`frames/rate`), RMS (`/32768`), and the decoded
PCM **SHA-256 == golden pcm_sha256** (byte-exact against the committed AAC stream - the media submodule
tracks `.m4a`, not `.wav`, and the golden was generated from that stream). Cross-format siblings
(`<slug>.flac/.opus`, where committed) are decoded and their RMS + dominant peak band checked against the
golden (flac tight, opus within lossy tolerance).

On-target the assets ride a git submodule; `ASSET_DIR` defaults to `/opt/cpu-audio-test/assets`. If the
golden tsv is absent the cell **honest-skips** (prints `AUDIO_REALASSETS SKIP ... OK 1`) so a missing
submodule never fails the gate - the synthetic legs always run and gate.

## Build / run

`prebuild.sh` extracts the base Alpine rootfs, `apk add`s the `ffmpeg` runtime for the target arch via
qemu-user, cross-compiles the four cells with a host musl-cross toolchain (the cells link only libc/libm
and shell out to the on-target ffmpeg CLI, so no target gcc is used - the staging gcc's cc1 cannot exec
under qemu-user), stages the real-media assets, and writes the `expected_cells` manifest.
`programs/run_all.sh` is the on-target three-gate runner. Four `build-*.toml` + `qemu-*.toml` cover
x86_64 / aarch64 / riscv64 / loongarch64 (nvme rootfs + virtio-net; loong/riscv carry `ax-driver/serial`;
loong uses the dynamic platform raw-binary boot path).

Run per arch:

```
cargo xtask starry app qemu -t cpu-audio-test --arch x86_64
cargo xtask starry app qemu -t cpu-audio-test --arch aarch64
cargo xtask starry app qemu -t cpu-audio-test --arch riscv64
cargo xtask starry app qemu -t cpu-audio-test --arch loongarch64
```

The host cross-compiler is resolved as `<triple>-gcc` on PATH, then `/opt/<triple>-cross/bin/<triple>-gcc`,
then `zig cc -target <triple>`, then `musl-gcc` for a native x86_64 build.

### Media assets

The real-media assets (`golden/audio/audio_golden.tsv` + the `.m4a`/`.flac`/`.opus` clips) ride the per-app
`assets` git submodule. Fetch them before building:

```
git submodule update --init apps/starry/cpu-audio-test/assets
git -C apps/starry/cpu-audio-test/assets lfs pull --include="audio/*,golden/*"
```

`prebuild.sh` inits + LFS-pulls the submodule itself when it is missing, or accepts a checked-out
`render-assets` tree via `$AUDIO_ASSET_SRC`. If no assets are found `audio_realassets` honest-skips and the
synthetic legs still gate.

## Host validation

Built + run on the host (ffmpeg 6.1.1, gcc 13): all four cells green, `TEST PASSED`.

```
AUDIO_FFT OK 23
AUDIO_CODEC OK 136
AUDIO_RESAMPLE OK 14
AUDIO_REALASSETS OK 59
cpu-audio-test: 4/4 audio carpets OK on x86_64 (expected 4: audio_fft audio_codec audio_resample audio_realassets )
TEST PASSED
```

Non-vacuity: mutating the expected FFT peak bin (`+1` in `audio_fft`, `+5` in `audio_codec` /
`audio_resample`) turns each cell into a real FAIL (exit 1, `fail>0`), proving the peak assertions are
load-bearing. Codecs/formats exercised on the host: pcm_s16le (wav), flac, libopus, aac, libmp3lame, plus
soxr resampling - all present in the host ffmpeg build; none were unavailable.
