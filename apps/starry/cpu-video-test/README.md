# cpu-video-test - industrial-grade video test carpet ("pyte for video")

Deterministic, per-frame + per-codec + A/V-sync assertions for the StarryOS multimedia stack. The
carpet decodes video to raw pixels and audio to raw PCM with the Alpine musl `ffmpeg`/`ffprobe` CLI and
asserts in the **pixel / signal / timing** domains against analytically-known or golden references. It
mirrors the `cpu-audio-test` carpet's structure and gate conventions.

No codec, DSP or video library is linked. `ffmpeg`/`ffprobe` own demux/decode/encode/probe only; every
comparison (raw-frame reader, PSNR, SSIM, radix-2 FFT, SHA-256, PTS/drift math) is self-written in
`programs/carpets/video_common.h` and the cells. No GPU, no display, `-smp 1`.

## Cells (each prints `VIDEO_<CELL> OK <n>` on a clean pass)

1. **video_frames** - frame-exact decode.
   - *Bad Apple binary-frame leg* (references `$ASSET_DIR`, honest-skips if absent): Bad Apple is a
     ~1-bit black/white silhouette animation, so a decoded frame is deterministically comparable
     pixel-exact. For each of the 16 golden frames: assert `sha256(rgb24)` == golden (whole-frame
     byte-exact), assert the `scale=8:8:flags=bicubic,format=gray` 8x8 luma signature == golden
     `luma8x8_hex`, and threshold each pixel to B/W (Rec.601 luma >= 128) and assert the white-pixel
     ratio == the golden ratio within 1e-4.
   - *Synthetic testsrc leg* (always runs, no asset): `smptebars` seven-bar closed-form colors at known
     columns; a C-synthesized rgb24 gradient and checkerboard pushed through `ffv1` (lossless) and
     asserted byte-identical + closed-form linear ramp on decode.

2. **video_codec** - transcode / round-trip matrix, fully synthetic source (smptebars clip). Codec x
   container cartesian `{ffv1, h264, hevc, vp9, mpeg2} x {mkv, mp4, webm, avi, mpg}`: encode -> decode
   first + middle frame back -> **ffv1** byte-exact vs the yuv reference (lossless identity); **lossy**
   PSNR + SSIM floors vs the yuv reference plus a flat-region structure check (a solid bar must stay
   flat). Decoded geometry must equal the source geometry for every container.

3. **video_meta** - CFR timing/geometry. Across several `{size, fps, duration}` points: decoded frame
   count == `round(dur*fps)`, resolution / pixel format (yuv420p) / `r_frame_rate` / sample-aspect-ratio
   exact, PTS strictly monotonic and evenly spaced with `dt == 1/fps` within a tight epsilon.

4. **video_avsync** - **audio track + A/V sync** (both picture and sound, and their time-alignment).
   Deterministic synthetic **synced master**: `testsrc` video (160x120 @ 25fps, 2s -> exactly 50 frames)
   muxed with a `sine` audio (1000 Hz, 44100 Hz, 2s -> exactly 88200 samples) using a **lossless** audio
   codec (flac) so the frame<->sample correspondence is analytically exact.
   - *Audio track*: demux `0:a` -> s16le, sample count == golden 88200, sample_rate/channels exact,
     RMS > 0, FFT peak bin == the analytically-known 1000 Hz tone bin, lossless demux deterministic
     (SHA-equal across two demuxes).
   - *A/V sync*: video frame count == golden; every `PTS_k == k/fps`; video and audio share the same
     container start offset (phase-aligned, not merely near zero); `audio_dur (samples/sr) ==
     video_dur (frames/fps)` within 1 ms (**no drift**); video span == audio span (streams cover the
     same extent - no end-to-end drift).
   - *Transcode preserves sync*: re-mux the synced master to `{h264/flac, hevc/flac, vp9/vorbis,
     ffv1/pcm, h264/aac}` and re-assert both streams decode and stay synced. Lossless audio stays
     sample-exact (1 ms); lossy audio (aac/vorbis) is allowed only priming drift (< 60 ms).

5. **video_realassets** - real transcodes (references `$ASSET_DIR`, honest-skips if absent). For each of
   the four Bad Apple transcodes `{h264, hevc, vp9, ffv1}`: assert `codec_name` / 640x480 / `30/1` fps /
   duration ~5.13s vs golden; first-frame `sha256(rgb24)` == golden; first-frame 8x8 luma signature ==
   golden `luma8x8_hex`; and the frame at `t=2.0` (which diverges across codecs) `sha256(rgb24)` ==
   golden - a per-codec discriminating check.

## Gate (three-gate, matches audio carpet)

`programs/run_all.sh` runs every cell listed in the prebuild-written `expected_cells` manifest and prints
`TEST PASSED` only when `fail==0 && total==EXPECTED==pass` with `EXPECTED>=1`. The synthetic cells
(`video_codec`, `video_meta`, `video_avsync`) always gate on their own generated clips; `video_frames`
always runs its synthetic leg and `video_realassets` honest-skips, so a run with `$ASSET_DIR` absent is
still a full non-vacuous gate, and a run with assets present additionally asserts the real media.

## Assets (`$ASSET_DIR`)

On-target the media rides the per-app `assets` git submodule mounted at `$ASSET_DIR` (default
`/opt/cpu-video-test/assets`). Fetch it before building:

```
git submodule update --init apps/starry/cpu-video-test/assets
git -C apps/starry/cpu-video-test/assets lfs pull --include="video/*,golden/*"
```

`prebuild.sh` inits + LFS-pulls the submodule itself when it is missing, stages `video/badapple_frames`,
`video/badapple_clips` and the golden tsvs, and also accepts a checked-out `render-assets/` tree via
`$VIDEO_ASSET_SRC`. When no assets are found the real-asset legs honest-skip and the synthetic legs still
gate.

## Golden derivation (host ffmpeg 6.1.1)

- rgb24 frame sha: `ffmpeg -i F -f rawvideo -pix_fmt rgb24 | sha256`.
- 8x8 luma signature: `scale=8:8:flags=bicubic,format=gray` (reproduces golden `luma8x8_hex` byte-exact).
- clip first frame: `select=eq(n,0) -vframes 1`; frame at t=2.0: `-ss 2.0 -i F -vframes 1`.
- Bad Apple white-ratio: threshold Rec.601 luma (int weights `(77R+150G+29B)>>8`) at 128.
- avsync golden: analytical - `testsrc` 50 frames @ 25fps + `sine` 88200 samples @ 44100 Hz, tone bin
  `round(1000*8192/44100)=186`, both spanning exactly 2.0 s.

The golden is exactly what host ffmpeg decodes; the carpet re-decodes (StarryOS ffmpeg on-target, the
same host at validation time) and asserts byte-exact (lossless) / PSNR+SSIM (lossy) / PTS-aligned == that
golden.

## Build / run

`prebuild.sh` `apk add`s the `ffmpeg`/`ffprobe` runtime for the target arch via qemu-user and
cross-compiles the five cells with a host musl-cross toolchain (the cells link only libc/libm and shell
out to the on-target ffmpeg/ffprobe CLI, so no target gcc is used - the staging gcc's cc1 cannot exec
under qemu-user). The host cross-compiler is resolved as `<triple>-gcc` on PATH, then
`/opt/<triple>-cross/bin/<triple>-gcc`, then `zig cc -target <triple>`, then `musl-gcc` for a native
x86_64 build.

`build-*.toml` carry `ax-driver/nvme` + `ax-driver/virtio-net`; the riscv64 and loongarch64 targets also
carry `ax-driver/serial` (dynamic platform serial console). `qemu-*.toml` boot single-vCPU with an nvme
rootfs and 3072M for the ffmpeg decode/encode buffers, running `run_all.sh` and gating on `TEST PASSED`.

Run per arch:

```
cargo xtask starry app qemu -t cpu-video-test --arch x86_64
cargo xtask starry app qemu -t cpu-video-test --arch aarch64
cargo xtask starry app qemu -t cpu-video-test --arch riscv64
cargo xtask starry app qemu -t cpu-video-test --arch loongarch64
```

## Non-vacuity (mutation-tested host-side)

- `video_frames`: flipping one golden luma-signature byte (`08`->`09` on frame_00) makes the 8x8-luma
  assertion FAIL (rc=1) - the check is real, not self-comparing.
- `video_avsync`: injecting a deliberate 300 ms audio delay into the synced master makes the sample-count,
  A/V-drift and span-match assertions FAIL loudly across the master and every transcode (rc=1) - the sync
  check genuinely detects desync.
