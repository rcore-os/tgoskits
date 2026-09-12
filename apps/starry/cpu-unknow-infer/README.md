# cpu-unknow-infer

Pure-CPU LLM inference correctness carpet for StarryOS. It runs `llama.cpp` (the `ggml`
CPU backend, no GPU) doing **greedy, deterministic** decoding of a small LLM and asserts
the generated token IDs match a committed golden reference **token-by-token**. This proves
the inference stack (llama.cpp + ggml CPU kernels) is numerically correct on StarryOS.

This is the C-side (CPU) counterpart of the GPU inference carpets: no OpenCL/Vulkan/CUDA
backend is built or used, only `libggml-cpu`.

## What it tests

The carpet covers **two models** - `qwen3-0.6b.gguf` (Qwen3) and
`deepseek-r1-distill-qwen-1.5b.gguf` (DeepSeek-R1-Distill) - as two model cells:
`infer_llamacpp_qwen3` and `infer_llamacpp_deepseek`. Each cell runs the same
`infer_llamacpp` binary, loading its model with `n_gpu_layers=0` (pure CPU) and, for each
committed prompt, greedy-decoding a fixed number of tokens with a fully deterministic
sampler:

- sampler chain = `greedy` only (pure argmax; equivalent to temperature=0 / top-k=1; no RNG)
- `n_threads = 1`, `n_threads_batch = 1`
- fixed `n_predict`, fixed context size

Because greedy decode is a deterministic function of the model weights and the ggml
kernels, the exact token sequence is reproducible. The carpet compares its generated token
IDs against a golden file per prompt and applies a **three-gate** per prompt:

1. **first-token argmax** - the argmax of the logits over the prompt (`ARGMAX0`) must equal
   the golden value.
2. **count** - the number of generated tokens must equal the golden count (and the
   end-of-generation state must match).
3. **token-by-token** - every generated token ID must equal the golden token ID at the same
   position.

Each cell prints `INFER_LLAMACPP OK <n>` (n = tokens verified for that model) only when
every gate of every prompt passes. `run_all.sh` then applies the launcher three-gate over
cells (`fail==0 && total==EXPECTED==pass`, plus `tokens>0`) and prints `TEST PASSED` only
when **both** model cells pass. A single wrong token ID in either model ->
`INFER_LLAMACPP FAILED` for that cell -> `TEST FAILED`. "Model loaded" alone is **not** a
pass; correctness is decided by exact token-ID equality, not by any load or print side
effect.

DeepSeek-R1-Distill emits `<think>` reasoning tokens (token 151649 `</think>` appears in one
golden); greedy decode is still deterministic, so whatever greedy produces for the fixed
`n_predict` is captured verbatim as the golden and asserted token-by-token like any other.

## Prompts, params and golden

Golden token files live under `programs/carpets/infer_llamacpp/golden/<arch>/` (one set per
arch - see "Golden determinism / reproducibility" below). Each file pins the prompt,
`n_predict`, first-token argmax, and the full generated token-ID sequence. The tables below
list the prompt/`n_predict`/`argmax0` (identical across arches - `argmax0` is the first token,
untied); the full `GEN` sequence is what differs per arch.

**Qwen3 cell** (`infer_llamacpp_qwen3`):

| golden file | prompt | n_predict | argmax0 | tokens |
| --- | --- | --- | --- | --- |
| `qwen3-0.6b.france.tokens` | `The capital of France is` | 24 | 12095 (`Paris`) | 24 |
| `qwen3-0.6b.math.tokens` | `Two plus two equals` | 32 | 1378 | 32 |
| `qwen3-0.6b.list.tokens` | `The first three prime numbers are` | 32 | 220 | 32 |

Qwen3 subtotal: **88** tokens -> `INFER_LLAMACPP OK 88`.

**DeepSeek cell** (`infer_llamacpp_deepseek`):

| golden file | prompt | n_predict | argmax0 | tokens |
| --- | --- | --- | --- | --- |
| `deepseek-r1-distill-qwen-1.5b.france.tokens` | `The capital of France is` | 8 | 12095 (`Paris`) | 8 |
| `deepseek-r1-distill-qwen-1.5b.math.tokens` | `Two plus two equals` | 8 | 3040 | 8 |
| `deepseek-r1-distill-qwen-1.5b.count.tokens` | `Count from one to five:` | 8 | 220 | 8 |

DeepSeek subtotal: **24** tokens -> `INFER_LLAMACPP OK 24`.

DeepSeek uses a shorter `n_predict` (8) than Qwen3 (24/32): the 1.5B model under
qemu-system TCG on riscv64 decodes several minutes per token, so 24 DeepSeek
tokens keeps the whole carpet inside the per-arch qemu timeout while still
asserting token-exact greedy decode. Greedy decode is prefix-stable, so the
8-token golden is exactly the first 8 tokens of a longer decode.

**EXPECTED cells = 2** (`infer_llamacpp_qwen3`, `infer_llamacpp_deepseek`). **Total tokens
verified = 112** (88 + 24). The passing summary on-target is
`2/2 model cells OK ... 112 tokens verified` followed by `TEST PASSED`.

### Golden determinism / reproducibility (per-arch)

The golden is **per-arch** (`golden/<arch>/*.tokens`). Greedy argmax is deterministic for a
fixed build but **not** bit-identical across ISAs: each arch's ggml CPU build rounds fused
multiply-add differently (x86_64 SSE2, aarch64 NEON+FMA, riscv64/loongarch64 scalar), so the
odd near-tied argmax flips and the sequences diverge downstream. A single shared golden would
false-fail on the arches it was not generated on; hence one golden set per arch.

Each arch's golden is generated by **that arch's own** cross-compiled build (the exact
`llama.cpp` commit `c92e806d1c81091c9035edce99c35374da1b465e` + flags the carpet runs
on-target), greedy sampler, `n_gpu_layers=0`, single thread, so it is byte-reproducible by the
same build on-target. All four sets decode coherent answers (e.g. Qwen3 "The capital of France
is" -> "Paris ...", "The first three prime numbers are" -> "2, 3, and 5") - the cross-ISA
differences are argmax tie-breaks, not wrong answers.

To regenerate a golden for an arch, run the carpet binary in emit mode
(`infer_llamacpp -e -m <model> -g <golden>...`): it prints `PROMPT`/`NPREDICT`/`ARGMAX0`/`GEN`
for this build to stdout (and the decoded text to stderr for a coherence check), skipping the
gate. Capture that into `golden/<arch>/`.

## Provenance

- **Model (Qwen3)**: `qwen3-0.6b.gguf`, Qwen3 0.6B Instruct, GGUF V3, Q8_0.
  sha256 `9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031` (~610 MiB).
- **Model (DeepSeek)**: `deepseek-r1-distill-qwen-1.5b.gguf`, DeepSeek-R1-Distill-Qwen-1.5B.
  sha256 `1741e5b2d062b07acf048bf0d2c514dadf2a48f94e2b4aa0cfe069af3838ee2f` (~1.1 GiB).
- **llama.cpp**: pinned commit `c92e806d1c81091c9035edce99c35374da1b465e` (ggml 0.16.0),
  built CPU-only: `GGML_NATIVE=OFF GGML_CPU=ON GGML_BLAS=OFF GGML_OPENMP=OFF`, no GPU backend,
  `BUILD_SHARED_LIBS=ON` (dynamic musl).

## Arch coverage and per-arch provisioning notes

`prebuild.sh` cross-compiles `llama.cpp` CPU-only **from source on the host** for the target
arch, then compiles the carpet binary against it, and stages `libllama` + `libggml*` + the
toolchain `libstdc++`/`libgcc_s` runtime + the carpet + **both models** (~610 MiB Qwen3 +
~1.1 GiB DeepSeek) + goldens into the overlay. The rootfs is grown to 8G to fit both models
plus the libs. The models ride an **nvme** drive (`ax-driver/nvme`, not virtio-blk).

Build model: a host musl-cross toolchain (`${triple}-gcc`/`${triple}-g++` from
`/opt/${triple}-cross`) drives a generated CMake toolchain file
(`CMAKE_SYSTEM_NAME=Linux` + the cross C/C++ compilers). This replaces the earlier approach of
running the target Alpine gcc/cmake under `qemu-user` - that fails because gcc spawns
`cc1`/`cc1plus` via `posix_spawn`, which `qemu-user` cannot exec. Building the identical
CPU-only ggml source natively on the host avoids `qemu-user` entirely and does not depend on
any Alpine `llama.cpp` package (all four arches build the same pinned source). The result is a
dynamic-musl ELF; the base Alpine rootfs supplies `ld-musl` + `libc`, and the C++ runtime is
shipped in `/opt/cpu-unknow-infer/lib` (on the rpath and `LD_LIBRARY_PATH`).

| arch | build/qemu | provisioning |
| --- | --- | --- |
| x86_64 | `-machine q35`, `uefi=false to_bin=false` | Host cross-compile (musl-cross g++), SSE2. Per-arch golden generated by this build; greedy-decode validated. |
| aarch64 | `to_bin=true` | Host cross-compile (musl-cross g++), NEON+FMA. Per-arch golden generated by this build; greedy-decode validated. |
| loongarch64 | `to_bin=true`, dynamic platform | Host cross-compile (`/opt/loongarch64-linux-musl-cross`). ggml forced to base scalar (`GGML_LASX=OFF GGML_LSX=OFF`) - gcc 13.2.0 rejects `-mlasx`/`-mlsx`. Per-arch golden generated by this build. |
| riscv64 | `to_bin=true` | Host cross-compile (`/opt/riscv64-linux-musl-cross`), **dynamic musl** (not static-PIE). ggml forced to base rv64gc (`GGML_RVV=OFF` etc.) - binutils 2.36 predates RVV 1.0. Per-arch golden generated by this build. |

x86_64 boots with `-machine q35` (avoids `Error loading uncompressed kernel without PVH ELF
Note`); aa/rv/loong use `to_bin=true` raw-binary boot.

## Layout

```
cpu-unknow-infer/
  prebuild.sh                              cross-compile llama.cpp CPU-only + carpet, stage both models/goldens
  build-<arch>.toml                        4x: x86_64/aarch64/riscv64/loongarch64, ax-driver/nvme
  qemu-<arch>.toml                         4x: nvme drive, -smp 1, 6144M, per-arch uefi/to_bin
  programs/
    run_all.sh                             per-model three-gate launcher (fail==0 && total==EXPECTED==pass)
    carpets/infer_llamacpp/
      src/infer_llamacpp.cpp               the carpet: greedy decode + token-by-token golden gate (+ -e emit mode)
      golden/<arch>/qwen3-0.6b.*.tokens                    per-arch Qwen3 golden token-ID references (3 prompts)
      golden/<arch>/deepseek-r1-distill-qwen-1.5b.*.tokens per-arch DeepSeek golden token-ID references (3 prompts)
```

The manifest (`expected_cells`, written by prebuild) has one `cell|model|golden-glob` line per
model cell; `run_all.sh` runs the carpet once per model and gates on all cells passing.

## Running

The app runner invokes `prebuild.sh` (host, per-arch) then boots StarryOS with the matching
`qemu-<arch>.toml`, which runs `sh /usr/bin/run_all.sh`. Success is the regex `^TEST PASSED$`.

```
cargo xtask starry app qemu -t cpu-unknow-infer --arch x86_64
cargo xtask starry app qemu -t cpu-unknow-infer --arch aarch64
cargo xtask starry app qemu -t cpu-unknow-infer --arch riscv64
cargo xtask starry app qemu -t cpu-unknow-infer --arch loongarch64
```

The prebuild builds `llama.cpp` from source with a host musl-cross toolchain, so
`/opt/${triple}-cross` (x86_64/aarch64/riscv64/loongarch64), `cmake`, and `curl` must be
available on the host. The two GGUF models are reused from `models/` next to the app or from
`../../../gpu-infer/models/` if already downloaded, else fetched (sha256-pinned).
