/* model_raster.h - barycentric + z-buffer software rasterizer (like the render scene_3dmodel cells).
 *
 * Project mesh vertices through a fixed MVP (model->view->perspective), rasterize each triangle with
 * barycentric coverage and a depth buffer (nearest wins), and produce a coverage mask + an 8-bit depth
 * image. The cube's silhouette under a chosen MVP is analytically known, so its covered-pixel count and
 * depth-occlusion (front face occludes back face) are asserted closed form; suzanne is bound to a
 * calibrated coverage count + depth signature. Deterministic fixed pipeline, integer framebuffer.
 */
#ifndef MODEL_RASTER_H
#define MODEL_RASTER_H

#include "model_common.h"

typedef struct {
    int W, H;
    unsigned char *cover;   /* 0/1 coverage mask */
    unsigned char *depth8;  /* quantized depth 0..255 (255 = far/empty) */
    double *zbuf;           /* float depth, +inf = empty */
} framebuf;

static void fb_init(framebuf *fb, int W, int H) {
    fb->W = W; fb->H = H;
    fb->cover  = (unsigned char*)calloc((size_t)W*H, 1);
    fb->depth8 = (unsigned char*)malloc((size_t)W*H);
    fb->zbuf   = (double*)malloc((size_t)W*H*sizeof(double));
    for (int i = 0; i < W*H; i++) { fb->depth8[i] = 255; fb->zbuf[i] = 1e300; }
}
static void fb_free(framebuf *fb) { free(fb->cover); free(fb->depth8); free(fb->zbuf); }

/* 4x4 * vec4 (column-major-agnostic: we use explicit row math). Homogeneous divide performed by caller. */
typedef struct { double m[16]; } mat4;

static void mat4_identity(mat4 *r) {
    memset(r->m, 0, sizeof r->m);
    r->m[0] = r->m[5] = r->m[10] = r->m[15] = 1.0;
}
static void mat4_mul(mat4 *r, const mat4 *a, const mat4 *b) {
    mat4 t;
    for (int i = 0; i < 4; i++) for (int j = 0; j < 4; j++) {
        double s = 0;
        for (int k = 0; k < 4; k++) s += a->m[i*4+k] * b->m[k*4+j];
        t.m[i*4+j] = s;
    }
    *r = t;
}
/* perspective (fovy radians, aspect, near, far) -> row-major */
static void mat4_perspective(mat4 *r, double fovy, double aspect, double zn, double zf) {
    double fp = 1.0 / tan(fovy * 0.5);
    memset(r->m, 0, sizeof r->m);
    r->m[0]  = fp / aspect;
    r->m[5]  = fp;
    r->m[10] = (zf + zn) / (zn - zf);
    r->m[11] = (2.0 * zf * zn) / (zn - zf);
    r->m[14] = -1.0;
}
/* translate */
static void mat4_translate(mat4 *r, double x, double y, double z) {
    mat4_identity(r); r->m[3] = x; r->m[7] = y; r->m[11] = z;
}
/* rotate about Y then X by fixed angles (a simple deterministic view orientation) */
static void mat4_roty(mat4 *r, double a) {
    mat4_identity(r); double c = cos(a), s = sin(a);
    r->m[0] = c; r->m[2] = s; r->m[8] = -s; r->m[10] = c;
}
static void mat4_rotx(mat4 *r, double a) {
    mat4_identity(r); double c = cos(a), s = sin(a);
    r->m[5] = c; r->m[6] = -s; r->m[9] = s; r->m[10] = c;
}

static void mat4_apply(const mat4 *m, double x, double y, double z, double out[4]) {
    out[0] = m->m[0]*x + m->m[1]*y + m->m[2]*z + m->m[3];
    out[1] = m->m[4]*x + m->m[5]*y + m->m[6]*z + m->m[7];
    out[2] = m->m[8]*x + m->m[9]*y + m->m[10]*z + m->m[11];
    out[3] = m->m[12]*x + m->m[13]*y + m->m[14]*z + m->m[15];
}

/* Rasterize a mesh with the given MVP into fb (barycentric + z-buffer, nearest depth wins). Normalizes
 * NDC depth into [0,1] over [near_z,far_z] for the depth8 buffer. Returns covered pixel count. */
static int raster_mesh(const mesh *m, const mat4 *mvp, framebuf *fb, double ndc_near, double ndc_far) {
    int W = fb->W, H = fb->H;
    for (int ti = 0; ti < m->nt; ti++) {
        const tri_idx *t = &m->t[ti];
        double c0[4], c1[4], c2[4];
        mat4_apply(mvp, m->v[t->a].x, m->v[t->a].y, m->v[t->a].z, c0);
        mat4_apply(mvp, m->v[t->b].x, m->v[t->b].y, m->v[t->b].z, c1);
        mat4_apply(mvp, m->v[t->c].x, m->v[t->c].y, m->v[t->c].z, c2);
        if (c0[3] <= 1e-9 || c1[3] <= 1e-9 || c2[3] <= 1e-9) continue;  /* behind eye */
        /* perspective divide -> NDC */
        double x0 = c0[0]/c0[3], y0 = c0[1]/c0[3], z0 = c0[2]/c0[3];
        double x1 = c1[0]/c1[3], y1 = c1[1]/c1[3], z1 = c1[2]/c1[3];
        double x2 = c2[0]/c2[3], y2 = c2[1]/c2[3], z2 = c2[2]/c2[3];
        /* NDC [-1,1] -> screen */
        double sx0 = (x0*0.5+0.5)*W, sy0 = (1.0-(y0*0.5+0.5))*H;
        double sx1 = (x1*0.5+0.5)*W, sy1 = (1.0-(y1*0.5+0.5))*H;
        double sx2 = (x2*0.5+0.5)*W, sy2 = (1.0-(y2*0.5+0.5))*H;
        int minx = (int)floor(fmin(sx0, fmin(sx1, sx2)));
        int maxx = (int)ceil (fmax(sx0, fmax(sx1, sx2)));
        int miny = (int)floor(fmin(sy0, fmin(sy1, sy2)));
        int maxy = (int)ceil (fmax(sy0, fmax(sy1, sy2)));
        if (minx < 0) minx = 0;
        if (miny < 0) miny = 0;
        if (maxx > W) maxx = W;
        if (maxy > H) maxy = H;
        double area = (sx1-sx0)*(sy2-sy0) - (sx2-sx0)*(sy1-sy0);
        if (fabs(area) < 1e-12) continue;
        for (int py = miny; py < maxy; py++) for (int px = minx; px < maxx; px++) {
            double fx = px + 0.5, fy = py + 0.5;
            double w0 = ((sx1-fx)*(sy2-fy) - (sx2-fx)*(sy1-fy)) / area;
            double w1 = ((sx2-fx)*(sy0-fy) - (sx0-fx)*(sy2-fy)) / area;
            double w2 = 1.0 - w0 - w1;
            if (w0 < 0 || w1 < 0 || w2 < 0) continue;   /* outside triangle */
            double z = w0*z0 + w1*z1 + w2*z2;           /* NDC depth */
            int idx = py*W + px;
            if (z < fb->zbuf[idx]) {
                fb->zbuf[idx] = z;
                fb->cover[idx] = 1;
                double dn = (z - ndc_near) / (ndc_far - ndc_near);
                if (dn < 0) dn = 0;
                if (dn > 1) dn = 1;
                fb->depth8[idx] = (unsigned char)(dn * 254.0);   /* 0..254; 255 stays = empty */
            }
        }
    }
    int cov = 0;
    for (int i = 0; i < W*H; i++) if (fb->cover[i]) cov++;
    return cov;
}

/* Build a canonical MVP that fits a mesh of the given bbox center + scale into the frame. */
static void build_mvp(mat4 *mvp, vec3 center, double dist, double roty, double rotx, int W, int H) {
    mat4 P, V, T, RY, RX, tmp;
    mat4_perspective(&P, 45.0*M_PI/180.0, (double)W/H, 0.1, 100.0);
    mat4_translate(&T, -center.x, -center.y, -center.z);
    mat4_roty(&RY, roty);
    mat4_rotx(&RX, rotx);
    mat4 back; mat4_translate(&back, 0, 0, -dist);
    /* V = back * RX * RY * T  (translate model to origin, rotate, push away) */
    mat4_mul(&tmp, &RX, &RY);
    mat4_mul(&V, &tmp, &T);
    mat4_mul(&V, &back, &V);
    mat4_mul(mvp, &P, &V);
}

#endif /* MODEL_RASTER_H */
