#!/usr/bin/env python3
# gles_py_full_api.py - full moderngl GLES-3.1-equivalent compute API carpet on a standalone
# surfaceless context (EGL/GLX, Mesa llvmpipe software path). Exercises the complete moderngl
# compute lifecycle: standalone context / version + info + extensions query / compute limits /
# storage buffers (SSBO) + uniform blocks (UBO) / compute_shader compile (incl. compile-error) /
# std430 bindings / uniforms / single- and multi-dimensional dispatch / run_indirect / explicit
# memory_barrier + finish coherency / timestamp query / buffer map orphan/write/read/read_into /
# read_chunks / write_chunks / sub-range read / clear / copy_buffer / external_buffer / assign /
# invalid-arg + read-past-end + copy-overflow + compile-error validation paths / clear_errors /
# zero-size dispatch / >=1M-element element-wise verification / a genuine negative control that
# corrupts a real device-output element and asserts an INDEPENDENT numpy reference flags it.
# Every assertion checks a computed compute result element-by-element against numpy, a real queried
# property against a known value, or a real raised error. Prints "GLES_PY_FULL_API OK <n>" only
# when every assertion passes and the count equals EXPECTED.
import sys, numpy as np, moderngl

P = [0]; F = [0]
def ok(c, d):
    if c: P[0] += 1
    else: F[0] += 1; sys.stderr.write("FAIL: %s\n" % d)
def skip(d):  # capability-gated: NON-COUNTING notice, never a counted pass
    sys.stderr.write("SKIP (non-counting): %s\n" % d)

# --- standalone surfaceless context (try GLES-3.1-equivalent compute profile require=430) ---
try:
    ctx = moderngl.create_context(standalone=True, require=430)
except Exception:
    ctx = moderngl.create_context(standalone=True)
ok(ctx is not None, "moderngl.create_context standalone")
ok(ctx.version_code >= 310, "version_code >= 310 (compute-capable)")
ok(ctx.error == "GL_NO_ERROR", "ctx.error clean at start")

# --- info query (GL_RENDERER / GL_VENDOR / GL_VERSION / compute limits) ---
info = ctx.info
ok(isinstance(info.get("GL_RENDERER"), str) and len(info["GL_RENDERER"]) > 0, "info GL_RENDERER")
ok(isinstance(info.get("GL_VENDOR"), str) and len(info["GL_VENDOR"]) > 0, "info GL_VENDOR")
ok(isinstance(info.get("GL_VERSION"), str) and len(info["GL_VERSION"]) > 0, "info GL_VERSION")
wg_count = info.get("GL_MAX_COMPUTE_WORK_GROUP_COUNT")
ok(isinstance(wg_count, tuple) and wg_count[0] >= 1, "GL_MAX_COMPUTE_WORK_GROUP_COUNT[0] >= 1")
wg_size = info.get("GL_MAX_COMPUTE_WORK_GROUP_SIZE")
ok(isinstance(wg_size, tuple) and wg_size[0] >= 64, "GL_MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 64")
ok(int(info.get("GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS", 0)) >= 64, "GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64")
ok(int(info.get("GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS", 0)) >= 3, "GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3")

# --- extensions enumeration (real string set, GL_MAX work-group count must exceed 1M in X) ---
exts = ctx.extensions
ok(isinstance(exts, (set, frozenset)) and len(exts) > 0, "ctx.extensions non-empty set")
ok(all(isinstance(e, str) for e in exts), "ctx.extensions all str")
ok(wg_count[0] >= (1 << 20) // 64, "GL_MAX_COMPUTE_WORK_GROUP_COUNT[0] admits >=1M-elt X dispatch")

# --- compute shaders: saxpy (uniform alpha + n guard) and elementwise mul ---
GLSL_VER = "430" if ctx.version_code >= 430 else "310 es\nprecision highp float;\nprecision highp int;"
SAXPY_SRC = """
#version %s
layout(local_size_x=64) in;
layout(std430, binding=0) readonly buffer A { float a[]; };
layout(std430, binding=1) readonly buffer B { float b[]; };
layout(std430, binding=2) writeonly buffer C { float c[]; };
uniform float alpha;
uniform uint n;
void main(){ uint i = gl_GlobalInvocationID.x; if (i < n) c[i] = alpha * a[i] + b[i]; }
""" % GLSL_VER

MUL_SRC = """
#version %s
layout(local_size_x=64) in;
layout(std430, binding=0) readonly buffer A { float a[]; };
layout(std430, binding=1) readonly buffer B { float b[]; };
layout(std430, binding=2) writeonly buffer C { float c[]; };
uniform uint n;
void main(){ uint i = gl_GlobalInvocationID.x; if (i < n) c[i] = a[i] * b[i]; }
""" % GLSL_VER

cs = ctx.compute_shader(SAXPY_SRC); ok(cs is not None, "ctx.compute_shader(saxpy) compiled+linked")
mul = ctx.compute_shader(MUL_SRC); ok(mul is not None, "ctx.compute_shader(mul) compiled+linked")

# --- compile-ERROR path: malformed GLSL must raise moderngl.Error (validation surface fires) ---
BAD_SRC = "#version %s\nlayout(local_size_x=64) in;\nvoid main(){ this is not valid glsl @@@ }" % GLSL_VER
try:
    ctx.compute_shader(BAD_SRC)
    ok(False, "malformed compute shader should raise moderngl.Error")
except moderngl.Error:
    ok(True, "malformed compute shader raises moderngl.Error (compile-error path)")

# --- program member introspection (uniforms + SSBO blocks present) ---
members = set(cs._members.keys())
ok("alpha" in members, "compute member 'alpha' (uniform)")
ok("n" in members, "compute member 'n' (uniform)")
ok({"A", "B", "C"}.issubset(members), "compute members A/B/C (SSBO blocks)")
ok(cs.get("alpha", None) is not None, "compute_shader.get('alpha')")

# --- storage buffers via ctx.buffer(data) / ctx.buffer(reserve=) ---
N = 1024
GROUPS = (N + 63) // 64
a = np.arange(N, dtype=np.float32)
b = (2.0 * np.arange(N) + 1.0).astype(np.float32)
buf_a = ctx.buffer(a.tobytes()); ok(buf_a.size == a.nbytes, "ctx.buffer(A) size == a.nbytes")
buf_b = ctx.buffer(b.tobytes()); ok(buf_b.size == b.nbytes, "ctx.buffer(B) size == b.nbytes")
buf_c = ctx.buffer(reserve=a.nbytes); ok(buf_c.size == a.nbytes, "ctx.buffer(reserve=) C size")
ok(buf_a.dynamic is False, "buffer.dynamic default False")

# --- bind SSBOs to std430 storage binding points ---
buf_a.bind_to_storage_buffer(0)
buf_b.bind_to_storage_buffer(1)
buf_c.bind_to_storage_buffer(2)

# --- vadd via saxpy with alpha=1: barrier + read, assert EVERY element c == a + b ---
cs["alpha"] = 1.0
cs["n"] = N
ok(abs(float(cs.get("alpha", None).value) - 1.0) < 1e-6, "uniform readback alpha==1.0")
cs.run(group_x=GROUPS)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)  # explicit SSBO coherency before readback
ctx.finish()                                        # explicit GPU sync
res = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(res.shape[0] == N, "vadd readback length == N")
ok(np.array_equal(res, a + b), "vadd: every element c == a + b")

# --- negative control: corrupt a real device-output element, assert INDEPENDENT ref flags it ---
ref_vadd = (a + b).astype(np.float32)
corrupt = res.copy(); corrupt[N // 2] += 1.0  # perturb one real device-output element
ok(not np.array_equal(corrupt, ref_vadd), "negative control: corrupted device output != numpy ref")
ok(int(np.count_nonzero(corrupt != ref_vadd)) == 1, "negative control: exactly one element flagged")
ok(np.array_equal(res, ref_vadd), "negative control: pristine device output still matches ref")

# --- saxpy with alpha=3 (uniform-driven): barrier + assert EVERY element c == 3*a + b ---
cs["alpha"] = 3.0
cs.run(group_x=GROUPS)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
res = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.allclose(res, 3.0 * a + b, rtol=0, atol=1e-4), "saxpy: every element c == 3*a + b (np.allclose)")
ok(np.array_equal(res, (3.0 * a + b).astype(np.float32)), "saxpy: exact vs f32 reference")

# --- run_indirect: same saxpy dispatched from a GPU-side (num_groups_x,y,z) buffer ---
indirect = ctx.buffer(np.array([GROUPS, 1, 1], dtype=np.uint32).tobytes())
buf_a.bind_to_storage_buffer(0); buf_b.bind_to_storage_buffer(1); buf_c.bind_to_storage_buffer(2)
buf_c.clear()
cs.run_indirect(indirect)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
res = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.array_equal(res, (3.0 * a + b).astype(np.float32)), "run_indirect: every element c == 3*a + b")

# --- elementwise mul with a separate program: assert EVERY element c == a * b ---
buf_a.bind_to_storage_buffer(0); buf_b.bind_to_storage_buffer(1); buf_c.bind_to_storage_buffer(2)
mul["n"] = N
mul.run(group_x=GROUPS)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
res = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.array_equal(res, (a * b).astype(np.float32)), "mul: every element c == a * b")

# --- multi-dimensional dispatch (group_x + group_y + local_size_y): assert 2D index math ---
W, H = 32, 16
MD_SRC = """
#version %s
layout(local_size_x=8, local_size_y=8, local_size_z=1) in;
layout(std430, binding=0) writeonly buffer O { uint o[]; };
uniform uint W;
void main(){
  uint x = gl_GlobalInvocationID.x; uint y = gl_GlobalInvocationID.y;
  o[y * W + x] = x * 1000u + y;
}
""" % GLSL_VER
md = ctx.compute_shader(MD_SRC)
buf_o = ctx.buffer(reserve=W * H * 4); buf_o.bind_to_storage_buffer(0)
md["W"] = W
md.run(group_x=W // 8, group_y=H // 8, group_z=1)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
grid = np.frombuffer(buf_o.read(), dtype=np.uint32).reshape(H, W)
xs, ys = np.meshgrid(np.arange(W, dtype=np.uint32), np.arange(H, dtype=np.uint32))
ok(np.array_equal(grid, (xs * 1000 + ys).astype(np.uint32)), "multidim dispatch: every (x,y) cell == x*1000+y")

# --- uniform block (UBO) path: bind_to_uniform_block, assert scaled output ---
UBO_SRC = """
#version %s
layout(local_size_x=64) in;
layout(std140, binding=0) uniform Params { float scale; uint n; };
layout(std430, binding=0) writeonly buffer C { float c[]; };
void main(){ uint i = gl_GlobalInvocationID.x; if (i < n) c[i] = float(i) * scale; }
""" % GLSL_VER
ubo_cs = ctx.compute_shader(UBO_SRC)
ok("Params" in set(ubo_cs._members.keys()), "compute member 'Params' (uniform block)")
ubo = ctx.buffer(np.array([2.5], dtype=np.float32).tobytes() + np.array([N], dtype=np.uint32).tobytes())
ubo.bind_to_uniform_block(0)
buf_ubo = ctx.buffer(reserve=N * 4); buf_ubo.bind_to_storage_buffer(0)
ubo_cs.run(group_x=GROUPS)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
res = np.frombuffer(buf_ubo.read(), dtype=np.float32)
ok(np.allclose(res, (np.arange(N) * 2.5).astype(np.float32), rtol=0, atol=1e-3), "UBO: every element c == i*scale (2.5)")

# --- timestamp query: time a dispatch, assert non-negative elapsed nanoseconds ---
tq = ctx.query(time=True)
buf_a.bind_to_storage_buffer(0); buf_b.bind_to_storage_buffer(1); buf_c.bind_to_storage_buffer(2)
with tq:
    cs.run(group_x=GROUPS)
ctx.finish()
ok(isinstance(tq.elapsed, int) and tq.elapsed >= 0, "ctx.query(time=True).elapsed >= 0 ns")

# --- buffer orphan + write + re-dispatch (a <- 2.0), assert vadd == 2 + b ---
buf_a.orphan()
a2 = np.full(N, 2.0, dtype=np.float32)
buf_a.write(a2.tobytes())
buf_a.bind_to_storage_buffer(0); buf_b.bind_to_storage_buffer(1); buf_c.bind_to_storage_buffer(2)
cs["alpha"] = 1.0
cs.run(group_x=GROUPS)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
res = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.array_equal(res, a2 + b), "vadd after orphan/write: every element == 2 + b")

# --- sub-range read (offset+size) matches the corresponding numpy slice ---
ref = (a2 + b).astype(np.float32)
sub = np.frombuffer(buf_c.read(size=256, offset=64), dtype=np.float32)
ok(sub.shape[0] == 64, "sub-range read length == 64 elements")
ok(np.array_equal(sub, ref[16:16 + 64]), "sub-range read matches numpy slice[16:80]")

# --- read_chunks: strided gather of every 4th element (chunk 4B, step 16B) ---
chunk = buf_c.read_chunks(4, 0, 16, N // 4)
strided = np.frombuffer(chunk, dtype=np.float32)
ok(np.array_equal(strided, ref[0::4]), "read_chunks strided gather == ref[0::4]")

# --- read_chunks with a non-zero start offset: gather ref[2::4] ---
chunk2 = buf_c.read_chunks(4, 8, 16, len(ref[2::4]))
ok(np.array_equal(np.frombuffer(chunk2, dtype=np.float32), ref[2::4]), "read_chunks(start=8) gather == ref[2::4]")

# --- read_into a preallocated bytearray, verify byte-identical to read() ---
out = bytearray(a.nbytes)
buf_c.read_into(out)
ok(np.array_equal(np.frombuffer(bytes(out), dtype=np.float32), ref), "read_into contents == vadd reference")

# --- write_chunks: scatter 4 sentinels at stride, verify only those positions changed ---
buf_w = ctx.buffer(np.zeros(16, dtype=np.float32).tobytes())
scatter = np.array([100, 200, 300, 400], dtype=np.float32)
buf_w.write_chunks(scatter.tobytes(), 0, 8, 4)  # one float every 8 bytes (every 2nd element)
wfull = np.frombuffer(buf_w.read(), dtype=np.float32)
expect_w = np.zeros(16, dtype=np.float32); expect_w[[0, 2, 4, 6]] = scatter
ok(np.array_equal(wfull, expect_w), "write_chunks scatter: positions 0,2,4,6 == 100..400 else 0")

# --- write partial sub-range then read it back (offset write) ---
patch = np.full(64, 9.0, dtype=np.float32)
buf_c.write(patch.tobytes(), offset=0)
head = np.frombuffer(buf_c.read(size=256, offset=0), dtype=np.float32)
ok(np.array_equal(head, patch), "partial write readback == 9.0 * 64")

# --- clear buffer to zero and verify ---
buf_c.clear()
zeros = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.array_equal(zeros, np.zeros(N, dtype=np.float32)), "clear -> every element == 0")

# --- ctx.copy_buffer: copy buf_a(=2.0) into cleared buf_c, verify every element ---
ctx.copy_buffer(buf_c, buf_a)
copied = np.frombuffer(buf_c.read(), dtype=np.float32)
ok(np.array_equal(copied, a2), "copy_buffer: every element c == a2 (2.0)")

# --- external_buffer: wrap buf_c's GL object, read through the alias, assert same bytes ---
ext = ctx.external_buffer(buf_c.glo, buf_c.size)
ok(ext.size == buf_c.size and ext.glo == buf_c.glo, "external_buffer wraps glo/size")
ok(np.array_equal(np.frombuffer(ext.read(), dtype=np.float32), a2), "external_buffer read == underlying data")

# --- buffer.assign returns a (buffer, index) binding pair for scope binding ---
pair = buf_a.assign(0)
ok(isinstance(pair, tuple) and pair[0] is buf_a and pair[1] == 0, "buffer.assign(0) -> (buffer, 0)")

# --- zero-size dispatch: prefill sentinel, run 0 groups, assert output untouched ---
sentinel = np.full(N, 7.0, dtype=np.float32)
buf_z = ctx.buffer(sentinel.tobytes()); buf_z.bind_to_storage_buffer(2)
buf_a.bind_to_storage_buffer(0); buf_b.bind_to_storage_buffer(1)
cs.run(group_x=0)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
zres = np.frombuffer(buf_z.read(), dtype=np.float32)
ok(np.array_equal(zres, sentinel), "zero-size dispatch leaves output sentinel unchanged")

# --- large case N >= 1<<20: barrier + element-wise verify all 1,048,576 outputs ---
BIG = 1 << 20
BG = (BIG + 63) // 64
big_a = np.arange(BIG, dtype=np.float32)
big_b = np.ones(BIG, dtype=np.float32)
bba = ctx.buffer(big_a.tobytes()); bbb = ctx.buffer(big_b.tobytes()); bbc = ctx.buffer(reserve=BIG * 4)
bba.bind_to_storage_buffer(0); bbb.bind_to_storage_buffer(1); bbc.bind_to_storage_buffer(2)
cs["alpha"] = 1.0; cs["n"] = BIG
cs.run(group_x=BG)
ctx.memory_barrier(ctx.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
big_res = np.frombuffer(bbc.read(), dtype=np.float32)
ok(big_res.shape[0] == BIG, "large dispatch readback length == 1<<20")
ok(np.array_equal(big_res, big_a + big_b), "large dispatch: all 1,048,576 elements c == a + b")

# --- validation / error paths (moderngl DOES raise on these; assert the specific exception) ---
small = ctx.buffer(reserve=256)
try:
    small.read(size=512)
    ok(False, "read past end should raise moderngl.Error")
except moderngl.Error:
    ok(True, "read(size>capacity) raises moderngl.Error")
try:
    small.read(size=64, offset=300)
    ok(False, "read with offset past end should raise moderngl.Error")
except moderngl.Error:
    ok(True, "read(offset past end) raises moderngl.Error")
try:
    ctx.copy_buffer(small, buf_a)  # dst 256B, but request whole src 4096B -> overflow
    ok(False, "copy_buffer overflow should raise moderngl.Error")
except moderngl.Error:
    ok(True, "copy_buffer(dst too small) raises moderngl.Error")

# --- clear_errors + clean error surface after the provoked failures ---
ctx.clear_errors()
ok(ctx.error == "GL_NO_ERROR", "ctx.error == GL_NO_ERROR after clear_errors")

# --- cleanup / release (assert a released buffer is actually unusable, not just ok(True)) ---
buf_a.release()
try:
    buf_a.read()
    ok(False, "released buffer.read() should fail")
except Exception:
    ok(True, "released buffer.read() raises (release took effect)")
for x in (buf_b, buf_c, buf_o, buf_ubo, ubo, buf_w, buf_z, bba, bbb, bbc, small, indirect):
    x.release()
# a released compute shader must reject run() too
cs.release()
try:
    cs.run(group_x=1)
    ok(False, "released compute_shader.run() should fail")
except Exception:
    ok(True, "released compute_shader.run() raises (release took effect)")
mul.release(); md.release(); ubo_cs.release()

EXPECTED = 60
TOTAL = P[0] + F[0]
print("gles-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], TOTAL, EXPECTED))
if F[0] == 0 and TOTAL == EXPECTED:
    print("GLES_PY_FULL_API OK %d" % P[0]); sys.exit(0)
print("GLES_PY_FULL_API FAIL"); sys.exit(1)
