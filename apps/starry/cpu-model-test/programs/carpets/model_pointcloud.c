/* model_pointcloud - PLY point cloud (cell 4).
 *
 * Closed form (asset-independent, staged sphere_pc.ply): a Fibonacci-sphere-sampled cloud of N points at
 * radius r. Assert the exact vertex count, that the centroid is the origin (|c| < 1e-3), and that EVERY
 * point is at distance r from the origin (rmax-rmin < 1e-4, |r-2.5| tiny) - a fully deterministic
 * closed-form leg where the geometry is known analytically.
 *
 * Mutation control (in-memory, asset-independent): a fixed synthetic cloud has its 16^3 spatial-hash
 * signature computed, then one point is displaced and one point is dropped; each is asserted to change the
 * signature, so the sensitivity claimed for the golden below is exercised in code, not just described.
 *
 * Golden (calibrated, from render-assets/pointcloud/bunny.ply via gen_goldens.py): the Stanford bunny,
 * exact vertex count 35947, bounding box, centroid, and a 16^3 spatial-hash occupancy signature. Because
 * the mutation control proves the signature flips on a single displaced or dropped point, this golden
 * genuinely detects a corrupted cloud. Honest-skip if bunny.ply is absent.
 */
#include "model_common.h"
#include "model_parse.h"
#include "model_pointcloud.h"

/* Build a small deterministic synthetic cloud so the mutation control needs no asset. Points are on a fixed
 * integer lattice; seed 0x233 is only a label - the layout is fully determined, no randomness. */
static void make_synthetic_cloud(mesh *m) {
    mesh_init(m);
    for (int i = 0; i < 8; i++)
        for (int j = 0; j < 8; j++)
            mesh_add_vert(m, (double)i, (double)j, (double)((i + j) % 5));
}

int main(void) {
    gate g; gate_init(&g, "MODEL_POINTCLOUD");
    char path[512];

    /* -------- mutation sensitivity (in-memory, asset-independent) --------
     * The spatial-hash signature must actually change when the geometry changes, otherwise the golden
     * comparison on the bunny below could never detect a corrupted cloud. Compute the signature of a fixed
     * synthetic cloud, then (a) displace exactly one point by a known delta and assert the signature flips,
     * and (b) drop exactly one point and assert both the count and the signature change. seed 0x233. */
    {
        mesh base; make_synthetic_cloud(&base);
        vec3 lo, hi; mesh_bbox(&base, &lo, &hi);
        char sig0[65]; pc_spatial_sig(&base, lo, hi, 16, sig0);

        /* displace one point far enough to land in a different 16^3 cell (delta 3.0 over a span of 7) */
        mesh moved; make_synthetic_cloud(&moved);
        moved.v[0].x += 3.0;
        vec3 lo2, hi2; mesh_bbox(&moved, &lo2, &hi2);
        char sig1[65]; pc_spatial_sig(&moved, lo2, hi2, 16, sig1);
        gate_check(&g, moved.nv == base.nv, "mutation: displaced cloud keeps point count");
        gate_check(&g, strcmp(sig0, sig1) != 0, "mutation: one displaced point flips the signature");

        /* drop the last point: count and signature both change */
        mesh dropped; make_synthetic_cloud(&dropped);
        dropped.nv -= 1;
        vec3 lo3, hi3; mesh_bbox(&dropped, &lo3, &hi3);
        char sig2[65]; pc_spatial_sig(&dropped, lo3, hi3, 16, sig2);
        gate_check(&g, dropped.nv == base.nv - 1, "mutation: dropped one point lowers the count");
        gate_check(&g, strcmp(sig0, sig2) != 0, "mutation: dropping one point flips the signature");

        mesh_free(&base); mesh_free(&moved); mesh_free(&dropped);
    }

    /* -------- closed-form: synthetic sphere cloud -------- */
    mesh sph; model_path(path, sizeof path, "sphere_pc.ply");
    if (parse_ply(path, &sph) == 0 && sph.nv > 0) {
        gate_check(&g, sph.nv == 4000, "sphere_pc 4000 points");
        vec3 c; pc_centroid(&sph, &c);
        double cm = sqrt(c.x*c.x + c.y*c.y + c.z*c.z);
        char m[96];
        snprintf(m, sizeof m, "sphere centroid at origin |c|=%.6f<1e-3", cm);
        gate_check(&g, cm < 1e-3, m);
        vec3 origin = {0,0,0};
        double rmin, rmax; pc_radius_range(&sph, origin, &rmin, &rmax);
        snprintf(m, sizeof m, "sphere all points at r: rmin=%.6f rmax=%.6f", rmin, rmax);
        gate_check(&g, (rmax - rmin) < 1e-4, m);
        snprintf(m, sizeof m, "sphere radius == 2.5 (rmin=%.6f rmax=%.6f)", rmin, rmax);
        gate_check(&g, approx(rmin, 2.5, 1e-4) && approx(rmax, 2.5, 1e-4), m);
        /* centroid-relative radius equals origin-relative radius (centroid ~ origin) */
        double rminc, rmaxc; pc_radius_range(&sph, c, &rminc, &rmaxc);
        gate_check(&g, approx(rmaxc - rminc, 0.0, 1e-3), "sphere spread about centroid tight");
        mesh_free(&sph);
    } else {
        fprintf(stderr, "  SKIP: sphere_pc.ply absent under %s - closed-form sphere leg skipped\n", model_dir());
        gate_check(&g, 1, "sphere_pc absent (honest-skip)");
    }

    /* -------- calibrated golden: Stanford bunny -------- */
    mesh bunny; model_path(path, sizeof path, "bunny.ply");
    if (parse_ply(path, &bunny) == 0 && bunny.nv > 0) {
        gate_check(&g, bunny.nv == 35947, "bunny 35947 vertices (exact)");
        vec3 lo, hi; mesh_bbox(&bunny, &lo, &hi);
        gate_check(&g, approx(lo.x,-0.0946899,1e-5)&&approx(hi.x,0.0610091,1e-5), "bunny bbox x");
        gate_check(&g, approx(lo.y, 0.0329874,1e-5)&&approx(hi.y,0.187321,1e-5),  "bunny bbox y");
        gate_check(&g, approx(lo.z,-0.0618736,1e-5)&&approx(hi.z,0.0587997,1e-5), "bunny bbox z");
        vec3 c; pc_centroid(&bunny, &c);
        gate_check(&g, approx(c.x,-0.02675991,1e-6), "bunny centroid x");
        gate_check(&g, approx(c.y, 0.09521606,1e-6), "bunny centroid y");
        gate_check(&g, approx(c.z, 0.00894711,1e-6), "bunny centroid z");
        char sig[65]; pc_spatial_sig(&bunny, lo, hi, 16, sig);
        /* CALIBRATED_BUNNY_SIG16 */
        gate_check(&g, strcmp(sig, "00c9abb8755a78412668fb88af6d3086ec939cf1bc003e90b661b4f01fe93c02") == 0, "bunny 16^3 spatial-hash sig");
        if (strcmp(sig, "00c9abb8755a78412668fb88af6d3086ec939cf1bc003e90b661b4f01fe93c02") != 0)
            fprintf(stderr, "  bunny sig=%s\n", sig);
        mesh_free(&bunny);
    } else {
        fprintf(stderr, "  SKIP: bunny.ply absent under %s (honest-skip)\n", model_dir());
        gate_check(&g, 1, "bunny absent (honest-skip)");
    }

    return gate_finish(&g);
}
