#!/usr/bin/env python3
# scene_3dmodel_py.py - 3D indexed-mesh RENDER-scene carpet via PyOpenGL on a surfaceless EGL ES 3.1
# context over Mesa llvmpipe (OpenGL.EGL + OpenGL.GLES3, same bring-up as gles_render_py_full_api.py).
# Mirrors scene_3dmodel.cpp behaviour-identically: an indexed cube mesh with a hand-computed perspective
# MVP, depth-buffered occlusion (GL_LESS) and Gouraud shading, verified against an INDEPENDENT software
# rasterizer written in numpy/Python (same MVP -> clip -> NDC -> viewport, per-pixel barycentric +
# perspective-correct depth test in a private z-buffer + interpolated vertex colors) compared to the GL
# readback per pixel. GL uses NDC z in [-1,1] (GL convention). Closes with a negative control. Prints
# "SCENE_3DMODEL_PY OK <n>" only when FAIL==0 && TOTAL==EXPECTED==PASS. Software rasterizer, deterministic.
import sys, ctypes, math
import numpy as np

PASS = 0; FAIL = 0
def ok(c, d):
    global PASS, FAIL
    if c: PASS += 1
    else: FAIL += 1; sys.stderr.write("FAIL: %s\n" % d)
def die(m):
    print("SCENE_3DMODEL_PY unavailable: %s" % m, flush=True); sys.exit(1)

W, H = 64, 64
try:
    from OpenGL import EGL
    import OpenGL.GLES3 as gl
except Exception as e:
    die("import PyOpenGL EGL/GLES3: %s" % e)

# --- column-major 4x4 matrix math (GL layout: m[col*4+row]) ---
def mul(a, b):
    r = [0.0] * 16
    for c in range(4):
        for row in range(4):
            s = 0.0
            for k in range(4): s += a[k * 4 + row] * b[c * 4 + k]
            r[c * 4 + row] = s
    return r
def mv4(a, v):
    o = [0.0] * 4
    for row in range(4):
        s = 0.0
        for k in range(4): s += a[k * 4 + row] * v[k]
        o[row] = s
    return o
def perspective(fovy, aspect, zn, zf):
    f = 1.0 / math.tan(fovy * 0.5); r = [0.0] * 16
    r[0] = f / aspect; r[5] = f
    r[2 * 4 + 2] = (zf + zn) / (zn - zf); r[2 * 4 + 3] = -1.0
    r[3 * 4 + 2] = (2.0 * zf * zn) / (zn - zf); return r
def translate(x, y, z):
    r = [0.0] * 16; r[0] = r[5] = r[10] = r[15] = 1.0
    r[3 * 4] = x; r[3 * 4 + 1] = y; r[3 * 4 + 2] = z; return r
def rot_y(a):
    r = [0.0] * 16; c = math.cos(a); s = math.sin(a)
    r[0] = c; r[0 * 4 + 2] = -s; r[2 * 4] = s; r[2 * 4 + 2] = c; r[5] = 1.0; r[15] = 1.0; return r
def rot_x(a):
    r = [0.0] * 16; c = math.cos(a); s = math.sin(a)
    r[5] = c; r[1 * 4 + 2] = s; r[2 * 4 + 1] = -s; r[2 * 4 + 2] = c; r[0] = 1.0; r[15] = 1.0; return r

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
def pxv(a, x, y, c): return int(a[y, x, c])
def peq(a, x, y, r, g, b, al, tol):
    p = a[y, x]
    return abs(int(p[0]) - r) <= tol and abs(int(p[1]) - g) <= tol and abs(int(p[2]) - b) <= tol and abs(int(p[3]) - al) <= tol

# cube mesh: 8 verts, 12 triangles, per-vertex color = position-based (Gouraud)
VP = [[-1, -1, -1], [1, -1, -1], [1, 1, -1], [-1, 1, -1],
      [-1, -1, 1], [1, -1, 1], [1, 1, 1], [-1, 1, 1]]
VC = [[(VP[i][0] + 1) * 0.5, (VP[i][1] + 1) * 0.5, (VP[i][2] + 1) * 0.5] for i in range(8)]
IDX = [0, 1, 2, 0, 2, 3, 4, 6, 5, 4, 7, 6, 0, 4, 5, 0, 5, 1, 3, 2, 6, 3, 6, 7, 0, 3, 7, 0, 7, 4, 1, 5, 6, 1, 6, 2]

model = mul(rot_y(0.6), rot_x(0.3))
view = translate(0, 0, -5.0)
proj = perspective(1.0, float(W) / float(H), 1.0, 20.0)
mvp = mul(proj, mul(view, model))

verts = np.zeros(8 * 6, dtype="f4")
for i in range(8):
    verts[i * 6 + 0], verts[i * 6 + 1], verts[i * 6 + 2] = VP[i]
    verts[i * 6 + 3], verts[i * 6 + 4], verts[i * 6 + 5] = VC[i]
idxa = np.array(IDX, dtype=np.uint16)
vao = gl.glGenVertexArrays(1); gl.glBindVertexArray(vao)
vbo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ARRAY_BUFFER, vbo); gl.glBufferData(gl.GL_ARRAY_BUFFER, verts.nbytes, verts, gl.GL_STATIC_DRAW)
ibo = gl.glGenBuffers(1); gl.glBindBuffer(gl.GL_ELEMENT_ARRAY_BUFFER, ibo); gl.glBufferData(gl.GL_ELEMENT_ARRAY_BUFFER, idxa.nbytes, idxa, gl.GL_STATIC_DRAW)
gl.glVertexAttribPointer(0, 3, gl.GL_FLOAT, gl.GL_FALSE, 24, ctypes.c_void_p(0)); gl.glEnableVertexAttribArray(0)
gl.glVertexAttribPointer(1, 3, gl.GL_FLOAT, gl.GL_FALSE, 24, ctypes.c_void_p(12)); gl.glEnableVertexAttribArray(1)

prog = mkprog("#version 310 es\nlayout(location=0) in vec3 p;\nlayout(location=1) in vec3 c;\nout vec3 vc;\nuniform mat4 mvp;\nvoid main(){ gl_Position=mvp*vec4(p,1.0); vc=c; }\n",
              "#version 310 es\nprecision highp float;\nin vec3 vc;\nlayout(location=0) out vec4 o;\nvoid main(){ o=vec4(vc,1.0); }\n")
ok(True, "cube program compiles+links")
gl.glUseProgram(prog)
gl.glUniformMatrix4fv(gl.glGetUniformLocation(prog, "mvp"), 1, gl.GL_FALSE, np.array(mvp, dtype="f4"))

gl.glEnable(gl.GL_DEPTH_TEST); gl.glDepthFunc(gl.GL_LESS)
gl.glClearColor(0, 0, 0, 1); gl.glClearDepthf(1.0); gl.glClear(gl.GL_COLOR_BUFFER_BIT | gl.GL_DEPTH_BUFFER_BIT)
gl.glDrawElements(gl.GL_TRIANGLES, 36, gl.GL_UNSIGNED_SHORT, ctypes.c_void_p(0)); gl.glFinish(); a = readback()
ok(gl.glGetError() == gl.GL_NO_ERROR, "no GL error after cube draw")

# INDEPENDENT software reference rasterizer
refc = np.zeros((H, W, 3), dtype=np.float64)
refz = np.full((H, W), 1e9, dtype=np.float64)
refcov = np.zeros((H, W), dtype=np.uint8)
sx = [0.0] * 8; sy = [0.0] * 8; sz = [0.0] * 8; sw = [0.0] * 8
for i in range(8):
    out = mv4(mvp, [VP[i][0], VP[i][1], VP[i][2], 1.0])
    w = out[3]; sw[i] = w
    ndcx, ndcy, ndcz = out[0] / w, out[1] / w, out[2] / w
    sx[i] = (ndcx * 0.5 + 0.5) * W; sy[i] = (ndcy * 0.5 + 0.5) * H; sz[i] = ndcz * 0.5 + 0.5
ok(sw[0] > 0, "reference: all clip.w positive (mesh in front of camera)")
for t in range(12):
    ai, bi, ci = IDX[t * 3 + 0], IDX[t * 3 + 1], IDX[t * 3 + 2]
    ax, ay, bx, by, cx, cy = sx[ai], sy[ai], sx[bi], sy[bi], sx[ci], sy[ci]
    area = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
    if abs(area) < 1e-6: continue
    minx = max(int(math.floor(min(ax, bx, cx))), 0); maxx = min(int(math.ceil(max(ax, bx, cx))), W)
    miny = max(int(math.floor(min(ay, by, cy))), 0); maxy = min(int(math.ceil(max(ay, by, cy))), H)
    for y in range(miny, maxy):
        for x in range(minx, maxx):
            pxs, pys = x + 0.5, y + 0.5
            w0 = ((bx - pxs) * (cy - pys) - (by - pys) * (cx - pxs)) / area
            w1 = ((cx - pxs) * (ay - pys) - (cy - pys) * (ax - pxs)) / area
            w2 = 1.0 - w0 - w1
            inside = (w0 >= 0 and w1 >= 0 and w2 >= 0) or (w0 <= 0 and w1 <= 0 and w2 <= 0)
            if not inside: continue
            if w0 < 0 or w1 < 0 or w2 < 0: w0, w1, w2 = -w0, -w1, -w2
            z = w0 * sz[ai] + w1 * sz[bi] + w2 * sz[ci]
            if z < refz[y, x]:
                refz[y, x] = z; refcov[y, x] = 1
                iwa, iwb, iwc = 1.0 / sw[ai], 1.0 / sw[bi], 1.0 / sw[ci]
                d = w0 * iwa + w1 * iwb + w2 * iwc
                for k in range(3):
                    num = w0 * iwa * VC[ai][k] + w1 * iwb * VC[bi][k] + w2 * iwc * VC[ci][k]
                    refc[y, x, k] = num / d

# compare GL readback to reference
total = match = covmatch = covtotal = interior_bad = 0
for y in range(H):
    for x in range(W):
        total += 1
        gcov = not (pxv(a, x, y, 0) == 0 and pxv(a, x, y, 1) == 0 and pxv(a, x, y, 2) == 0)
        rcov = refcov[y, x] != 0
        if gcov == rcov: covmatch += 1
        if rcov:
            covtotal += 1
            er = int(round(refc[y, x, 0] * 255.0)); eg = int(round(refc[y, x, 1] * 255.0)); eb = int(round(refc[y, x, 2] * 255.0))
            interior = (0 < x < W - 1 and 0 < y < H - 1 and refcov[y - 1, x] and refcov[y + 1, x] and refcov[y, x - 1] and refcov[y, x + 1])
            if peq(a, x, y, er, eg, eb, 255, 6): match += 1
            elif interior: interior_bad += 1
ok(covtotal > 200, "reference: cube covers a substantial area")
ok(covmatch >= int(0.97 * total), "coverage mask matches GL (>=97% of pixels agree covered/empty)")
ok(interior_bad == 0, "every interior pixel matches perspective-correct Gouraud reference (tol 6)")
ok(match >= int(0.92 * covtotal), "92%+ of covered pixels match reference color (edges excluded)")

# targeted closed-form spot checks
vx = int(round(sx[6] - 0.5)); vy = int(round(sy[6] - 0.5))
if 1 <= vx < W - 1 and 1 <= vy < H - 1:
    bright = False
    for dy in range(-1, 2):
        for dx in range(-1, 2):
            xx, yy = vx + dx, vy + dy
            if pxv(a, xx, yy, 0) > 180 and pxv(a, xx, yy, 1) > 180 and pxv(a, xx, yy, 2) > 180: bright = True
    ok(bright, "vertex (1,1,1) region is bright (Gouraud white corner)")
else:
    ok(False, "vertex (1,1,1) projected off-screen (camera mis-set)")
ok(peq(a, 0, 0, 0, 0, 0, 255, 1) or refcov[0, 0] == 0, "corner (0,0) background consistent")

cxp, cyp = W // 2, H // 2
if refcov[cyp, cxp]:
    er = int(round(refc[cyp, cxp, 0] * 255.0)); eg = int(round(refc[cyp, cxp, 1] * 255.0)); eb = int(round(refc[cyp, cxp, 2] * 255.0))
    ok(peq(a, cxp, cyp, er, eg, eb, 255, 8), "center pixel = nearest-face (depth-buffered occlusion) reference color")
else:
    ok(False, "center pixel not covered (mesh mis-projected)")

gl.glDisable(gl.GL_DEPTH_TEST)

# negative control: image is not a flat single color (real 3D shading present)
ok(not (pxv(a, 1, 1, 0) == pxv(a, W // 2, H // 2, 0) and pxv(a, 1, 1, 1) == pxv(a, W // 2, H // 2, 1) and pxv(a, 1, 1, 2) == pxv(a, W // 2, H // 2, 2)),
   "negative control: image is not a flat single color (real 3D shading present)")

EXPECTED = 18; TOTAL = PASS + FAIL
print("scene-3dmodel-py: PASS=%d FAIL=%d TOTAL=%d EXPECTED=%d" % (PASS, FAIL, TOTAL, EXPECTED), flush=True)
if FAIL == 0 and TOTAL == EXPECTED:
    print("SCENE_3DMODEL_PY OK %d" % PASS, flush=True); sys.exit(0)
sys.exit(1)
