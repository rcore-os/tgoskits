# cpu-subtitle-test - the "pyte for subtitles"

An industrial-grade subtitle test carpet for StarryOS covering **SubRip (.srt) + WebVTT (.vtt) + Advanced
SubStation Alpha (.ass) parsing + cross-format timing conversion**. Where `pyte` gives a headless terminal
you can assert against cell-by-cell, this gives a headless subtitle pipeline you can assert against **with
fully deterministic or structural goldens**: every cell drives a real parser and checks the output against
a value that is either analytically known (synthetic cues authored in-code, so `00:00:01,000` -> `1000 ms`
and `01:02:03,456` -> `3723456 ms` are exact) or a structural property computed host-side from the real
bilibili-sourced files (cue/dialogue count, first/last timestamp, monotonic ordering, valid UTF-8).
"Subtitle parsed" alone is not a test here.

## Copyright note

The real `.srt`/`.ass` files under `render-assets/subtitles` are bilibili-sourced and copyrighted. This
carpet asserts **STRUCTURE** (cue count, timestamps, index order, encoding, style/layer uniformity), **never
the literal dialogue text**. No cell stores, echoes or asserts the dialogue content - the structural goldens
are counts + timing bounds computed from the files, and `tools/gen_goldens.py` recomputes them printing
structure only.

## What is self-written vs reused

The SubRip / WebVTT / ASS parsers, the millisecond timestamp arithmetic (SRT/VTT `HH:MM:SS.mmm` with comma
vs dot separator, ASS `H:MM:SS.cc` centiseconds), the cross-format converter, the UTF-8 validator, the ASS
override-tag stripper and all the comparison logic are **self-written** under `programs/carpets/`. These are
small, well-specified text formats, so a clean parser is not "reinventing a heavy lib" - and writing them
ourselves is exactly what lets a synthetic cue set round-trip SRT<->VTT millisecond-exact and lets three
independent readers converge on the same timing model. No heavy subtitle library (libass) is pulled; the
cells link statically, so on target there is **no runtime subtitle-library dependency** - only libc.

## Assets

Staged into the image under `/opt/cpu-subtitle-test/assets`:

- **Real subtitles** (from `render-assets/subtitles`): `tashouheng.srt` (SubRip) and `badapple.ass` (ASS).
  Structural goldens only - see below.
- The synthetic cues for cells 1-4 are authored **in-code** (no asset), so those legs are fully
  deterministic and always run.

The prebuild always stages both real files from the media submodule (they are plain git blobs, not LFS), so
the real-asset leg (`subtitle_realassets`) has its assets present on target; a missing asset is a hard FAIL,
not a skip. The synthetic legs always run, so every cell always has assertions.

Provision the real assets before building (they live in the `assets` git submodule):

```
git submodule update --init apps/starry/cpu-subtitle-test/assets
```

`tashouheng.srt` and `badapple.ass` are plain-blob (not LFS), so a plain checkout of the submodule
materializes them with no `git lfs pull`. The prebuild also runs this init itself if the submodule is empty.

## Cells

Each cell prints `SUBTITLE_<CELL> OK <n>` only when `fail==0 && total==pass==<n>` (three-gate). `run_all.sh`
gates on the capability manifest: `fail==0 && total==EXPECTED==pass`, EXPECTED>=1 floor. Assertion counts
below are the real host green run.

### `subtitle_srt` - SubRip parser - 32 assertions

Closed-form on **synthetic in-code** SRT strings (timestamps known analytically):

- **Canonical 3-cue SRT**: cue count; exact-ms parse (`00:00:01,000` -> 1000, `00:00:02,500` -> 2500,
  `01:02:03,456` -> 3723456, `01:02:04,000` -> 3724000); index sequence 1..3; monotonic non-overlapping
  ordering (each start >= previous end); duration = end-start (1500/1500/544 ms); multi-line body joined
  with `\n`.
- **CRLF variant** of the same content parses identically (CR stripped, no stray `\r` in text).
- **BOM prefix** tolerated (index parsed past the UTF-8 BOM).
- **Edge cases**: empty-text cue (zero-length body, correct duration), trailing blank lines.
- **10-cue monotonic sequence** (index 1..10, 500 ms each): index sequence, exact timing, duration,
  monotonic + end>=start.

### `subtitle_ass` - Advanced SubStation Alpha parser - 27 assertions

Closed-form on **synthetic in-code** ASS document parsing `[Script Info]` / `[V4+ Styles]` / `[Events]`:

- **Events Format ordering honored**: columns mapped by the declared `Format:` line, not by position;
  a second synthetic doc **reorders** the Format to `Start,End,Layer,Style,Text` and the parser reads
  Layer/Style/timing from the reordered columns correctly.
- **Dialogue count** + centisecond->ms Start/End (`0:00:01.50` -> 1500, `1:02:03.99` -> 3723990), per-line
  Layer (0,2,0) and Style (Default/Title), monotonic + end>=start.
- **Style table**: Name/Fontname/Fontsize and **PrimaryColour `&HAABBGGRR`** decomposed
  (`&H00AABBCC` -> AA=00 BB=AA GG=BB RR=CC; `&HFF112233` -> AA=FF BB=11 GG=22 RR=33).
- **Override-tag stripping**: `{\pos(..)}Styled` -> plain-text length 6; `Line{\i1}A{\i0}B` -> length 6
  (LineAB) - measures length, never content.

### `subtitle_vtt` - WebVTT parser - 16 assertions

Closed-form on **synthetic in-code** WebVTT:

- **WEBVTT header** required (a non-WEBVTT buffer is rejected); **NOTE** comment blocks skipped; cue
  settings (`position:`/`align:`) after `-->` tolerated and not folded into the body; optional cue-identifier
  line tolerated.
- Exact-ms parse (`00:00:01.000` -> 1000, `00:01:00.250` -> 60250) with the dot separator; multi-line join;
  monotonic + end>=start; the short `MM:SS.mmm` form (`05:30.500` -> 330500).

### `subtitle_convert` - cross-format timing round-trip - 9 assertions

- A synthetic cue set is serialized to SRT and to VTT, re-parsed, and **every cue's start/end is preserved
  exactly** across both; SRT uses `,` at the millisecond boundary, VTT uses `.` and the WEBVTT header.
- Full round-trip SRT -> parse -> VTT-string -> parse preserves all timestamps.
- **Optional ffmpeg cross-check** (host only): ffmpeg converts the SRT to VTT and the carpet asserts the
  cue count + first/last timestamp match its own parser (structure only). Honest-skips if ffmpeg is absent.

### `subtitle_realassets` - real .srt/.ass, STRUCTURE only - 24 assertions

Parses the real files and asserts **structural** properties against host-computed goldens - never text:

- `tashouheng.srt`: **48 cues**, index sequence contiguous **0..47**, first start **27400 ms**, last end
  **207080 ms**, starts monotonic, every end>=start, all timestamps within `[0, media]`, valid UTF-8.
- `badapple.ass`: **54 Dialogue lines**, first start **0 ms**, last end **210170 ms**, monotonic, end>=start,
  within `[0, media]`, all **Layer 0** / **Style Default**, style table **Default / Arial / 20 /
  &H00FFFFFF**, valid UTF-8.
- Hard-FAILs the whole cell if `SUBTITLE_DIR` / `ASSET_DIR` is absent (the prebuild always stages the assets,
  so their absence is a provisioning breakage, not a legitimate skip).

## Determinism of the goldens

Cells 1-4 are fully deterministic: the synthetic cues are authored in the C source, so every timestamp is a
compile-time constant and every assertion is closed form (no calibration, no asset). All timestamp parsing
is integer arithmetic on decimal fields, so the ms values are exact across arches. The real-asset goldens in
cell 5 are integer counts and integer-ms bounds computed from the shipped files; `tools/gen_goldens.py`
recomputes them independently (printing structure only) as a cross-check. A parse divergence, an off-by-one
in timestamp arithmetic, a separator mistake or a dropped cue flips a closed-form check or a golden and the
cell FAILs loudly.

**Mutation-tested**: changing the `subtitle_srt` expected timestamp (1000 -> 1001) fails `subtitle_srt`;
changing the expected `subtitle_realassets` SRT cue count (48 -> 49) fails `subtitle_realassets`; changing
the expected ASS last-end (210170 -> 210171) fails `subtitle_realassets`.

## Coverage

- **Formats**: SubRip (.srt), WebVTT (.vtt), Advanced SubStation Alpha (.ass) - 3 mainstream subtitle
  formats, all self-parsed.
- **Timing**: SRT/VTT `HH:MM:SS.mmm` (comma vs dot), ASS `H:MM:SS.cc` centiseconds, short `MM:SS.mmm`,
  millisecond-exact; duration, monotonic non-overlap, within-media bounds.
- **Structure**: cue/dialogue count, index sequence, style table (Name/Fontname/Fontsize/PrimaryColour
  &HAABBGGRR), Layer, Format-column ordering, override-tag stripping (length), cue settings, NOTE skipping.
- **Encoding**: UTF-8 well-formedness (self-written RFC-3629 validator), BOM, CRLF vs LF, trailing blanks,
  empty text.
- **Conversion**: SRT<->VTT ms-exact round-trip + optional ffmpeg structural cross-check.

## Running

Provision the real assets once, then run per arch (the assets init is idempotent - the prebuild also does
it if the submodule is empty):

```
git submodule update --init apps/starry/cpu-subtitle-test/assets

cargo xtask starry app qemu -t cpu-subtitle-test --arch x86_64
cargo xtask starry app qemu -t cpu-subtitle-test --arch aarch64
cargo xtask starry app qemu -t cpu-subtitle-test --arch riscv64
cargo xtask starry app qemu -t cpu-subtitle-test --arch loongarch64
```

The prebuild cross-compiles the cells on the host with the musl-cross toolchain for the target arch (no
qemu-user), stages `tashouheng.srt` / `badapple.ass`, and writes the capability manifest; each run boots
QEMU and prints `TEST PASSED` on the three-gate.

## Layout

```
cpu-subtitle-test/
  prebuild.sh                       # host cross-compile cells + stage assets into the per-arch overlay
  build-<arch>.toml x4              # ArceOS build features per arch
  qemu-<arch>.toml x4               # QEMU boot + run_all.sh + success/fail regex per arch
  tools/
    gen_goldens.py                  # host-side: recompute the real-file structural goldens (structure only)
  programs/
    run_all.sh                      # on-target three-gate runner
    carpets/
      subtitle_common.h             # gate + cue/track types + ms timestamp arithmetic + UTF-8 validator (self-written)
      subtitle_parse.h              # SRT / VTT / ASS parsers + style table + override-tag stripper (self-written)
      subtitle_srt.c                # cell 1
      subtitle_ass.c                # cell 2
      subtitle_vtt.c                # cell 3
      subtitle_convert.c            # cell 4
      subtitle_realassets.c         # cell 5
      third_party/                  # (empty - no vendored dep; all formats self-parsed)
```
