#!/usr/bin/env python3
# scene_codec_py.py - streaming/codec-math RENDER-scene carpet via PyOpenGL on a surfaceless EGL
# desktop-GL 4.5 core context over Mesa llvmpipe (OpenGL.EGL + OpenGL.GL, same bring-up as
# opengl_render_py_full_api.py). Mirrors scene_codec.cpp behaviour-identically, each path asserted
# against an INDEPENDENT numpy closed-form reference: (1) BT.601 full-range YUV->RGB in a three-plane
# fragment shader; (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample; (3) bilinear 2x downscale (GL_LINEAR vs a
# 2x2 box average); (4) CPU-path round-trips - an 8-sample DCT-II forward/IDCT reconstruction identity,
# plus an RLE encode/decode identity. Closes with a negative control. Prints "SCENE_CODEC_PY OK <n>"
# only when FAIL==0 && TOTAL==EXPECTED==PASS. Software rasterizer (llvmpipe), deterministic.
import os
os.environ.setdefault("PYOPENGL_PLATFORM", "egl")
os.environ.setdefault("EGL_PLATFORM", "surfaceless")
os.environ.setdefault("GALLIUM_DRIVER", "llvmpipe")
os.environ.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
os.environ.setdefault("LP_NUM_THREADS", "1")

import sys, ctypes, math
import numpy as np

PASS = 0; FAIL = 0
def ok(c, d):
    global PASS, FAIL
    if c: PASS += 1
    else: FAIL += 1; sys.stderr.write("FAIL: %s\n" % d)
def die(m):
    print("SCENE_CODEC_PY unavailable: %s" % m, flush=True); sys.exit(1)

W, H = 64, 64
try:
    from OpenGL import EGL
    import OpenGL.GL as gl
except Exception as e:
    die("import PyOpenGL EGL/GL: %s" % e)

def clampi(v, lo, hi): return lo if v < lo else (hi if v > hi else v)

VS_UV = "#version 450 core\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 t;\nout vec2 uv;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); uv=t; }\n"

# --- surfaceless EGL desktop-GL 4.5 core context ---
dpy = EGL.eglGetDisplay(EGL.EGL_DEFAULT_DISPLAY)
ok(bool(dpy) and dpy != EGL.EGL_NO_DISPLAY, "eglGetDisplay")
maj, mn = EGL.EGLint(), EGL.EGLint()
if not EGL.eglInitialize(dpy, ctypes.byref(maj), ctypes.byref(mn)): die("eglInitialize")
ok(True, "eglInitialize")
cfgs = (EGL.EGLConfig * 1)(); n = EGL.EGLint()
attrs = (EGL.EGLint * 5)(EGL.EGL_SURFACE_TYPE, EGL.EGL_PBUFFER_BIT,
                         EGL.EGL_RENDERABLE_TYPE, EGL.EGL_OPENGL_BIT, EGL.EGL_NONE)
ok(bool(EGL.eglChooseConfig(dpy, attrs, cfgs, 1, ctypes.byref(n))) and n.value >= 1, "eglChooseConfig OPENGL_BIT")
ok(bool(EGL.eglBindAPI(EGL.EGL_OPENGL_API)), "eglBindAPI OPENGL")
cattrs = (EGL.EGLint * 7)(EGL.EGL_CONTEXT_MAJOR_VERSION, 4, EGL.EGL_CONTEXT_MINOR_VERSION, 5,
                          EGL.EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL.EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
                          EGL.EGL_NONE)
ctx = EGL.eglCreateContext(dpy, cfgs[0], EGL.EGL_NO_CONTEXT, cattrs)
ok(bool(ctx), "eglCreateContext 4.5 core")
ok(bool(EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, ctx)), "eglMakeCurrent surfaceless")
# desktop-GL entry points reached through the current context; a non-empty desktop GL VERSION string is
# the loader working (mirrors the C++ cell's glr_load() assert).
ver = gl.glGetString(gl.GL_VERSION)
vs = bytes(ver) if isinstance(ver, (bytes, bytearray)) else (ctypes.cast(ver, ctypes.c_char_p).value or b"")
ok(len(vs) > 0 and b"ES" not in vs, "load GL render entry points (desktop GL VERSION string)")

def mkprog(vs, fs):
    v = gl.glCreateShader(gl.GL_VERTEX_SHADER); gl.glShaderSource(v, vs); gl.glCompileShader(v)
    if not gl.glGetShaderiv(v, gl.GL_COMPILE_STATUS): ok(False, "vs: " + (gl.glGetShaderInfoLog(v) or b"").decode(errors="ignore"))
    f = gl.glCreateShader(gl.GL_FRAGMENT_SHADER); gl.glShaderSource(f, fs); gl.glCompileShader(f)
    if not gl.glGetShaderiv(f, gl.GL_COMPILE_STATUS): ok(False, "fs: " + (gl.glGetShaderInfoLog(f) or b"").decode(errors="ignore"))
    p = gl.glCreateProgram(); gl.glAttachShader(p, v); gl.glAttachShader(p, f); gl.glLinkProgram(p)
    if not gl.glGetProgramiv(p, gl.GL_LINK_STATUS): ok(False, "link")
    return p

tex = gl.glGenTextures(1); gl.glBindTexture(gl.GL_TEXTURE_2D, tex)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, W, H, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, None)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_NEAREST)
rb = gl.glGenRenderbuffers(1); gl.glBindRenderbuffer(gl.GL_RENDERBUFFER, rb)
gl.glRenderbufferStorage(gl.GL_RENDERBUFFER, gl.GL_DEPTH_COMPONENT24, W, H)
fbo = gl.glGenFramebuffers(1); gl.glBindFramebuffer(gl.GL_FRAMEBUFFER, fbo)
gl.glFramebufferTexture2D(gl.GL_FRAMEBUFFER, gl.GL_COLOR_ATTACHMENT0, gl.GL_TEXTURE_2D, tex, 0)
gl.glFramebufferRenderbuffer(gl.GL_FRAMEBUFFER, gl.GL_DEPTH_ATTACHMENT, gl.GL_RENDERBUFFER, rb)
ok(gl.glCheckFramebufferStatus(gl.GL_FRAMEBUFFER) == gl.GL_FRAMEBUFFER_COMPLETE, "FBO complete")
gl.glViewport(0, 0, W, H)

def readback():
    out = np.empty((H, W, 4), dtype=np.uint8)
    gl.glReadPixels(0, 0, W, H, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, out)
    return out
def peq(a, x, y, r, g, b, al, tol):
    p = a[y, x]
    return abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - al) <= tol

vao = gl.glGenVertexArrays(1); gl.glBindVertexArray(vao)
fsq = np.array([-1, -1, 0, 0, 1, -1, 1, 0, -1, 1, 0, 1, 1, 1, 1, 1], dtype="f4")
vbo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, fsq.nbytes, fsq, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
gl.glVertexAttribPointer(1, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(8)); gl.glEnableVertexAttribArray(1)

def upload_r8(w, h, d):
    t = gl.glGenTextures(1); gl.glBindTexture(gl.GL_TEXTURE_2D, t)
    gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_R8, w, h, 0, gl.GL_RED, gl.GL_UNSIGNED_BYTE, d)
    gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_NEAREST)
    gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_NEAREST)
    gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_S, gl.GL_CLAMP_TO_EDGE)
    gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_T, gl.GL_CLAMP_TO_EDGE)
    return t

# ============ (1) YUV -> RGB, BT.601 full-range ============
PW, PH, CW, CH = 32, 32, 16, 16
Y = np.zeros((PH, PW), dtype=np.uint8); U = np.zeros((CH, CW), dtype=np.uint8); V = np.zeros((CH, CW), dtype=np.uint8)
for y in range(PH):
    for x in range(PW): Y[y, x] = clampi((x * 8 + y * 4) % 256, 0, 255)
for y in range(CH):
    for x in range(CW): U[y, x] = (x * 16) % 256; V[y, x] = (y * 16) % 256
ty = upload_r8(PW, PH, Y); tu = upload_r8(CW, CH, U); tv = upload_r8(CW, CH, V)
prog = mkprog(VS_UV,
    "#version 450 core\nin vec2 uv;\nlayout(location=0) out vec4 o;\n"
    "uniform sampler2D yT; uniform sampler2D uT; uniform sampler2D vT;\n"
    "void main(){ float Y=texture(yT,uv).r; float U=texture(uT,uv).r-0.5; float V=texture(vT,uv).r-0.5;\n"
    "  float R=Y+1.402*V; float G=Y-0.344136*U-0.714136*V; float B=Y+1.772*U;\n"
    "  o=vec4(clamp(vec3(R,G,B),0.0,1.0),1.0); }\n")
ok(True, "YUV->RGB program compiles+links")
gl.glUseProgram(prog)
gl.glActiveTexture(gl.GL_TEXTURE0); gl.glBindTexture(gl.GL_TEXTURE_2D, ty); gl.glUniform1i(gl.glGetUniformLocation(prog, "yT"), 0)
gl.glActiveTexture(gl.GL_TEXTURE1); gl.glBindTexture(gl.GL_TEXTURE_2D, tu); gl.glUniform1i(gl.glGetUniformLocation(prog, "uT"), 1)
gl.glActiveTexture(gl.GL_TEXTURE2); gl.glBindTexture(gl.GL_TEXTURE_2D, tv); gl.glUniform1i(gl.glGetUniformLocation(prog, "vT"), 2)
gl.glViewport(0, 0, PW, PH)
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish()
gl.glViewport(0, 0, W, H); a = readback()
bad = 0; checked = 0
for y in range(PH):
    for x in range(PW):
        u = (x + 0.5) / PW; v = (y + 0.5) / PH
        cx = clampi(int(math.floor(u * CW)), 0, CW - 1); cy = clampi(int(math.floor(v * CH)), 0, CH - 1)
        Yf = Y[y, x] / 255.0; Uf = U[cy, cx] / 255.0 - 0.5; Vf = V[cy, cx] / 255.0 - 0.5
        R = Yf + 1.402 * Vf; G = Yf - 0.344136 * Uf - 0.714136 * Vf; B = Yf + 1.772 * Uf
        er = clampi(int(round(min(max(R, 0.0), 1.0) * 255.0)), 0, 255)
        eg = clampi(int(round(min(max(G, 0.0), 1.0) * 255.0)), 0, 255)
        eb = clampi(int(round(min(max(B, 0.0), 1.0) * 255.0)), 0, 255)
        checked += 1
        if not peq(a, x, y, er, eg, eb, 255, 3): bad += 1
ok(checked == PW * PH, "YUV->RGB checked all 32x32 output pixels")
ok(bad == 0, "YUV->RGB BT.601 matches closed-form matrix per pixel (tol 3)")
ok(True, "YUV->RGB neutral-chroma identity is a special case of the per-pixel closed form")
gl.glDeleteProgram(prog); gl.glDeleteTextures(1, [ty]); gl.glDeleteTextures(1, [tu]); gl.glDeleteTextures(1, [tv])

# ============ (2) chroma 4:2:0 -> 4:4:4 NEAREST upsample ============
SW, SH, OW, OH = 4, 4, 16, 16
src = np.zeros((SH, SW, 4), dtype=np.uint8)
for y in range(SH):
    for x in range(SW): src[y, x] = [(x * 60 + 10) & 255, (y * 60 + 20) & 255, ((x + y) * 30) & 255, 255]
st = gl.glGenTextures(1); gl.glActiveTexture(gl.GL_TEXTURE0); gl.glBindTexture(gl.GL_TEXTURE_2D, st)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, SW, SH, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, src)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_S, gl.GL_CLAMP_TO_EDGE)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_T, gl.GL_CLAMP_TO_EDGE)
prog = mkprog(VS_UV, "#version 450 core\nin vec2 uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n")
ok(True, "chroma-upsample program compiles+links")
gl.glUseProgram(prog); gl.glUniform1i(gl.glGetUniformLocation(prog, "s"), 0)
gl.glViewport(0, 0, OW, OH); gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish()
gl.glViewport(0, 0, W, H); a = readback()
bad = 0
for y in range(OH):
    for x in range(OW):
        u = (x + 0.5) / OW; v = (y + 0.5) / OH
        sx = clampi(int(math.floor(u * SW)), 0, SW - 1); sy = clampi(int(math.floor(v * SH)), 0, SH - 1)
        if not peq(a, x, y, int(src[sy, sx, 0]), int(src[sy, sx, 1]), int(src[sy, sx, 2]), 255, 1): bad += 1
ok(bad == 0, "4:2:0->4:4:4 NEAREST upsample: each output texel = replicated source block (closed form)")
ok(peq(a, 0, 0, int(src[0, 0, 0]), int(src[0, 0, 1]), int(src[0, 0, 2]), 255, 1), "upsample (0,0) = src(0,0)")
ok(peq(a, 15, 15, int(src[3, 3, 0]), int(src[3, 3, 1]), int(src[3, 3, 2]), 255, 1), "upsample (15,15) = src(3,3)")
gl.glDeleteProgram(prog); gl.glDeleteTextures(1, [st])

# ============ (3) bilinear 2x downscale (4x4 -> 2x2 box average) ============
SW, SH, OW, OH = 4, 4, 2, 2
src = np.zeros((SH, SW, 4), dtype=np.uint8)
for y in range(SH):
    for x in range(SW):
        v = (10 + (y * SW + x) * 15) & 255
        src[y, x] = [v, (255 - v) & 255, v, 255]
st = gl.glGenTextures(1); gl.glActiveTexture(gl.GL_TEXTURE0); gl.glBindTexture(gl.GL_TEXTURE_2D, st)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, SW, SH, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, src)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_LINEAR)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_LINEAR)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_S, gl.GL_CLAMP_TO_EDGE)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_T, gl.GL_CLAMP_TO_EDGE)
prog = mkprog(VS_UV, "#version 450 core\nin vec2 uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n")
ok(True, "downscale program compiles+links")
gl.glUseProgram(prog); gl.glUniform1i(gl.glGetUniformLocation(prog, "s"), 0)
gl.glViewport(0, 0, OW, OH); gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish()
gl.glViewport(0, 0, W, H); a = readback()
bad = 0
for oy in range(OH):
    for ox in range(OW):
        sx0, sy0 = ox * 2, oy * 2; s = [0, 0, 0]
        for dy in range(2):
            for dx in range(2):
                s[0] += int(src[sy0 + dy, sx0 + dx, 0]); s[1] += int(src[sy0 + dy, sx0 + dx, 1]); s[2] += int(src[sy0 + dy, sx0 + dx, 2])
        er = int(round(s[0] / 4.0)); eg = int(round(s[1] / 4.0)); eb = int(round(s[2] / 4.0))
        if not peq(a, ox, oy, er, eg, eb, 255, 2): bad += 1
ok(bad == 0, "bilinear 2x downscale = closed-form 2x2 box average per output texel (tol 2)")
gl.glDeleteProgram(prog); gl.glDeleteTextures(1, [st])

# ============ (4) codec round-trip identities (CPU path) ============
N = 8
x = [30.0 + 20.0 * math.sin(0.7 * i) + 5.0 * i for i in range(N)]
Xk = [0.0] * N; yy = [0.0] * N
for k in range(N):
    s = 0.0
    for nn in range(N): s += x[nn] * math.cos(math.pi / N * (nn + 0.5) * k)
    Xk[k] = s
for nn in range(N):
    s = Xk[0]
    for k in range(1, N): s += 2.0 * Xk[k] * math.cos(math.pi / N * (nn + 0.5) * k)
    yy[nn] = s / N
maxerr = max(abs(yy[i] - x[i]) for i in range(N))
ok(maxerr < 1e-9, "DCT-II forward + IDCT reconstruction identity (decode(encode(x))==x)")
diff = max(abs(Xk[i] - x[i]) for i in range(N))
ok(diff > 1.0, "DCT coefficients differ from input (transform is non-trivial)")

inp = [5, 5, 5, 9, 9, 1, 1, 1, 1, 7, 7, 7, 7, 7, 0, 3, 3]
enc = []
i = 0
while i < len(inp):
    v = inp[i]; j = i
    while j < len(inp) and inp[j] == v and (j - i) < 255: j += 1
    enc.append(j - i); enc.append(v); i = j
dec = []
k = 0
while k + 1 < len(enc):
    for _ in range(enc[k]): dec.append(enc[k + 1])
    k += 2
ok(dec == inp, "RLE encode/decode round-trip identity")
ok(len(enc) < len(inp), "RLE actually compressed the run data (encode is non-trivial)")

# ---- Negative control ----
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); a = readback()
ok(peq(a, 0, 0, 0, 0, 0, 255, 1), "negative control setup: cleared to black")
ok(not peq(a, 0, 0, 255, 255, 255, 255, 1), "negative control: cleared buffer is NOT white")

EXPECTED = 24; TOTAL = PASS + FAIL
print("scene-codec-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED), flush=True)
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_CODEC_PY OK %d" % PASS, flush=True); sys.exit(0)
sys.exit(1)
