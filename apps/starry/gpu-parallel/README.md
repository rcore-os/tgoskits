# gpu-parallel

Parallel/concurrent compute correctness carpet on StarryOS, across the two software CPU compute
backends already proven on StarryOS: Mesa lavapipe (a real Vulkan compute queue over llvmpipe's LLVM
CPU JIT) and a CPU OpenCL runtime (rusticl over llvmpipe, or pocl when folded in). No host GPU is
required. Each carpet drives the parallel-compute axis - concurrent dispatch, multiple command
queues, asynchronous multi-submit, and multi-workgroup grids with shared-memory reduction and
cross-workgroup atomic counters - and asserts every result element against a closed-form reference
plus a race/ordering check. A carpet prints `<name> OK <n>` only when its failure count is zero and
the assertion total equals a pinned `EXPECTED` constant.

## Cells and assertions

| Cell | Backend | Assertions | Runs |
|:--|:--|--:|:--|
| `vk_parallel` | Vulkan C (`vulkan/vulkan.h`), Mesa lavapipe | 93 | on-target (all arches) + host |
| `cl_parallel` | OpenCL C (`CL/cl.h`), rusticl / pocl | 77 | on-target where a CPU OpenCL runtime is packaged (x86_64 / aarch64) + host |

Total: 170 assertions.

## Cases covered

Each carpet covers the four parallel-compute cases end to end:

- **(a) concurrent dispatch** - multiple compute dispatches enqueued in flight without waiting
  between them (Vulkan: back-to-back `vkQueueSubmit` with per-submission fences, then one
  `vkWaitForFences` over all; OpenCL: one NDRange per queue, flushed, then `clWaitForEvents` over all
  producer events). Every one of the independent results is verified element-wise, and a negative
  control proves the element-wise checker rejects a single corrupted element.
- **(b) multi-queue** - the Vulkan compute family's `queueCount` is queried; when it exposes two or
  more queues a distinct second `VkQueue` is fetched and fed real work in the async multi-submit,
  otherwise the count is asserted honestly as 1 and concurrency is driven through multiple independent
  command buffers on the single queue family. OpenCL creates four in-order command queues on one
  context plus an out-of-order queue where the device advertises it (asserted honestly as unsupported
  otherwise), distributes the work across them, and verifies every result.
- **(c) async multi-submit** - many submissions enqueued without blocking, then a single wait, with
  ordering and completion verified: Vulkan checks every fence reports `VK_SUCCESS` and a single batch
  `vkQueueSubmit` of all command buffers is correct; OpenCL gates a dependent kernel on an event
  wait-list, gates a compute on two non-blocking `clEnqueueWriteBuffer` events, and gates a kernel on
  a user event released by `clSetUserEventStatus` with a completion callback, asserting each produced
  result and the `CL_COMPLETE` event status.
- **(d) multi-workgroup** - a 1,048,576-element grid split across 4,096 workgroups is verified
  element-wise; a workgroup-shared-memory reduction (GLSL `shared` / OpenCL `__local`) is checked so
  every per-workgroup partial equals its CPU workgroup sum and the combined total equals the exact CPU
  total; and a global atomic counter written by every work-item across every workgroup
  (`atomicAdd` / `atomic_add`) must reach exactly N and the closed-form integer sum, with a second
  accumulating dispatch reaching exactly 2N - the "atomic sum == N, no lost updates" cross-workgroup
  race check.

Ordering is additionally exercised with pipeline barriers (`SHADER_WRITE -> SHADER_READ` producer to
consumer in one command buffer), cross-submit binary semaphores, and device events
(`vkCmdSetEvent` / `vkCmdWaitEvents`), each asserting the consumer read the producer's output and not
a stale value. Boundary dispatches (zero workgroups leave the output untouched; a non-power-of-two
grid exercises the `i < n` tail guard) and the documented validation-error enums
(`CL_INVALID_BUFFER_SIZE`, `CL_INVALID_WORK_GROUP_SIZE`, `CL_INVALID_ARG_SIZE`,
`CL_BUILD_PROGRAM_FAILURE`, `VK_ERROR_*`, `VK_TIMEOUT`, `VK_NOT_READY`) are asserted directly.

## Backend and runtime

Provisioned from Alpine edge (main + community) as musl packages: `mesa-vulkan-swrast` (lavapipe),
`vulkan-loader`, `vulkan-headers`, `opencl-headers`, `glslang` / `shaderc` (GLSL to SPIR-V), and
best-effort `mesa-rusticl` + `opencl-icd-loader` (OpenCL over llvmpipe), plus the `llvm-libs` closure.
`prebuild.sh` cross-compiles the two carpets against the provisioned musl headers/libraries under
qemu-user, compiles the four GLSL compute shaders to SPIR-V, and stages the binaries plus the mesa
closure into the per-arch rootfs. When `POCL_PREBUILT` points at a matching-arch pocl staging tree,
pocl's `libOpenCL` is folded in and `cl_parallel` is linked against it instead of rusticl.
`programs/run_all.sh` runs the carpets and prints `TEST PASSED` when the Vulkan core carpet reports
`OK`, none fails, and any present OpenCL carpet also reports `OK`.

Runtime environment on target:

- `XDG_RUNTIME_DIR` must point at a writable directory; lavapipe maps host-visible memory through a
  file under it.
- `VK_DRIVER_FILES` selects the lavapipe ICD; the ICD JSON's absolute `library_path` resolves against
  the rootfs root.
- `RUSTICL_ENABLE=llvmpipe` and `OCL_ICD_VENDORS` select the software OpenCL device;
  `POCL_DEVICES=basic` selects pocl's single-threaded CPU device when pocl is folded in.
- `LP_NUM_THREADS=1` pins the mesa thread pool to one thread, matching StarryOS's single vCPU.

## Availability

Alpine edge builds `mesa-vulkan-swrast` for all four target architectures, so `vk_parallel` runs
on-target on x86_64, aarch64, riscv64 and loongarch64. `mesa-rusticl` is a Rust mesa component Alpine
does not build for every arch (absent on riscv64 / loongarch64 today), so `cl_parallel` is additive:
it runs where a CPU OpenCL runtime is packaged and is reported but does not gate the Vulkan core. Both
carpets run in the host reference layer against the same lavapipe / CPU OpenCL devices the on-target
binaries use.

## Single-core execution

StarryOS runs on one vCPU (SMP is off by default), so the software backends execute every workgroup on
a single thread. `run_all.sh` pins the mesa thread pool with `LP_NUM_THREADS=1` and pocl with
`POCL_DEVICES=basic`, and prints the detected CPU count, so the single-core reality is explicit. The
carpets assert numerical/atomic correctness and API ordering semantics, not throughput; the results
are independent of thread count. The cross-workgroup atomic and shared-memory reduction checks remain
meaningful on one thread because the software scheduler still interleaves the workgroups: a lost
atomic update or a misplaced barrier still produces a wrong count or partial. Where the compute queue
family exposes `queueCount == 1` (lavapipe), "multi-queue" is exercised as asynchronous multi-submit
across multiple command buffers rather than hardware-parallel queues, and is asserted as such.

## Run

```
cargo xtask starry app qemu -t gpu-parallel --arch x86_64
cargo xtask starry app qemu -t gpu-parallel --arch aarch64
cargo xtask starry app qemu -t gpu-parallel --arch riscv64
cargo xtask starry app qemu -t gpu-parallel --arch loongarch64
```
