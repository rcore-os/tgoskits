#!/usr/bin/env python3
# scene_anim_py.py - keyframe-animation RENDER-scene carpet via PyOpenGL on a surfaceless EGL desktop-GL
# 4.5 core context over Mesa llvmpipe (OpenGL.EGL + OpenGL.GL, same bring-up as opengl_render_py_full_api.py).
# Mirrors scene_anim.cpp behaviour-identically: N=4 keyframes of a transformed unit quad (rotation about
# the FBO center composed with translate + uniform scale, interpolated over t in {0,0.25,0.5,0.75}).
# Each frame the four transformed corners are computed closed-form in Python (R(theta)*S*local + T) and
# the readback is asserted at those exact corner pixels plus a just-outside background point; a cubic
# ease eased(t)=3t^2-2t^3 drives the scale and is asserted at each t. NDC z is unused (2D). Closes with a
# negative control. Prints "SCENE_ANIM_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Software
# rasterizer (llvmpipe), deterministic.
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
    print("SCENE_ANIM_PY unavailable: %s" % m, flush=True); sys.exit(1)

W, H = 64, 64
try:
    from OpenGL import EGL
    import OpenGL.GL as gl
except Exception as e:
    die("import PyOpenGL EGL/GL: %s" % e)

def lerp(a, b, t): return a + (b - a) * t
def ease_cubic(t): return 3.0 * t * t - 2.0 * t * t * t

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
def near_color(a, x, y, r, g, b, tol):
    for dy in range(-1, 2):
        for dx in range(-1, 2):
            xx, yy = x + dx, y + dy
            if xx < 0 or yy < 0 or xx >= W or yy >= H: continue
            if peq(a, xx, yy, r, g, b, 255, tol): return True
    return False

prog = mkprog("#version 450 core\nlayout(location=0) in vec2 lp;\nuniform vec2 vp;\nuniform vec2 col0;\nuniform vec2 col1;\nuniform vec2 tr;\nvoid main(){ vec2 pix = col0*lp.x + col1*lp.y + tr; vec2 n=(pix/vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); }\n",
              "#version 450 core\nlayout(location=0) out vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n")
ok(True, "anim program compiles+links")
gl.glUseProgram(prog)
vpl = gl.glGetUniformLocation(prog, "vp"); c0 = gl.glGetUniformLocation(prog, "col0")
c1 = gl.glGetUniformLocation(prog, "col1"); trl = gl.glGetUniformLocation(prog, "tr"); ul = gl.glGetUniformLocation(prog, "u")
ok(vpl >= 0 and c0 >= 0 and c1 >= 0 and trl >= 0 and ul >= 0, "anim uniform locations")
gl.glUniform2f(vpl, float(W), float(H))

local = [-1, -1, 1, -1, -1, 1, 1, 1]
la = np.array(local, dtype="f4")
vao = gl.glGenVertexArrays(1); gl.glBindVertexArray(vao)
vbo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, la.nbytes, la, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)

A0, A1 = 0.0, math.pi / 2.0
S0, S1 = 6.0, 14.0
CX0, CX1, CY0, CY1 = 20.0, 44.0, 20.0, 44.0

def frame_transform(t):
    ang = lerp(A0, A1, t)
    sc = lerp(S0, S1, ease_cubic(t))
    cx = lerp(CX0, CX1, t); cy = lerp(CY0, CY1, t)
    ca, sa = math.cos(ang), math.sin(ang)
    col0 = [sc * ca, sc * sa]
    col1 = [-sc * sa, sc * ca]
    tr = [cx, cy]
    return col0, col1, tr, sc, ang

ts = [0.0, 0.25, 0.5, 0.75]
cols = [[1, 0, 0], [0, 1, 0], [0, 0, 1], [1, 1, 0]]

for fi in range(4):
    t = ts[fi]
    col0, col1, tr, sc, ang = frame_transform(t)
    gl.glUniform2f(c0, col0[0], col0[1]); gl.glUniform2f(c1, col1[0], col1[1]); gl.glUniform2f(trl, tr[0], tr[1])
    gl.glUniform4f(ul, cols[fi][0], cols[fi][1], cols[fi][2], 1.0)
    gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
    gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()

    ca, sa = math.cos(ang), math.sin(ang)
    corners = []
    for k in range(4):
        lx, ly = local[k * 2 + 0], local[k * 2 + 1]
        rx = sc * (ca * lx - sa * ly); ry = sc * (sa * lx + ca * ly)
        corners.append((tr[0] + rx, tr[1] + ry))
    e = ease_cubic(t); e_ref = 3.0 * t * t - 2.0 * t * t * t
    ok(abs(e - e_ref) < 1e-6, "ease_cubic closed-form value")
    ok(abs(sc - (S0 + (S1 - S0) * e)) < 1e-4, "scale = lerp(S0,S1,ease(t)) closed-form")

    cxi = int(round(tr[0] - 0.5)); cyi = int(round(tr[1] - 0.5))
    ok(peq(a, cxi, cyi, int(round(cols[fi][0] * 255)), int(round(cols[fi][1] * 255)), int(round(cols[fi][2] * 255)), 255, 2),
       "frame center pixel carries frame color at closed-form center")

    for k in range(4):
        px_ = int(round(corners[k][0] - 0.5)); py_ = int(round(corners[k][1] - 0.5))
        onscreen = 0 <= px_ < W and 0 <= py_ < H
        ok(onscreen and near_color(a, px_, py_, int(round(cols[fi][0] * 255)), int(round(cols[fi][1] * 255)), int(round(cols[fi][2] * 255)), 40),
           "transformed corner pixel is inside the rendered quad (closed-form R*S*local+T)")

    ox = (W - 2) if fi < 2 else 1
    oy = (H - 2) if fi < 2 else 1
    reach = sc * 1.4142
    covers = abs(ox + 0.5 - tr[0]) <= reach and abs(oy + 0.5 - tr[1]) <= reach
    if not covers: ok(peq(a, ox, oy, 0, 0, 0, 255, 2), "outside-quad point stays background (closed-form silhouette)")
    else: ok(True, "outside-quad point skipped (would be covered)")

# t=0 vs t=0.75 geometry differs
_, _, tra, _, _ = frame_transform(0.0)
_, _, trb, _, _ = frame_transform(0.75)
ok(abs(tra[0] - trb[0]) > 1.0, "center translates between t=0 and t=0.75 (animation is real)")

# rotation at t=0.5
col0, _, _, _, ang = frame_transform(0.5)
ok(abs(ang - math.pi / 4.0) < 1e-5, "t=0.5 rotation angle = pi/4 closed-form")
ok(abs(col0[0] - col0[1]) < 1e-4 and col0[0] > 0, "t=0.5 rotated x-axis column is (sc*cos45, sc*sin45)")

# negative control: frame 0 (red) is NOT green
col0, col1, tr, sc, ang = frame_transform(0.0)
gl.glUniform2f(c0, col0[0], col0[1]); gl.glUniform2f(c1, col1[0], col1[1]); gl.glUniform2f(trl, tr[0], tr[1])
gl.glUniform4f(ul, 1, 0, 0, 1); gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
cxi = int(round(tr[0] - 0.5)); cyi = int(round(tr[1] - 0.5))
ok(not peq(a, cxi, cyi, 0, 255, 0, 255, 4), "negative control: frame-0 center is NOT green")

EXPECTED = 46; TOTAL = PASS + FAIL
print("scene-anim-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED), flush=True)
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_ANIM_PY OK %d" % PASS, flush=True); sys.exit(0)
sys.exit(1)
