/* model_common.h - shared primitives for the cpu-model-test carpet (a "pyte for 3D models").
 *
 * Each cell drives a real geometry pipeline the carpet implements itself - OBJ/STL/PLY mesh parsing,
 * mesh-plane intersection (3D-print slicing), a barycentric+z-buffer software rasterizer, and point-cloud
 * loading - and asserts the output against a golden that is either CLOSED FORM (a unit cube sliced at any
 * interior Z is a 1x1 square: perimeter 4.0, area 1.0; a sphere-sampled cloud has centroid at the origin
 * and every point at distance r) or a value calibrated once host-side with this exact code (bunny.ply
 * count/bbox/centroid/spatial-hash signature; suzanne render signature; per-layer slice goldens in
 * render-assets/golden/slice_golden.json). "Model loaded" alone is NOT a test here.
 *
 * The OBJ/STL/PLY parsers and the slicer/rasterizer are self-written because these formats are small and
 * well-specified - a clean parser is not "reinventing a heavy lib". Only glTF/glb, which is a JSON+binary
 * container, is either parsed with a vendored single-header (cgltf.h) or honest-skipped, never hand-rolled.
 *
 * Determinism: the slicer's raw-segment shoelace matches render-assets/models/slice_golden.py exactly, so
 * the per-layer perimeter/area goldens reproduce across arches. The spatial-hash signature quantizes into a
 * fixed 16^3 integer grid and hashes the LE uint32 counts, so it is integer-exact given the same parsed
 * doubles. All float parsing goes through strtod (double), matching the host golden pass.
 */
#ifndef MODEL_COMMON_H
#define MODEL_COMMON_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <math.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* -------- asset locations -------- */
static const char *model_dir(void) {
    const char *d = getenv("MODEL_DIR");
    if (d && *d) return d;
    d = getenv("ASSET_DIR");
    if (d && *d) return d;
    return "/opt/cpu-model-test/assets";
}
static const char *model_path(char *buf, size_t n, const char *name) {
    snprintf(buf, n, "%s/%s", model_dir(), name);
    return buf;
}

/* -------- self-written SHA-256 (over the spatial-hash count array / render buffer) -------- */
typedef struct { uint32_t h[8]; uint64_t len; unsigned char buf[64]; size_t n; } sha256_ctx;
static const uint32_t SHA_K[64] = {
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2 };
static uint32_t sha_ror(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }
static void sha256_init(sha256_ctx *c) {
    c->h[0]=0x6a09e667; c->h[1]=0xbb67ae85; c->h[2]=0x3c6ef372; c->h[3]=0xa54ff53a;
    c->h[4]=0x510e527f; c->h[5]=0x9b05688c; c->h[6]=0x1f83d9ab; c->h[7]=0x5be0cd19;
    c->len = 0; c->n = 0;
}
static void sha256_block(sha256_ctx *c, const unsigned char *p) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
        w[i] = (p[i*4]<<24)|(p[i*4+1]<<16)|(p[i*4+2]<<8)|p[i*4+3];
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = sha_ror(w[i-15],7)^sha_ror(w[i-15],18)^(w[i-15]>>3);
        uint32_t s1 = sha_ror(w[i-2],17)^sha_ror(w[i-2],19)^(w[i-2]>>10);
        w[i] = w[i-16]+s0+w[i-7]+s1;
    }
    uint32_t a=c->h[0],b=c->h[1],cc=c->h[2],d=c->h[3],e=c->h[4],f=c->h[5],g=c->h[6],h=c->h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t S1 = sha_ror(e,6)^sha_ror(e,11)^sha_ror(e,25);
        uint32_t ch = (e&f)^((~e)&g);
        uint32_t t1 = h+S1+ch+SHA_K[i]+w[i];
        uint32_t S0 = sha_ror(a,2)^sha_ror(a,13)^sha_ror(a,22);
        uint32_t maj = (a&b)^(a&cc)^(b&cc);
        uint32_t t2 = S0+maj;
        h=g; g=f; f=e; e=d+t1; d=cc; cc=b; b=a; a=t1+t2;
    }
    c->h[0]+=a; c->h[1]+=b; c->h[2]+=cc; c->h[3]+=d; c->h[4]+=e; c->h[5]+=f; c->h[6]+=g; c->h[7]+=h;
}
static void sha256_update(sha256_ctx *c, const void *data, size_t len) {
    const unsigned char *p = (const unsigned char *)data;
    c->len += len;
    while (len > 0) {
        size_t take = 64 - c->n; if (take > len) take = len;
        memcpy(c->buf + c->n, p, take);
        c->n += take; p += take; len -= take;
        if (c->n == 64) { sha256_block(c, c->buf); c->n = 0; }
    }
}
static void sha256_hex(sha256_ctx *c, char out[65]) {
    uint64_t bits = c->len * 8;
    unsigned char pad = 0x80;
    sha256_update(c, &pad, 1);
    unsigned char z = 0;
    while (c->n != 56) sha256_update(c, &z, 1);
    unsigned char lb[8];
    for (int i = 0; i < 8; i++) lb[i] = (bits >> (56 - i*8)) & 0xff;
    sha256_update(c, lb, 8);
    for (int i = 0; i < 8; i++) sprintf(out + i*8, "%08x", c->h[i]);
}
static void sha256_buf(const void *buf, size_t len, char out[65]) {
    sha256_ctx c; sha256_init(&c); sha256_update(&c, buf, len); sha256_hex(&c, out);
}

/* -------- three-gate marker (identical semantics to the image/font carpets) -------- */
typedef struct { int pass, total, fail; const char *name; } gate;
static void gate_init(gate *g, const char *name) { g->pass = g->total = g->fail = 0; g->name = name; }
static void gate_check(gate *g, int cond, const char *msg) {
    g->total++;
    if (cond) g->pass++;
    else { g->fail++; fprintf(stderr, "  FAIL: %s\n", msg); }
}
static int gate_finish(gate *g) {
    if (g->fail == 0 && g->total == g->pass && g->total > 0) {
        printf("%s OK %d\n", g->name, g->total);
        return 0;
    }
    printf("%s FAILED pass=%d total=%d fail=%d\n", g->name, g->pass, g->total, g->fail);
    return 1;
}

/* ======================================================================== */
/*                     geometry: mesh + point-cloud types                    */
/* ======================================================================== */

typedef struct { double x, y, z; } vec3;
typedef struct { int a, b, c; } tri_idx;   /* indices into the vertex array */

typedef struct {
    vec3 *v;        int nv, cap_v;
    tri_idx *t;     int nt, cap_t;   /* triangles (quads are fan-triangulated on load) */
} mesh;

static void mesh_init(mesh *m) { memset(m, 0, sizeof *m); }
static void mesh_free(mesh *m) { free(m->v); free(m->t); memset(m, 0, sizeof *m); }
static int mesh_add_vert(mesh *m, double x, double y, double z) {
    if (m->nv == m->cap_v) { m->cap_v = m->cap_v ? m->cap_v*2 : 256;
        m->v = (vec3*)realloc(m->v, (size_t)m->cap_v*sizeof(vec3)); }
    m->v[m->nv].x = x; m->v[m->nv].y = y; m->v[m->nv].z = z; return m->nv++;
}
static void mesh_add_tri(mesh *m, int a, int b, int c) {
    if (m->nt == m->cap_t) { m->cap_t = m->cap_t ? m->cap_t*2 : 256;
        m->t = (tri_idx*)realloc(m->t, (size_t)m->cap_t*sizeof(tri_idx)); }
    m->t[m->nt].a = a; m->t[m->nt].b = b; m->t[m->nt].c = c; m->nt++;
}

/* bounding box over the vertex array */
static void mesh_bbox(const mesh *m, vec3 *lo, vec3 *hi) {
    lo->x = lo->y = lo->z =  1e300;
    hi->x = hi->y = hi->z = -1e300;
    for (int i = 0; i < m->nv; i++) {
        vec3 p = m->v[i];
        if (p.x < lo->x) lo->x = p.x;
        if (p.x > hi->x) hi->x = p.x;
        if (p.y < lo->y) lo->y = p.y;
        if (p.y > hi->y) hi->y = p.y;
        if (p.z < lo->z) lo->z = p.z;
        if (p.z > hi->z) hi->z = p.z;
    }
}
static int approx(double a, double b, double tol) { double d = a - b; return (d < 0 ? -d : d) <= tol; }

#endif /* MODEL_COMMON_H */
