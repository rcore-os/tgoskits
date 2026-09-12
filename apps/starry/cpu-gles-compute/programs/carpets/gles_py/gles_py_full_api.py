#!/usr/bin/env python3
# gles_py_full_api.py - full OpenGL ES 3.1 Python (PyOpenGL) COMPUTE API carpet on EGL-surfaceless /
# llvmpipe (moderngl is desktop-GL only and Alpine ships no py3-moderngl on any arch, so the GLES
# Python cell uses PyOpenGL's OpenGL.EGL + OpenGL.GLES3 exactly like the sibling render/gles cell and
# the desktop opengl_py compute cell). It creates a surfaceless EGL OpenGL ES 3.1 compute context
# (EGL_OPENGL_ES2_BIT config - ES3 configs advertise via the ES2 renderable bit - eglBindAPI
# EGL_OPENGL_ES_API, EGL_CONTEXT_MAJOR_VERSION=3 / MINOR=1) and exercises the GLES 3.1 compute-shader
# API surface (off-screen context / string query / limits / compute-shader compile incl. compile-error
# / program link incl. link-error / SSBO / buffer-base binding / uniform / dispatch / indirect dispatch
# / memory-barrier / fence-sync / map-read / map-write / copy-sub-data / block introspection incl.
# resource-name + resource-property / error-path validation / boundary + oversubscription + large
# 1M-element dispatch / negative controls) and asserts vadd/saxpy/mul operator results per-element
# against numpy. Prints "GLES_PY_FULL_API OK <n>" only when every assertion passes and the count equals
# EXPECTED. Runs on-target on every arch (mesa-gles + mesa-egl + PyOpenGL) the same way as gles_c.
# Software rasterizer (llvmpipe), single-threaded, deterministic - no GPU.
#
# The context is a surfaceless EGL OpenGL ES context (EGL_OPENGL_ES2_BIT / EGL_OPENGL_ES_API), selected
# via PYOPENGL_PLATFORM=egl before OpenGL is imported. GLES 3.1 core omits several desktop-GL buffer
# entry points, so this cell diverges from the desktop opengl_py exactly where the ES API differs and
# documents each drop inline:
#   - glGetBufferSubData: NOT in GLES. All SSBO read-back is via glMapBufferRange(GL_MAP_READ_BIT) +
#     ctypes.from_address (read_ssbo below).
#   - glBufferStorage / immutable storage: NOT in GLES 3.1. The write-map round-trip uses a mutable
#     glBufferData buffer mapped with GL_MAP_WRITE_BIT|GL_MAP_INVALIDATE_BUFFER_BIT instead.
#   - glClearBufferData / glClearBufferSubData: NOT in GLES. Buffers are cleared / sentinel-filled by
#     uploading a numpy fill through glBufferSubData.
#   - glShaderStorageBlockBinding: NOT in the GLES3 client entry set (would need eglGetProcAddress). The
#     shader's layout(binding=) is authoritative and verified via glGetProgramResourceiv GL_BUFFER_BINDING.
#   - GL_TIME_ELAPSED timer query: NOT core GLES 3.1 (only via GL_EXT_disjoint_timer_query). GPU/CPU
#     ordering is asserted with the core fence-sync path (glFenceSync/glClientWaitSync/glGetSynciv) instead.
# PyOpenGL wraps every GL call with an auto glGetError check, so a real GL validation error surfaces as
# an OpenGL.error.GLError whose .err carries the exact GL_INVALID_* enum - error paths assert that enum.
import os
os.environ.setdefault("PYOPENGL_PLATFORM", "egl")
os.environ.setdefault("EGL_PLATFORM", "surfaceless")
os.environ.setdefault("GALLIUM_DRIVER", "llvmpipe")
os.environ.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
os.environ.setdefault("LP_NUM_THREADS", "1")

import sys, ctypes
import numpy as np
import OpenGL.GLES3 as gl
from OpenGL import EGL
from OpenGL.error import GLError

P = [0]; F = [0]
def ok(c, d):
    if c: P[0] += 1
    else: F[0] += 1; sys.stderr.write("FAIL: %s\n" % d)

def die():
    total = P[0] + F[0]
    print("gles-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (P[0], F[0], total, EXPECTED))
    sys.stdout.flush()
    if F[0] == 0 and total == EXPECTED:
        print("GLES_PY_FULL_API OK %d" % P[0]); sys.stdout.flush(); os._exit(0)
    print("GLES_PY_FULL_API FAIL"); sys.stdout.flush(); os._exit(1)

# GLSL ES 3.1 compute shader (#version 310 es + explicit precision, NOT desktop #version 430).
CS = """#version 310 es
precision highp float;
precision highp int;
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
CS_BAD = """#version 310 es
precision highp float;
layout(local_size_x=64) in;
void main(){ this is not valid glsl @@@ ;;; }
"""

EXPECTED = 93
N = 1024
NB = N * 4

# GLES has no glGetBufferSubData; read back an SSBO through a READ mapping + ctypes.from_address.
def read_ssbo(buf, n=N):
    GL_SSBO = gl.GL_SHADER_STORAGE_BUFFER
    gl.glBindBuffer(GL_SSBO, int(buf))
    ptr = gl.glMapBufferRange(GL_SSBO, 0, n * 4, gl.GL_MAP_READ_BIT)
    addr = ptr if isinstance(ptr, int) else ctypes.cast(ptr, ctypes.c_void_p).value
    out = np.frombuffer((ctypes.c_float * n).from_address(int(addr)), dtype=np.float32).copy()
    gl.glUnmapBuffer(GL_SSBO)
    return out

# GLES has no glClearBufferData; fill/clear an SSBO by uploading a numpy constant via glBufferSubData.
def fill_ssbo(buf, value, n=N):
    gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(buf))
    gl.glBufferSubData(gl.GL_SHADER_STORAGE_BUFFER, 0, n * 4, np.full(n, value, dtype=np.float32))

# --- surfaceless EGL OpenGL ES 3.1 context (same bring-up idiom as the gles_c cell) ---
dpy = EGL.eglGetDisplay(EGL.EGL_DEFAULT_DISPLAY)
ok(dpy != EGL.EGL_NO_DISPLAY, "eglGetDisplay")
emaj, emin = EGL.EGLint(), EGL.EGLint()
if not EGL.eglInitialize(dpy, ctypes.byref(emaj), ctypes.byref(emin)): die()
ok(True, "eglInitialize")
apis = EGL.eglQueryString(dpy, EGL.EGL_CLIENT_APIS)
apis_b = ctypes.cast(apis, ctypes.c_char_p).value or b""
ok(b"OpenGL_ES" in apis_b or b"OpenGL ES" in apis_b or b"OpenGL" in apis_b, "eglQueryString CLIENT_APIS advertises ES")
cfgs = (EGL.EGLConfig * 1)(); ncfg = EGL.EGLint()
# ES3 configs are advertised through EGL_OPENGL_ES2_BIT (there is no separate ES3 renderable bit).
cfgattr = (EGL.EGLint * 5)(EGL.EGL_SURFACE_TYPE, EGL.EGL_PBUFFER_BIT,
                           EGL.EGL_RENDERABLE_TYPE, EGL.EGL_OPENGL_ES2_BIT, EGL.EGL_NONE)
ok(bool(EGL.eglChooseConfig(dpy, cfgattr, cfgs, 1, ctypes.byref(ncfg))) and ncfg.value >= 1,
   "eglChooseConfig EGL_OPENGL_ES2_BIT")
ok(bool(EGL.eglBindAPI(EGL.EGL_OPENGL_ES_API)), "eglBindAPI EGL_OPENGL_ES_API")
ok(EGL.eglQueryAPI() == EGL.EGL_OPENGL_ES_API, "eglQueryAPI == OPENGL_ES")
ctxattr = (EGL.EGLint * 5)(EGL.EGL_CONTEXT_MAJOR_VERSION, 3, EGL.EGL_CONTEXT_MINOR_VERSION, 1, EGL.EGL_NONE)
ctx = EGL.eglCreateContext(dpy, cfgs[0], EGL.EGL_NO_CONTEXT, ctxattr)
ok(bool(ctx) and ctx != EGL.EGL_NO_CONTEXT, "eglCreateContext ES 3.1")
if not ctx or ctx == EGL.EGL_NO_CONTEXT: die()
ok(bool(EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, ctx)), "eglMakeCurrent surfaceless")
# current-context handle must be the one we just created (real queried identity, not merely non-null)
cur = EGL.eglGetCurrentContext()
ok(ctypes.cast(cur, ctypes.c_void_p).value == ctypes.cast(ctx, ctypes.c_void_p).value,
   "eglGetCurrentContext identity == created ctx")

# --- string queries: assert this IS a GLES context and the version is >= 3.1 (where compute exists) ---
ver = gl.glGetString(gl.GL_VERSION)
ver_s = (bytes(ver) if isinstance(ver, (bytes, bytearray))
         else (ctypes.cast(ver, ctypes.c_char_p).value or b"")).decode(errors="ignore")
ok(ver_s != "", "glGetString(GL_VERSION)")
ok("ES" in ver_s, "context is OpenGL ES (GL_VERSION has 'ES'): %s" % ver_s)
major = int(gl.glGetIntegerv(gl.GL_MAJOR_VERSION)); minor = int(gl.glGetIntegerv(gl.GL_MINOR_VERSION))
ok((major, minor) >= (3, 1), "GLES version >= 3.1 (%d.%d) - compute shaders available" % (major, minor))
glsl = gl.glGetString(gl.GL_SHADING_LANGUAGE_VERSION)
glsl_s = (bytes(glsl) if isinstance(glsl, (bytes, bytearray))
          else (ctypes.cast(glsl, ctypes.c_char_p).value or b"")).decode(errors="ignore")
ok(glsl_s != "" and "ES" in glsl_s, "glGetString(GL_SHADING_LANGUAGE_VERSION) is ES GLSL: %s" % glsl_s)
ok(gl.glGetString(gl.GL_RENDERER) is not None, "glGetString(GL_RENDERER)")
ok(gl.glGetString(gl.GL_VENDOR) is not None, "glGetString(GL_VENDOR)")

# --- compute work-group limits ---
wgc0 = int(gl.glGetIntegeri_v(gl.GL_MAX_COMPUTE_WORK_GROUP_COUNT, 0)[0])
ok(wgc0 >= 1, "glGetIntegeri_v MAX_COMPUTE_WORK_GROUP_COUNT[0] >= 1")
wgs0 = int(gl.glGetIntegeri_v(gl.GL_MAX_COMPUTE_WORK_GROUP_SIZE, 0)[0])
ok(wgs0 >= 1, "glGetIntegeri_v MAX_COMPUTE_WORK_GROUP_SIZE[0] >= 1")
inv = int(gl.glGetIntegerv(gl.GL_MAX_COMPUTE_WORK_GROUP_INVOCATIONS))
ok(inv >= 64, "glGetIntegerv MAX_COMPUTE_WORK_GROUP_INVOCATIONS >= 64 (GLES 3.1 floor 128)")
maxbind = int(gl.glGetIntegerv(gl.GL_MAX_SHADER_STORAGE_BUFFER_BINDINGS))
ok(maxbind >= 3, "MAX_SHADER_STORAGE_BUFFER_BINDINGS >= 3")
ok(int(gl.glGetIntegerv(gl.GL_MAX_COMPUTE_SHADER_STORAGE_BLOCKS)) >= 3, "MAX_COMPUTE_SHADER_STORAGE_BLOCKS >= 3")
# our local_size_x=64 must fit inside the invocation limit reported above; the 1M grid must fit in X count
ok(64 <= inv, "shader local_size_x(64) <= MAX_COMPUTE_WORK_GROUP_INVOCATIONS")
ok(wgc0 >= (1 << 20) // 64, "MAX_COMPUTE_WORK_GROUP_COUNT[0] admits the >=1M-elt X dispatch (16384 groups)")

# --- compile compute shader (happy path) ---
sh = gl.glCreateShader(gl.GL_COMPUTE_SHADER)
ok(sh != 0, "glCreateShader(GL_COMPUTE_SHADER)")
gl.glShaderSource(sh, CS)
gl.glCompileShader(sh)
cstat = int(gl.glGetShaderiv(sh, gl.GL_COMPILE_STATUS))
if not cstat:
    sys.stderr.write("shader log: %s\n" % gl.glGetShaderInfoLog(sh))
ok(cstat == gl.GL_TRUE, "glCompileShader GL_COMPILE_STATUS")
# GL_SHADER_TYPE reflection must report COMPUTE for this shader object
ok(int(gl.glGetShaderiv(sh, gl.GL_SHADER_TYPE)) == int(gl.GL_COMPUTE_SHADER),
   "glGetShaderiv GL_SHADER_TYPE == GL_COMPUTE_SHADER")

# --- compile-error negative path: broken GLSL ES must FAIL to compile and yield a non-empty info log ---
shb = gl.glCreateShader(gl.GL_COMPUTE_SHADER)
gl.glShaderSource(shb, CS_BAD)
gl.glCompileShader(shb)
bad_stat = int(gl.glGetShaderiv(shb, gl.GL_COMPILE_STATUS))
ok(bad_stat == gl.GL_FALSE, "broken shader glCompileShader GL_COMPILE_STATUS == GL_FALSE")
bad_log = gl.glGetShaderInfoLog(shb)
bad_log_s = bad_log.decode() if isinstance(bad_log, bytes) else str(bad_log)
ok(len(bad_log_s) > 0, "glGetShaderInfoLog non-empty on compile failure")
gl.glDeleteShader(shb)

# --- link program (happy path) ---
prog = gl.glCreateProgram()
ok(prog != 0, "glCreateProgram")
gl.glAttachShader(prog, sh)
gl.glLinkProgram(prog)
lstat = int(gl.glGetProgramiv(prog, gl.GL_LINK_STATUS))
if not lstat:
    sys.stderr.write("link log: %s\n" % gl.glGetProgramInfoLog(prog))
ok(lstat == gl.GL_TRUE, "glLinkProgram GL_LINK_STATUS")
# GL_ATTACHED_SHADERS must report the one compute shader we attached
ok(int(gl.glGetProgramiv(prog, gl.GL_ATTACHED_SHADERS)) == 1, "glGetProgramiv GL_ATTACHED_SHADERS == 1")

# --- link-error negative path: an empty program with no compute stage must FAIL to link ---
prog_bad = gl.glCreateProgram()
gl.glLinkProgram(prog_bad)
lbad = int(gl.glGetProgramiv(prog_bad, gl.GL_LINK_STATUS))
ok(lbad == gl.GL_FALSE, "empty program glLinkProgram GL_LINK_STATUS == GL_FALSE")
lbad_log = gl.glGetProgramInfoLog(prog_bad)
lbad_log_s = lbad_log.decode() if isinstance(lbad_log, bytes) else str(lbad_log)
ok(len(lbad_log_s) > 0, "glGetProgramInfoLog non-empty on link failure")
gl.glDeleteProgram(prog_bad)

gl.glDeleteShader(sh)
ok(int(gl.glGetShaderiv(sh, gl.GL_DELETE_STATUS)) == gl.GL_TRUE,
   "glDeleteShader -> GL_DELETE_STATUS flagged (deleted, kept alive by program attach)")

# --- SSBOs (mutable glBufferData; GLES 3.1 has no immutable glBufferStorage) ---
a = np.arange(N, dtype=np.float32)
b = (2.0 * np.arange(N) + 1.0).astype(np.float32)
bufs = gl.glGenBuffers(3)
ok(all(int(bufs[i]) != 0 for i in range(3)), "glGenBuffers(3)")
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[0])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, NB, a, gl.GL_STATIC_DRAW)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[1])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, NB, b, gl.GL_STATIC_DRAW)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[2])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, NB, None, gl.GL_DYNAMIC_COPY)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glBufferData A/B/C")
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_SIZE))[0]) == NB, "glGetBufferParameteriv GL_BUFFER_SIZE")
# glGetBufferParameteriv usage hint must echo the STATIC_DRAW we requested
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_USAGE))[0]) == int(gl.GL_STATIC_DRAW),
   "glGetBufferParameteriv GL_BUFFER_USAGE == GL_STATIC_DRAW")
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(bufs[i]))
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glBindBufferBase x3")

# round-trip a numpy upload straight back through the map-read helper
ok(np.array_equal(read_ssbo(bufs[0]), a), "map-read round-trips uploaded A")

# --- uniforms + dispatch: vadd (alpha=1, mode=0) ---
gl.glUseProgram(prog)
ok(int(gl.glGetIntegerv(gl.GL_CURRENT_PROGRAM)) == int(prog), "glUseProgram -> GL_CURRENT_PROGRAM == prog")
loc_alpha = gl.glGetUniformLocation(prog, "alpha")
loc_n = gl.glGetUniformLocation(prog, "n")
loc_mode = gl.glGetUniformLocation(prog, "mode")
ok(loc_alpha >= 0 and loc_n >= 0 and loc_mode >= 0, "glGetUniformLocation alpha/n/mode")
gl.glUniform1f(loc_alpha, 1.0)
gl.glUniform1ui(loc_n, N)
gl.glUniform1ui(loc_mode, 0)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glUniform1f/glUniform1ui")
# read the uniform back with glGetUniformfv and confirm the driver stored our value
ufv_out = (ctypes.c_float * 1)()
gl.glGetUniformfv(prog, loc_alpha, ufv_out)
ok(abs(float(ufv_out[0]) - 1.0) < 1e-6, "glGetUniformfv alpha == 1.0")
gl.glDispatchCompute((N + 63) // 64, 1, 1)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glDispatchCompute vadd")
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glMemoryBarrier")
vadd = read_ssbo(bufs[2])
ok(np.array_equal(vadd, a + b), "vadd == a+b (numpy per-element, map-read)")

# --- fence sync: fence after the vadd dispatch, wait on the CPU, assert it signals ---
fence = gl.glFenceSync(gl.GL_SYNC_GPU_COMMANDS_COMPLETE, 0)
ok(bool(fence), "glFenceSync GL_SYNC_GPU_COMMANDS_COMPLETE")
gl.glFlush()
wr = gl.glClientWaitSync(fence, gl.GL_SYNC_FLUSH_COMMANDS_BIT, 5_000_000_000)
ok(wr in (int(gl.GL_ALREADY_SIGNALED), int(gl.GL_CONDITION_SATISFIED)),
   "glClientWaitSync -> ALREADY/CONDITION_SATISFIED (not TIMEOUT)")
sstat = gl.glGetSynciv(fence, gl.GL_SYNC_STATUS, 1, None)
ok(int(sstat[1]) == int(gl.GL_SIGNALED), "glGetSynciv GL_SYNC_STATUS == GL_SIGNALED")
gl.glDeleteSync(fence)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glDeleteSync")

# GLES 3.1 core has no GL_TIME_ELAPSED timer query (only GL_EXT_disjoint_timer_query), and
# glGetQueryObjectiv/GL_TIME_ELAPSED are absent from OpenGL.GLES3 - the desktop opengl_py timer-query
# block is intentionally dropped here; the fence-sync block above is the GLES ordering assertion.

# --- read back the vadd result through glMapBufferRange (READ) ---
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[2]))
ptr = gl.glMapBufferRange(gl.GL_SHADER_STORAGE_BUFFER, 0, NB, gl.GL_MAP_READ_BIT)
addr = ptr if isinstance(ptr, int) else ctypes.cast(ptr, ctypes.c_void_p).value
ok(addr not in (None, 0), "glMapBufferRange GL_MAP_READ_BIT")
# while mapped, GL_BUFFER_MAPPED must report TRUE and GL_BUFFER_MAP_LENGTH the mapped byte count
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_MAPPED))[0]) == gl.GL_TRUE,
   "glGetBufferParameteriv GL_BUFFER_MAPPED == GL_TRUE while mapped")
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_MAP_LENGTH))[0]) == NB,
   "glGetBufferParameteriv GL_BUFFER_MAP_LENGTH == NB")
mapped = np.frombuffer((ctypes.c_float * N).from_address(int(addr)), dtype=np.float32).copy()
ok(np.array_equal(mapped, a + b), "mapped range == a+b (numpy per-element)")
ok(bool(gl.glUnmapBuffer(gl.GL_SHADER_STORAGE_BUFFER)), "glUnmapBuffer")
# after unmap the mapping flag must return to FALSE
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_MAPPED))[0]) == gl.GL_FALSE,
   "glGetBufferParameteriv GL_BUFFER_MAPPED == GL_FALSE after unmap")

# --- saxpy: re-dispatch with alpha=3 (mode=0) ---
gl.glUniform1f(loc_alpha, 3.0)
gl.glDispatchCompute((N + 63) // 64, 1, 1)
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
saxpy = read_ssbo(bufs[2])
ok(np.array_equal(saxpy, 3.0 * a + b), "saxpy == 3*a+b (numpy per-element)")

# --- mul: mode=1 ---
gl.glUniform1ui(loc_mode, 1)
gl.glDispatchCompute((N + 63) // 64, 1, 1)
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
mul = read_ssbo(bufs[2])
ok(np.array_equal(mul, a * b), "mul == a*b (numpy per-element)")

# --- glDispatchComputeIndirect: same saxpy dispatched from a GPU-side (x,y,z) group buffer ---
gl.glUniform1f(loc_alpha, 3.0); gl.glUniform1ui(loc_mode, 0); gl.glUniform1ui(loc_n, N)
fill_ssbo(bufs[2], 0.0)
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(bufs[i]))
groups = np.array([(N + 63) // 64, 1, 1], dtype=np.uint32)
dib = gl.glGenBuffers(1)
gl.glBindBuffer(gl.GL_DISPATCH_INDIRECT_BUFFER, int(dib))
gl.glBufferData(gl.GL_DISPATCH_INDIRECT_BUFFER, groups.nbytes, groups, gl.GL_STATIC_DRAW)
gl.glDispatchComputeIndirect(0)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glDispatchComputeIndirect no GL error")
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), 3.0 * a + b), "indirect dispatch == 3*a+b (numpy per-element)")
gl.glDeleteBuffers(1, dib)

# --- glBufferSubData update A<-2 then vadd determinism ---
a2 = np.full(N, 2.0, dtype=np.float32)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
gl.glBufferSubData(gl.GL_SHADER_STORAGE_BUFFER, 0, NB, a2)
ok(np.array_equal(read_ssbo(bufs[0]), a2), "glBufferSubData A<-2 readback")
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(bufs[i]))
gl.glUniform1f(loc_alpha, 1.0)
gl.glUniform1ui(loc_mode, 0)
gl.glDispatchCompute((N + 63) // 64, 1, 1)
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), a2 + b), "vadd after subdata == 2+b (numpy per-element)")

# --- glCopyBufferSubData buf0(=2) -> buf2, verify element-wise ---
gl.glBindBuffer(gl.GL_COPY_READ_BUFFER, int(bufs[0]))
gl.glBindBuffer(gl.GL_COPY_WRITE_BUFFER, int(bufs[2]))
gl.glCopyBufferSubData(gl.GL_COPY_READ_BUFFER, gl.GL_COPY_WRITE_BUFFER, 0, 0, NB)
ok(np.array_equal(read_ssbo(bufs[2]), a2), "glCopyBufferSubData buf0->buf2 == 2 (numpy per-element)")

# --- fill buf2 with 5.0 via glBufferSubData (GLES substitute for glClearBufferData) ---
fill_ssbo(bufs[2], 5.0)
ok(np.array_equal(read_ssbo(bufs[2]), np.full(N, 5.0, dtype=np.float32)), "glBufferSubData fill == 5.0 (numpy per-element)")

# --- glBindBufferRange + program resource introspection ---
gl.glBindBufferRange(gl.GL_SHADER_STORAGE_BUFFER, 0, int(bufs[0]), 0, NB)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glBindBufferRange")
idx = int(gl.glGetProgramResourceIndex(prog, gl.GL_SHADER_STORAGE_BLOCK, "A"))
ok(idx != int(gl.GL_INVALID_INDEX), "glGetProgramResourceIndex SSBO block A")
nres = int(gl.glGetProgramInterfaceiv(prog, gl.GL_SHADER_STORAGE_BLOCK, gl.GL_ACTIVE_RESOURCES))
ok(nres == 3, "glGetProgramInterfaceiv ACTIVE_RESOURCES == 3 (A/B/C)")

# glGetProgramResourceName must decode back to the exact block name for each index
def res_name(i):
    r = gl.glGetProgramResourceName(prog, gl.GL_SHADER_STORAGE_BLOCK, i, 64)
    ln, arr = r[0], r[1]
    return bytes(bytearray(int(x) & 0xFF for x in arr[:ln])).decode()
ok(res_name(idx) == "A", "glGetProgramResourceName(idx of A) == 'A'")

# glGetProgramResourceiv must report GL_BUFFER_BINDING matching the shader's layout(binding=) for each
# block. GLES 3.1 has no glShaderStorageBlockBinding, so the shader layout(binding=) is authoritative -
# this reflection query is exactly how we verify it (A0/B1/C2).
props = (ctypes.c_uint * 1)(int(gl.GL_BUFFER_BINDING))
def res_binding(name):
    i = int(gl.glGetProgramResourceIndex(prog, gl.GL_SHADER_STORAGE_BLOCK, name))
    r = gl.glGetProgramResourceiv(prog, gl.GL_SHADER_STORAGE_BLOCK, i, 1, props, 1, None, None)
    return int(np.ravel(np.asarray(r[1]))[0])
ok(res_binding("A") == 0 and res_binding("B") == 1 and res_binding("C") == 2,
   "glGetProgramResourceiv GL_BUFFER_BINDING == shader layout(binding) A0/B1/C2")

# --- WRITE-mapped host-visible round trip on a mutable buffer (GLES has no immutable glBufferStorage) ---
sbuf = gl.glGenBuffers(1)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(sbuf))
gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, NB, None, gl.GL_DYNAMIC_DRAW)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glBufferData mutable host-visible")
ok(int(np.ravel(gl.glGetBufferParameteriv(gl.GL_SHADER_STORAGE_BUFFER, gl.GL_BUFFER_SIZE))[0]) == NB,
   "mutable storage GL_BUFFER_SIZE == NB")
# write path: map WRITE|INVALIDATE, fill via host pointer, unmap, read back through the map-read helper
wptr = gl.glMapBufferRange(gl.GL_SHADER_STORAGE_BUFFER, 0, NB,
                           gl.GL_MAP_WRITE_BIT | gl.GL_MAP_INVALIDATE_BUFFER_BIT)
waddr = wptr if isinstance(wptr, int) else ctypes.cast(wptr, ctypes.c_void_p).value
ok(waddr not in (None, 0), "glMapBufferRange GL_MAP_WRITE_BIT|GL_MAP_INVALIDATE_BUFFER_BIT")
warr = (ctypes.c_float * N).from_address(int(waddr))
wvals = (np.arange(N, dtype=np.float32) * 7.0 + 1.0).astype(np.float32)
for i in range(N):
    warr[i] = float(wvals[i])
ok(bool(gl.glUnmapBuffer(gl.GL_SHADER_STORAGE_BUFFER)), "glUnmapBuffer after write map")
ok(np.array_equal(read_ssbo(sbuf), wvals), "write-mapped buffer round-trips (numpy per-element)")

# use the written buffer as SSBO A in a real dispatch, verify the compute result element-wise
gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, 0, int(sbuf))     # A <- wvals
gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, 1, int(bufs[1]))  # B <- b
gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, 2, int(bufs[2]))  # C
gl.glUniform1f(loc_alpha, 1.0); gl.glUniform1ui(loc_mode, 0); gl.glUniform1ui(loc_n, N)
gl.glDispatchCompute((N + 63) // 64, 1, 1)
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), wvals + b), "dispatch over write-mapped A == wvals+b (numpy)")
# restore A <- bufs[0]
gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, 0, int(bufs[0]))

# --- error/validation paths: PyOpenGL raises GLError carrying the exact GL enum ---
# 1) glBindBufferBase with an out-of-range binding index -> GL_INVALID_VALUE
raised = None
try:
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, maxbind + 4, int(bufs[0]))
except GLError as e:
    raised = int(e.err)
ok(raised == int(gl.GL_INVALID_VALUE), "glBindBufferBase OOB index -> GL_INVALID_VALUE")
gl.glGetError()
# 2) glBufferData with a negative size -> GL_INVALID_VALUE
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(bufs[0]))
raised = None
try:
    gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, -16, None, gl.GL_STATIC_DRAW)
except GLError as e:
    raised = int(e.err)
ok(raised == int(gl.GL_INVALID_VALUE), "glBufferData negative size -> GL_INVALID_VALUE")
gl.glGetError()
# 3) glMapBufferRange with offset+length beyond the buffer size -> GL_INVALID_VALUE
raised = None
try:
    gl.glMapBufferRange(gl.GL_SHADER_STORAGE_BUFFER, 0, NB * 8, gl.GL_MAP_READ_BIT)
except GLError as e:
    raised = int(e.err)
ok(raised == int(gl.GL_INVALID_VALUE), "glMapBufferRange over-length -> GL_INVALID_VALUE")
gl.glGetError()
# 4) glBindBuffer with a bogus target enum -> GL_INVALID_ENUM
raised = None
try:
    gl.glBindBuffer(0x9999, int(bufs[0]))
except GLError as e:
    raised = int(e.err)
ok(raised == int(gl.GL_INVALID_ENUM), "glBindBuffer bad target -> GL_INVALID_ENUM")
gl.glGetError()

# --- boundary: zero-element dispatch is a no-op (C sentinel stays untouched) ---
fill_ssbo(bufs[2], -1.0)
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(bufs[i]))
gl.glUniform1ui(loc_n, 0)
gl.glDispatchCompute(0, 1, 1)
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
ok(np.array_equal(read_ssbo(bufs[2]), np.full(N, -1.0, dtype=np.float32)),
   "zero-size dispatch is a no-op (sentinel untouched)")

# --- boundary: large >= 1,000,000-element dispatch verified element-wise vs numpy ---
BIG = 1 << 20  # 1,048,576 elements > 1e6
BB = BIG * 4
big_a = (np.arange(BIG, dtype=np.float32) % 1000.0).astype(np.float32)
big_b = ((np.arange(BIG, dtype=np.float32) * 2.0) % 997.0).astype(np.float32)
gbufs = gl.glGenBuffers(3)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(gbufs[0])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, BB, big_a, gl.GL_STATIC_DRAW)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(gbufs[1])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, BB, big_b, gl.GL_STATIC_DRAW)
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(gbufs[2])); gl.glBufferData(gl.GL_SHADER_STORAGE_BUFFER, BB, None, gl.GL_DYNAMIC_COPY)
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(gbufs[i]))
gl.glUniform1f(loc_alpha, 1.0); gl.glUniform1ui(loc_mode, 0); gl.glUniform1ui(loc_n, BIG)
gl.glDispatchCompute((BIG + 63) // 64, 1, 1)  # 16384 workgroups -> multi-workgroup tiling
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
big_got = read_ssbo(gbufs[2], BIG)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "large 1M dispatch no GL error")
ok(np.array_equal(big_got, big_a + big_b), "1M-element vadd == a+b (numpy per-element, 1048576 elems)")

# --- oversubscription / bounds guard: dispatch the FULL 1M grid but set n=BIG/2 ---
# the i>=n guard must leave the upper half at its sentinel while the lower half computes.
half = BIG // 2
gl.glBindBuffer(gl.GL_SHADER_STORAGE_BUFFER, int(gbufs[2]))
gl.glBufferSubData(gl.GL_SHADER_STORAGE_BUFFER, 0, BB, np.full(BIG, -7.0, dtype=np.float32))
for i in range(3):
    gl.glBindBufferBase(gl.GL_SHADER_STORAGE_BUFFER, i, int(gbufs[i]))
gl.glUniform1ui(loc_n, half)
gl.glDispatchCompute((BIG + 63) // 64, 1, 1)  # full grid, guard limits writes to first half
gl.glMemoryBarrier(gl.GL_SHADER_STORAGE_BARRIER_BIT)
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
ok(int(np.count_nonzero(corrupt != (big_a + big_b))) == 1,
   "negative control: exactly one element differs from the reference")
# (b) a KNOWN-WRONG closed-form reference must not match the real vadd output
ok(not np.array_equal(big_got, big_a * big_b),
   "negative control: vadd output != a*b (wrong operator caught)")
# (c) re-read the real device output and confirm it is genuinely equal (guards against a stuck 'not-equal')
ok(np.array_equal(read_ssbo(gbufs[2], BIG)[:half], (big_a + big_b)[:half]),
   "negative control sanity: real output still matches correct reference")
gl.glDeleteBuffers(3, gbufs)

ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glGetError == GL_NO_ERROR (final)")

# --- cleanup ---
gl.glDeleteBuffers(3, bufs)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glDeleteBuffers")
gl.glDeleteBuffers(1, sbuf)
ok(int(gl.glGetError()) == gl.GL_NO_ERROR, "glDeleteBuffers storage")
gl.glDeleteProgram(prog)
ok(int(gl.glGetProgramiv(prog, gl.GL_DELETE_STATUS)) == gl.GL_TRUE, "glDeleteProgram -> GL_DELETE_STATUS")
# the context we are about to destroy must be the one currently bound (real queried identity)
pre_destroy = EGL.eglGetCurrentContext()
ok(ctypes.cast(pre_destroy, ctypes.c_void_p).value == ctypes.cast(ctx, ctypes.c_void_p).value,
   "eglDestroyContext: current ctx identity confirmed before destroy")
EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, EGL.EGL_NO_CONTEXT)
EGL.eglDestroyContext(dpy, ctx)
EGL.eglTerminate(dpy)

die()
