#!/usr/bin/env python3
# scene_2dui_py.py - 2D UI compositing RENDER-scene carpet via PyOpenGL on a surfaceless EGL ES 3.1
# context over Mesa llvmpipe (OpenGL.EGL + OpenGL.GLES3, same bring-up as gles_render_py_full_api.py).
# Mirrors scene_2dui.cpp behaviour-identically: an orthographic pixel-space projection, and every scene
# primitive verified against an INDEPENDENT numpy closed-form reference (never derived from the GL
# output) - filled rectangles, an analytic rounded-rect (inside/corner-arc/outside), a nine-patch scaled
# border frame, an 8x8 bitmap-font glyph blit (every lit/unlit texel), a scissor-clipped fill, and
# MULTI-LAYER Porter-Duff over compositing of 3 stacked semi-transparent layers Co = Cs*As + Cd*(1-As)
# matched channel-by-channel incl alpha. Closes with a negative control. Prints "SCENE_2DUI_PY OK <n>"
# only when FAIL==0 && TOTAL==EXPECTED==PASS. Software rasterizer (llvmpipe), deterministic.
import sys, ctypes
import numpy as np

PASS = 0; FAIL = 0
def ok(c, d):
    global PASS, FAIL
    if c: PASS += 1
    else: FAIL += 1; sys.stderr.write("FAIL: %s\n" % d)
def die(m):
    print("SCENE_2DUI_PY unavailable: %s" % m, flush=True); sys.exit(1)

W, H = 64, 64
try:
    from OpenGL import EGL
    import OpenGL.GLES3 as gl
except Exception as e:
    die("import PyOpenGL EGL/GLES3: %s" % e)

def clampi(v, lo, hi): return lo if v < lo else (hi if v > hi else v)
def q8(f): return clampi(int(round(f * 255.0)), 0, 255)

# --- surfaceless EGL OpenGL ES 3.1 context ---
dpy = EGL.eglGetDisplay(EGL.EGL_DEFAULT_DISPLAY)
ok(bool(dpy) and dpy != EGL.EGL_NO_DISPLAY, "eglGetDisplay")
maj, mn = EGL.EGLint(), EGL.EGLint()
if not EGL.eglInitialize(dpy, ctypes.byref(maj), ctypes.byref(mn)): die("eglInitialize")
ok(True, "eglInitialize")
cfgs = (EGL.EGLConfig * 1)(); n = EGL.EGLint()
attrs = (EGL.EGLint * 5)(EGL.EGL_SURFACE_TYPE, EGL.EGL_PBUFFER_BIT,
                         EGL.EGL_RENDERABLE_TYPE, EGL.EGL_OPENGL_ES2_BIT, EGL.EGL_NONE)
ok(bool(EGL.eglChooseConfig(dpy, attrs, cfgs, 1, ctypes.byref(n))) and n.value >= 1, "eglChooseConfig ES")
ok(bool(EGL.eglBindAPI(EGL.EGL_OPENGL_ES_API)), "eglBindAPI ES")
cattrs = (EGL.EGLint * 5)(EGL.EGL_CONTEXT_MAJOR_VERSION, 3, EGL.EGL_CONTEXT_MINOR_VERSION, 1, EGL.EGL_NONE)
ctx = EGL.eglCreateContext(dpy, cfgs[0], EGL.EGL_NO_CONTEXT, cattrs)
ok(bool(ctx), "eglCreateContext ES 3.1")
ok(bool(EGL.eglMakeCurrent(dpy, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, ctx)), "eglMakeCurrent surfaceless")

VS_PIX = "#version 310 es\nlayout(location=0) in vec2 p;\nuniform vec2 vp;\nvoid main(){ vec2 n = (p/vp)*2.0 - 1.0; gl_Position=vec4(n,0.0,1.0); }\n"
FS_UNI = "#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\nuniform vec4 u;\nvoid main(){ o=u; }\n"

def mkprog(vs, fs):
    v = gl.glCreateShader(gl.GL_VERTEX_SHADER); gl.glShaderSource(v, vs); gl.glCompileShader(v)
    if not gl.glGetShaderiv(v, gl.GL_COMPILE_STATUS): ok(False, "vs: " + (gl.glGetShaderInfoLog(v) or b"").decode(errors="ignore"))
    f = gl.glCreateShader(gl.GL_FRAGMENT_SHADER); gl.glShaderSource(f, fs); gl.glCompileShader(f)
    if not gl.glGetShaderiv(f, gl.GL_COMPILE_STATUS): ok(False, "fs: " + (gl.glGetShaderInfoLog(f) or b"").decode(errors="ignore"))
    p = gl.glCreateProgram(); gl.glAttachShader(p, v); gl.glAttachShader(p, f); gl.glLinkProgram(p)
    if not gl.glGetProgramiv(p, gl.GL_LINK_STATUS): ok(False, "link")
    return p

# off-screen FBO: RGBA8 color texture + DEPTH24 renderbuffer
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
vbo = gl.glGenBuffers(1)

prog = mkprog(VS_PIX, FS_UNI); ok(True, "pixel-fill program compiles+links")
gl.glUseProgram(prog)
vp_loc = gl.glGetUniformLocation(prog, "vp"); u_loc = gl.glGetUniformLocation(prog, "u")
ok(vp_loc >= 0 and u_loc >= 0, "uniform locations")
gl.glUniform2f(vp_loc, float(W), float(H))
ok(gl.glGetError() == gl.GL_NO_ERROR, "no GL error after setup")

def fill_rect(x0, y0, x1, y1, r, g, b, a):
    v = np.array([x0, y0, x1, y0, x0, y1, x0, y1, x1, y0, x1, y1], dtype="f4")
    gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, v.nbytes, v, gl.GL_DYNAMIC_DRAW)
    gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
    gl.glUniform4f(u_loc, r, g, b, a); gl.glDrawArrays(gl.GL_TRIANGLES, 0, 6)

# ---- Scene A: filled rectangles ----
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
fill_rect(8, 8, 16, 24, 1, 0, 0, 1)
fill_rect(40, 32, 48, 52, 0, 1, 0, 1)
gl.glFinish(); a = readback()
xs, ys = np.meshgrid(np.arange(W), np.arange(H))
rectA = (xs >= 8) & (xs < 16) & (ys >= 8) & (ys < 24)
rectB = (xs >= 40) & (xs < 48) & (ys >= 32) & (ys < 52)
exp = np.zeros((H, W, 4), dtype=int); exp[..., 3] = 255
exp[rectA] = [255, 0, 0, 255]; exp[rectB] = [0, 255, 0, 255]
ok(np.all(np.abs(a.astype(int) - exp) <= 1), "filled rectangles: every pixel matches closed-form rect coverage")
ok(peq(a, 10, 10, 255, 0, 0, 255, 1), "rect A interior red")
ok(peq(a, 44, 40, 0, 255, 0, 255, 1), "rect B interior green")
ok(peq(a, 30, 30, 0, 0, 0, 255, 1), "gap between rects is background")

# ---- Scene B: analytic rounded-rect ----
FS_RR = ("#version 310 es\nprecision highp float;\nlayout(location=0) out vec4 o;\n"
         "uniform vec4 box;\nuniform float rad;\nuniform vec4 col;\n"
         "void main(){ vec2 p=gl_FragCoord.xy; float x0=box.x,y0=box.y,x1=box.z,y1=box.w;\n"
         "  bool inside = p.x>=x0&&p.x<x1&&p.y>=y0&&p.y<y1;\n"
         "  if(!inside){ discard; }\n"
         "  vec2 c = p; bool corner=false; vec2 cc=vec2(0.0);\n"
         "  if(p.x<x0+rad&&p.y<y0+rad){corner=true;cc=vec2(x0+rad,y0+rad);}\n"
         "  else if(p.x>=x1-rad&&p.y<y0+rad){corner=true;cc=vec2(x1-rad,y0+rad);}\n"
         "  else if(p.x<x0+rad&&p.y>=y1-rad){corner=true;cc=vec2(x0+rad,y1-rad);}\n"
         "  else if(p.x>=x1-rad&&p.y>=y1-rad){corner=true;cc=vec2(x1-rad,y1-rad);}\n"
         "  if(corner && distance(c,cc)>rad){ discard; }\n"
         "  o=col; }\n")
prr = mkprog(VS_PIX, FS_RR); ok(True, "rounded-rect program compiles+links")
gl.glUseProgram(prr); gl.glUniform2f(gl.glGetUniformLocation(prr, "vp"), float(W), float(H))
gl.glUniform4f(gl.glGetUniformLocation(prr, "box"), 12, 12, 52, 52)
gl.glUniform1f(gl.glGetUniformLocation(prr, "rad"), 8.0)
gl.glUniform4f(gl.glGetUniformLocation(prr, "col"), 1, 1, 0, 1)
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
fq = np.array([0, 0, W, 0, 0, H, 0, H, W, 0, W, H], dtype="f4")
gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, fq.nbytes, fq, gl.GL_DYNAMIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 8, ctypes.c_void_p(0))
gl.glDrawArrays(gl.GL_TRIANGLES, 0, 6); gl.glFinish(); a = readback()
def covered(x, y):
    cx, cy = x + 0.5, y + 0.5; x0, y0, x1, y1, r = 12.0, 12.0, 52.0, 52.0, 8.0
    if not (cx >= x0 and cx < x1 and cy >= y0 and cy < y1): return False
    corner = False; ccx = ccy = 0.0
    if cx < x0 + r and cy < y0 + r: corner = True; ccx = x0 + r; ccy = y0 + r
    elif cx >= x1 - r and cy < y0 + r: corner = True; ccx = x1 - r; ccy = y0 + r
    elif cx < x0 + r and cy >= y1 - r: corner = True; ccx = x0 + r; ccy = y1 - r
    elif cx >= x1 - r and cy >= y1 - r: corner = True; ccx = x1 - r; ccy = y1 - r
    if corner:
        dx = cx - ccx; dy = cy - ccy
        if (dx * dx + dy * dy) ** 0.5 > r: return False
    return True
bad = 0; lit = 0
for y in range(H):
    for x in range(W):
        cov = covered(x, y)
        if cov: lit += 1
        er, eg, eb = (255, 255, 0) if cov else (0, 0, 0)
        if not peq(a, x, y, er, eg, eb, 255, 1): bad += 1
ok(bad == 0, "rounded-rect: every pixel matches analytic corner-arc coverage")
ok(lit > 0, "rounded-rect: some pixels covered")
ok(peq(a, 32, 32, 255, 255, 0, 255, 1), "rounded-rect center lit")
ok(peq(a, 12, 12, 0, 0, 0, 255, 1), "rounded-rect clipped corner (12,12) is background")
ok(peq(a, 32, 13, 255, 255, 0, 255, 1), "rounded-rect straight top edge lit")
gl.glDeleteProgram(prr); gl.glUseProgram(prog)

# ---- Scene C: nine-patch-style scaled border frame ----
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
fill_rect(4, 4, 60, 60, 0, 0, 1, 1)
fill_rect(10, 10, 54, 54, 0.1, 0.1, 0.1, 1.0)
gl.glFinish(); a = readback()
inbox = (xs >= 4) & (xs < 60) & (ys >= 4) & (ys < 60)
ininner = (xs >= 10) & (xs < 54) & (ys >= 10) & (ys < 54)
exp = np.zeros((H, W, 4), dtype=int); exp[..., 3] = 255
exp[inbox] = [0, 0, 255, 255]; exp[ininner] = [q8(0.1), q8(0.1), q8(0.1), 255]
ok(np.all(np.abs(a.astype(int) - exp) <= 1), "nine-patch border frame: closed-form border-vs-interior coverage")
ok(peq(a, 5, 32, 0, 0, 255, 255, 1), "nine-patch left border blue")
ok(peq(a, 32, 5, 0, 0, 255, 255, 1), "nine-patch top border blue")
ok(peq(a, 32, 32, q8(0.1), q8(0.1), q8(0.1), 255, 1), "nine-patch hollow interior")

# ---- Scene D: 8x8 bitmap-font glyph blit ('H') ----
GLYPH_H = [0x00, 0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00]
rgba = np.zeros((8, 8, 4), dtype=np.uint8)
for r in range(8):
    for c in range(8):
        v = 255 if ((GLYPH_H[r] >> (7 - c)) & 1) else 0
        rgba[r, c] = [v, v, v, 255]
gtex = gl.glGenTextures(1); gl.glActiveTexture(gl.GL_TEXTURE0); gl.glBindTexture(gl.GL_TEXTURE_2D, gtex)
gl.glTexImage2D(gl.GL_TEXTURE_2D, 0, gl.GL_RGBA8, 8, 8, 0, gl.GL_RGBA, gl.GL_UNSIGNED_BYTE, rgba)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MIN_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_MAG_FILTER, gl.GL_NEAREST)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_S, gl.GL_CLAMP_TO_EDGE)
gl.glTexParameteri(gl.GL_TEXTURE_2D, gl.GL_TEXTURE_WRAP_T, gl.GL_CLAMP_TO_EDGE)
ptex = mkprog("#version 310 es\nlayout(location=0) in vec2 p;\nlayout(location=1) in vec2 t;\nout vec2 uv;\nuniform vec2 vp;\nvoid main(){ vec2 n=(p/vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); uv=t; }\n",
              "#version 310 es\nprecision highp float;\nin vec2 uv;\nlayout(location=0) out vec4 o;\nuniform sampler2D s;\nvoid main(){ o=texture(s,uv); }\n")
ok(True, "glyph program compiles+links")
gl.glUseProgram(ptex); gl.glUniform2f(gl.glGetUniformLocation(ptex, "vp"), float(W), float(H))
gl.glUniform1i(gl.glGetUniformLocation(ptex, "s"), 0)
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gq = np.array([20, 20, 0, 0, 28, 20, 1, 0, 20, 28, 0, 1, 20, 28, 0, 1, 28, 20, 1, 0, 28, 28, 1, 1], dtype="f4")
gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, gq.nbytes, gq, gl.GL_DYNAMIC_DRAW)
gl.glVertexAttribPointer(0, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
gl.glVertexAttribPointer(1, 2, gl.GL_FLOAT, gl.GL_FALSE, 16, ctypes.c_void_p(8)); gl.glEnableVertexAttribArray(1)
gl.glDrawArrays(gl.GL_TRIANGLES, 0, 6); gl.glFinish(); a = readback()
bad = 0
for dy in range(8):
    for dx in range(8):
        sx, sy = 20 + dx, 20 + dy
        v = 255 if ((GLYPH_H[dy] >> (7 - dx)) & 1) else 0
        if not peq(a, sx, sy, v, v, v, 255, 1): bad += 1
ok(bad == 0, "glyph blit: all 64 texels match hardcoded 8x8 'H' bitmap")
ok(peq(a, 21, 23, 255, 255, 255, 255, 1), "glyph crossbar lit (col1,row3)")
ok(peq(a, 23, 20, 0, 0, 0, 255, 1), "glyph row0 blank")
ok(peq(a, 24, 21, 0, 0, 0, 255, 1), "glyph row1 middle blank (0x42)")
gl.glDeleteProgram(ptex); gl.glDeleteTextures(1, [gtex]); gl.glDisableVertexAttribArray(1); gl.glUseProgram(prog)

# ---- Scene E: scissor-clipped fill ----
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glEnable(gl.GL_SCISSOR_TEST); gl.glScissor(16, 16, 20, 20)
fill_rect(0, 0, float(W), float(H), 1, 0, 1, 1)
gl.glDisable(gl.GL_SCISSOR_TEST); gl.glFinish(); a = readback()
inr = (xs >= 16) & (xs < 36) & (ys >= 16) & (ys < 36)
exp = np.zeros((H, W, 4), dtype=int); exp[..., 3] = 255
exp[inr] = [255, 0, 255, 255]
ok(np.all(np.abs(a.astype(int) - exp) <= 1), "scissor-clipped fill: magenta only within [16,36)^2")
ok(peq(a, 20, 20, 255, 0, 255, 255, 1), "scissor inside magenta")
ok(peq(a, 40, 40, 0, 0, 0, 255, 1), "scissor outside background")

# ---- Scene F: MULTI-LAYER Porter-Duff over compositing ----
bg = [0.10, 0.10, 0.10, 1.0]
layers = [
    (1.0, 0.0, 0.0, 0.50, 8, 8, 56, 56),
    (0.0, 1.0, 0.0, 0.25, 12, 12, 52, 52),
    (0.0, 0.0, 1.0, 0.75, 16, 16, 48, 48),
]
gl.glClearColor(bg[0], bg[1], bg[2], bg[3]); gl.glClear(gl.GL_COLOR_BUFFER_BIT)
gl.glEnable(gl.GL_BLEND); gl.glBlendFunc(gl.GL_SRC_ALPHA, gl.GL_ONE_MINUS_SRC_ALPHA); gl.glBlendEquation(gl.GL_FUNC_ADD)
for l in layers: fill_rect(l[4], l[5], l[6], l[7], l[0], l[1], l[2], l[3])
gl.glDisable(gl.GL_BLEND); gl.glFinish(); a = readback()
def composite(tx, ty):
    c = list(bg)
    for l in layers:
        cx, cy = tx + 0.5, ty + 0.5
        if cx >= l[4] and cx < l[6] and cy >= l[5] and cy < l[7]:
            as_ = l[3]; src = [l[0], l[1], l[2], l[3]]
            for k in range(4): c[k] = src[k] * as_ + c[k] * (1.0 - as_)
    return c
bad = 0
for y in range(H):
    for x in range(W):
        e = composite(x, y)
        if not peq(a, x, y, q8(e[0]), q8(e[1]), q8(e[2]), q8(e[3]), 2): bad += 1
ok(bad == 0, "multi-layer over: every pixel matches Porter-Duff over accumulation (incl partial-overlap regions)")
c = list(bg)
for li in [[1, 0, 0, 0.5], [0, 1, 0, 0.25], [0, 0, 1, 0.75]]:
    as_ = li[3]
    for k in range(4): c[k] = li[k] * as_ + c[k] * (1.0 - as_)
ok(peq(a, 32, 32, q8(c[0]), q8(c[1]), q8(c[2]), q8(c[3]), 2), "multi-layer over center pixel matches hand-iterated over")
as_ = 0.5
er = 1.0 * as_ + bg[0] * (1 - as_); eg = 0 * as_ + bg[1] * (1 - as_); eb = 0 * as_ + bg[2] * (1 - as_); ea = as_ * as_ + bg[3] * (1 - as_)
ok(peq(a, 10, 32, q8(er), q8(eg), q8(eb), q8(ea), 2), "multi-layer over: single-layer region matches one over")

# ---- Negative control ----
gl.glClearColor(0, 0, 0, 1); gl.glClear(gl.GL_COLOR_BUFFER_BIT); fill_rect(8, 8, 16, 24, 1, 0, 0, 1); gl.glFinish(); a = readback()
ok(not peq(a, 10, 10, 0, 255, 0, 255, 4), "negative control: red rect pixel is NOT green")
ok(not peq(a, 30, 30, 255, 0, 0, 255, 4), "negative control: background is NOT red")

EXPECTED = 37; TOTAL = PASS + FAIL
print("scene-2dui-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED), flush=True)
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_2DUI_PY OK %d" % PASS, flush=True); sys.exit(0)
sys.exit(1)
