/* model_pointcloud.h - point-cloud stats: bbox, centroid, spatial-hash signature.
 *
 * A PLY point cloud parses (via model_parse.h parse_ply) into the mesh vertex array (faces, if any, are
 * ignored for cloud stats). We compute:
 *   - exact vertex count (an integer invariant of the file),
 *   - bounding box + centroid (double reductions),
 *   - a spatial-hash signature: quantize every point into a fixed 16^3 integer grid over the bbox and
 *     SHA-256 the little-endian uint32 occupancy counts. Integer-exact given the same parsed doubles. The
 *     model_pointcloud.c mutation control displaces one point and drops one point and asserts the signature
 *     changes in each case, so this reproducible golden is proven to flip on a single altered point.
 * For the synthetic sphere cloud the centroid is the origin and every point is at distance r - closed form.
 */
#ifndef MODEL_POINTCLOUD_H
#define MODEL_POINTCLOUD_H

#include "model_common.h"

static void pc_centroid(const mesh *m, vec3 *c) {
    double sx=0, sy=0, sz=0;
    for (int i = 0; i < m->nv; i++) { sx += m->v[i].x; sy += m->v[i].y; sz += m->v[i].z; }
    double n = m->nv ? (double)m->nv : 1.0;
    c->x = sx/n; c->y = sy/n; c->z = sz/n;
}

/* max distance of any point from a given center, and min - to assert "all at radius r" tightly. */
static void pc_radius_range(const mesh *m, vec3 c, double *rmin, double *rmax) {
    double lo = 1e300, hi = -1e300;
    for (int i = 0; i < m->nv; i++) {
        double dx = m->v[i].x - c.x, dy = m->v[i].y - c.y, dz = m->v[i].z - c.z;
        double r = sqrt(dx*dx + dy*dy + dz*dz);
        if (r < lo) lo = r;
        if (r > hi) hi = r;
    }
    *rmin = lo; *rmax = hi;
}

/* 16^3 spatial-hash occupancy signature over the bbox. Mirrors gen_goldens.py spatial_hash_sig. */
static void pc_spatial_sig(const mesh *m, vec3 lo, vec3 hi, int grid, char out[65]) {
    double sx = (hi.x > lo.x) ? (hi.x - lo.x) : 1.0;
    double sy = (hi.y > lo.y) ? (hi.y - lo.y) : 1.0;
    double sz = (hi.z > lo.z) ? (hi.z - lo.z) : 1.0;
    size_t ncell = (size_t)grid*grid*grid;
    uint32_t *counts = (uint32_t*)calloc(ncell, sizeof(uint32_t));
    for (int i = 0; i < m->nv; i++) {
        int ix = (int)((m->v[i].x - lo.x) / sx * grid); if (ix<0) ix=0; if (ix>=grid) ix=grid-1;
        int iy = (int)((m->v[i].y - lo.y) / sy * grid); if (iy<0) iy=0; if (iy>=grid) iy=grid-1;
        int iz = (int)((m->v[i].z - lo.z) / sz * grid); if (iz<0) iz=0; if (iz>=grid) iz=grid-1;
        counts[((size_t)ix*grid + iy)*grid + iz]++;
    }
    /* hash the LE uint32 counts */
    unsigned char *bytes = (unsigned char*)malloc(ncell * 4);
    for (size_t i = 0; i < ncell; i++) {
        bytes[i*4+0] = counts[i] & 0xff;
        bytes[i*4+1] = (counts[i]>>8) & 0xff;
        bytes[i*4+2] = (counts[i]>>16) & 0xff;
        bytes[i*4+3] = (counts[i]>>24) & 0xff;
    }
    sha256_buf(bytes, ncell*4, out);
    free(counts); free(bytes);
}

#endif /* MODEL_POINTCLOUD_H */
