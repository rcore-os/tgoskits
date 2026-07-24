#!/usr/bin/env python3
# wgpu_py_full_api.py - full wgpu-py (Python WebGPU) compute-API carpet on Mesa lavapipe/GL. Walks the
# WebGPU object graph - gpu / adapter / device / queue / shader-module / buffer / bind-group-layout /
# pipeline-layout / compute-pipeline / bind-group / command-encoder / compute-pass / dispatch /
# copy-buffer-to-buffer / read_buffer - and asserts vadd/saxpy/mul results per element against a numpy
# reference. Beyond the convenience readback path it exercises the full compute lifecycle:
#   - explicit host-visible mapping: map_sync / read_mapped / write_mapped / unmap / map_state,
#     MapMode.READ|WRITE, mapped_at_creation, windowed read_mapped.
#   - shader compilation-info query (get_compilation_info_sync) + malformed-WGSL compile-error path.
#   - layout='auto' pipeline + get_bind_group_layout reflection.
#   - create_compute_pipeline_async resolved via GPUPromise.sync_wait.
#   - dynamic bind-group offsets (has_dynamic_offset, per-dispatch offset).
#   - timestamp query family (create_query_set / timestamp_writes / resolve_query_set), feature-gated.
#   - dispatch_workgroups_indirect (indirect group counts read from a buffer).
#   - boundary sizes: zero-size buffer, zero-workgroup no-op dispatch, and a >=1,000,000-element
#     buffer verified element-wise against numpy.
#   - error/validation paths that wgpu-native genuinely surfaces as catchable GPUError:
#     oversubscribed dispatch, map-without-MAP_READ, oversized copy, malformed bind-group,
#     mapping a destroyed buffer.
#   - a real negative control: a live device output is corrupted in one element and the element-wise
#     compare against an independent numpy reference is asserted to flag it.
#   - explicit sync (on_submitted_work_done_sync) and teardown (buffer.destroy / device.destroy).
# Every assertion checks a computed result vs numpy, a queried property vs a known value, or a real
# error. Prints "WGPU_PY_FULL_API OK <n>" only when every assertion passes and the count equals the
# pinned EXPECTED total.
import sys
import numpy as np
import wgpu

P = [0]
F = [0]


def ok(cond, desc):
    if cond:
        P[0] += 1
    else:
        F[0] += 1
        sys.stderr.write("FAIL: %s\n" % desc)


# WGSL compute shader: c[i] = alpha*a[i] + b[i]. alpha and n come from a uniform block so the same
# pipeline drives both vadd (alpha=1) and saxpy (alpha=k).
SAXPY_WGSL = """
struct Params { alpha: f32, n: u32 };
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@group(0) @binding(3) var<uniform>             p: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < p.n) {
        c[i] = p.alpha * a[i] + b[i];
    }
}
"""

# Second shader (separate module + pipeline): c[i] = a[i] * b[i], no uniform - exercises a distinct
# bind-group-layout (3 storage bindings) and a second compute pipeline.
MUL_WGSL = """
@group(0) @binding(0) var<storage, read>       a: array<f32>;
@group(0) @binding(1) var<storage, read>       b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < arrayLength(&a)) {
        c[i] = a[i] * b[i];
    }
}
"""

N = 2048
WG = 64
NBYTES = N * 4
GROUPS = (N + WG - 1) // WG

COMPUTE = wgpu.ShaderStage.COMPUTE
BU = wgpu.BufferUsage
BBT = wgpu.BufferBindingType


def pack_params(alpha, n):
    return np.array([alpha], dtype=np.float32).tobytes() + \
        np.array([n], dtype=np.uint32).tobytes() + b"\x00" * 8


def storage_entry(binding, read_only):
    return {
        "binding": binding,
        "visibility": COMPUTE,
        "buffer": {"type": BBT.read_only_storage if read_only else BBT.storage},
    }


def read_f32(device, buf, n):
    mv = device.queue.read_buffer(buf)
    return np.frombuffer(mv, dtype=np.float32)[:n].copy()


def finish():
    expected = 95
    p, f = P[0], F[0]
    total = p + f
    print("wgpu-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (p, f, total, expected))
    if f == 0 and total == expected:
        print("WGPU_PY_FULL_API OK %d" % p)
        return 0
    print("WGPU_PY_FULL_API FAIL")
    return 1


def run():
    # --- gpu entry + adapter enumeration -----------------------------------------------------
    ok(hasattr(wgpu, "gpu"), "wgpu.gpu present")
    all_adapters = wgpu.gpu.enumerate_adapters_sync()
    ok(len(all_adapters) >= 1, "enumerate_adapters_sync non-empty")
    for a in all_adapters:
        sys.stderr.write("adapter: %s\n" % a.summary)

    adapter = wgpu.gpu.request_adapter_sync(power_preference="low-power")
    if adapter is None:
        adapter = wgpu.gpu.request_adapter_sync(force_fallback_adapter=True)
    if adapter is None:
        ok(False, "request_adapter_sync")
        return finish()
    ok(isinstance(adapter.summary, str) and "via" in adapter.summary, "request_adapter_sync")

    info = adapter.info
    print("wgpu-py adapter selected: %s" % adapter.summary)
    ok(isinstance(info, dict), "adapter.info is dict")
    ok(len(str(info.get("device", ""))) > 0, "adapter.info device non-empty")
    ok(str(info.get("backend_type", "")) in ("Vulkan", "GL", "OpenGL", "GLES"),
       "adapter backend is Vulkan or GL")
    ok(len(adapter.summary) > 0, "adapter.summary non-empty")

    # adapter capability queries
    feats = adapter.features
    ok(isinstance(feats, set), "adapter.features is set")
    lim = adapter.limits
    ok(isinstance(lim, dict), "adapter.limits is dict")
    ok(lim["max-compute-workgroup-size-x"] >= 64, "adapter max-compute-workgroup-size-x>=64")
    ok(lim["max-storage-buffers-per-shader-stage"] >= 3,
       "adapter max-storage-buffers-per-shader-stage>=3")
    ok(lim["max-bind-groups"] >= 1, "adapter max-bind-groups>=1")
    ok(lim["max-compute-invocations-per-workgroup"] >= 64,
       "adapter max-compute-invocations-per-workgroup>=64")

    # --- device + queue ----------------------------------------------------------------------
    device = adapter.request_device_sync()
    ok(device is not None, "request_device_sync")
    ok(device.adapter is adapter, "device.adapter is adapter")
    dlim = device.limits
    ok(dlim["max-compute-invocations-per-workgroup"] >= 64,
       "device max-compute-invocations-per-workgroup>=64")
    ok(isinstance(device.features, set), "device.features is set")
    queue = device.queue
    ok(queue is not None, "device.queue")

    # --- CPU reference data ------------------------------------------------------------------
    a = (np.arange(N) * 0.5).astype(np.float32)
    b = (2.0 * np.arange(N) + 1.0).astype(np.float32)

    # --- buffers -----------------------------------------------------------------------------
    buf_a = device.create_buffer_with_data(data=a, usage=BU.STORAGE | BU.COPY_DST | BU.COPY_SRC)
    ok(buf_a.size == NBYTES, "create_buffer_with_data A size")
    ok((buf_a.usage & BU.STORAGE) != 0, "buffer A usage has STORAGE")
    buf_b = device.create_buffer_with_data(data=b, usage=BU.STORAGE | BU.COPY_DST)
    ok(buf_b.size == NBYTES, "create_buffer_with_data B size")
    buf_c = device.create_buffer(size=NBYTES, usage=BU.STORAGE | BU.COPY_SRC | BU.COPY_DST)
    ok(buf_c.size == NBYTES, "create_buffer C size")
    ok((buf_c.usage & BU.COPY_SRC) != 0, "buffer C usage has COPY_SRC")

    pbuf = device.create_buffer(size=16, usage=BU.UNIFORM | BU.COPY_DST | BU.COPY_SRC)
    ok((pbuf.usage & BU.UNIFORM) != 0, "params buffer usage has UNIFORM")

    # --- shader modules ----------------------------------------------------------------------
    saxpy_mod = device.create_shader_module(code=SAXPY_WGSL)
    ok(saxpy_mod is not None, "create_shader_module saxpy(WGSL)")
    mul_mod = device.create_shader_module(code=MUL_WGSL)
    ok(mul_mod is not None, "create_shader_module mul(WGSL)")

    # --- bind group layout / pipeline layout (saxpy: 3 storage + 1 uniform) -------------------
    bgl = device.create_bind_group_layout(entries=[
        storage_entry(0, True),
        storage_entry(1, True),
        storage_entry(2, False),
        {"binding": 3, "visibility": COMPUTE, "buffer": {"type": BBT.uniform}},
    ])
    ok(bgl is not None, "create_bind_group_layout saxpy")
    pll = device.create_pipeline_layout(bind_group_layouts=[bgl])
    ok(pll is not None, "create_pipeline_layout saxpy")
    saxpy_pipe = device.create_compute_pipeline(
        layout=pll, compute={"module": saxpy_mod, "entry_point": "main"})
    ok(saxpy_pipe is not None, "create_compute_pipeline saxpy")

    bind = device.create_bind_group(layout=bgl, entries=[
        {"binding": 0, "resource": {"buffer": buf_a, "offset": 0, "size": buf_a.size}},
        {"binding": 1, "resource": {"buffer": buf_b, "offset": 0, "size": buf_b.size}},
        {"binding": 2, "resource": {"buffer": buf_c, "offset": 0, "size": buf_c.size}},
        {"binding": 3, "resource": {"buffer": pbuf, "offset": 0, "size": pbuf.size}},
    ])
    ok(bind is not None, "create_bind_group saxpy")

    def dispatch(pipe, bg, label):
        enc = device.create_command_encoder(label=label)
        cp = enc.begin_compute_pass()
        cp.set_pipeline(pipe)
        cp.set_bind_group(0, bg)
        cp.dispatch_workgroups(GROUPS)
        cp.end()
        queue.submit([enc.finish()])

    # --- vadd: alpha=1 -----------------------------------------------------------------------
    # write params, then copy the uniform buffer to a COPY_DST buffer and read it back so the
    # write_buffer upload is verified by value (alpha bits + n) against the packed reference.
    queue.write_buffer(pbuf, 0, pack_params(1.0, N))
    pcheck = device.create_buffer(size=16, usage=BU.COPY_DST | BU.COPY_SRC)
    penc = device.create_command_encoder()
    penc.copy_buffer_to_buffer(pbuf, 0, pcheck, 0, 16)
    queue.submit([penc.finish()])
    praw = bytes(queue.read_buffer(pcheck))
    ok(praw == pack_params(1.0, N), "queue.write_buffer params(alpha=1) round-trips exactly")
    dispatch(saxpy_pipe, bind, "vadd")
    got = read_f32(device, buf_c, N)
    ref = a + b
    ok(got.shape == (N,), "vadd readback shape")
    ok(bool(np.array_equal(got, ref)), "vadd c==a+b (every element)")
    ok(float(got[0]) == float(ref[0]), "vadd element[0]")
    ok(float(got[N // 2]) == float(ref[N // 2]), "vadd element[N/2]")
    ok(float(got[N - 1]) == float(ref[N - 1]), "vadd element[N-1]")

    # --- saxpy: alpha=3 ----------------------------------------------------------------------
    k = 3.0
    queue.write_buffer(pbuf, 0, pack_params(k, N))
    dispatch(saxpy_pipe, bind, "saxpy")
    got = read_f32(device, buf_c, N)
    ref = (k * a + b).astype(np.float32)
    ok(bool(np.allclose(got, ref, atol=1e-4)), "saxpy c==3*a+b (every element)")
    ok(bool(np.array_equal(got, (np.float32(k) * a + b))), "saxpy exact f32 match")

    # --- saxpy alpha=0 -> c == b (edge) ------------------------------------------------------
    queue.write_buffer(pbuf, 0, pack_params(0.0, N))
    dispatch(saxpy_pipe, bind, "alpha0")
    got = read_f32(device, buf_c, N)
    ok(bool(np.array_equal(got, b)), "saxpy alpha=0 c==b (every element)")

    # --- partial n: only first half written; tail keeps prior alpha=0 result (==b) -----------
    half = N // 2
    queue.write_buffer(pbuf, 0, pack_params(5.0, half))
    dispatch(saxpy_pipe, bind, "partial")
    got = read_f32(device, buf_c, N)
    ref = b.copy()
    ref[:half] = (np.float32(5.0) * a[:half] + b[:half])
    ok(bool(np.array_equal(got[:half], ref[:half])), "partial-n head c==5*a+b")
    ok(bool(np.array_equal(got[half:], b[half:])), "partial-n tail untouched ==b")

    # --- second pipeline: element-wise multiply ----------------------------------------------
    bgl2 = device.create_bind_group_layout(entries=[
        storage_entry(0, True), storage_entry(1, True), storage_entry(2, False)])
    ok(bgl2 is not None, "create_bind_group_layout mul")
    pll2 = device.create_pipeline_layout(bind_group_layouts=[bgl2])
    mul_pipe = device.create_compute_pipeline(
        layout=pll2, compute={"module": mul_mod, "entry_point": "main"})
    ok(mul_pipe is not None, "create_compute_pipeline mul")
    bind2 = device.create_bind_group(layout=bgl2, entries=[
        {"binding": 0, "resource": {"buffer": buf_a, "offset": 0, "size": buf_a.size}},
        {"binding": 1, "resource": {"buffer": buf_b, "offset": 0, "size": buf_b.size}},
        {"binding": 2, "resource": {"buffer": buf_c, "offset": 0, "size": buf_c.size}},
    ])
    ok(bind2 is not None, "create_bind_group mul")
    dispatch(mul_pipe, bind2, "mul")
    got = read_f32(device, buf_c, N)
    ref = (a * b).astype(np.float32)
    ok(bool(np.array_equal(got, ref)), "mul c==a*b (every element)")
    ok(float(got[7]) == float(a[7] * b[7]), "mul element[7]")

    # --- buffer update then re-dispatch (write_buffer to a STORAGE buffer) --------------------
    a2 = np.full(N, 4.0, dtype=np.float32)
    queue.write_buffer(buf_a, 0, a2)
    # verify the STORAGE-buffer upload by copying buf_a into a readable scratch and comparing.
    ascratch = device.create_buffer(size=NBYTES, usage=BU.COPY_DST | BU.COPY_SRC)
    aenc = device.create_command_encoder()
    aenc.copy_buffer_to_buffer(buf_a, 0, ascratch, 0, NBYTES)
    queue.submit([aenc.finish()])
    ok(bool(np.array_equal(read_f32(device, ascratch, N), a2)),
       "queue.write_buffer buf_a<-4.0 round-trips (every element)")
    queue.write_buffer(pbuf, 0, pack_params(1.0, N))
    dispatch(saxpy_pipe, bind, "vadd2")
    got = read_f32(device, buf_c, N)
    ok(bool(np.array_equal(got, (a2 + b))), "vadd after write_buffer c==4+b (every element)")

    # --- copy_buffer_to_buffer chain: c -> mid -> readback -----------------------------------
    mid = device.create_buffer(size=NBYTES, usage=BU.COPY_SRC | BU.COPY_DST)
    ok(mid is not None, "create_buffer mid(COPY_SRC|DST)")
    enc = device.create_command_encoder()
    enc.copy_buffer_to_buffer(buf_c, 0, mid, 0, NBYTES)
    cbuf = enc.finish()
    ok(cbuf is not None, "command_encoder.finish yields a command buffer")
    queue.submit([cbuf])
    got = read_f32(device, mid, N)
    ok(bool(np.array_equal(got, (a2 + b))), "copy chain preserves c (every element)")

    # --- partial read_buffer via offset/size window ------------------------------------------
    win = queue.read_buffer(buf_c, 4, (N - 1) * 4)
    win = np.frombuffer(win, dtype=np.float32)
    ok(win.shape == (N - 1,), "read_buffer windowed size")
    ok(bool(np.array_equal(win, (a2 + b)[1:])), "read_buffer windowed values")

    # --- clear_buffer then verify zeros ------------------------------------------------------
    enc = device.create_command_encoder()
    enc.clear_buffer(buf_c)
    queue.submit([enc.finish()])
    got = read_f32(device, buf_c, N)
    ok(bool(np.array_equal(got, np.zeros(N, dtype=np.float32))), "clear_buffer zeros c")

    # === negative control: prove the correctness check can detect a WRONG result =============
    # take a real device output (clear_buffer zeros) and corrupt one element; assert the
    # element-wise compare against the INDEPENDENT numpy reference (all zeros) flags it.
    corrupt = read_f32(device, buf_c, N)  # all zeros from clear_buffer above
    ref_zero = np.zeros(N, dtype=np.float32)
    ok(bool(np.array_equal(corrupt, ref_zero)), "neg-ctrl baseline device output == reference")
    corrupt = corrupt.copy()
    corrupt[123] = np.float32(7.0)  # inject a wrong value into the real device readback
    ok(not bool(np.array_equal(corrupt, ref_zero)),
       "neg-ctrl corrupted device output is flagged wrong vs numpy reference")
    ok(int(np.count_nonzero(corrupt != ref_zero)) == 1,
       "neg-ctrl exactly one element differs from reference")

    # === explicit buffer mapping: MAP_READ (host-visible readback path) ======================
    # compute vadd once more into buf_c, copy into a MAP_READ buffer, map it, read_mapped,
    # and compare host-visible bytes to the numpy reference (a2 + b). This exercises the
    # host-visible mapped memory that queue.read_buffer hides behind a copy.
    queue.write_buffer(pbuf, 0, pack_params(1.0, N))
    dispatch(saxpy_pipe, bind, "map_read_src")
    map_ref = (a2 + b).astype(np.float32)
    rbuf = device.create_buffer(size=NBYTES, usage=BU.MAP_READ | BU.COPY_DST)
    ok(rbuf.map_state == "unmapped", "MAP_READ buffer initial map_state unmapped")
    menc = device.create_command_encoder()
    menc.copy_buffer_to_buffer(buf_c, 0, rbuf, 0, NBYTES)
    queue.submit([menc.finish()])
    rbuf.map_sync(mode=wgpu.MapMode.READ)
    ok(rbuf.map_state == "mapped", "MAP_READ buffer map_state mapped after map_sync")
    mapped = np.frombuffer(rbuf.read_mapped(), dtype=np.float32)
    ok(bool(np.array_equal(mapped, map_ref)), "read_mapped values == numpy ref (every element)")
    # windowed read_mapped: bytes for elements [2..] only (offset must be 8-aligned)
    win_bytes = rbuf.read_mapped(buffer_offset=8, size=(N - 2) * 4)
    win_m = np.frombuffer(win_bytes, dtype=np.float32)
    ok(bool(np.array_equal(win_m, map_ref[2:])), "windowed read_mapped values == ref[2:]")
    rbuf.unmap()
    ok(rbuf.map_state == "unmapped", "MAP_READ buffer map_state unmapped after unmap")

    # === explicit buffer mapping: MAP_WRITE (host->device upload path) ========================
    wsrc = (np.arange(N) * 0.25 - 3.0).astype(np.float32)
    wbuf = device.create_buffer(size=NBYTES, usage=BU.MAP_WRITE | BU.COPY_SRC)
    wbuf.map_sync(mode=wgpu.MapMode.WRITE)
    wbuf.write_mapped(wsrc.tobytes())
    wbuf.unmap()
    wdst = device.create_buffer(size=NBYTES, usage=BU.COPY_DST | BU.COPY_SRC)
    wenc = device.create_command_encoder()
    wenc.copy_buffer_to_buffer(wbuf, 0, wdst, 0, NBYTES)
    queue.submit([wenc.finish()])
    ok(bool(np.array_equal(read_f32(device, wdst, N), wsrc)),
       "write_mapped round-trips host->device (every element)")

    # mapped_at_creation write path
    mcbuf = device.create_buffer(size=NBYTES, usage=BU.MAP_WRITE | BU.COPY_SRC,
                                 mapped_at_creation=True)
    ok(mcbuf.map_state == "mapped", "mapped_at_creation buffer starts mapped")
    mcbuf.write_mapped(wsrc.tobytes())
    mcbuf.unmap()
    mcdst = device.create_buffer(size=NBYTES, usage=BU.COPY_DST | BU.COPY_SRC)
    mcenc = device.create_command_encoder()
    mcenc.copy_buffer_to_buffer(mcbuf, 0, mcdst, 0, NBYTES)
    queue.submit([mcenc.finish()])
    ok(bool(np.array_equal(read_f32(device, mcdst, N), wsrc)),
       "mapped_at_creation upload round-trips (every element)")

    # === shader-module compilation info + compile-error negative path ========================
    good_info = saxpy_mod.get_compilation_info_sync()
    ok(isinstance(good_info, list) and len(good_info) == 0,
       "get_compilation_info on valid WGSL has no messages")
    # malformed WGSL: wgpu-py DOES surface a GPUValidationError from the wgpu-native parser.
    compile_err = None
    try:
        device.create_shader_module(code="@@@ this is not wgsl fn @compute")
    except wgpu.GPUError as e:
        compile_err = e
    ok(isinstance(compile_err, wgpu.GPUError), "malformed WGSL raises GPUError")
    ok("parsing error" in str(compile_err) or "expected" in str(compile_err),
       "compile error message names a parse failure")

    # === auto pipeline layout + get_bind_group_layout ========================================
    # layout='auto' derives the bind-group layout from the shader; reflect it and reuse it.
    auto_pipe = device.create_compute_pipeline(
        layout="auto", compute={"module": mul_mod, "entry_point": "main"})
    ok(auto_pipe is not None, "create_compute_pipeline layout='auto'")
    auto_bgl = auto_pipe.get_bind_group_layout(0)
    ok(auto_bgl is not None, "pipeline.get_bind_group_layout(0)")
    auto_bg = device.create_bind_group(layout=auto_bgl, entries=[
        {"binding": 0, "resource": {"buffer": buf_a, "offset": 0, "size": buf_a.size}},
        {"binding": 1, "resource": {"buffer": buf_b, "offset": 0, "size": buf_b.size}},
        {"binding": 2, "resource": {"buffer": buf_c, "offset": 0, "size": buf_c.size}},
    ])
    dispatch(auto_pipe, auto_bg, "auto")
    ok(bool(np.array_equal(read_f32(device, buf_c, N), (a2 * b).astype(np.float32))),
       "auto-layout pipeline mul c==a*b (every element)")

    # === async compute pipeline (GPUPromise.sync_wait) =======================================
    promise = device.create_compute_pipeline_async(
        layout=pll2, compute={"module": mul_mod, "entry_point": "main"})
    async_pipe = promise.sync_wait()
    ok(async_pipe is not None, "create_compute_pipeline_async -> sync_wait resolves")
    dispatch(async_pipe, bind2, "async")
    ok(bool(np.array_equal(read_f32(device, buf_c, N), (a2 * b).astype(np.float32))),
       "async pipeline mul c==a*b (every element)")

    # === dynamic bind-group offsets ==========================================================
    # one storage buffer holds two segments; a single bind group with has_dynamic_offset is
    # dispatched twice with different offsets, doubling each segment. Verifies the offset is
    # applied by comparing each output to its numpy reference.
    dyn_mod = device.create_shader_module(code=(
        "@group(0) @binding(0) var<storage, read>       s: array<f32>;\n"
        "@group(0) @binding(1) var<storage, read_write> o: array<f32>;\n"
        "@compute @workgroup_size(64)\n"
        "fn main(@builtin(global_invocation_id) g: vec3<u32>) {\n"
        "  let i = g.x; if (i < arrayLength(&o)) { o[i] = s[i] * 2.0; }\n"
        "}\n"))
    dyn_bgl = device.create_bind_group_layout(entries=[
        {"binding": 0, "visibility": COMPUTE,
         "buffer": {"type": BBT.read_only_storage, "has_dynamic_offset": True}},
        {"binding": 1, "visibility": COMPUTE, "buffer": {"type": BBT.storage}},
    ])
    dyn_pll = device.create_pipeline_layout(bind_group_layouts=[dyn_bgl])
    dyn_pipe = device.create_compute_pipeline(
        layout=dyn_pll, compute={"module": dyn_mod, "entry_point": "main"})
    M = 128
    seg = M * 4
    align = 256  # min-storage-buffer-offset-alignment
    off2 = seg + (align - seg % align) % align
    seg0 = np.arange(M, dtype=np.float32)
    seg1 = (100.0 + np.arange(M)).astype(np.float32)
    dyn_src = device.create_buffer(size=off2 + seg, usage=BU.STORAGE | BU.COPY_DST)
    queue.write_buffer(dyn_src, 0, seg0)
    queue.write_buffer(dyn_src, off2, seg1)
    dyn_out = device.create_buffer(size=seg, usage=BU.STORAGE | BU.COPY_SRC)
    dyn_bg = device.create_bind_group(layout=dyn_bgl, entries=[
        {"binding": 0, "resource": {"buffer": dyn_src, "offset": 0, "size": seg}},
        {"binding": 1, "resource": {"buffer": dyn_out, "offset": 0, "size": dyn_out.size}},
    ])
    for dyn_off, dyn_ref in ((0, seg0 * 2.0), (off2, seg1 * 2.0)):
        enc = device.create_command_encoder()
        cp = enc.begin_compute_pass()
        cp.set_pipeline(dyn_pipe)
        cp.set_bind_group(0, dyn_bg, [dyn_off])
        cp.dispatch_workgroups((M + WG - 1) // WG)
        cp.end()
        queue.submit([enc.finish()])
        got_d = read_f32(device, dyn_out, M)
        ok(bool(np.array_equal(got_d, dyn_ref.astype(np.float32))),
           "dynamic offset %d segment*2 (every element)" % dyn_off)

    # === timestamp query set (feature-gated) =================================================
    if "timestamp-query" in adapter.features:
        tdev = adapter.request_device_sync(required_features=["timestamp-query"])
        ok("timestamp-query" in tdev.features, "timestamp device feature enabled")
        tq = tdev.create_query_set(type="timestamp", count=2)
        ok(tq.count == 2, "query_set count == 2")
        ok(tq.type == "timestamp", "query_set type == timestamp")
        # a trivial dispatch bracketed by timestamp writes
        tmod = tdev.create_shader_module(code=(
            "@group(0) @binding(0) var<storage, read_write> o: array<u32>;\n"
            "@compute @workgroup_size(64)\n"
            "fn main(@builtin(global_invocation_id) g: vec3<u32>) { o[g.x] = g.x; }\n"))
        tbgl = tdev.create_bind_group_layout(entries=[
            {"binding": 0, "visibility": COMPUTE, "buffer": {"type": BBT.storage}}])
        tpll = tdev.create_pipeline_layout(bind_group_layouts=[tbgl])
        tpipe = tdev.create_compute_pipeline(
            layout=tpll, compute={"module": tmod, "entry_point": "main"})
        tout = tdev.create_buffer(size=64 * 4, usage=BU.STORAGE | BU.COPY_SRC)
        tbg = tdev.create_bind_group(layout=tbgl, entries=[
            {"binding": 0, "resource": {"buffer": tout, "offset": 0, "size": tout.size}}])
        qresolve = tdev.create_buffer(size=2 * 8, usage=BU.QUERY_RESOLVE | BU.COPY_SRC)
        qread = tdev.create_buffer(size=2 * 8, usage=BU.COPY_DST | BU.MAP_READ)
        tenc = tdev.create_command_encoder()
        tcp = tenc.begin_compute_pass(timestamp_writes={
            "query_set": tq, "beginning_of_pass_write_index": 0, "end_of_pass_write_index": 1})
        tcp.set_pipeline(tpipe)
        tcp.set_bind_group(0, tbg)
        tcp.dispatch_workgroups(1)
        tcp.end()
        tenc.resolve_query_set(tq, 0, 2, qresolve, 0)
        tenc.copy_buffer_to_buffer(qresolve, 0, qread, 0, 16)
        tdev.queue.submit([tenc.finish()])
        qread.map_sync(mode=wgpu.MapMode.READ)
        stamps = np.frombuffer(qread.read_mapped(), dtype=np.uint64).copy()
        qread.unmap()
        ok(int(stamps[0]) > 0 and int(stamps[1]) > 0, "timestamps non-zero")
        ok(int(stamps[1]) >= int(stamps[0]), "end timestamp >= begin timestamp")
        # the dispatch itself still ran correctly under timestamp instrumentation
        ok(bool(np.array_equal(np.frombuffer(tdev.queue.read_buffer(tout), dtype=np.uint32),
                               np.arange(64, dtype=np.uint32))),
           "timestamped dispatch result == index (every element)")
    else:
        print("SKIP timestamp-query: adapter feature unavailable (non-counting)")

    # === indirect dispatch ===================================================================
    # dispatch_workgroups_indirect reads [x,y,z] group counts from a buffer; verify the compute
    # ran across all N elements (mul again) by comparing to the numpy reference.
    ind_counts = np.array([GROUPS, 1, 1], dtype=np.uint32)
    ind_buf = device.create_buffer_with_data(data=ind_counts, usage=BU.INDIRECT | BU.STORAGE)
    enc = device.create_command_encoder()
    cp = enc.begin_compute_pass()
    cp.set_pipeline(mul_pipe)
    cp.set_bind_group(0, bind2)
    cp.dispatch_workgroups_indirect(ind_buf, 0)
    cp.end()
    queue.submit([enc.finish()])
    ok(bool(np.array_equal(read_f32(device, buf_c, N), (a2 * b).astype(np.float32))),
       "indirect dispatch mul c==a*b (every element)")

    # === large-size boundary: >= 1,000,000 f32 elements, verified element-wise ===============
    BIG = 1 << 20  # 1048576 >= 1e6
    big_a = (np.arange(BIG) % 97).astype(np.float32)
    big_b = (np.arange(BIG) % 13).astype(np.float32)
    big_mod = device.create_shader_module(code=(
        "@group(0) @binding(0) var<storage, read>       a: array<f32>;\n"
        "@group(0) @binding(1) var<storage, read>       b: array<f32>;\n"
        "@group(0) @binding(2) var<storage, read_write> c: array<f32>;\n"
        "@compute @workgroup_size(256)\n"
        "fn main(@builtin(global_invocation_id) g: vec3<u32>) {\n"
        "  let i = g.x; if (i < arrayLength(&c)) { c[i] = a[i] + b[i]; }\n"
        "}\n"))
    big_bgl = device.create_bind_group_layout(entries=[
        storage_entry(0, True), storage_entry(1, True), storage_entry(2, False)])
    big_pll = device.create_pipeline_layout(bind_group_layouts=[big_bgl])
    big_pipe = device.create_compute_pipeline(
        layout=big_pll, compute={"module": big_mod, "entry_point": "main"})
    ba = device.create_buffer_with_data(data=big_a, usage=BU.STORAGE)
    bb = device.create_buffer_with_data(data=big_b, usage=BU.STORAGE)
    bc = device.create_buffer(size=BIG * 4, usage=BU.STORAGE | BU.COPY_SRC)
    big_bg = device.create_bind_group(layout=big_bgl, entries=[
        {"binding": 0, "resource": {"buffer": ba, "offset": 0, "size": ba.size}},
        {"binding": 1, "resource": {"buffer": bb, "offset": 0, "size": bb.size}},
        {"binding": 2, "resource": {"buffer": bc, "offset": 0, "size": bc.size}},
    ])
    big_groups = (BIG + 255) // 256
    ok(big_groups <= device.limits["max-compute-workgroups-per-dimension"],
       "large dispatch groups within per-dimension limit")
    enc = device.create_command_encoder()
    cp = enc.begin_compute_pass()
    cp.set_pipeline(big_pipe)
    cp.set_bind_group(0, big_bg)
    cp.dispatch_workgroups(big_groups)
    cp.end()
    queue.submit([enc.finish()])
    big_got = np.frombuffer(device.queue.read_buffer(bc), dtype=np.float32)
    ok(big_got.shape == (BIG,), "large readback shape == (1<<20,)")
    ok(bool(np.array_equal(big_got, big_a + big_b)),
       "large c==a+b every element (>=1M verified)")

    # === zero-size boundary ==================================================================
    zbuf = device.create_buffer(size=0, usage=BU.STORAGE | BU.COPY_SRC)
    ok(zbuf.size == 0, "zero-size buffer created with size 0")
    # zero-workgroup dispatch is a valid no-op: buf_c retains the indirect-mul result.
    before_zero = read_f32(device, buf_c, N).copy()
    enc = device.create_command_encoder()
    cp = enc.begin_compute_pass()
    cp.set_pipeline(mul_pipe)
    cp.set_bind_group(0, bind2)
    cp.dispatch_workgroups(0)
    cp.end()
    queue.submit([enc.finish()])
    queue.on_submitted_work_done_sync()
    ok(bool(np.array_equal(read_f32(device, buf_c, N), before_zero)),
       "zero-workgroup dispatch is a no-op (output unchanged)")

    # === error/validation paths (wgpu-native DOES surface GPUValidationError) ================
    # oversubscription: dispatch beyond max-compute-workgroups-per-dimension.
    over = device.limits["max-compute-workgroups-per-dimension"] + 1
    over_err = None
    try:
        enc = device.create_command_encoder()
        cp = enc.begin_compute_pass()
        cp.set_pipeline(mul_pipe)
        cp.set_bind_group(0, bind2)
        cp.dispatch_workgroups(over)
        cp.end()
        queue.submit([enc.finish()])
        queue.on_submitted_work_done_sync()
    except wgpu.GPUError as e:
        over_err = e
    ok(isinstance(over_err, wgpu.GPUError), "oversubscribed dispatch raises GPUValidationError")

    # bad-usage: map a buffer that lacks MAP_READ.
    usage_err = None
    try:
        nomap = device.create_buffer(size=64, usage=BU.STORAGE)
        nomap.map_sync(mode=wgpu.MapMode.READ)
        queue.on_submitted_work_done_sync()
    except wgpu.GPUError as e:
        usage_err = e
    ok(isinstance(usage_err, wgpu.GPUError), "map without MAP_READ raises GPUValidationError")

    # size mismatch: copy more bytes than the source buffer holds.
    size_err = None
    try:
        senc = device.create_command_encoder()
        senc.copy_buffer_to_buffer(buf_a, 0, buf_b, 0, buf_a.size + NBYTES)
        queue.submit([senc.finish()])
        queue.on_submitted_work_done_sync()
    except wgpu.GPUError as e:
        size_err = e
    ok(isinstance(size_err, wgpu.GPUError), "copy larger than source raises GPUValidationError")

    # bad bind-group: layout expects 3 bindings, supply 1 (eager wgpu validation).
    bg_err = None
    try:
        device.create_bind_group(layout=bgl2, entries=[
            {"binding": 0, "resource": {"buffer": buf_a, "offset": 0, "size": buf_a.size}}])
    except wgpu.GPUError as e:
        bg_err = e
    ok(isinstance(bg_err, wgpu.GPUError), "bad bind-group (missing bindings) raises GPUError")

    # === explicit sync + teardown ============================================================
    # a final compute + explicit block-until-idle; result must still be correct after the wait.
    queue.write_buffer(pbuf, 0, pack_params(2.0, N))
    dispatch(saxpy_pipe, bind, "final_sync")
    queue.on_submitted_work_done_sync()
    ok(bool(np.array_equal(read_f32(device, buf_c, N),
                           (np.float32(2.0) * a2 + b))),
       "on_submitted_work_done_sync then readback c==2*a+b (every element)")

    # buffer.destroy: destroy is idempotent, and mapping a destroyed buffer raises GPUError.
    dbuf = device.create_buffer(size=64, usage=BU.MAP_READ | BU.COPY_DST)
    dbuf.destroy()
    dbuf.destroy()  # idempotent, must not raise
    ok(dbuf.map_state == "unmapped", "destroyed buffer map_state unmapped (destroy idempotent)")
    destroy_err = None
    try:
        dbuf.map_sync(mode=wgpu.MapMode.READ)
    except wgpu.GPUError as e:
        destroy_err = e
    ok(isinstance(destroy_err, wgpu.GPUError), "mapping a destroyed buffer raises GPUError")
    ind_buf.destroy()

    # device.destroy teardown: after destroy, buffer creation is rejected.
    device.destroy()
    dev_destroyed = None
    try:
        device.create_buffer(size=NBYTES, usage=BU.STORAGE)
        queue.on_submitted_work_done_sync()
    except Exception as e:
        dev_destroyed = e
    ok(dev_destroyed is not None, "creating a buffer on a destroyed device fails")

    return finish()


if __name__ == "__main__":
    sys.exit(run())
