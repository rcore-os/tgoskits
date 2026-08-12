/* model_parse.h - self-written OBJ / STL (binary+ascii) / PLY (ascii+binary) parsers.
 *
 * These formats are small and well specified; a clean parser is not "reinventing a heavy lib". Quads in
 * OBJ / PLY faces are fan-triangulated on load so a mesh is always a triangle soup. All coordinates go
 * through strtod (double) so the parse is deterministic and matches the host golden pass. glTF/glb is a
 * JSON+binary container and is NOT hand-rolled here - see model_gltf.h (vendored cgltf or honest-skip).
 */
#ifndef MODEL_PARSE_H
#define MODEL_PARSE_H

#include "model_common.h"
#include <ctype.h>

/* --------------------------------------------------------------------- */
/*                                  OBJ                                    */
/* --------------------------------------------------------------------- */
/* Parse the "v x y z" vertices and "f a b c [d ...]" faces. Face tokens may be
 * "i", "i/j", "i//k" or "i/j/k"; only the vertex index (before the first '/') is
 * used. Negative (relative) indices are supported. Faces with >3 verts are fan-
 * triangulated (v0, vk, vk+1). Returns 0 on success. */
static int parse_obj(const char *path, mesh *m) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    mesh_init(m);
    char line[4096];
    while (fgets(line, sizeof line, f)) {
        if (line[0] == 'v' && (line[1] == ' ' || line[1] == '\t')) {
            double x, y, z;
            if (sscanf(line + 2, "%lf %lf %lf", &x, &y, &z) == 3)
                mesh_add_vert(m, x, y, z);
        } else if (line[0] == 'f' && (line[1] == ' ' || line[1] == '\t')) {
            int idx[64]; int n = 0;
            char *p = line + 1;
            while (n < 64) {
                while (*p == ' ' || *p == '\t') p++;
                if (*p == '\0' || *p == '\n' || *p == '\r') break;
                long vi = strtol(p, &p, 10);       /* vertex index (1-based, maybe negative) */
                if (vi < 0) vi = m->nv + vi + 1;    /* relative index */
                idx[n++] = (int)(vi - 1);
                while (*p && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r') p++; /* skip /vt/vn */
            }
            for (int k = 1; k + 1 < n; k++)
                mesh_add_tri(m, idx[0], idx[k], idx[k+1]);
        }
    }
    fclose(f);
    return 0;
}

/* --------------------------------------------------------------------- */
/*                                  STL                                    */
/* --------------------------------------------------------------------- */
/* A file is ASCII STL iff it begins with "solid" and "facet" appears in its head
 * (matches slice_golden.py's heuristic); otherwise binary. Binary STL: 80-byte
 * header, uint32 triangle count, then 50 bytes/triangle (normal 3f + 3 verts 9f
 * + 2-byte attr). Every STL triangle emits 3 fresh vertices (no vertex sharing). */
static int stl_is_ascii(const unsigned char *d, size_t n) {
    if (n < 5 || memcmp(d, "solid", 5) != 0) return 0;
    size_t lim = n < 512 ? n : 512;
    for (size_t i = 0; i + 5 <= lim; i++)
        if (memcmp(d + i, "facet", 5) == 0) return 1;
    return 0;
}
/* Parse an STL from an in-memory buffer. Shared by parse_stl and by the negative-control tests, which feed
 * a truncated buffer to assert rejection without an on-disk asset. Rejects a binary STL whose declared
 * triangle count does not fit the buffer (truncated/garbage) rather than accepting a partial mesh. */
static int parse_stl_buf(const unsigned char *d, size_t got, mesh *m) {
    mesh_init(m);
    if (stl_is_ascii(d, got)) {
        char *p = (char*)d;
        double vx[3], vy[3], vz[3]; int vn = 0;
        while (*p) {
            char *nl = strchr(p, '\n');
            /* find "vertex" on this logical line */
            char *v = strstr(p, "vertex");
            char *end = nl ? nl : p + strlen(p);
            if (v && v < end) {
                double x, y, z;
                if (sscanf(v + 6, "%lf %lf %lf", &x, &y, &z) == 3) {
                    vx[vn] = x; vy[vn] = y; vz[vn] = z; vn++;
                    if (vn == 3) {
                        int a = mesh_add_vert(m, vx[0], vy[0], vz[0]);
                        int b = mesh_add_vert(m, vx[1], vy[1], vz[1]);
                        int c = mesh_add_vert(m, vx[2], vy[2], vz[2]);
                        mesh_add_tri(m, a, b, c); vn = 0;
                    }
                }
            }
            if (!nl) break;
            p = nl + 1;
        }
    } else {
        if (got < 84) return -1;
        uint32_t ntri; memcpy(&ntri, d + 80, 4);   /* LE on all our targets */
        /* Reject a truncated binary STL: the header claims ntri triangles at 50 bytes each; if the buffer
         * cannot hold them the file is corrupt and must fail rather than yield a partial mesh. */
        if ((size_t)ntri > (got - 84) / 50) { mesh_free(m); return -1; }
        size_t off = 84;
        for (uint32_t i = 0; i < ntri; i++) {
            float fv[12]; memcpy(fv, d + off + 0, 48);   /* normal(3) + 3 verts(9) */
            int a = mesh_add_vert(m, fv[3], fv[4], fv[5]);
            int b = mesh_add_vert(m, fv[6], fv[7], fv[8]);
            int c = mesh_add_vert(m, fv[9], fv[10], fv[11]);
            mesh_add_tri(m, a, b, c);
            off += 50;
        }
    }
    return 0;
}
static int parse_stl(const char *path, mesh *m) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    if (sz < 0) { fclose(f); return -1; }
    unsigned char *d = (unsigned char*)malloc((size_t)sz + 1);
    if (!d) { fclose(f); return -1; }
    size_t got = fread(d, 1, (size_t)sz, f); fclose(f); d[got] = 0;
    int rc = parse_stl_buf(d, got, m);
    free(d);
    return rc;
}

/* --------------------------------------------------------------------- */
/*                                  PLY                                    */
/* --------------------------------------------------------------------- */
/* Supports ascii and binary_little_endian. Vertex element: reads the x/y/z float
 * properties (by name) and skips the rest. Face element (optional): a list
 * property "uchar int vertex_indices"; polygons are fan-triangulated. Point
 * clouds (no face element) parse fine and yield an empty triangle set.
 *
 * This is a focused PLY reader for the property layouts the carpet uses (bunny =
 * float x y z confidence intensity + uchar/int face list; cube = float x y z +
 * face list). It handles per-property byte sizes for the common scalar types so
 * both ascii and binary parse to identical vertices. */

typedef enum { PT_I8, PT_U8, PT_I16, PT_U16, PT_I32, PT_U32, PT_F32, PT_F64, PT_UNK } ply_type;
static ply_type ply_type_of(const char *s) {
    if (!strcmp(s,"char")||!strcmp(s,"int8"))   return PT_I8;
    if (!strcmp(s,"uchar")||!strcmp(s,"uint8")) return PT_U8;
    if (!strcmp(s,"short")||!strcmp(s,"int16")) return PT_I16;
    if (!strcmp(s,"ushort")||!strcmp(s,"uint16"))return PT_U16;
    if (!strcmp(s,"int")||!strcmp(s,"int32"))   return PT_I32;
    if (!strcmp(s,"uint")||!strcmp(s,"uint32")) return PT_U32;
    if (!strcmp(s,"float")||!strcmp(s,"float32"))return PT_F32;
    if (!strcmp(s,"double")||!strcmp(s,"float64"))return PT_F64;
    return PT_UNK;
}
static int ply_type_size(ply_type t) {
    switch (t) { case PT_I8: case PT_U8: return 1; case PT_I16: case PT_U16: return 2;
        case PT_I32: case PT_U32: case PT_F32: return 4; case PT_F64: return 8; default: return 0; }
}

typedef struct { char name[32]; ply_type type; int is_list; ply_type count_type; ply_type item_type; } ply_prop;

static double ply_read_bin_scalar(const unsigned char *p, ply_type t) {
    switch (t) {
        case PT_I8:  return (double)*(const int8_t*)p;
        case PT_U8:  return (double)*(const uint8_t*)p;
        case PT_I16: { int16_t v; memcpy(&v,p,2); return v; }
        case PT_U16: { uint16_t v; memcpy(&v,p,2); return v; }
        case PT_I32: { int32_t v; memcpy(&v,p,4); return v; }
        case PT_U32: { uint32_t v; memcpy(&v,p,4); return v; }
        case PT_F32: { float v; memcpy(&v,p,4); return v; }
        case PT_F64: { double v; memcpy(&v,p,8); return v; }
        default: return 0.0;
    }
}

/* Parse a PLY from an in-memory buffer (d must be NUL-terminated at d[got]). Shared by parse_ply and by the
 * negative-control tests, which feed a header with no end_header / bad format to assert rejection. */
static int parse_ply_buf(unsigned char *d, size_t got, mesh *m) {
    mesh_init(m);

    /* locate end_header */
    const char *tag = "end_header\n";
    unsigned char *he = NULL;
    for (size_t i = 0; i + 11 <= got; i++)
        if (memcmp(d + i, tag, 11) == 0) { he = d + i + 11; break; }
    if (!he) return -1;

    /* parse header text */
    int is_ascii = 0, is_le = 0;
    int n_vert = 0, n_face = 0;
    ply_prop vprops[32]; int nvp = 0;
    ply_prop fprop; memset(&fprop, 0, sizeof fprop); int have_face = 0;
    int cur = -1;   /* 0=vertex,1=face */
    char hdr[8192]; size_t hn = (size_t)(he - d); if (hn >= sizeof hdr) hn = sizeof hdr - 1;
    memcpy(hdr, d, hn); hdr[hn] = 0;
    /* iterate header lines without strtok_r (keep to plain C11 + libc) */
    for (char *ln = hdr; ln && *ln; ) {
        char *nl = strchr(ln, '\n');
        if (nl) *nl = 0;
        char a[64], b[64], c[64], e[64];
        int k = sscanf(ln, "%63s %63s %63s %63s", a, b, c, e);
        if (k >= 2 && !strcmp(a, "format")) {
            is_ascii = !strcmp(b, "ascii");
            is_le    = !strcmp(b, "binary_little_endian");
        } else if (k >= 3 && !strcmp(a, "element")) {
            if (!strcmp(b, "vertex")) { cur = 0; n_vert = atoi(c); }
            else if (!strcmp(b, "face")) { cur = 1; n_face = atoi(c); have_face = 1; }
            else cur = -1;
        } else if (k >= 3 && !strcmp(a, "property")) {
            if (!strcmp(b, "list")) {
                if (cur == 1) {  /* property list <count> <item> <name> */
                    fprop.is_list = 1;
                    fprop.count_type = ply_type_of(c);
                    fprop.item_type  = ply_type_of(e);
                }
            } else if (cur == 0 && nvp < 32) {
                ply_prop pp; memset(&pp, 0, sizeof pp);
                pp.type = ply_type_of(b); pp.is_list = 0;
                size_t cl = strlen(c); if (cl >= sizeof pp.name) cl = sizeof pp.name - 1;
                memcpy(pp.name, c, cl); pp.name[cl] = 0;
                vprops[nvp++] = pp;
            }
        }
        ln = nl ? nl + 1 : NULL;
    }
    if (!is_ascii && !is_le) return -1;

    /* find x/y/z property indices */
    int ix=-1, iy=-1, iz=-1;
    for (int i = 0; i < nvp; i++) {
        if (!strcmp(vprops[i].name, "x")) ix = i;
        else if (!strcmp(vprops[i].name, "y")) iy = i;
        else if (!strcmp(vprops[i].name, "z")) iz = i;
    }
    if (ix < 0 || iy < 0 || iz < 0) return -1;

    if (is_ascii) {
        /* tokenize the body */
        char *p = (char*)he;
        for (int i = 0; i < n_vert; i++) {
            double vals[32];
            for (int j = 0; j < nvp; j++) vals[j] = strtod(p, &p);
            mesh_add_vert(m, vals[ix], vals[iy], vals[iz]);
        }
        for (int i = 0; i < n_face; i++) {
            long cnt = strtol(p, &p, 10);
            int fi[64]; int fn = 0;
            for (long j = 0; j < cnt && fn < 64; j++) fi[fn++] = (int)strtol(p, &p, 10);
            for (int k = 1; k + 1 < fn; k++) mesh_add_tri(m, fi[0], fi[k], fi[k+1]);
        }
    } else {
        unsigned char *p = he;
        /* per-vertex byte stride + offsets */
        int off_x=0, off_y=0, off_z=0, stride=0;
        for (int i = 0; i < nvp; i++) {
            if (i == ix) off_x = stride;
            if (i == iy) off_y = stride;
            if (i == iz) off_z = stride;
            stride += ply_type_size(vprops[i].type);
        }
        for (int i = 0; i < n_vert; i++) {
            double x = ply_read_bin_scalar(p + off_x, vprops[ix].type);
            double y = ply_read_bin_scalar(p + off_y, vprops[iy].type);
            double z = ply_read_bin_scalar(p + off_z, vprops[iz].type);
            mesh_add_vert(m, x, y, z);
            p += stride;
        }
        if (have_face) {
            int cs = ply_type_size(fprop.count_type);
            int is = ply_type_size(fprop.item_type);
            for (int i = 0; i < n_face; i++) {
                long cnt = (long)ply_read_bin_scalar(p, fprop.count_type); p += cs;
                int fi[64]; int fn = 0;
                for (long j = 0; j < cnt; j++) {
                    int vi = (int)ply_read_bin_scalar(p, fprop.item_type); p += is;
                    if (fn < 64) fi[fn++] = vi;
                }
                for (int k = 1; k + 1 < fn; k++) mesh_add_tri(m, fi[0], fi[k], fi[k+1]);
            }
        }
    }
    return 0;
}
static int parse_ply(const char *path, mesh *m) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    if (sz < 0) { fclose(f); return -1; }
    unsigned char *d = (unsigned char*)malloc((size_t)sz + 1);
    if (!d) { fclose(f); return -1; }
    size_t got = fread(d, 1, (size_t)sz, f); fclose(f); d[got] = 0;
    int rc = parse_ply_buf(d, got, m);
    free(d);
    return rc;
}

#endif /* MODEL_PARSE_H */
