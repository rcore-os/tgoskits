/* model_slice.h - mesh-plane intersection (3D-print slicing).
 *
 * Slice a triangle mesh at plane z = Z into contour segments, then reduce to per-layer perimeter, enclosed
 * area, and segment count. The segment extraction + shoelace mirror render-assets/models/slice_golden.py
 * EXACTLY so the per-layer goldens reproduce:
 *   - for each triangle, an edge (a,b) crosses the plane iff (za<Z && zb>=Z) || (zb<Z && za>=Z); the two
 *     crossing points form one segment (triangles that graze the plane contribute 0 or is dropped when
 *     !=2 crossings).
 *   - perimeter = sum of segment lengths; area = |sum over segments of (x0*y1 - x1*y0)| / 2 (shoelace over
 *     the raw, unordered segment set - valid because each closed loop's directed segments telescope).
 *
 * For primitives the contour is analytically known: a unit cube sliced at any interior Z is a 1x1 square
 * (perimeter 4.0, area 1.0, 8 segments = 2 crossing edges on each of the 4 side quads' 2 triangles). This
 * is the closed-form leg. For suzanne/benchy the per-layer goldens are the calibrated slice_golden.json.
 */
#ifndef MODEL_SLICE_H
#define MODEL_SLICE_H

#include "model_common.h"

typedef struct { double x0, y0, x1, y1; } seg2;

/* Extract crossing segments at plane z. Returns the segment count; fills segs[] (caller-sized). */
static int slice_mesh(const mesh *m, double z, seg2 *segs, int cap) {
    int n = 0;
    for (int ti = 0; ti < m->nt; ti++) {
        const tri_idx *t = &m->t[ti];
        vec3 tv[3] = { m->v[t->a], m->v[t->b], m->v[t->c] };
        double px[2], py[2]; int np = 0;
        for (int i = 0; i < 3 && np < 2; i++) {
            vec3 a = tv[i], b = tv[(i+1)%3];
            double za = a.z, zb = b.z;
            if ((za < z && zb >= z) || (zb < z && za >= z)) {
                double tt = (z - za) / (zb - za);
                px[np] = a.x + tt * (b.x - a.x);
                py[np] = a.y + tt * (b.y - a.y);
                np++;
            }
        }
        if (np == 2 && n < cap) {
            segs[n].x0 = px[0]; segs[n].y0 = py[0];
            segs[n].x1 = px[1]; segs[n].y1 = py[1];
            n++;
        }
    }
    return n;
}

/* perimeter + area over the raw segment set (mirrors contour_stats in slice_golden.py). */
static void contour_stats(const seg2 *segs, int n, double *perim, double *area) {
    double per = 0.0, area2 = 0.0;
    for (int i = 0; i < n; i++) {
        double dx = segs[i].x1 - segs[i].x0, dy = segs[i].y1 - segs[i].y0;
        per += sqrt(dx*dx + dy*dy);
        area2 += segs[i].x0 * segs[i].y1 - segs[i].x1 * segs[i].y0;
    }
    *perim = per;
    *area  = (area2 < 0 ? -area2 : area2) / 2.0;
}

/* Count distinct closed loops by chaining segments end-to-end within a tolerance. Used to assert the loop
 * count (e.g. benchy hull+cabin punch-outs). Non-destructive: works on a scratch copy of endpoints. */
static int contour_loops(const seg2 *segs, int n, double eps) {
    if (n == 0) return 0;
    char *used = (char*)calloc(n, 1);
    int loops = 0;
    for (int s = 0; s < n; s++) {
        if (used[s]) continue;
        loops++;
        double cx = segs[s].x1, cy = segs[s].y1;
        double sx = segs[s].x0, sy = segs[s].y0;
        used[s] = 1;
        int advanced = 1;
        while (advanced) {
            advanced = 0;
            for (int j = 0; j < n; j++) {
                if (used[j]) continue;
                double d0 = hypot(segs[j].x0 - cx, segs[j].y0 - cy);
                double d1 = hypot(segs[j].x1 - cx, segs[j].y1 - cy);
                if (d0 <= eps) { cx = segs[j].x1; cy = segs[j].y1; used[j] = 1; advanced = 1; break; }
                if (d1 <= eps) { cx = segs[j].x0; cy = segs[j].y0; used[j] = 1; advanced = 1; break; }
            }
            if (hypot(cx - sx, cy - sy) <= eps) break;  /* loop closed */
        }
    }
    free(used);
    return loops;
}

#endif /* MODEL_SLICE_H */
