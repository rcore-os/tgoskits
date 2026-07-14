#!/usr/bin/env python3
# opengl_moderngl_full_api.py - desktop-GL Python compute API carpet through moderngl on
# OSMesa/llvmpipe (GL 4.5 core, standalone/off-screen context; glDispatchCompute needs GL 4.3+).
# moderngl wraps the same GL 4.3 compute surface the PyOpenGL cell drives, but through its object
# model: Context.compute_shader / Buffer / Buffer.bind_to_storage_buffer / ComputeShader.run /
# ComputeShader.run_indirect / Context.memory_barrier / Buffer.read / Buffer.read_into /
# Buffer.write / Buffer.clear / Buffer.orphan / Context.copy_buffer / Context.query(time=...) /
# Context.error / Context.finish. Every method exercised here maps to a documented moderngl API
# entry (moderngl.readthedocs.io Context / Buffer / ComputeShader reference). The carpet asserts
# vadd / saxpy (incl. alpha=0) / mul / shared-memory reduction results per element against numpy,
# drives compile-error and GL-error paths, checks boundary sizes (zero-size dispatch, a >=1<<20
# grid verified element-wise) and carries negative controls proving the checker rejects wrong data.
# Prints "OPENGL_MODERNGL_FULL_API OK <n>" only when every assertion passes and the count equals
# EXPECTED.
#
# moderngl selects the GL backend through its standalone context: on a headless host it uses the
# EGL/OSMesa/GLX loader available; here LIBGL_ALWAYS_SOFTWARE=1 + GALLIUM_DRIVER=llvmpipe pin it to
# the Mesa llvmpipe CPU rasterizer, so no GPU is required. create_standalone_context(require=430)
# demands a >= 4.3 core context (compute shaders); moderngl raises if the driver cannot supply one.
import os, sys
os.environ.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
os.environ.setdefault("GALLIUM_DRIVER", "llvmpipe")

import numpy as np
import moderngl

P = [0]; F = [0]
def ok(c, d):
    if c: P[0] += 1
    else: F[0] += 1; sys.stderr.write("FAIL: %s\n" % d)

EXPECTED = 48
N = 1024

# saxpy + mul compute shader: mode 0 -> c = alpha*a + b, mode 1 -> c = a*b, with an i<n tail guard.
CS = """#version 430
layout(local_size_x=64) in;
layout(std430,binding=0) readonly buffer A { float a[]; };
layout(std430,binding=1) readonly buffer B { float b[]; };
layout(std430,binding=2) writeonly buffer C { float c[]; };
uniform float alpha; uniform uint n; uniform uint mode;
void main(){
  uint i = gl_GlobalInvocationID.x;
  if (i >= n) return;
  if (mode == 0u) c[i] = alpha*a[i] + b[i];
  else            c[i] = a[i] * b[i];
}
"""

# per-workgroup shared-memory tree reduction: out[wg] = sum(in[wg*256 .. wg*256+255]).
RED = """#version 430
layout(local_size_x=256) in;
layout(std430,binding=0) readonly buffer In { float src[]; };
layout(std430,binding=1) writeonly buffer Out { float dst[]; };
shared float s[256];
void main(){
  uint lid = gl_LocalInvocationID.x; uint gid = gl_GlobalInvocationID.x;
  s[lid] = src[gid]; barrier();
  for (uint stride = 128u; stride > 0u; stride >>= 1u){ if (lid < stride) s[lid] += s[lid+stride]; barrier(); }
  if (lid == 0u) dst[gl_WorkGroupID.x] = s[0];
}
"""

# a syntactically broken compute shader used to exercise the compile-failure path.
CS_BAD = """#version 430
layout(local_size_x=64) in;
void main(){ this is not valid glsl @@@ ;;; }
"""

def die():
    total = P[0] + F[0]
    print("opengl-moderngl: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], total, EXPECTED))
    sys.stdout.flush()
    if F[0] == 0 and total == EXPECTED:
        print("OPENGL_MODERNGL_FULL_API OK %d" % P[0]); sys.stdout.flush(); os._exit(0)
    print("OPENGL_MODERNGL_FULL_API FAIL"); sys.stdout.flush(); os._exit(1)

def npf(x): return np.asarray(x, dtype=np.float32)

def read_f32(buf, n):
    return np.frombuffer(buf.read(size=n * 4), dtype=np.float32).copy()

# --- standalone (off-screen) GL 4.3+ core context via moderngl ---
try:
    ctx = moderngl.create_standalone_context(require=430)
except Exception as e:
    sys.stderr.write("create_standalone_context failed: %s\n" % e); die()
ok(ctx is not None, "moderngl.create_standalone_context(require=430)")
ok(ctx.version_code >= 430, "Context.version_code >= 430 (%d)" % ctx.version_code)

# --- context introspection: renderer / vendor / GL version strings + compute limits ---
info = ctx.info
ok(info["GL_VERSION"] != "", "Context.info GL_VERSION non-empty")
ok("OpenGL ES" not in info["GL_VERSION"], "context is desktop GL (not ES)")
rnd = info["GL_RENDERER"]
ok(("llvmpipe" in rnd) or ("softpipe" in rnd) or ("SWR" in rnd), "GL_RENDERER is a software rasterizer (%s)" % rnd)
ok(info["GL_VENDOR"] != "", "Context.info GL_VENDOR non-empty")
ok(int(info["GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS"]) >= 256, "GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 256")
ok(int(info["GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS"]) >= 3, "GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3")
ok(int(info["GL_MAX_SHADER_STORAGE_BLOCK_SIZE"]) >= N * 4, "GL_MAX_SHADER_STORAGE_BLOCK_SIZE >= buffer bytes")
ok(int(info["GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS"]) >= 3, "GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS >= 3")
wgc = info["GL_MAX_COMPUTE_WORK_GROUP_COUNT"]
ok(int(wgc[0]) >= 65535, "GL_MAX_COMPUTE_WORK_GROUP_COUNT[0] >= 65535")

# --- compile compute shader (happy path) + uniform reflection ---
cs = ctx.compute_shader(CS)
ok(cs is not None, "Context.compute_shader compiles+links")
ok(cs.glo != 0, "ComputeShader.glo (GL program object) != 0")
ok("alpha" in cs and "n" in cs and "mode" in cs, "ComputeShader membership: alpha/n/mode uniforms present")
ok(cs.get("nope", None) is None, "ComputeShader.get(missing) returns default (no such uniform)")

# --- compile-error path: broken GLSL must raise moderngl.Error ---
raised = None
try:
    ctx.compute_shader(CS_BAD)
except moderngl.Error as e:
    raised = str(e)
ok(raised is not None and len(raised) > 0, "broken compute shader raises moderngl.Error with a non-empty log")

# --- SSBO buffers via Context.buffer + Buffer.write ---
a = np.arange(N, dtype=np.float32)
b = (2.0 * np.arange(N) + 1.0).astype(np.float32)
buf_a = ctx.buffer(a.tobytes())
buf_b = ctx.buffer(b.tobytes())
buf_c = ctx.buffer(reserve=N * 4)
ok(buf_a.size == N * 4 and buf_c.size == N * 4, "Buffer.size == N*4 for created SSBOs")
ok(np.array_equal(read_f32(buf_a, N), a), "Buffer.read round-trips uploaded A")

# Buffer.read_into: copy device data into a host bytearray without allocating a new buffer object
dst = bytearray(N * 4)
buf_a.read_into(dst)
ok(np.array_equal(np.frombuffer(bytes(dst), dtype=np.float32), a), "Buffer.read_into fills host buffer == A")

# bind SSBOs to std430 binding points 0/1/2 (matches shader layout(binding=))
buf_a.bind_to_storage_buffer(0)
buf_b.bind_to_storage_buffer(1)
buf_c.bind_to_storage_buffer(2)
ok(ctx.error == "GL_NO_ERROR", "bind_to_storage_buffer x3 leaves GL_NO_ERROR")

# --- vadd: alpha=1, mode=0 ---
cs["alpha"].value = 1.0
cs["n"].value = N
cs["mode"].value = 0
ok(abs(cs["alpha"].value - 1.0) < 1e-6, "Uniform read-back alpha == 1.0")
cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
vadd = read_f32(buf_c, N)
ok(np.array_equal(vadd, a + b), "vadd == a+b (numpy per-element)")
ok(not np.array_equal(vadd, a * a + b), "NEG vadd: checker rejects a*a+b reference")

# --- saxpy: alpha=3 ---
cs["alpha"].value = 3.0
cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
saxpy = read_f32(buf_c, N)
ok(np.array_equal(saxpy, 3.0 * a + b), "saxpy == 3*a+b (numpy per-element)")
ok(not np.array_equal(saxpy, 3.0 * a + b + 1.0), "NEG saxpy: checker rejects off-by-one reference")

# --- saxpy alpha=0: degenerates to c = b, an explicit alpha=0 operator case ---
cs["alpha"].value = 0.0
cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
saxpy0 = read_f32(buf_c, N)
ok(np.array_equal(saxpy0, b), "saxpy alpha=0 == b (numpy per-element)")

# --- mul: mode=1 ---
cs["mode"].value = 1
cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
mul = read_f32(buf_c, N)
ok(np.array_equal(mul, a * b), "mul == a*b (numpy per-element)")

# --- Buffer.write partial update A<-2, then vadd determinism ---
a2 = np.full(N, 2.0, dtype=np.float32)
buf_a.write(a2.tobytes())
ok(np.array_equal(read_f32(buf_a, N), a2), "Buffer.write A<-2 readback")
cs["alpha"].value = 1.0; cs["mode"].value = 0
cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_f32(buf_c, N), a2 + b), "vadd after Buffer.write == 2+b (numpy per-element)")

# --- Context.copy_buffer buf_a(=2) -> buf_c, verify element-wise ---
ctx.copy_buffer(buf_c, buf_a)
ok(np.array_equal(read_f32(buf_c, N), a2), "Context.copy_buffer buf_a->buf_c == 2 (numpy per-element)")

# --- Buffer.clear fills buf_c with zeros (byte clear) ---
buf_c.clear()
ok(np.array_equal(read_f32(buf_c, N), np.zeros(N, dtype=np.float32)), "Buffer.clear zeroes buffer (numpy per-element)")

# --- Buffer.orphan reallocates storage of a new size ---
buf_c.orphan(size=N * 4)
ok(buf_c.size == N * 4, "Buffer.orphan keeps requested size")
buf_c.bind_to_storage_buffer(2)

# --- timer query around a dispatch: Query.elapsed becomes a non-negative nanosecond count ---
q = ctx.query(time=True)
with q:
    cs.run(group_x=(N + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
ok(isinstance(q.elapsed, int) and q.elapsed >= 0, "Context.query(time=True) Query.elapsed >= 0 nanoseconds")

# --- ComputeShader.run_indirect: dispatch args sourced from a buffer ---
import struct
buf_c.clear()
ind = ctx.buffer(struct.pack("III", (N + 63) // 64, 1, 1))
cs["alpha"].value = 5.0; cs["mode"].value = 0
cs.run_indirect(ind, offset=0)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT | moderngl.BUFFER_UPDATE_BARRIER_BIT)
ctx.finish()
indirect = read_f32(buf_c, N)
ok(np.array_equal(indirect, 5.0 * a2 + b), "run_indirect saxpy == 5*a+b (numpy per-element)")

# --- shared-memory reduction: out[wg] = sum(block of 256) ---
RN = 4096; RG = RN // 256
rin = (np.arange(RN) % 13).astype(np.float32)
red = ctx.compute_shader(RED)
ok(red is not None, "reduction ComputeShader compiles")
rbuf_in = ctx.buffer(rin.tobytes())
rbuf_out = ctx.buffer(reserve=RG * 4)
rbuf_in.bind_to_storage_buffer(0)
rbuf_out.bind_to_storage_buffer(1)
red.run(group_x=RG)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
partial = read_f32(rbuf_out, RG)
expected_block = rin.reshape(RG, 256).sum(axis=1)
ok(np.allclose(partial, expected_block, rtol=1e-3, atol=1e-3), "reduction per-block partial == numpy block sum")
ok(abs(float(partial.sum()) - float(rin.sum())) <= 1e-3 * (1.0 + float(rin.sum())), "reduction total == numpy total")
ok(not np.array_equal(partial, np.zeros(RG, dtype=np.float32)), "NEG reduction: checker rejects zero reference")

# --- boundary: zero-size dispatch is a no-op (buffer keeps its sentinel) ---
sentinel = np.full(N, -1.0, dtype=np.float32)
buf_c.write(sentinel.tobytes())
cs["n"].value = 0
cs.run(group_x=0)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
ok(np.array_equal(read_f32(buf_c, N), sentinel), "zero-size dispatch leaves buffer unchanged (sentinel)")
cs["n"].value = N

# --- boundary: large >= 1,000,000-element dispatch verified element-wise vs numpy ---
BIG = 1 << 20  # 1,048,576 > 1e6
big_a = (np.arange(BIG, dtype=np.float32) % 1000.0).astype(np.float32)
big_b = ((np.arange(BIG, dtype=np.float32) * 2.0) % 997.0).astype(np.float32)
gbuf_a = ctx.buffer(big_a.tobytes())
gbuf_b = ctx.buffer(big_b.tobytes())
gbuf_c = ctx.buffer(reserve=BIG * 4)
gbuf_a.bind_to_storage_buffer(0)
gbuf_b.bind_to_storage_buffer(1)
gbuf_c.bind_to_storage_buffer(2)
cs["alpha"].value = 1.0; cs["mode"].value = 0; cs["n"].value = BIG
cs.run(group_x=(BIG + 63) // 64)  # 16384 workgroups -> multi-workgroup tiling
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
big_got = read_f32(gbuf_c, BIG)
ok(ctx.error == "GL_NO_ERROR", "large 1M dispatch leaves GL_NO_ERROR")
ok(np.array_equal(big_got, big_a + big_b), "1M-element vadd == a+b (numpy per-element, 1048576 elems)")

# --- oversubscription: dispatch the FULL 1M grid with n=BIG/2, guard leaves upper half untouched ---
half = BIG // 2
half_sentinel = np.full(BIG, -7.0, dtype=np.float32)
gbuf_c.write(half_sentinel.tobytes())
cs["n"].value = half
cs.run(group_x=(BIG + 63) // 64)
ctx.memory_barrier(moderngl.SHADER_STORAGE_BARRIER_BIT)
ctx.finish()
os_got = read_f32(gbuf_c, BIG)
ok(np.array_equal(os_got[:half], (big_a + big_b)[:half]), "oversubscription: lower half computed correctly")
ok(np.array_equal(os_got[half:], np.full(BIG - half, -7.0, dtype=np.float32)),
   "oversubscription: i>=n guard leaves upper half untouched")

# --- negative control: a single corrupted output element is caught vs numpy ---
corrupt = big_got.copy(); corrupt[123456] += 1.0
ok(not np.array_equal(corrupt, big_a + big_b), "NEG: single corrupted element flagged vs numpy reference")
ok(not np.array_equal(big_got, big_a * big_b), "NEG: vadd output != a*b (wrong operator caught)")

# --- error path: Context.error reports GL state; clear_errors resets it ---
ok(ctx.error == "GL_NO_ERROR", "Context.error == GL_NO_ERROR on the clean path")
ctx.clear_errors()
ok(ctx.error == "GL_NO_ERROR", "Context.error still clean after Context.clear_errors")

# --- release device objects (Buffer.release / ComputeShader.release) ---
for buf in (buf_a, buf_b, buf_c, rbuf_in, rbuf_out, gbuf_a, gbuf_b, gbuf_c, ind):
    buf.release()
cs.release(); red.release()
ok(ctx.error == "GL_NO_ERROR", "release all objects leaves GL_NO_ERROR")
ctx.release()

die()
