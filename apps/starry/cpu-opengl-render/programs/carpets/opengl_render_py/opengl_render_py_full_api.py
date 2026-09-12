#!/usr/bin/env python3
# opengl_render_py_full_api.py - desktop-OpenGL RENDER carpet via PyOpenGL on a surfaceless EGL desktop-GL
# 4.x core context over Mesa llvmpipe (moderngl is unavailable in Alpine, so the Python desktop-GL cell
# uses PyOpenGL's OpenGL.EGL + OpenGL.GL exactly like the sibling gles-render app does for GLES). Renders
# into an off-screen FBO (RGBA8 color texture + depth renderbuffer) and reads pixels back with
# glReadPixels, checking every pixel against a numpy closed-form reference for: clear-color, a solid quad
# through a compiled program, an axis-aligned linear gradient (a triangle-strip quad interpolates
# per-triangle, so only an axis-aligned gradient matches a full-quad closed form), a procedural
# checkerboard from gl_FragCoord, viewport restriction, scissor clears, the depth test (LESS occlusion),
# alpha blending (SRC_ALPHA/ONE_MINUS_SRC_ALPHA over all channels incl alpha), a 1x1 FBO, a sub-rectangle
# readback. Exhaustive per-API coverage: primitive topologies (indexed GL_TRIANGLES / GL_TRIANGLE_FAN /
# GL_LINES / GL_POINTS), a blend factor+equation matrix (ONE/ZERO, ONE/ONE, ZERO/ONE, DST_COLOR, GL_MAX
# and GL_FUNC_REVERSE_SUBTRACT), the full depth-func matrix (8 comparisons at window depth 0.75; NDC z in
# [-1,1] for desktop GL, so z=0.5 maps to window depth 0.75), face culling + winding (FRONT_AND_BACK /
# BACK CCW vs CW), a color write mask (glColorMask), 2x2 texture upload+NEAREST sampling, and state
# queries (glGetIntegerv GL_VIEWPORT / glIsEnabled / glGetBooleanv GL_DEPTH_WRITEMASK / glGetFloatv
# GL_COLOR_CLEAR_VALUE / glGetError), closing with a negative control. Prints
# "OPENGL_RENDER_PY_FULL_API OK <n>" only when every assertion passes and the count equals EXPECTED.
# Software rasterizer (llvmpipe), single-threaded, deterministic - no GPU.
import os
os.environ.setdefault("PYOPENGL_PLATFORM", "egl")
os.environ.setdefault("EGL_PLATFORM", "surfaceless")
os.environ.setdefault("GALLIUM_DRIVER", "llvmpipe")
os.environ.setdefault("LIBGL_ALWAYS_SOFTWARE", "1")
os.environ.setdefault("LP_NUM_THREADS", "1")

import sys, ctypes
import numpy as np

PASS = 0; FAIL = 0
def ok(c, d):
    global PASS, FAIL
    if c: PASS += 1
    else: FAIL += 1; sys.stderr.write("FAIL: %s\n" % d)
def die(m):
    print("OPENGL_RENDER_PY_FULL_API unavailable: %s" % m, flush=True); sys.exit(1)

W, H = 64, 64
try:
    from OpenGL import EGL
    import OpenGL.GL as gl
except Exception as e:
    die("import PyOpenGL EGL/GL: %s" % e)

# --- surfaceless EGL desktop-GL 4.x core context (same bring-up idiom as the C++ cell) ---
dpy = EGL.eglGetDisplay(EGL.EGL_DEFAULT_DISPLAY)
ok(dpy != EGL.EGL_NO_DISPLAY, "eglGetDisplay")
maj, mn = EGL.EGLint(), EGL.EGLint()
if not EGL.eglInitialize(dpy, ctypes.byref(maj), ctypes.byref(mn)): die("eglInitialize")
ok(True, "eglInitialize")
ok(maj.value >= 1, "EGL major>=1")
apis = EGL.eglQueryString(dpy, EGL.EGL_CLIENT_APIS)
apis_b = ctypes.cast(apis, ctypes.c_char_p).value or b""
ok(b"OpenGL" in apis_b, "CLIENT_APIS has OpenGL")
cfgs = (EGL.EGLConfig * 1)(); n = EGL.EGLint()
attrs = (EGL.EGLint * 5)(EGL.EGL_SURFACE_TYPE, EGL.EGL_PBUFFER_BIT,
                         EGL.EGL_RENDERABLE_TYPE, EGL.EGL_OPENGL_BIT, EGL.EGL_NONE)
ok(bool(EGL.eglChooseConfig(dpy, attrs, cfgs, 1, ctypes.byref(n))) and n.value >= 1, "eglChooseConfig OPENGL_BIT")
ok(bool(EGL.eglBindAPI(EGL.EGL_OPENGL_API)), "eglBindAPI OPENGL")
ok(EGL.eglQueryAPI() == EGL.EGL_OPENGL_API, "eglQueryAPI OPENGL")
cattrs = (EGL.EGLint * 7)(EGL.EGL_CONTEXT_MAJOR_VERSION, 4, EGL.EGL_CONTEXT_MINOR_VERSION, 5,
                          EGL.EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL.EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
                          EGL.EGL_NONE)
ctx = EGL.eglCreateContext(dpy, cfgs[0], EGL.EGL_NO_CONTEXT, cattrs)
ok(bool(ctx) and ctx != EGL.EGL_NO_CONTEXT, "eglCreateContext 4.5 core")
ok(bool(EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, ctx)), "eglMakeCurrent surfaceless")
ver = gl.glGetString(gl.GL_VERSION)
vs = bytes(ver) if isinstance(ver, (bytes, bytearray)) else (ctypes.cast(ver, ctypes.c_char_p).value or b"")
ok(len(vs) > 0 and b"ES" not in vs, "glGetString GL_VERSION is desktop GL (%s)" % vs.decode(errors="ignore"))
ok(gl.glGetString(gl.GL_RENDERER) is not None, "glGetString GL_RENDERER")

VS_POS = "#version 450 core\nlayout(location=0) in vec2 p;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); }\n"
VS_POS3 = "#version 450 core\nlayout(location=0) in vec3 p;\nvoid main(){ gl_Position=vec4(p,1.0); }\n"
VS_COL = ("#version 450 core\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec4 c;\nout vec4 vc;\n"
          "void main(){ gl_Position=vec4(p,0.0,1.0); vc=c; }\n")
FS_UNI = "#version 450 core\nout vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n"
FS_VCOL = "#version 450 core\nin vec4 vc;\nout vec4 o;\nvoid main(){ o=vc; }\n"
FS_CHECK = ("#version 450 core\nout vec4 o;\nvoid main(){ ivec2 c=ivec2(gl_FragCoord.xy); "
            "bool e=(((c.x>>3)+(c.y>>3))&1)==0; o=e?vec4(1.0):vec4(0.0,0.0,0.0,1.0); }\n")

def mkprog(vs, fs):
    v = gl.glCreateShader(gl.GL_VERTEX_SHADER); gl.glShaderSource(v, vs); gl.glCompileShader(v)
    if not gl.glGetShaderiv(v, gl.GL_COMPILE_STATUS):
        ok(False, "vs: " + (gl.glGetShaderInfoLog(v) or b"").decode(errors="ignore"))
    f = gl.glCreateShader(gl.GL_FRAGMENT_SHADER); gl.glShaderSource(f, fs); gl.glCompileShader(f)
    if not gl.glGetShaderiv(f, gl.GL_COMPILE_STATUS):
        ok(False, "fs: " + (gl.glGetShaderInfoLog(f) or b"").decode(errors="ignore"))
    p = gl.glCreateProgram(); gl.glAttachShader(p, v); gl.glAttachShader(p, f); gl.glLinkProgram(p)
    if not gl.glGetProgramiv(p, gl.GL_LINK_STATUS):
        ok(False, "link")
    gl.glDeleteShader(v); gl.glDeleteShader(f)
    return p

# --- offscreen FBO: RGBA8 color texture + depth renderbuffer ---
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
ok(gl.glGetError() == gl.GL_NO_ERROR, "no GL error after FBO setup")

def readback():
    out = np.empty((H, W, 4), dtype=np.uint8)
    gl.glReadPixels(0, 0, W, H, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, out)
    return out
def peq(a, x, y, r, g, b, al, tol):
    p = a[y, x]
    return abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol and \
           abs(int(p[2]) - b) <= tol and abs(int(p[3]) - al) <= tol
def all_eq(a, r, g, b, al, tol):
    return bool(np.all(np.abs(a.astype(int) - np.array([r, g, b, al])) <= tol))

# fullscreen quad (triangle strip: BL,BR,TL,TR) in NDC
quad = np.array([-1, -1, 1, -1, -1, 1, 1, 1], dtype="f4")
vao = gl.glGenVertexArrays(1); gl.glBindVertexArray(vao)
vbo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glBufferData(gl.GL_ARRAY_BUFFER, quad.nbytes, quad, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)

# --- Test 1: glClear to a known color, whole-buffer readback ---
gl.glClearColor(0.0, 0.25, 0.5, 1.0); gl.glClear(gl.GL_COLOR_BUFFER_BIT); a = readback()
ok(all_eq(a, 0, 64, 128, 255, 2), "clear color 0,0.25,0.5,1 -> every pixel (0,64,128,255)")
ok(peq(a, 0, 0, 0, 64, 128, 255, 2), "clear pixel (0,0)")
ok(peq(a, W - 1, H - 1, 0, 64, 128, 255, 2), "clear pixel (63,63)")

# --- Test 2: solid-color quad through a compiled+linked program ---
pu = mkprog(VS_POS, FS_UNI); ok(True, "solid program compiles+links")
gl.glUseProgram(pu); ul = gl.glGetUniformLocation(pu, "u"); ok(ul >= 0, "uniform u location")
gl.glUniform4f(ul, 1.0, 0.0, 0.0, 1.0)
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 255, 0, 0, 255, 1), "solid red quad fills every pixel")
ok(gl.glGetError() == gl.GL_NO_ERROR, "no GL error after solid draw")

# --- Test 3: per-vertex interpolated gradient, axis-aligned linear closed-form (horizontal red->blue) ---
gcol = np.array([1, 0, 0, 1, 0, 0, 1, 1, 1, 0, 0, 1, 0, 0, 1, 1], dtype="f4")  # left red, right blue
cbo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, cbo)
gl.glBufferData(gl.GL_ARRAY_BUFFER, gcol.nbytes, gcol, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(1, 4, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(1)
pg = mkprog(VS_COL, FS_VCOL); ok(True, "gradient program compiles+links")
gl.glUseProgram(pg); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
u = (np.arange(W) + 0.5) / W; ref_r = np.rint((1 - u) * 255).astype(int); ref_b = np.rint(u * 255).astype(int)
bad = 0
for x in range(W):
    col = a[:, x, :].astype(int)  # gradient is horizontal, y-independent
    if not (np.all(np.abs(col[:, 0] - ref_r[x]) <= 4) and np.all(col[:, 1] <= 4)
            and np.all(np.abs(col[:, 2] - ref_b[x]) <= 4) and np.all(np.abs(col[:, 3] - 255) <= 4)):
        bad += 1
ok(bad == 0, "gradient matches horizontal-linear closed-form for all columns")
ok(peq(a, 0, 0, 255, 0, 0, 255, 8), "gradient left edge ~ red")
ok(peq(a, W - 1, H - 1, 0, 0, 255, 255, 8), "gradient right edge ~ blue")
ok(peq(a, W // 2, H // 2, 128, 0, 128, 255, 4), "gradient center ~ (128,0,128)")
gl.glDisableVertexAttribArray(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)

# --- Test 4: procedural checkerboard from gl_FragCoord (8px cells) ---
pc = mkprog(VS_POS, FS_CHECK); ok(True, "checker program compiles+links")
gl.glUseProgram(pc); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
xs, ys = np.meshgrid(np.arange(W), np.arange(H))
checker = ((((xs >> 3) + (ys >> 3)) & 1) == 0)
exp = np.where(checker[..., None], np.array([255, 255, 255, 255]), np.array([0, 0, 0, 255]))
ok(np.all(np.abs(a.astype(int) - exp) <= 1), "checkerboard matches (x/8+y/8) parity for all pixels")
ok(peq(a, 0, 0, 255, 255, 255, 255, 1), "checker cell (0,0) white")
ok(peq(a, 8, 0, 0, 0, 0, 255, 1), "checker cell (8,0) black")

# --- Test 5: viewport restriction ---
gl.glUseProgram(pu); gl.glUniform4f(ul, 0.0, 1.0, 0.0, 1.0)
gl.glViewport(0, 0, W, H); gl.glClearColor(1, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)  # whole red
gl.glViewport(0, 0, W // 2, H // 2); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish()
gl.glViewport(0, 0, W, H); a = readback()
ok(peq(a, 5, 5, 0, 255, 0, 255, 1), "viewport: inside (5,5) green")
ok(peq(a, W - 5, H - 5, 255, 0, 0, 255, 1), "viewport: outside (59,59) still red")
ok(peq(a, W // 2 + 2, H // 2 + 2, 255, 0, 0, 255, 1), "viewport: just outside quadrant red")

# --- Test 6: scissor-box clear ---
gl.glClearColor(0, 0, 1, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)  # whole blue
gl.glEnable(gl.GL_SCISSOR_TEST); gl.glScissor(16, 16, 32, 32)
gl.glClearColor(0, 1, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)  # green only in box
gl.glDisable(gl.GL_SCISSOR_TEST); a = readback()
ok(peq(a, 32, 32, 0, 255, 0, 255, 1), "scissor: inside box green")
ok(peq(a, 2, 2, 0, 0, 255, 255, 1), "scissor: outside box blue")
ok(peq(a, 50, 50, 0, 0, 255, 255, 1), "scissor: past box blue")

# --- Test 7: depth test (GL_LESS occlusion) ---
gl.glEnable(gl.GL_DEPTH_TEST); gl.glDepthFunc(gl.GL_LESS)
gl.glClearColor(0, 0, 0, 1); gl.glClearDepth(1.0); gl.glClear(gl.GL_COLOR_BUFFER_BIT | gl.GL_DEPTH_BUFFER_BIT)
pd = mkprog(VS_POS3, FS_UNI); ok(True, "depth program compiles+links")
gl.glUseProgram(pd); ud = gl.glGetUniformLocation(pd, "u")
gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
farq = np.array([-1, -1, 0.5, 1, -1, 0.5, -1, 1, 0.5, 1, 1, 0.5], dtype="f4")  # far red at z=0.5
gl.glBufferData(gl.GL_ARRAY_BUFFER, farq.nbytes, farq, gl.GL_DYNAMIC_DRAW)
gl.glVertexAttribPointer(0, 3, gl.GL_FLOAT, gl.GL_FALSE, 12, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
gl.glUniform4f(ud, 1, 0, 0, 1); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4)
nearq = np.array([0, -1, -0.5, 1, -1, -0.5, 0, 1, -0.5, 1, 1, -0.5], dtype="f4")  # near green over right half
gl.glBufferData(gl.GL_ARRAY_BUFFER, nearq.nbytes, nearq, gl.GL_DYNAMIC_DRAW)
gl.glVertexAttribPointer(0, 3, gl.GL_FLOAT, gl.GL_FALSE, 12, ctypes.c_void_p(0))
gl.glUniform4f(ud, 0, 1, 0, 1); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(peq(a, W - 4, H // 2, 0, 255, 0, 255, 1), "depth: near green wins on right half")
ok(peq(a, 4, H // 2, 255, 0, 0, 255, 1), "depth: far red on left half")
gl.glDisable(gl.GL_DEPTH_TEST)
gl.glBufferData(gl.GL_ARRAY_BUFFER, quad.nbytes, quad, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

# --- Test 8: alpha blending (SRC_ALPHA / ONE_MINUS_SRC_ALPHA), closed-form ---
gl.glUseProgram(pu)
gl.glClearColor(1, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)  # dst red opaque
gl.glEnable(gl.GL_BLEND); gl.glBlendFunc(gl.GL_SRC_ALPHA, gl.GL_ONE_MINUS_SRC_ALPHA)
gl.glUniform4f(ul, 0.0, 0.0, 1.0, 0.5)  # src blue, alpha 0.5
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); gl.glDisable(gl.GL_BLEND); a = readback()
# rgb = 0.5*(0,0,1) + 0.5*(1,0,0) = (128,0,128); a = 0.5*0.5 + 0.5*1.0 = 0.75 -> 191
ok(all_eq(a, 128, 0, 128, 191, 3), "alpha blend 0.5*blue over red -> rgb(128,0,128) a191")

# --- Test 9: sub-rectangle readback ---
gl.glUseProgram(pu); gl.glUniform4f(ul, 0.2, 0.4, 0.6, 1.0)
gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish()
sub = np.empty((4, 4, 4), dtype=np.uint8); gl.glReadPixels(10, 10, 4, 4, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, sub)
ok(np.all(np.abs(sub.astype(int) - np.array([51, 102, 153, 255])) <= 2), "sub-rect (10,10,4x4) == (51,102,153,255)")

# --- Test 10: 1x1 FBO render + readback boundary ---
t1 = gl.glGenTextures(1); gl.glBindTexture(gl.GL_TEXTURE_2D, t1)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, 1, 1, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, None)
f1 = gl.glGenFramebuffers(1); gl.glBindFramebuffer(gl.GL_FRAMEBUFFER, f1)
gl.glFramebufferTexture2D(gl.GL_FRAMEBUFFER, gl.GL_COLOR_ATTACHMENT0, gl.GL_TEXTURE_2D, t1, 0)
ok(gl.glCheckFramebufferStatus(gl.GL_FRAMEBUFFER) == gl.GL_FRAMEBUFFER_COMPLETE, "1x1 FBO complete")
gl.glViewport(0, 0, 1, 1); gl.glClearColor(0.5, 0.5, 0.5, 1.0); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
one = np.empty((1, 1, 4), dtype=np.uint8); gl.glReadPixels(0, 0, 1, 1, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, one); one = one.reshape(4)
ok(abs(int(one[0]) - 128) <= 2 and abs(int(one[1]) - 128) <= 2 and abs(int(one[2]) - 128) <= 2, "1x1 pixel (128,128,128)")
gl.glBindFramebuffer(gl.GL_FRAMEBUFFER, fbo); gl.glViewport(0, 0, W, H)

# ============================ exhaustive per-API render coverage ============================
gl.glUseProgram(pu); gl.glBindVertexArray(vao); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)

# --- Test 11: primitive topologies (glDrawElements + fan + lines + points) ---
gl.glUniform4f(ul, 1, 0, 0, 1)
ibo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ELEMENT_ARRAY_BUFFER, ibo)
idx = np.array([0, 1, 2, 2, 1, 3], dtype=np.uint16)
gl.glBufferData(gl.GL_ELEMENT_ARRAY_BUFFER, idx.nbytes, idx, gl.GL_STATIC_DRAW)
gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawElements(gl.GL_TRIANGLES, 6, gl.GL_UNSIGNED_SHORT, ctypes.c_void_p(0)); gl.glFinish(); a = readback()
ok(all_eq(a, 255, 0, 0, 255, 1), "GL_TRIANGLES via glDrawElements fills quad")
gl.glDeleteBuffers(1, [ibo]); gl.glBindBuffer(gl.GL_ELEMENT_ARRAY_BUFFER, 0)
gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

fan = np.array([0, 0, -1, -1, 1, -1, 1, 1, -1, 1, -1, -1], dtype="f4")  # center + 4 corners
fb = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, fb)
gl.glBufferData(gl.GL_ARRAY_BUFFER, fan.nbytes, fan, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))
gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_FAN, 0, 6); gl.glFinish(); a = readback()
ok(all_eq(a, 255, 0, 0, 255, 1), "GL_TRIANGLE_FAN fills quad")
gl.glDeleteBuffers(1, [fb]); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

ln = np.array([-1, 0, 1, 0], dtype="f4")  # horizontal line across NDC y=0
lb = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, lb)
gl.glBufferData(gl.GL_ARRAY_BUFFER, ln.nbytes, ln, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_LINES, 0, 2); gl.glFinish(); a = readback()
mid = sum(1 for x in range(W) if peq(a, x, H // 2, 255, 0, 0, 255, 2) or peq(a, x, H // 2 - 1, 255, 0, 0, 255, 2))
ok(mid >= W - 2, "GL_LINES draws the middle row")
ok(peq(a, 0, H - 1, 0, 0, 0, 255, 2), "GL_LINES leaves top row clear")
gl.glDeleteBuffers(1, [lb]); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

pt = np.array([0, 0], dtype="f4")  # single point at NDC center
pb = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, pb)
gl.glBufferData(gl.GL_ARRAY_BUFFER, pt.nbytes, pt, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))
gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_POINTS, 0, 1); gl.glFinish(); a = readback()
hit = any(peq(a, x, y, 255, 0, 0, 255, 2) for y in range(H // 2 - 2, H // 2 + 3) for x in range(W // 2 - 2, W // 2 + 3))
ok(hit, "GL_POINTS draws a pixel at the center")
ok(peq(a, 0, 0, 0, 0, 0, 255, 2), "GL_POINTS leaves corner clear")
gl.glDeleteBuffers(1, [pb]); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

# --- Test 12: blend factor + equation matrix (closed-form) ---
gl.glEnable(gl.GL_BLEND)
gl.glBlendEquation(gl.GL_FUNC_ADD); gl.glBlendFunc(gl.GL_ONE, gl.GL_ZERO)
gl.glClearColor(0.5, 0.5, 0.5, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0, 0, 1, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 0, 0, 255, 255, 2), "blend ONE/ZERO: src replaces dst")
gl.glBlendFunc(gl.GL_ONE, gl.GL_ONE)
gl.glClearColor(0.5, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0, 0, 0.5, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 128, 0, 128, 255, 2), "blend ONE/ONE FUNC_ADD: src+dst = (128,0,128)")
gl.glBlendFunc(gl.GL_ZERO, gl.GL_ONE)
gl.glClearColor(0.2, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0, 1, 0, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 51, 0, 0, 255, 2), "blend ZERO/ONE: dst kept (51,0,0)")
gl.glBlendFunc(gl.GL_DST_COLOR, gl.GL_ZERO)
gl.glClearColor(0.5, 0.5, 0.5, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0, 0, 1, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 0, 0, 128, 255, 2), "blend DST_COLOR/ZERO: src*dst modulate (0,0,128)")
gl.glBlendEquation(gl.GL_MAX); gl.glBlendFunc(gl.GL_ONE, gl.GL_ONE)
gl.glClearColor(0.2, 0.6, 0.2, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0.6, 0.2, 0.6, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 153, 153, 153, 255, 2), "blend equation GL_MAX: max(src,dst) per channel")
gl.glBlendEquation(gl.GL_FUNC_REVERSE_SUBTRACT); gl.glBlendFunc(gl.GL_ONE, gl.GL_ONE)
gl.glClearColor(1, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 0.25, 0, 0, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
# reverse-subtract applies to every channel incl alpha: rgb dst-src=(191,0,0), a=1-1=0
ok(all_eq(a, 191, 0, 0, 0, 3), "blend equation REVERSE_SUBTRACT: dst-src rgb (191,0,0) a0")
gl.glBlendEquation(gl.GL_FUNC_ADD); gl.glDisable(gl.GL_BLEND)

# --- Test 13: depth-func matrix (NDC z=0.5 -> window depth 0.75; clear depth 0.75) ---
gl.glEnable(gl.GL_DEPTH_TEST); gl.glDepthMask(gl.GL_TRUE); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
dq = np.array([-1, -1, 0.5, 1, -1, 0.5, -1, 1, 0.5, 1, 1, 0.5], dtype="f4")
gl.glBufferData(gl.GL_ARRAY_BUFFER, dq.nbytes, dq, gl.GL_DYNAMIC_DRAW)
gl.glVertexAttribPointer(0, 3, gl.GL_FLOAT, gl.GL_FALSE, 12, ctypes.c_void_p(0))
gl.glUseProgram(pd); udd = gl.glGetUniformLocation(pd, "u")
dt = [(gl.GL_ALWAYS, True, "ALWAYS"), (gl.GL_NEVER, False, "NEVER"), (gl.GL_LESS, False, "LESS"),
      (gl.GL_LEQUAL, True, "LEQUAL"), (gl.GL_EQUAL, True, "EQUAL"), (gl.GL_GREATER, False, "GREATER"),
      (gl.GL_GEQUAL, True, "GEQUAL"), (gl.GL_NOTEQUAL, False, "NOTEQUAL")]
for f, draws, name in dt:
    gl.glDepthFunc(f); gl.glClearColor(0, 0, 0, 1); gl.glClearDepth(0.75)
    gl.glClear(gl.GL_COLOR_BUFFER_BIT | gl.GL_DEPTH_BUFFER_BIT); gl.glUniform4f(udd, 0, 1, 0, 1)
    gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
    ok(peq(a, W // 2, H // 2, 0, 255, 0, 255, 2) == draws, "depthFunc " + name)
gl.glDisable(gl.GL_DEPTH_TEST); gl.glDepthFunc(gl.GL_LESS)
gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, quad.nbytes, quad, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))

# --- Test 14: face culling + winding order ---
gl.glUseProgram(pu); gl.glUniform4f(ul, 1, 0, 0, 1)
gl.glDisable(gl.GL_CULL_FACE); gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 255, 0, 0, 255, 1), "cull disabled: quad drawn")
gl.glEnable(gl.GL_CULL_FACE); gl.glCullFace(gl.GL_FRONT_AND_BACK); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 0, 0, 0, 255, 1), "cull FRONT_AND_BACK: nothing drawn")
gl.glCullFace(gl.GL_BACK); gl.glFrontFace(gl.GL_CCW); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback(); ccw = peq(a, W // 2, H // 2, 255, 0, 0, 255, 2)
gl.glFrontFace(gl.GL_CW); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback(); cw = peq(a, W // 2, H // 2, 255, 0, 0, 255, 2)
ok(ccw != cw, "cull BACK: CCW vs CW winding flips visibility")
gl.glDisable(gl.GL_CULL_FACE); gl.glFrontFace(gl.GL_CCW)

# --- Test 15: color write mask (glColorMask masks the green channel) ---
gl.glUseProgram(pu); gl.glColorMask(gl.GL_TRUE, gl.GL_FALSE, gl.GL_TRUE, gl.GL_TRUE)
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glUniform4f(ul, 1, 1, 1, 1)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(all_eq(a, 255, 0, 255, 255, 2), "glColorMask green off: white draw -> (255,0,255)")
gl.glColorMask(gl.GL_TRUE, gl.GL_TRUE, gl.GL_TRUE, gl.GL_TRUE)

# --- Test 16: texture upload + sampling (2x2 texels, NEAREST) ---
ptx = mkprog("#version 450 core\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 t;\nout vec2 uv;\nvoid main(){ gl_Position=vec4(p,0.0,1.0); uv=t; }\n",
             "#version 450 core\nin vec2 uv;\nout vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n")
ok(True, "texture program compiles+links")
tx = np.array([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255], dtype=np.uint8)  # BL red BR green TL blue TR white
smp = gl.glGenTextures(1); gl.glActiveTexture(gl.GL_TEXTURE0); gl.glBindTexture(gl.GL_TEXTURE_2D, smp)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, 2, 2, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, tx)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_NEAREST)
tq = np.array([-1, -1, 0, 0, 1, -1, 1, 0, -1, 1, 0, 1, 1, 1, 1, 1], dtype="f4")
tb = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, tb)
gl.glBufferData(gl.GL_ARRAY_BUFFER, tq.nbytes, tq, gl.GL_STATIC_DRAW)
gl.glUseProgram(ptx)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
gl.glVertexAttribPointer(1, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(8)); gl.glEnableVertexAttribArray(1)
gl.glUniform1i(gl.glGetUniformLocation(ptx, "s"), 0)
gl.glClear(gl.GL_COLOR_BUFFER_BIT); gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(peq(a, W // 4, H // 4, 255, 0, 0, 255, 2), "texture NEAREST: bottom-left texel red")
ok(peq(a, 3 * W // 4, H // 4, 0, 255, 0, 255, 2), "texture NEAREST: bottom-right texel green")
ok(peq(a, W // 4, 3 * H // 4, 0, 0, 255, 255, 2), "texture NEAREST: top-left texel blue")
ok(peq(a, 3 * W // 4, 3 * H // 4, 255, 255, 255, 255, 2), "texture NEAREST: top-right texel white")
gl.glDeleteProgram(ptx); gl.glDeleteTextures(1, [smp]); gl.glDeleteBuffers(1, [tb]); gl.glDisableVertexAttribArray(1)
gl.glUseProgram(pu); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)

# --- Test 17: state queries (glGet*, glIsEnabled) reflect the render state ---
vp = gl.glGetIntegerv(gl.GL_VIEWPORT)
ok(int(vp[0]) == 0 and int(vp[1]) == 0 and int(vp[2]) == W and int(vp[3]) == H, "glGetIntegerv GL_VIEWPORT == [0,0,W,H]")
gl.glEnable(gl.GL_BLEND); ok(bool(gl.glIsEnabled(gl.GL_BLEND)), "glIsEnabled(GL_BLEND) true after enable")
gl.glDisable(gl.GL_BLEND); ok(not bool(gl.glIsEnabled(gl.GL_BLEND)), "glIsEnabled(GL_BLEND) false after disable")
ok(bool(np.ravel(gl.glGetBooleanv(gl.GL_DEPTH_WRITEMASK))[0]), "glGetBooleanv GL_DEPTH_WRITEMASK true (default)")
gl.glClearColor(0.25, 0.5, 0.75, 1); cc = gl.glGetFloatv(gl.GL_COLOR_CLEAR_VALUE)
ok(abs(float(cc[0]) - 0.25) < 1e-3 and abs(float(cc[1]) - 0.5) < 1e-3 and abs(float(cc[2]) - 0.75) < 1e-3,
   "glGetFloatv GL_COLOR_CLEAR_VALUE round-trips")
ok(gl.glGetError() == gl.GL_NO_ERROR, "glGetError == GL_NO_ERROR after full render suite")

# --- Negative control: the pixel checker rejects a known-wrong expected ---
gl.glUseProgram(pu); gl.glUniform4f(ul, 1, 0, 0, 1); gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glDrawArrays(gl.GL_TRIANGLE_STRIP, 0, 4); gl.glFinish(); a = readback()
ok(not all_eq(a, 0, 255, 0, 255, 2), "negative control: red buffer is NOT green")
ok(not peq(a, 0, 0, 0, 0, 0, 255, 2), "negative control: red pixel is NOT black")

gl.glDeleteProgram(pu); gl.glDeleteProgram(pg); gl.glDeleteProgram(pc); gl.glDeleteProgram(pd)
gl.glDeleteBuffers(1, [vbo]); gl.glDeleteBuffers(1, [cbo]); gl.glDeleteVertexArrays(1, [vao])
gl.glDeleteRenderbuffers(1, [rb]); gl.glDeleteTextures(1, [tex]); gl.glDeleteFramebuffers(1, [fbo])
EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, EGL.EGL_NO_CONTEXT)
EGL.eglDestroyContext(dpy, ctx); EGL.eglTerminate(dpy)

EXPECTED = 79; TOTAL = PASS + FAIL
print("opengl-render-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED), flush=True)
if FAIL == 0 and TOTAL == EXPECTED:
    print("OPENGL_RENDER_PY_FULL_API OK %d" % PASS, flush=True); sys.exit(0)
sys.exit(1)
