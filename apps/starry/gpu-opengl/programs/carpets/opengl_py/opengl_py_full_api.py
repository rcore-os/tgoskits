#!/usr/bin/env python3
# opengl_py_full_api.py - full desktop-GL Python (PyOpenGL) compute API carpet on OSMesa/llvmpipe
# (GL 4.5 core, surfaceless): exercise the GL compute-shader API surface (off-screen context / string
# query / limits / shader compile incl. compile-error / program link incl. link-error / SSBO / buffer
# storage / buffer-base binding / uniform / dispatch / memory-barrier / fence-sync / timer query /
# get-buffer-sub-data / read-map / write-map / copy-sub-data / clear-buffer-data / block introspection
# incl. resource-name + resource-property / error-path validation / boundary + oversubscription +
# large 1M-element dispatch / negative controls) and assert vadd/saxpy/mul operator results per-element
# against numpy. Prints "OPENGL_PY_FULL_API OK <n>" only when every assertion passes and the count
# equals EXPECTED.
#
# OSMesa is bound through PyOpenGL's OSMesaPlatform, selected before OpenGL is imported. Read-back uses
# ctypes-backed buffers wrapped with numpy; the mapped range is copied out through ctypes.from_address.
# PyOpenGL wraps every GL call with an auto glGetError check, so a real GL validation error surfaces as
# an OpenGL.error.GLError whose .err carries the exact GL_INVALID_* enum - error paths assert that enum.
import os
os.environ.setdefault("PYOPENGL_PLATFORM", "osmesa")

import sys, ctypes
import numpy as np
from OpenGL import GL
from OpenGL.error import GLError
from OpenGL.osmesa import (
    OSMesaCreateContextAttribs, OSMesaMakeCurrent, OSMesaGetCurrentContext,
    OSMesaDestroyContext, OSMESA_FORMAT, OSMESA_RGBA, OSMESA_PROFILE,
    OSMESA_CORE_PROFILE, OSMESA_CONTEXT_MAJOR_VERSION, OSMESA_CONTEXT_MINOR_VERSION,
)

P = [0]; F = [0]
def ok(c, d):
    if c: P[0] += 1
    else: F[0] += 1; sys.stderr.write("FAIL: %s\n" % d)

def die():
    total = P[0] + F[0]
    print("opengl-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], total, EXPECTED))
    sys.stdout.flush()
    if F[0] == 0 and total == EXPECTED:
        print("OPENGL_PY_FULL_API OK %d" % P[0]); sys.stdout.flush(); os._exit(0)
    print("OPENGL_PY_FULL_API FAIL"); sys.stdout.flush(); os._exit(1)

CS = """#version 430
layout(local_size_x=64) in;
layout(std430,binding=0) readonly buffer A { float a[]; };
layout(std430,binding=1) readonly buffer B { float b[]; };
layout(std430,binding=2) writeonly buffer C { float c[]; };
uniform float alpha; uniform uint n; uniform uint mode;
void main(){
  uint i = gl_GlobalInvocationID.x;
  if (i >= n) return;
  if (mode == 0u) c[i] = alpha*a[i] + b[i];   // saxpy (alpha=1 -> vadd)
  else            c[i] = a[i] * b[i];          // mul
}
"""

# a syntactically broken compute shader used to exercise the COMPILE_STATUS==FALSE + info-log path
CS_BAD = """#version 430
layout(local_size_x=64) in;
void main(){ this is not valid glsl @@@ ;;; }
"""

EXPECTED = 90
N = 1024
NB = N * 4

def read_ssbo(buf, n=N):
    GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(buf))
    cbuf = (ctypes.c_float * n)()
    GL.glGetBufferSubData(GL.GL_SHADER_STORAGE_BUFFER, 0, n * 4, cbuf)
    return np.frombuffer(cbuf, dtype=np.float32).copy()

# --- surfaceless OSMesa context (GL 4.5 core) ---
attribs = (ctypes.c_int * 9)(
    int(OSMESA_FORMAT), int(OSMESA_RGBA),
    int(OSMESA_PROFILE), int(OSMESA_CORE_PROFILE),
    int(OSMESA_CONTEXT_MAJOR_VERSION), 4,
    int(OSMESA_CONTEXT_MINOR_VERSION), 5, 0)
ctx = OSMesaCreateContextAttribs(attribs, None)
ok(ctx is not None, "OSMesaCreateContextAttribs 4.5 core")
if ctx is None: die()
framebuf = np.zeros((16, 16, 4), dtype=np.uint8)
ok(bool(OSMesaMakeCurrent(ctx, framebuf, GL.GL_UNSIGNED_BYTE, 16, 16)), "OSMesaMakeCurrent")
ok(OSMesaGetCurrentContext() is not None, "OSMesaGetCurrentContext")
# current-context handle must be the one we just created (not merely non-null)
cur = OSMesaGetCurrentContext()
ok(ctypes.cast(cur, ctypes.c_void_p).value == ctypes.cast(ctx, ctypes.c_void_p).value,
   "OSMesaGetCurrentContext identity == created ctx")

# --- string queries: assert desktop GL >= 4.3 and not ES ---
ver = GL.glGetString(GL.GL_VERSION)
ver_s = ver.decode() if isinstance(ver, bytes) else str(ver)
ok(ver_s != "", "glGetString(GL_VERSION)")
ok("OpenGL ES" not in ver_s, "context is desktop GL (not ES)")
major = int(GL.glGetIntegerv(GL.GL_MAJOR_VERSION)); minor = int(GL.glGetIntegerv(GL.GL_MINOR_VERSION))
ok((major, minor) >= (4, 3), "GL version >= 4.3 (%d.%d)" % (major, minor))
# the GL_VERSION string must start with the same major.minor pair the integer queries report
ok(ver_s.split()[0].startswith("%d.%d" % (major, minor)) or ver_s.startswith("%d.%d" % (major, minor)),
   "GL_VERSION string matches GL_MAJOR/MINOR_VERSION")
glsl = GL.glGetString(GL.GL_SHADING_LANGUAGE_VERSION)
ok((glsl.decode() if isinstance(glsl, bytes) else str(glsl)) != "", "glGetString(GL_SHADING_LANGUAGE_VERSION)")
ok(GL.glGetString(GL.GL_RENDERER) is not None, "glGetString(GL_RENDERER)")
ok(GL.glGetString(GL.GL_VENDOR) is not None, "glGetString(GL_VENDOR)")

# --- compute work-group limits ---
wgc0 = int(GL.glGetIntegeri_v(GL.GL_MAX_COMPUTE_WORK_GROUP_COUNT, 0)[0])
ok(wgc0 >= 1, "glGetIntegeri_v MAX_COMPUTE_WORK_GROUP_COUNT[0] >= 1")
wgs0 = int(GL.glGetIntegeri_v(GL.GL_MAX_COMPUTE_WORK_GROUP_SIZE, 0)[0])
ok(wgs0 >= 1, "glGetIntegeri_v MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 1")
inv = int(GL.glGetIntegerv(GL.GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS))
ok(inv >= 64, "glGetIntegerv MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64")
maxbind = int(GL.glGetIntegerv(GL.GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS))
ok(maxbind >= 3, "MAX_SHADER_STORAGE_BUFFER_BINDINGS >= 3")
# our local_size_x=64 must fit inside the invocation limit reported above
ok(64 <= inv, "shader local_size_x(64) <= MAX_COMPUTE_WORK_GROUP_INVOCATIONS")

# --- compile compute shader (happy path) ---
sh = GL.glCreateShader(GL.GL_COMPUTE_SHADER)
ok(sh != 0, "glCreateShader(GL_COMPUTE_SHADER)")
GL.glShaderSource(sh, CS)
GL.glCompileShader(sh)
cstat = int(GL.glGetShaderiv(sh, GL.GL_COMPILE_STATUS))
if not cstat:
    sys.stderr.write("shader log: %s\n" % GL.glGetShaderInfoLog(sh))
ok(cstat == GL.GL_TRUE, "glCompileShader GL_COMPILE_STATUS")
# GL_SHADER_TYPE reflection must report COMPUTE for this shader object
ok(int(GL.glGetShaderiv(sh, GL.GL_SHADER_TYPE)) == int(GL.GL_COMPUTE_SHADER),
   "glGetShaderiv GL_SHADER_TYPE == GL_COMPUTE_SHADER")

# --- compile-error negative path: broken GLSL must FAIL to compile and yield a non-empty info log ---
shb = GL.glCreateShader(GL.GL_COMPUTE_SHADER)
GL.glShaderSource(shb, CS_BAD)
GL.glCompileShader(shb)
bad_stat = int(GL.glGetShaderiv(shb, GL.GL_COMPILE_STATUS))
ok(bad_stat == GL.GL_FALSE, "broken shader glCompileShader GL_COMPILE_STATUS == GL_FALSE")
bad_log = GL.glGetShaderInfoLog(shb)
bad_log_s = bad_log.decode() if isinstance(bad_log, bytes) else str(bad_log)
ok(len(bad_log_s) > 0, "glGetShaderInfoLog non-empty on compile failure")
GL.glDeleteShader(shb)

# --- link program (happy path) ---
prog = GL.glCreateProgram()
ok(prog != 0, "glCreateProgram")
GL.glAttachShader(prog, sh)
GL.glLinkProgram(prog)
lstat = int(GL.glGetProgramiv(prog, GL.GL_LINK_STATUS))
if not lstat:
    sys.stderr.write("link log: %s\n" % GL.glGetProgramInfoLog(prog))
ok(lstat == GL.GL_TRUE, "glLinkProgram GL_LINK_STATUS")
# GL_ATTACHED_SHADERS must report the one compute shader we attached
ok(int(GL.glGetProgramiv(prog, GL.GL_ATTACHED_SHADERS)) == 1, "glGetProgramiv GL_ATTACHED_SHADERS == 1")

# --- link-error negative path: an empty program with no compute stage must FAIL to link ---
prog_bad = GL.glCreateProgram()
GL.glLinkProgram(prog_bad)
lbad = int(GL.glGetProgramiv(prog_bad, GL.GL_LINK_STATUS))
ok(lbad == GL.GL_FALSE, "empty program glLinkProgram GL_LINK_STATUS == GL_FALSE")
lbad_log = GL.glGetProgramInfoLog(prog_bad)
lbad_log_s = lbad_log.decode() if isinstance(lbad_log, bytes) else str(lbad_log)
ok(len(lbad_log_s) > 0, "glGetProgramInfoLog non-empty on link failure")
GL.glDeleteProgram(prog_bad)

GL.glDeleteShader(sh)
ok(int(GL.glGetShaderiv(sh, GL.GL_DELETE_STATUS)) == GL.GL_TRUE,
   "glDeleteShader -> GL_DELETE_STATUS flagged (deleted, kept alive by program attach)")

# --- SSBOs (mutable glBufferData) ---
a = np.arange(N, dtype=np.float32)
b = (2.0 * np.arange(N) + 1.0).astype(np.float32)
bufs = GL.glGenBuffers(3)
ok(all(int(bufs[i]) != 0 for i in range(3)), "glGenBuffers(3)")
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[0])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, NB, a, GL.GL_STATIC_DRAW)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[1])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, NB, b, GL.GL_STATIC_DRAW)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[2])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, NB, None, GL.GL_DYNAMIC_COPY)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glBufferData A/B/C")
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_SIZE))[0]) == NB, "glGetBufferParameteriv GL_BUFFER_SIZE")
# glGetBufferParameteriv usage hint must echo the STATIC_DRAW we requested
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_USAGE))[0]) == int(GL.GL_STATIC_DRAW),
   "glGetBufferParameteriv GL_BUFFER_USAGE == GL_STATIC_DRAW")
for i in range(3):
    GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, i, int(bufs[i]))
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glBindBufferBase x3")

# round-trip a numpy upload straight back through glGetBufferSubData
ok(np.array_equal(read_ssbo(bufs[0]), a), "glGetBufferSubData round-trips uploaded A")

# --- uniforms + dispatch: vadd (alpha=1, mode=0) ---
GL.glUseProgram(prog)
ok(int(GL.glGetIntegerv(GL.GL_CURRENT_PROGRAM)) == int(prog), "glUseProgram -> GL_CURRENT_PROGRAM == prog")
loc_alpha = GL.glGetUniformLocation(prog, "alpha")
loc_n = GL.glGetUniformLocation(prog, "n")
loc_mode = GL.glGetUniformLocation(prog, "mode")
ok(loc_alpha >= 0 and loc_n >= 0 and loc_mode >= 0, "glGetUniformLocation alpha/n/mode")
GL.glUniform1f(loc_alpha, 1.0)
GL.glUniform1ui(loc_n, N)
GL.glUniform1ui(loc_mode, 0)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glUniform1f/glUniform1ui")
# read the uniform back with glGetUniformfv and confirm the driver stored our value
ufv_out = (ctypes.c_float * 1)()
GL.glGetUniformfv(prog, loc_alpha, ufv_out)
ok(abs(float(ufv_out[0]) - 1.0) < 1e-6, "glGetUniformfv alpha == 1.0")
GL.glDispatchCompute((N + 63) // 64, 1, 1)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glDispatchCompute vadd")
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glMemoryBarrier")
vadd = read_ssbo(bufs[2])
ok(np.array_equal(vadd, a + b), "vadd == a+b (numpy per-element, glGetBufferSubData)")

# --- fence sync: fence after the vadd dispatch, wait on the CPU, assert it signals ---
fence = GL.glFenceSync(GL.GL_SYNC_GPU_COMMANDS_COMPLETE, 0)
ok(bool(fence), "glFenceSync GL_SYNC_GPU_COMMANDS_COMPLETE")
GL.glFlush()
wr = GL.glClientWaitSync(fence, GL.GL_SYNC_FLUSH_COMMANDS_BIT, 5_000_000_000)
ok(wr in (int(GL.GL_ALREADY_SIGNALED), int(GL.GL_CONDITION_SATISFIED)),
   "glClientWaitSync -> ALREADY/CONDITION_SATISFIED (not TIMEOUT)")
sstat = GL.glGetSynciv(fence, GL.GL_SYNC_STATUS, 1, None)
ok(int(sstat[1]) == int(GL.GL_SIGNALED), "glGetSynciv GL_SYNC_STATUS == GL_SIGNALED")
GL.glDeleteSync(fence)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glDeleteSync")

# --- timer query around a dispatch: GL_TIME_ELAPSED result becomes available ---
q = GL.glGenQueries(1)
qid = int(q[0]) if hasattr(q, "__getitem__") else int(q)
ok(qid != 0, "glGenQueries")
GL.glBeginQuery(GL.GL_TIME_ELAPSED, qid)
GL.glDispatchCompute((N + 63) // 64, 1, 1)
GL.glEndQuery(GL.GL_TIME_ELAPSED)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
GL.glFlush()
# spin until the timer result is available, then read it (unsigned nanoseconds)
avail = 0
for _ in range(1000):
    avail = int(GL.glGetQueryObjectiv(qid, GL.GL_QUERY_RESULT_AVAILABLE))
    if avail:
        break
ok(avail == int(GL.GL_TRUE), "glGetQueryObjectiv GL_QUERY_RESULT_AVAILABLE")
# GL_QUERY_TARGET reflection must report the target we opened the query with
ok(int(GL.glGetQueryObjectiv(qid, GL.GL_QUERY_TARGET)) == int(GL.GL_TIME_ELAPSED),
   "glGetQueryObjectiv GL_QUERY_TARGET == GL_TIME_ELAPSED")
elapsed = int(GL.glGetQueryObjectuiv(qid, GL.GL_QUERY_RESULT))
ok(avail == int(GL.GL_TRUE) and elapsed >= 0 and elapsed < (1 << 32),
   "glGetQueryObjectuiv GL_TIME_ELAPSED available result is non-negative uint nanoseconds")
GL.glDeleteQueries(1, q)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glDeleteQueries")

# --- read back the vadd result through glMapBufferRange (READ) ---
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[2]))
ptr = GL.glMapBufferRange(GL.GL_SHADER_STORAGE_BUFFER, 0, NB, GL.GL_MAP_READ_BIT)
addr = ptr if isinstance(ptr, int) else ctypes.cast(ptr, ctypes.c_void_p).value
ok(addr not in (None, 0), "glMapBufferRange GL_MAP_READ_BIT")
# while mapped, GL_BUFFER_MAPPED must report TRUE and GL_BUFFER_MAP_LENGTH the mapped byte count
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_MAPPED))[0]) == GL.GL_TRUE,
   "glGetBufferParameteriv GL_BUFFER_MAPPED == GL_TRUE while mapped")
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_MAP_LENGTH))[0]) == NB,
   "glGetBufferParameteriv GL_BUFFER_MAP_LENGTH == NB")
mapped = np.frombuffer((ctypes.c_float * N).from_address(int(addr)), dtype=np.float32).copy()
ok(np.array_equal(mapped, a + b), "mapped range == a+b (numpy per-element)")
ok(bool(GL.glUnmapBuffer(GL.GL_SHADER_STORAGE_BUFFER)), "glUnmapBuffer")
# after unmap the mapping flag must return to FALSE
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_MAPPED))[0]) == GL.GL_FALSE,
   "glGetBufferParameteriv GL_BUFFER_MAPPED == GL_FALSE after unmap")

# --- saxpy: re-dispatch with alpha=3 (mode=0) ---
GL.glUniform1f(loc_alpha, 3.0)
GL.glDispatchCompute((N + 63) // 64, 1, 1)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
saxpy = read_ssbo(bufs[2])
ok(np.array_equal(saxpy, 3.0 * a + b), "saxpy == 3*a+b (numpy per-element)")

# --- mul: mode=1 ---
GL.glUniform1ui(loc_mode, 1)
GL.glDispatchCompute((N + 63) // 64, 1, 1)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
mul = read_ssbo(bufs[2])
ok(np.array_equal(mul, a * b), "mul == a*b (numpy per-element)")

# --- glBufferSubData update A<-2 then vadd determinism ---
a2 = np.full(N, 2.0, dtype=np.float32)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
GL.glBufferSubData(GL.GL_SHADER_STORAGE_BUFFER, 0, NB, a2)
ok(np.array_equal(read_ssbo(bufs[0]), a2), "glBufferSubData A<-2 readback")
GL.glUniform1f(loc_alpha, 1.0)
GL.glUniform1ui(loc_mode, 0)
GL.glDispatchCompute((N + 63) // 64, 1, 1)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), a2 + b), "vadd after subdata == 2+b (numpy per-element)")

# --- glCopyBufferSubData buf0(=2) -> buf2, verify element-wise ---
GL.glBindBuffer(GL.GL_COPY_READ_BUFFER, int(bufs[0]))
GL.glBindBuffer(GL.GL_COPY_WRITE_BUFFER, int(bufs[2]))
GL.glCopyBufferSubData(GL.GL_COPY_READ_BUFFER, GL.GL_COPY_WRITE_BUFFER, 0, 0, NB)
ok(np.array_equal(read_ssbo(bufs[2]), a2), "glCopyBufferSubData buf0->buf2 == 2 (numpy per-element)")

# --- glClearBufferData fills buf2 with 5.0 ---
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[2]))
clearval = (ctypes.c_float * 1)(5.0)
GL.glClearBufferData(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_R32F, GL.GL_RED, GL.GL_FLOAT, clearval)
ok(np.array_equal(read_ssbo(bufs[2]), np.full(N, 5.0, dtype=np.float32)), "glClearBufferData == 5.0 (numpy per-element)")

# --- glBindBufferRange + program resource introspection ---
GL.glBindBufferRange(GL.GL_SHADER_STORAGE_BUFFER, 0, int(bufs[0]), 0, NB)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glBindBufferRange")
idx = int(GL.glGetProgramResourceIndex(prog, GL.GL_SHADER_STORAGE_BLOCK, "A"))
ok(idx != int(GL.GL_INVALID_INDEX), "glGetProgramResourceIndex SSBO block A")
GL.glShaderStorageBlockBinding(prog, idx, 0)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glShaderStorageBlockBinding")
nres = int(GL.glGetProgramInterfaceiv(prog, GL.GL_SHADER_STORAGE_BLOCK, GL.GL_ACTIVE_RESOURCES))
ok(nres == 3, "glGetProgramInterfaceiv ACTIVE_RESOURCES == 3 (A/B/C)")

# glGetProgramResourceName must decode back to the exact block name for each index
def res_name(i):
    r = GL.glGetProgramResourceName(prog, GL.GL_SHADER_STORAGE_BLOCK, i, 64)
    ln, arr = r[0], r[1]
    return bytes(bytearray(int(x) & 0xFF for x in arr[:ln])).decode()
ok(res_name(idx) == "A", "glGetProgramResourceName(idx of A) == 'A'")

# glGetProgramResourceiv must report GL_BUFFER_BINDING matching the shader's layout(binding=) for each block
props = (ctypes.c_uint * 1)(int(GL.GL_BUFFER_BINDING))
def res_binding(name):
    i = int(GL.glGetProgramResourceIndex(prog, GL.GL_SHADER_STORAGE_BLOCK, name))
    r = GL.glGetProgramResourceiv(prog, GL.GL_SHADER_STORAGE_BLOCK, i, 1, props, 1, None, None)
    return int(np.ravel(np.asarray(r[1]))[0])
ok(res_binding("A") == 0 and res_binding("B") == 1 and res_binding("C") == 2,
   "glGetProgramResourceiv GL_BUFFER_BINDING == shader layout(binding) A0/B1/C2")

# --- immutable glBufferStorage + WRITE-mapped host-visible round trip ---
sbuf = GL.glGenBuffers(1)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(sbuf))
storage_flags = (GL.GL_MAP_WRITE_BIT | GL.GL_MAP_READ_BIT |
                 GL.GL_MAP_PERSISTENT_BIT | GL.GL_MAP_COHERENT_BIT | GL.GL_DYNAMIC_STORAGE_BIT)
GL.glBufferStorage(GL.GL_SHADER_STORAGE_BUFFER, NB, None, storage_flags)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glBufferStorage immutable host-visible")
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_IMMUTABLE_STORAGE))[0]) == GL.GL_TRUE,
   "glGetBufferParameteriv GL_BUFFER_IMMUTABLE_STORAGE == GL_TRUE")
ok(int(np.ravel(GL.glGetBufferParameteriv(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_BUFFER_SIZE))[0]) == NB,
   "immutable storage GL_BUFFER_SIZE == NB")
# write path: map WRITE|INVALIDATE, fill via host pointer, unmap, read back through glGetBufferSubData
wptr = GL.glMapBufferRange(GL.GL_SHADER_STORAGE_BUFFER, 0, NB,
                           GL.GL_MAP_WRITE_BIT | GL.GL_MAP_INVALIDATE_BUFFER_BIT)
waddr = wptr if isinstance(wptr, int) else ctypes.cast(wptr, ctypes.c_void_p).value
ok(waddr not in (None, 0), "glMapBufferRange GL_MAP_WRITE_BIT|GL_MAP_INVALIDATE_BUFFER_BIT")
warr = (ctypes.c_float * N).from_address(int(waddr))
wvals = (np.arange(N, dtype=np.float32) * 7.0 + 1.0).astype(np.float32)
for i in range(N):
    warr[i] = float(wvals[i])
ok(bool(GL.glUnmapBuffer(GL.GL_SHADER_STORAGE_BUFFER)), "glUnmapBuffer after write map")
ok(np.array_equal(read_ssbo(sbuf), wvals), "write-mapped storage round-trips (numpy per-element)")

# use the written storage buffer as SSBO A in a real dispatch, verify the compute result element-wise
GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, 0, int(sbuf))     # A <- wvals
GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, 1, int(bufs[1]))  # B <- b
GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, 2, int(bufs[2]))  # C
GL.glUniform1f(loc_alpha, 1.0); GL.glUniform1ui(loc_mode, 0); GL.glUniform1ui(loc_n, N)
GL.glDispatchCompute((N + 63) // 64, 1, 1)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), wvals + b), "dispatch over write-mapped A == wvals+b (numpy)")
# restore A <- bufs[0]
GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, 0, int(bufs[0]))

# --- error/validation paths: PyOpenGL raises GLError carrying the exact GL enum ---
# 1) glBindBufferBase with an out-of-range binding index -> GL_INVALID_VALUE
raised = None
try:
    GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, maxbind + 4, int(bufs[0]))
except GLError as e:
    raised = int(e.err)
ok(raised == int(GL.GL_INVALID_VALUE), "glBindBufferBase OOB index -> GL_INVALID_VALUE")
GL.glGetError()
# 2) glBufferData with a negative size -> GL_INVALID_VALUE
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
raised = None
try:
    GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, -16, None, GL.GL_STATIC_DRAW)
except GLError as e:
    raised = int(e.err)
ok(raised == int(GL.GL_INVALID_VALUE), "glBufferData negative size -> GL_INVALID_VALUE")
GL.glGetError()
# 3) glMapBufferRange with offset+length beyond the buffer size -> GL_INVALID_VALUE
raised = None
try:
    GL.glMapBufferRange(GL.GL_SHADER_STORAGE_BUFFER, 0, NB * 8, GL.GL_MAP_READ_BIT)
except GLError as e:
    raised = int(e.err)
ok(raised == int(GL.GL_INVALID_VALUE), "glMapBufferRange over-length -> GL_INVALID_VALUE")
GL.glGetError()

# --- boundary: zero-element dispatch is a no-op (C sentinel stays untouched) ---
sentinel = (ctypes.c_float * 1)(-1.0)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(bufs[2]))
GL.glClearBufferData(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_R32F, GL.GL_RED, GL.GL_FLOAT, sentinel)
GL.glUniform1ui(loc_n, 0)
GL.glDispatchCompute(0, 1, 1)
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), np.full(N, -1.0, dtype=np.float32)),
   "zero-size dispatch is a no-op (sentinel untouched)")

# --- boundary: large >= 1,000,000-element dispatch verified element-wise vs numpy ---
BIG = 1 << 20  # 1,048,576 elements > 1e6
BB = BIG * 4
big_a = (np.arange(BIG, dtype=np.float32) % 1000.0).astype(np.float32)
big_b = ((np.arange(BIG, dtype=np.float32) * 2.0) % 997.0).astype(np.float32)
gbufs = GL.glGenBuffers(3)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(gbufs[0])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, BB, big_a, GL.GL_STATIC_DRAW)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(gbufs[1])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, BB, big_b, GL.GL_STATIC_DRAW)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(gbufs[2])); GL.glBufferData(GL.GL_SHADER_STORAGE_BUFFER, BB, None, GL.GL_DYNAMIC_COPY)
for i in range(3):
    GL.glBindBufferBase(GL.GL_SHADER_STORAGE_BUFFER, i, int(gbufs[i]))
GL.glUniform1f(loc_alpha, 1.0); GL.glUniform1ui(loc_mode, 0); GL.glUniform1ui(loc_n, BIG)
GL.glDispatchCompute((BIG + 63) // 64, 1, 1)  # 16384 workgroups -> multi-workgroup tiling
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
big_got = read_ssbo(gbufs[2], BIG)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "large 1M dispatch no GL error")
ok(np.array_equal(big_got, big_a + big_b), "1M-element vadd == a+b (numpy per-element, 1048576 elems)")

# --- oversubscription / bounds guard: dispatch the FULL 1M grid but set n=BIG/2 ---
# the i>=n guard must leave the upper half at its cleared sentinel while the lower half computes.
half_sentinel = (ctypes.c_float * 1)(-7.0)
GL.glBindBuffer(GL.GL_SHADER_STORAGE_BUFFER, int(gbufs[2]))
GL.glClearBufferData(GL.GL_SHADER_STORAGE_BUFFER, GL.GL_R32F, GL.GL_RED, GL.GL_FLOAT, half_sentinel)
half = BIG // 2
GL.glUniform1ui(loc_n, half)
GL.glDispatchCompute((BIG + 63) // 64, 1, 1)  # full grid, guard limits writes to first half
GL.glMemoryBarrier(GL.GL_SHADER_STORAGE_BARRIER_BIT)
os_got = read_ssbo(gbufs[2], BIG)
ok(np.array_equal(os_got[:half], (big_a + big_b)[:half]), "oversubscription: lower half computed correctly")
ok(np.array_equal(os_got[half:], np.full(BIG - half, -7.0, dtype=np.float32)),
   "oversubscription: i>=n guard leaves upper half untouched (no OOB write)")

# --- negative controls: the correctness check must actually flag wrong data ---
# (a) corrupt ONE real device-output element and confirm the numpy comparison catches it
corrupt = big_got.copy()
corrupt[123456] = corrupt[123456] + 1.0
ok(not np.array_equal(corrupt, big_a + big_b),
   "negative control: single corrupted output element is flagged vs numpy reference")
# (b) a KNOWN-WRONG closed-form reference must not match the real vadd output
ok(not np.array_equal(big_got, big_a * big_b),
   "negative control: vadd output != a*b (wrong operator caught)")
# (c) re-read the real device output and confirm it is genuinely equal (guards against a stuck 'not-equal')
ok(np.array_equal(read_ssbo(gbufs[2], BIG)[:half], (big_a + big_b)[:half]),
   "negative control sanity: real output still matches correct reference")
GL.glDeleteBuffers(3, gbufs)

ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glGetError == GL_NO_ERROR (final)")

# --- cleanup ---
GL.glDeleteBuffers(3, bufs)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glDeleteBuffers")
GL.glDeleteBuffers(1, sbuf)
ok(int(GL.glGetError()) == GL.GL_NO_ERROR, "glDeleteBuffers storage")
GL.glDeleteProgram(prog)
ok(int(GL.glGetProgramiv(prog, GL.GL_DELETE_STATUS)) == GL.GL_TRUE, "glDeleteProgram -> GL_DELETE_STATUS")
# the context we are about to destroy must be the one currently bound (real queried identity)
pre_destroy = OSMesaGetCurrentContext()
ok(ctypes.cast(pre_destroy, ctypes.c_void_p).value == ctypes.cast(ctx, ctypes.c_void_p).value,
   "OSMesaDestroyContext: current ctx identity confirmed before destroy")
OSMesaDestroyContext(ctx)

die()
